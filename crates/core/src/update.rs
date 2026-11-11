use crate::{
    Error, Result,
    protocol::{self, envelope::Payload, *},
    resource::Control,
    store::Writer,
};
use std::{
    collections::BTreeMap,
    io::{BufReader, Read},
    path::Path,
    process::{Command, Stdio},
};

pub struct WorkerCommand {
    pub program: String,
    pub args: Vec<String>,
    pub request: AnalyzeRequest,
    pub execution_grant: crate::settings::ProjectExecutionGrant,
}

pub fn index(database: &Path, workers: Vec<WorkerCommand>) -> Result<u64> {
    index_scoped(database, workers, None)
}
pub fn index_scoped(
    database: &Path,
    workers: Vec<WorkerCommand>,
    replacements: Option<&[String]>,
) -> Result<u64> {
    index_checked(database, workers, replacements, &mut |_| {}, || Ok(()))
}

pub fn index_checked(
    database: &Path,
    workers: Vec<WorkerCommand>,
    replacements: Option<&[String]>,
    progress: &mut impl FnMut(Progress),
    before_publish: impl FnOnce() -> Result<()>,
) -> Result<u64> {
    index_controlled(
        database,
        workers,
        replacements,
        progress,
        before_publish,
        &Control::default(),
    )
}
pub fn index_controlled(
    database: &Path,
    workers: Vec<WorkerCommand>,
    replacements: Option<&[String]>,
    progress: &mut impl FnMut(Progress),
    before_publish: impl FnOnce() -> Result<()>,
    control: &Control,
) -> Result<u64> {
    index_published(
        database,
        workers,
        replacements,
        progress,
        before_publish,
        control,
        Publication::Serving(None),
    )
}
#[derive(Clone, Copy)]
pub(crate) enum Publication<'a> {
    Serving(Option<&'a crate::cursor::CursorService>),
    Replacement(&'a crate::recovery::Candidate),
}
pub(crate) fn index_published(
    database: &Path,
    workers: Vec<WorkerCommand>,
    replacements: Option<&[String]>,
    progress: &mut impl FnMut(Progress),
    before_publish: impl FnOnce() -> Result<()>,
    control: &Control,
    publication: Publication<'_>,
) -> Result<u64> {
    control.check()?;
    // No writer, toolchain, dependency setup or process is opened before the
    // trusted host's separate identity-bound authorization has been checked.
    for worker in &workers {
        worker
            .execution_grant
            .check_root(Path::new(&worker.request.workspace_root))?;
    }
    let execution_grants: Vec<_> = workers.iter().map(|w| w.execution_grant.clone()).collect();
    let mut writer = match publication {
        Publication::Replacement(candidate) => Writer::open_destination(
            database,
            &candidate.database,
            None,
            candidate.minimum_generation,
            control,
        )?,
        Publication::Serving(_) => Writer::open_controlled(database, replacements, control)?,
    };
    let preparation_path = database.with_extension("preparation.json");
    let mut preparations: BTreeMap<String, PreparationResult> =
        match std::fs::read(&preparation_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
    for (number, mut worker) in workers.into_iter().enumerate() {
        let requested_policy = protocol::prepare_analysis_policy(&mut worker.request)?;
        let target_policy = worker.request.target_equivalence_policy;
        if target_policy != 0 && target_policy != crate::grouping::POLICY_VERSION {
            return Err(Error::UpdateRequired(
                "unsupported target equivalence policy".into(),
            ));
        }
        let classification_policy = worker.request.test_classification_policy;
        if classification_policy != 0 && classification_policy != crate::testing::POLICY_VERSION {
            return Err(Error::UpdateRequired(
                "unsupported test classification policy".into(),
            ));
        }
        worker.request.preparations = preparations.values().cloned().collect();
        let job_id = format!("{}-{number}", writer.generation);
        control.check()?;
        let mut child = control
            .spawn(
                Command::new(&worker.program)
                    .args(&worker.args)
                    .current_dir(&worker.request.workspace_root)
                    .env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
                    .env("DOTNET_NOLOGO", "1")
                    .env(
                        "KNOW_CODE_BUILD_PROFILE",
                        if cfg!(debug_assertions) {
                            "debug"
                        } else {
                            "release"
                        },
                    )
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped()),
            )
            .map_err(|e| match e {
                Error::Io(e) => Error::Provider(format!(
                    "cannot launch {}: {e}; install the required toolchain and put it on PATH",
                    worker.program
                )),
                other => other,
            })?;
        let mut stderr = child.child.stderr.take().expect("piped stderr");
        let errors = std::thread::spawn(move || {
            let mut retained = std::collections::VecDeque::with_capacity(8192);
            let mut buffer = [0; 1024];
            while let Ok(n) = stderr.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                for byte in &buffer[..n] {
                    if retained.len() == 8192 {
                        retained.pop_front();
                    }
                    retained.push_back(*byte);
                }
            }
            String::from_utf8_lossy(&retained.into_iter().collect::<Vec<_>>()).into_owned()
        });
        let execution = (|| -> Result<()> {
            protocol::write_frame(
                &mut child.child.stdin.take().expect("piped stdin"),
                &Envelope {
                    protocol_version: VERSION,
                    job_id: job_id.clone(),
                    payload: Some(Payload::Analyze(worker.request)),
                },
            )?;
            let mut stdout = BufReader::new(child.child.stdout.take().expect("piped stdout"));
            let mut hello = false;
            let mut completed = false;
            while let Some(message) = protocol::read_frame(&mut stdout)? {
                control.check()?;
                if message.job_id != job_id || completed {
                    return Err(Error::Protocol(
                        "unexpected job identity or messages after completion".into(),
                    ));
                }
                match message.payload {
                    Some(Payload::Provider(info)) if !hello => {
                        protocol::check_analysis_policy(requested_policy, &info)?;
                        if target_policy != 0
                            && (info.target_equivalence_policy != target_policy
                                || info.language != "csharp")
                        {
                            return Err(Error::UpdateRequired("worker must acknowledge target equivalence policy; update the runtime bundle".into()));
                        }
                        if classification_policy != 0
                            && (info.test_classification_policy != classification_policy
                                || !["typescript", "csharp"].contains(&info.language.as_str()))
                        {
                            return Err(Error::UpdateRequired("worker must acknowledge test classification policy; update the runtime bundle".into()));
                        }
                        eprintln!(
                            "analyzing {} with compiler {}",
                            info.language, info.compiler_version
                        );
                        hello = true;
                    }
                    Some(Payload::Complete(done)) if hello && done.success => completed = true,
                    Some(Payload::Progress(status)) if hello => {
                        if status.stage.len() > 64
                            || status.project_path.len() > 4096
                            || status.message.len() > 8192
                        {
                            return Err(Error::Protocol("oversized progress message".into()));
                        }
                        progress(status);
                    }
                    Some(Payload::Preparation(state)) if hello => {
                        if state.key.is_empty()
                            || state.key.len() > 4096
                            || state.fingerprint.len() > 128
                            || state.error.len() > 32768
                        {
                            return Err(Error::Protocol("invalid preparation checkpoint".into()));
                        }
                        preparations.insert(state.key.clone(), state);
                        let next = preparation_path.with_extension("json.next");
                        std::fs::write(&next, serde_json::to_vec(&preparations)?)?;
                        std::fs::rename(next, &preparation_path)?;
                    }
                    Some(payload) if hello => {
                        if let Payload::Compilation(compilation) = &payload
                            && target_policy != 0
                            && compilation.target_equivalence_policy != target_policy
                        {
                            return Err(Error::Protocol(
                                "requested target equivalence metadata missing or incompatible"
                                    .into(),
                            ));
                        }
                        if classification_policy != 0 {
                            let classification = match &payload {
                                Payload::Compilation(c) => Some(c.test_classification.as_ref()),
                                Payload::Source(s) if s.test_classification.is_some() => {
                                    Some(s.test_classification.as_ref())
                                }
                                _ => None,
                            };
                            if classification.is_some_and(|c| {
                                c.is_none_or(|c| c.policy_version != classification_policy)
                            }) {
                                return Err(Error::Protocol("requested test classification metadata missing or incompatible".into()));
                            }
                        }
                        writer.stage(payload)?;
                    }
                    _ => {
                        return Err(Error::Protocol(
                            "missing provider handshake or invalid completion".into(),
                        ));
                    }
                }
            }
            if !completed {
                return Err(Error::Protocol(
                    "worker ended without successful completion; previous generation preserved"
                        .into(),
                ));
            }
            Ok(())
        })();
        if execution.is_err() {
            child.terminate();
        }
        let exit = child.wait()?;
        let details = errors.join().unwrap_or_default();
        control.check()?;
        execution.map_err(|error| match error {
            Error::UpdateRequired(_)
            | Error::IndexCorrupt { .. }
            | Error::IndexRecovering(_)
            | Error::ResourceLimited(_)
            | Error::DiskPressure(_)
            | Error::Cancelled
            | Error::Sqlite(_)
            | Error::Io(_) => error,
            other => Error::Provider(format!("{other}\n{details}")),
        })?;
        if !exit.success() {
            return Err(Error::Provider(format!(
                "worker {} exited with {exit}; previous generation preserved\n{details}",
                worker.program
            )));
        }
    }
    control.check()?;
    for grant in execution_grants {
        grant.check()?;
    }
    before_publish()?;
    match publication {
        Publication::Replacement(_) => writer.publish_controlled(&control.staging()),
        Publication::Serving(Some(cursors)) => cursors.publish(database, writer, control),
        Publication::Serving(None) => writer.publish_controlled(control),
    }
}
