//! Shared owned-state fixture for deterministic coordinator unit barriers.
use super::*;

pub(super) fn shared(control: Control) -> Arc<Shared> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.artifacts/cancel-commit")
        .join(format!(
            "{}-{}",
            std::process::id(),
            REGISTRATION.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&root).unwrap();
    let publisher = Arc::new(Publisher::new(&root));
    let registry = Connection::open_in_memory().unwrap();
    registry.execute_batch("CREATE TABLE workspaces(database TEXT PRIMARY KEY,root TEXT UNIQUE NOT NULL,configuration TEXT NOT NULL,generation INTEGER NOT NULL,inputs TEXT NOT NULL,error TEXT NOT NULL,failed_input_hash TEXT NOT NULL,outcome_code TEXT NOT NULL,cancel_pending INTEGER NOT NULL,disk_pause TEXT NOT NULL); CREATE TABLE identities(database TEXT PRIMARY KEY,identity TEXT NOT NULL); CREATE TABLE manifests(database TEXT PRIMARY KEY,inputs TEXT NOT NULL);").unwrap();
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
                phase: "analyzing".into(),
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
        pending: false,
        cancel_pending: false,
        disk_pause: None,
        recovery_blocked: false,
        recovery_explicit: false,
        recovering: false,
        upgrade_format: None,
    };
    let discovery = crate::discovery::Discovery::open(&registry).unwrap();
    Arc::new(Shared {
        entries: Mutex::new(BTreeMap::from([("owned".into(), entry)])),
        publisher,
        changed: Condvar::new(),
        registry: Mutex::new(registry),
        discovery: Mutex::new(discovery),
        stop: AtomicBool::new(false),
        maintenance: Mutex::new(None),
        analysis_running: AtomicBool::new(false),
        resources: Governor::new(Limits::default()).unwrap(),
        active_job: Mutex::new(Some(("owned".into(), control))),
        overflow: AtomicBool::new(false),
        clients: AtomicUsize::new(0),
        cursors: Arc::new(know_code_core::cursor::CursorService::new(&root, Vec::new()).unwrap()),
    })
}
