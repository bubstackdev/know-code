//! Enrollment-bound waits over the existing coordinator state.
use super::*;

pub(super) fn wait_for_workspace(
    shared: &Shared,
    request: &ControlRequest,
) -> AnyResult<WorkspaceStatus> {
    let mut entries = shared.entries.lock().unwrap();
    let entry = entries
        .get_mut(&request.database)
        .ok_or("workspace is not registered")?;
    entry.active = Instant::now();
    let target = entry.status.observed_epoch;
    let registration = entry.registration;
    let until = Instant::now() + Duration::from_millis(u64::from(request.wait_ms.min(120000)));
    loop {
        let entry = entries
            .get(&request.database)
            .ok_or("workspace was unregistered while waiting")?;
        if entry.registration != registration {
            return Err("workspace registration changed while waiting; start a new wait".into());
        }
        let status = entry.snapshot();
        let reconciling = entry.pending
            || shared
                .active_job
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|(db, _)| db == &request.database);
        if status.published_epoch >= target
            || matches!(
                status.phase.as_str(),
                "failed"
                    | "unavailable"
                    | "resource_limited"
                    | "recovery_failed"
                    | "trust_required"
            )
            || (status.phase == "cancelled" && !reconciling)
            || shared.stop.load(Ordering::Acquire)
        {
            break;
        }
        let Some(remaining) = until.checked_duration_since(Instant::now()) else {
            break;
        };
        let (next, timeout) = shared.changed.wait_timeout(entries, remaining).unwrap();
        entries = next;
        if timeout.timed_out() {
            break;
        }
    }
    let entry = entries
        .get(&request.database)
        .ok_or("workspace was unregistered while waiting")?;
    if entry.registration != registration {
        return Err("workspace registration changed while waiting; start a new wait".into());
    }
    Ok(entry.snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_wait_is_safe_when_enrollment_is_removed_or_replaced() {
        for replaced in [false, true] {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../.artifacts/wait-enrollment")
                .join(format!(
                    "{}-{}",
                    std::process::id(),
                    REGISTRATION.fetch_add(1, Ordering::Relaxed)
                ));
            std::fs::create_dir_all(&root).unwrap();
            let publisher = Arc::new(Publisher::new(&root));
            let active = Instant::now() - Duration::from_secs(60);
            let entry = Entry {
                registration: 1,
                project_trusted: true,
                trust_epoch: 0,
                analysis_error: String::new(),
                failed_input_hash: String::new(),
                status: LiveStatus::new(
                    WorkspaceStatus {
                        database: "owned".into(),
                        observed_epoch: 1,
                        phase: "queued".into(),
                        available: true,
                        ..Default::default()
                    },
                    &publisher,
                ),
                configuration: String::new(),
                identity: String::new(),
                special_inputs: BTreeSet::new(),
                dirty: BTreeSet::new(),
                force: false,
                rehash: false,
                retry_preparation: false,
                queued: Instant::now(),
                active,
                watch_pending: false,
                pending: true,
                cancel_pending: false,
                disk_pause: None,
                recovery_blocked: false,
                recovery_explicit: false,
                recovering: false,
                upgrade_format: None,
            };
            let shared = Arc::new(Shared {
                entries: Mutex::new(BTreeMap::from([("owned".into(), entry)])),
                publisher,
                changed: Condvar::new(),
                registry: Mutex::new(Connection::open_in_memory().unwrap()),
                discovery: Mutex::new(
                    crate::discovery::Discovery::open(&Connection::open_in_memory().unwrap())
                        .unwrap(),
                ),
                stop: AtomicBool::new(false),
                maintenance: Mutex::new(None),
                analysis_running: AtomicBool::new(false),
                resources: Governor::new(Limits::default()).unwrap(),
                active_job: Mutex::new(None),
                overflow: AtomicBool::new(false),
                clients: AtomicUsize::new(0),
                cursors: Arc::new(
                    know_code_core::cursor::CursorService::new(&root, Vec::new()).unwrap(),
                ),
            });
            let waiter_shared = Arc::clone(&shared);
            let waiter = thread::spawn(move || {
                wait_for_workspace(
                    &waiter_shared,
                    &ControlRequest {
                        database: "owned".into(),
                        wait_ms: 5000,
                        ..Default::default()
                    },
                )
                .map_err(|error| error.to_string())
            });
            // Observing this marker while owning entries proves the actual
            // waiter released the mutex after capturing enrollment identity.
            let until = Instant::now() + Duration::from_secs(1);
            loop {
                if shared.entries.lock().unwrap()["owned"].active != active {
                    break;
                }
                assert!(Instant::now() < until, "waiter never entered");
                thread::yield_now();
            }
            {
                let mut entries = shared.entries.lock().unwrap();
                if replaced {
                    entries.get_mut("owned").unwrap().registration = 2;
                } else {
                    entries.remove("owned");
                }
                shared.changed.notify_all();
            }
            let error = waiter.join().unwrap().unwrap_err();
            assert!(error.contains(if replaced {
                "registration changed"
            } else {
                "unregistered"
            }));
            assert!(
                shared.entries.lock().is_ok(),
                "wait must not poison shared state"
            );
            assert!(shared.cursors.sweep().is_ok());
        }
    }
}
