//! Terminal ownership of dispatched analysis tasks.
use super::*;

// An unwind is terminal for the coordinator: mutex/transaction invariants may
// be poisoned. Always notify its owner rather than stranding the analysis slot.
pub(super) fn finish_job(
    sender: &SyncSender<Event>,
    database: String,
    work: impl FnOnce() -> JobResult,
) {
    let event = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
        Ok(result) => Event::Finished(result),
        Err(_) => Event::JobPanicked(database),
    };
    let _ = sender.send(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn job_unwind_has_a_terminal_owner_event() {
        let (sender, receiver) = mpsc::sync_channel(1);
        finish_job(&sender, "owned workspace".into(), || {
            panic!("owned analysis unwind")
        });
        assert!(
            matches!(receiver.recv_timeout(Duration::from_secs(1)).unwrap(), Event::JobPanicked(database) if database == "owned workspace")
        );
    }
}
