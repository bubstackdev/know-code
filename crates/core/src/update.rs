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
        None,
    )
}
pub(crate) fn index_published(
    database: &Path,
    workers: Vec<WorkerCommand>,
    replacements: Option<&[String]>,
    progress: &mut impl FnMut(Progress),
    before_publish: impl FnOnce() -> Result<()>,
    control: &Control,
    cursors: Option<&crate::cursor::CursorService>,
) -> Result<u64> {
    control.check()?;
    let mut writer = Writer::open_controlled(database, replacements, control)?;
    let preparation_path = database.with_extension("preparation.json");
    let mut preparations: BTreeMap<String, PreparationResult> =
        match std::fs::read(&preparation_path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => return Err(error.into()),
        };
    for (number, mut worker) in workers.into_iter().enumerate() {
        let requested_policy = protocol::prepare_analysis_policy(&mut worker.request)?;
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
                    Some(payload) if hello => writer.stage(payload)?,
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
            Error::UpdateRequired(_) => error,
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
    before_publish()?;
    if let Some(cursors) = cursors {
        cursors.publish(database, writer, control)
    } else {
        writer.publish_controlled(control)
    }
}
