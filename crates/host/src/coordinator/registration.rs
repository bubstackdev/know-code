//! Enrollment preparation outside the coordinator's shared entry lock.
use super::*;

pub(super) fn register_workspace(
    shared: &Shared,
    data: &Path,
    request: &ControlRequest,
    discovered: bool,
) -> AnyResult<WorkspaceStatus> {
    register_with_adoption(shared, data, request, discovered, recovery::adopt)
}

fn register_with_adoption(
    shared: &Shared,
    data: &Path,
    request: &ControlRequest,
    discovered: bool,
    adopt: impl FnOnce(&Path, u64) -> know_code_core::Result<()>,
) -> AnyResult<WorkspaceStatus> {
    let root = std::fs::canonicalize(&request.workspace_root)?;
    let actual_identity = workspace::identity(&root)?;
    let database = workspace::database_path(&root, data)
        .to_string_lossy()
        .into_owned();
    let configuration = request.configuration.as_str();
    // Capture only enrollment identity and the trusted generation. Adoption
    // can wait for a lifecycle owner; never retain the global entries lock.
    let (registration, generation) = {
        let entries = shared.entries.lock().unwrap();
        if discovered {
            let suppressed: bool = shared.registry.lock().unwrap().query_row(
                "SELECT EXISTS(SELECT 1 FROM discovery_suppressed WHERE root=?)",
                [root.to_string_lossy().as_ref()],
                |row| row.get(0),
            )?;
            if suppressed {
                return Err("repository enrollment is explicitly suppressed".into());
            }
            if let Some(entry) = entries.get(&database) {
                if entry.identity != actual_identity {
                    return Err(
                        "workspace identity changed; explicitly unregister the old workspace"
                            .into(),
                    );
                }
                return Ok(entry.snapshot());
            }
        }
        let current = entries.get(&database);
        if current.is_some_and(|entry| entry.identity != actual_identity) {
            return Err(
                "workspace identity changed; unwatch before registering the replacement".into(),
            );
        }
        (
            current.map(|entry| entry.registration),
            current.map_or(0, |entry| entry.status.generation),
        )
    };
    std::fs::create_dir_all(
        Path::new(&database)
            .parent()
            .ok_or("workspace database parent missing")?,
    )?;
    adopt(Path::new(&database), generation)?;
    // Do not attach an old preparation to a replacement repository/enrollment.
    // The adopted selector remains owned by recovery; no physical path or
    // readiness snapshot from before its lifecycle lock is reused here.
    if workspace::identity(&root)? != actual_identity {
        return Err("workspace identity changed during registration; start a new request".into());
    }
    let persistent = {
        let mut entries = shared.entries.lock().unwrap();
        if registration.is_some()
            && entries.get(&database).map(|entry| entry.registration) != registration
        {
            return Err(
                "workspace registration changed during preparation; start a new request".into(),
            );
        }

        if discovered {
            let suppressed: bool = shared.registry.lock().unwrap().query_row(
                "SELECT EXISTS(SELECT 1 FROM discovery_suppressed WHERE root=?)",
                [root.to_string_lossy().as_ref()],
                |r| r.get(0),
            )?;
            if suppressed {
                return Err("repository enrollment is explicitly suppressed".into());
            }
            if let Some(entry) = entries.get(&database) {
                if entry.identity != actual_identity {
                    return Err(
                        "workspace identity changed; explicitly unregister the old workspace"
                            .into(),
                    );
                }
                return Ok(entry.snapshot());
            }
        }
        if entries
            .get(&database)
            .is_some_and(|entry| entry.identity != actual_identity)
        {
            return Err(
                "workspace identity changed; unwatch before registering the replacement".into(),
            );
        }
        // Never promote a new personal choice into legacy fallback state.
        let legacy = entries
            .get(&database)
            .map(|entry| entry.configuration.clone())
            .unwrap_or_default();
        let mut settings_changed = false;
        if !configuration.is_empty() {
            let mut personal = settings::personal(&root, data)?;
            if !matches!(&personal.csharp.configuration, settings::Selection::Named(value) if value == configuration)
            {
                personal.csharp.configuration = settings::Selection::Named(configuration.into());
                settings::save(&root, data, &legacy, &personal)?;
                settings_changed = true;
            }
        }
        let requested_settings = settings::resolve(&root, data, &legacy).ok();
        let existing_entry = entries.contains_key(&database);
        let entry = entries.entry(database.clone()).or_insert_with(|| Entry {
            registration: REGISTRATION.fetch_add(1, Ordering::Relaxed),
            project_trusted: settings::project_execution_allowed(&root, data).unwrap_or(false),
            trust_epoch: 0,
            analysis_error: String::new(),
            failed_input_hash: String::new(),
            status: LiveStatus::new(
                WorkspaceStatus {
                    workspace_root: root.to_string_lossy().into_owned(),
                    database,
                    phase: "queued".into(),
                    observed_epoch: 1,
                    available: true,
                    requested_settings,
                    ..Default::default()
                },
                &shared.publisher,
            ),
            configuration: legacy,
            identity: actual_identity.clone(),
            special_inputs: settings::paths(&root, data).into_iter().collect(),
            dirty: BTreeSet::new(),
            force: false,
            rehash: true,
            retry_preparation: false,
            watch_pending: true,
            pending: true,
            cancel_pending: false,
            disk_pause: None,
            recovery_blocked: false,
            recovery_explicit: false,
            recovering: false,
            upgrade_format: None,
            queued: Instant::now(),
            active: Instant::now(),
        });
        if entry.identity != actual_identity {
            return Err(
                "workspace identity changed; unwatch before registering the replacement".into(),
            );
        }
        if !existing_entry {
            restore_recovery(entry)?;
        }
        if settings_changed {
            release_disk_pause(entry);
            queue(entry, None, true);
        }
        entry.project_trusted = settings::project_execution_allowed(&root, data).unwrap_or(false);
        entry.status.requested_settings = settings::resolve(&root, data, &entry.configuration).ok();
        if request.operation == "refresh" {
            release_disk_pause(entry);
            entry.recovery_explicit = true;
            entry.recovery_blocked = false;
            if entry.cancel_pending {
                entry.cancel_pending = false;
                if let Some((_, control)) = shared
                    .active_job
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|(db, _)| db == &entry.status.database)
                {
                    control.cancel();
                }
            }
            entry.retry_preparation = true;
            queue(entry, None, true);
        }
        persist(shared, entry, None)?;
        shared.cursors.register(Path::new(&entry.status.database))?;
        if !discovered {
            shared.registry.lock().unwrap().execute(
                "DELETE FROM discovery_suppressed WHERE root=?",
                [root.to_string_lossy().as_ref()],
            )?;
        }
        entry.snapshot()
    };
    Ok(persistent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn cursor_maintenance_does_not_block_watch_or_sibling_control() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
            .join(".artifacts/ef")
            .join(format!(
                "{}-{}",
                std::process::id(),
                REGISTRATION.fetch_add(1, Ordering::Relaxed)
            ));
        let data = root.join("d");
        let workspace = root.join("w");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        let governor = Governor::new(Limits::default()).unwrap();
        let session = governor.start().unwrap();
        let shared = super::super::test_support::shared(session.control());
        let limits = shared.resources.limits();
        let socket = control::socket_path(&data);
        let listener = UnixListener::bind(&socket).unwrap();
        let stopping = Arc::new(AtomicBool::new(false));
        let (sender, _events) = mpsc::sync_channel(32);
        // Scope cleanup must wake accept and release the cursor barrier even
        // when a prerequisite assertion fails; no sleeps or child processes.
        struct StopServer(Arc<AtomicBool>, PathBuf);
        impl Drop for StopServer {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
                let _ = UnixStream::connect(&self.1);
            }
        }
        let (watch, status, cancelled, resources) = thread::scope(|scope| {
            let stop = StopServer(Arc::clone(&stopping), socket.clone());
            let serving_state = Arc::clone(&shared);
            let serving_data = data.clone();
            let serving = scope.spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::Acquire) {
                        break;
                    }
                    let stream = stream.unwrap();
                    let state = Arc::clone(&serving_state);
                    let data = serving_data.clone();
                    let sender = sender.clone();
                    scope.spawn(move || {
                        let _ = super::super::serve(stream, &state, &sender, &data);
                    });
                }
            });
            let (entered, held) = mpsc::channel();
            let (release, released) = mpsc::channel::<()>();
            let held_state = Arc::clone(&shared);
            let held_database = data.join("cursor-maintenance.db");
            let holding = scope.spawn(move || {
                held_state
                    .cursors
                    .publish_replacement(&held_database, &Control::default(), || {
                        entered.send(()).unwrap();
                        let _ = released.recv();
                        Ok(())
                    })
            });
            held.recv_timeout(Duration::from_secs(5)).unwrap();
            // These are actual framed control requests. WATCH follows the
            // normal attach/probe/register path; each RPC keeps its existing
            // 500ms read budget while cursor maintenance remains held.
            let unavailable_executable = data.join("must-not-start-coordinator");
            let watch = control::attach_workspace(
                &data,
                &workspace,
                String::new(),
                false,
                Some(&unavailable_executable),
            );
            let status = control::request(
                &data,
                ControlRequest {
                    operation: "status".into(),
                    database: "owned".into(),
                    ..Default::default()
                },
            );
            let cancelled = control::request(
                &data,
                ControlRequest {
                    operation: "cancel".into(),
                    database: "owned".into(),
                    ..Default::default()
                },
            );
            let resources = control::request(
                &data,
                ControlRequest {
                    operation: "resources".into(),
                    resources: Some(ResourceLimits {
                        cpu_cores: 1.0,
                        memory_mib: limits.memory_mib,
                        rebuild_disk_mib: limits.rebuild_disk_mib,
                        min_free_disk_mib: limits.min_free_disk_mib,
                    }),
                    ..Default::default()
                },
            );
            // Release before checking results, including the old blocked-lock
            // failure, so server threads cannot strand a failed test.
            drop(release);
            holding.join().unwrap().unwrap();
            drop(stop);
            serving.join().unwrap();
            (watch, status, cancelled, resources)
        });
        assert_eq!(watch.unwrap().workspace_root, workspace.to_string_lossy());
        assert_eq!(status.unwrap().workspaces[0].database, "owned");
        assert_eq!(cancelled.unwrap().workspaces[0].database, "owned");
        assert!(shared.entries.lock().unwrap()["owned"].cancel_pending);
        assert_eq!(resources.unwrap().resources.unwrap().cpu_cores, 1.0);
        std::fs::remove_file(socket).unwrap();
    }

    #[test]
    fn held_adoption_keeps_sibling_status_available_and_revalidates_enrollment() {
        for change in 0..4 {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.artifacts/register-lifecycle")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    REGISTRATION.fetch_add(1, Ordering::Relaxed)
                ));
            let workspace = root.join("workspace");
            let data = root.join("data");
            std::fs::create_dir_all(&workspace).unwrap();
            let database = workspace::database_path(&workspace.canonicalize().unwrap(), &data);
            std::fs::create_dir_all(database.parent().unwrap()).unwrap();
            let shared = super::super::test_support::shared(Control::default());
            let source = super::super::test_support::shared(Control::default());
            let mut target = source.entries.lock().unwrap().remove("owned").unwrap();
            target.registration = REGISTRATION.fetch_add(1, Ordering::Relaxed);
            target.identity = workspace::identity(&workspace).unwrap();
            target.status.database = database.to_string_lossy().into_owned();
            target.status.workspace_root = workspace
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            target.status.generation = 0;
            let mut concurrent = if change == 3 {
                Some(target)
            } else {
                shared
                    .entries
                    .lock()
                    .unwrap()
                    .insert(database.to_string_lossy().into_owned(), target);
                None
            };
            thread::scope(|scope| {
                // Scope-local ownership releases the held lifecycle lock even
                // if a prerequisite assertion unwinds before the normal release.
                let lock = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(database.with_extension("lifecycle.lock"))
                    .unwrap();
                lock.lock().unwrap();
                let (prepared, preparing) = mpsc::sync_channel(0);
                let registering = Arc::clone(&shared);
                let request = ControlRequest {
                    workspace_root: workspace.to_string_lossy().into_owned(),
                    configuration: "Release".into(),
                    ..Default::default()
                };
                let registration_data = &data;
                let registration = scope.spawn(move || {
                    register_with_adoption(
                        &registering,
                        registration_data,
                        &request,
                        false,
                        |path, generation| {
                            prepared.send(()).unwrap();
                            recovery::adopt(path, generation)
                        },
                    )
                    .map_err(|error| error.to_string())
                });
                preparing.recv_timeout(Duration::from_secs(5)).unwrap();
                let sibling = {
                    let mut entries = shared.entries.try_lock().ok();
                    let sibling = entries.as_ref().map(|entries| entries["owned"].snapshot());
                    if let Some(entries) = entries.as_mut() {
                        let key = database.to_string_lossy();
                        match change {
                            1 => {
                                entries.remove(key.as_ref());
                            }
                            2 => entries.get_mut(key.as_ref()).unwrap().registration += 1,
                            3 => {
                                entries.insert(key.into_owned(), concurrent.take().unwrap());
                            }
                            _ => {}
                        }
                    }
                    sibling
                };
                // Release before assertions/join, including on a lock-liveness
                // regression. No worker/compiler/index process is started.
                lock.unlock().unwrap();
                let outcome = registration.join().unwrap();
                assert_eq!(sibling.unwrap().database, "owned");
                if change == 1 || change == 2 {
                    assert!(outcome.unwrap_err().contains("registration changed"));
                    assert!(
                        !database.parent().unwrap().join("settings.json").exists(),
                        "superseded registration wrote personal configuration"
                    );
                } else {
                    let status = outcome.unwrap();
                    assert_eq!(status.database, database.to_string_lossy());
                    assert_eq!(
                        status
                            .requested_settings
                            .unwrap()
                            .csharp
                            .unwrap()
                            .configuration
                            .unwrap(),
                        "Release"
                    );
                }
                if change == 1 {
                    assert!(
                        !shared
                            .entries
                            .lock()
                            .unwrap()
                            .contains_key(database.to_str().unwrap())
                    );
                }
            });
        }
    }
}
