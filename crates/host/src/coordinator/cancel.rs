//! Job-bound cancellation without coupling workspace reads to commit I/O.
use super::*;

pub(super) fn cancel_workspace(shared: &Shared, database: &str) -> AnyResult<WorkspaceStatus> {
    cancel_with(shared, database, |control| {
        control.cancel() || !control.committed()
    })
}

fn cancel_with(
    shared: &Shared,
    database: &str,
    cancel: impl FnOnce(&Control) -> bool,
) -> AnyResult<WorkspaceStatus> {
    let (registration, trust_epoch, observed_epoch, control) = {
        let mut entries = shared.entries.lock().unwrap();
        let entry = entries
            .get_mut(database)
            .ok_or("workspace is not registered")?;
        if entry.cancel_pending {
            return Ok(entry.snapshot());
        }
        let active = shared.active_job.lock().unwrap();
        let Some((_, control)) = active.as_ref().filter(|(db, _)| db == database) else {
            drop(active);
            if entry.pending && entry.status.available {
                begin_cancellation(entry);
                persist(shared, entry, None)?;
            }
            return Ok(entry.snapshot());
        };
        (
            entry.registration,
            entry.trust_epoch,
            entry.status.observed_epoch,
            control.clone(),
        )
    };
    // Control serializes cancellation with commit. Never wait for that lock
    // while owning entries or active_job: sibling status/wait stays available.
    let accepted = cancel(&control);
    let mut entries = shared.entries.lock().unwrap();
    let entry = entries
        .get_mut(database)
        .ok_or("workspace was unregistered during cancellation")?;
    if entry.registration != registration {
        return Err(
            "workspace registration changed during cancellation; start a new request".into(),
        );
    }
    let active = shared.active_job.lock().unwrap();
    let replacement = active
        .as_ref()
        .is_some_and(|(db, current)| db == database && !current.same_job(&control));
    drop(active);
    if accepted
        && !replacement
        && !entry.cancel_pending
        && entry.trust_epoch == trust_epoch
        && entry.status.observed_epoch == observed_epoch
    {
        // Later enrollment, trust, refresh/save and job ownership must win over
        // this earlier request. With the same intent, normal baseline capture
        // remains queued even if the cancelled old task has already finished.
        begin_cancellation(entry);
        persist(shared, entry, None)?;
    }
    Ok(entry.snapshot())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_blocked_cancel_leaves_entries_readable_and_does_not_cancel_new_intent() {
        // Commit wins; replacement job, replacement enrollment, later refresh
        // and later trust decision respectively supersede the earlier request.
        for change in 0..7 {
            let governor = Governor::new(Limits::default()).unwrap();
            let session = governor.start().unwrap();
            let control = session.control();
            let shared = super::super::test_support::shared(control.clone());
            thread::scope(|scope| {
                let (entered, committed) = mpsc::sync_channel(0);
                let (release, released) = mpsc::sync_channel(0);
                let commit_control = control.clone();
                let publication = scope.spawn(move || {
                    let mut connection = Connection::open_in_memory().unwrap();
                    connection
                        .execute_batch("CREATE TABLE saved(value INTEGER)")
                        .unwrap();
                    let transaction = connection.transaction().unwrap();
                    transaction
                        .execute("INSERT INTO saved VALUES(1)", [])
                        .unwrap();
                    let result = commit_control.publish(|| {
                        entered.send(()).unwrap();
                        released.recv().unwrap();
                        if change == 0 {
                            transaction.commit().map_err(know_code_core::Error::from)
                        } else {
                            drop(transaction);
                            Err(know_code_core::Error::Cancelled)
                        }
                    });
                    (
                        result.is_ok(),
                        connection
                            .query_row("SELECT count(*) FROM saved", [], |row| row.get::<_, u32>(0))
                            .unwrap(),
                    )
                });
                committed.recv_timeout(Duration::from_secs(5)).unwrap();
                let (snapshot, captured) = mpsc::sync_channel(0);
                let cancelling = Arc::clone(&shared);
                let cancellation = scope.spawn(move || {
                    cancel_with(&cancelling, "owned", |control| {
                        snapshot.send(()).unwrap();
                        control.cancel() || !control.committed()
                    })
                    .map_err(|error| error.to_string())
                });
                captured.recv_timeout(Duration::from_secs(5)).unwrap();
                let available = {
                    let mut entries = shared.entries.try_lock().ok();
                    if let Some(entries) = entries.as_mut() {
                        let entry = entries.get_mut("owned").unwrap();
                        match change {
                            1 => {
                                *shared.active_job.lock().unwrap() =
                                    Some(("owned".into(), Control::default()))
                            }
                            2 => entry.registration += 1,
                            3 => {
                                entry.recovery_explicit = true;
                                queue(entry, None, true);
                            }
                            4 => entry.trust_epoch += 1,
                            6 => {
                                shared.active_job.lock().unwrap().take();
                            }
                            _ => {}
                        }
                    }
                    entries.is_some()
                };
                // Always release the real transaction/control boundary before
                // asserting liveness, so a regression cannot strand a thread.
                release.send(()).unwrap();
                let result = cancellation.join().unwrap();
                let (published, rows) = publication.join().unwrap();
                assert!(available, "cancel held entries while waiting for commit");
                assert_eq!((published, rows), (change == 0, u32::from(change == 0)));
                if change == 2 {
                    assert!(result.unwrap_err().contains("registration changed"));
                } else {
                    assert!(result.is_ok(), "{result:?}");
                }
                let entries = shared.entries.lock().unwrap();
                let entry = &entries["owned"];
                assert_eq!(
                    entry.cancel_pending,
                    change >= 5,
                    "earlier cancellation must preserve newer intent and capture the same intent"
                );
                if change == 3 {
                    assert!(entry.force && entry.recovery_explicit && entry.pending);
                }
                if change >= 5 {
                    assert_eq!(entry.status.phase, "cancelling");
                    let pending: bool = shared
                        .registry
                        .lock()
                        .unwrap()
                        .query_row(
                            "SELECT cancel_pending FROM workspaces WHERE database='owned'",
                            [],
                            |row| row.get(0),
                        )
                        .unwrap();
                    assert!(pending);
                }
            });
        }
    }
}
