//! Compiled only into a separate diagnostic add-on, never the default artifact.
use super::*;
use napi::bindgen_prelude::AsyncTask;
use std::{
    collections::BTreeMap,
    sync::{Condvar, OnceLock},
    time::Duration,
};

struct GateState {
    open: bool,
    entered: u32,
}
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}
#[derive(Clone)]
pub(super) struct Hook {
    gate: Arc<Gate>,
    panic: bool,
}
impl Hook {
    pub(super) fn enter(
        &self,
        cancellation: Option<&know_code_core::execution::QueryCancellation>,
    ) {
        let mut state = self.gate.state.lock().unwrap();
        state.entered += 1;
        self.gate.changed.notify_all();
        while !state.open && !cancellation.is_some_and(|c| c.is_cancelled()) {
            state = self
                .gate
                .changed
                .wait_timeout(state, Duration::from_millis(10))
                .unwrap()
                .0;
        }
        drop(state);
        if self.panic && !cancellation.is_some_and(|c| c.is_cancelled()) {
            panic!("owned native compute panic");
        }
    }
}
static GATES: OnceLock<Mutex<BTreeMap<u32, Arc<Gate>>>> = OnceLock::new();
static NEXT_GATE: AtomicUsize = AtomicUsize::new(1);
fn gates() -> &'static Mutex<BTreeMap<u32, Arc<Gate>>> {
    GATES.get_or_init(Default::default)
}
fn gate(id: u32) -> Result<Arc<Gate>> {
    gates()
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| napi::Error::from_reason("unknown diagnostic gate"))
}
#[napi]
pub fn test_create_gate() -> Result<u32> {
    let mut gates = gates().lock().unwrap();
    if gates.len() == 8 {
        return Err(napi::Error::from_reason("diagnostic gates full"));
    }
    let id = u32::try_from(NEXT_GATE.fetch_add(1, Ordering::AcqRel))
        .map_err(|_| napi::Error::from_reason("diagnostic IDs exhausted"))?;
    gates.insert(
        id,
        Arc::new(Gate {
            state: Mutex::new(GateState {
                open: false,
                entered: 0,
            }),
            changed: Condvar::new(),
        }),
    );
    Ok(id)
}
#[napi]
pub fn test_gate_entered(id: u32) -> Result<u32> {
    Ok(gate(id)?.state.lock().unwrap().entered)
}
#[napi]
pub fn test_release_gate(id: u32) -> Result<()> {
    let gate = gate(id)?;
    gate.state.lock().unwrap().open = true;
    gate.changed.notify_all();
    Ok(())
}
#[napi]
pub fn test_drop_gate(id: u32) {
    gates().lock().unwrap().remove(&id);
}
fn query(scope: String, id: u32, panic: bool) -> Result<NativeTask> {
    let reservation = NativeReservation::new(Some(scope.clone()));
    let mut task = query_task(
        format!("{scope}/missing.db"),
        "{\"op\":\"symbols\"}".into(),
        0,
        &reservation,
        None,
    )
    .map_err(napi::Error::from_reason)?;
    task.hook = Some(Hook {
        gate: gate(id)?,
        panic,
    });
    Ok(task)
}
fn control(scope: String, id: u32, panic: bool, urgent: bool) -> Result<NativeTask> {
    let request = serde_json::json!({"op": if urgent {"status"} else {"wait"},
        "data_directory":format!("{scope}/data"), "wait_ms":1})
    .to_string();
    let mut task = control_task(request).map_err(napi::Error::from_reason)?;
    task.hook = Some(Hook {
        gate: gate(id)?,
        panic,
    });
    Ok(task)
}
#[napi]
pub fn test_query(env: Env, scope: String, id: u32, panic: bool) -> Result<NativeOutput> {
    boundary(|| start(env, query(scope, id, panic)?))
}
#[napi]
pub fn test_control(
    env: Env,
    scope: String,
    id: u32,
    panic: bool,
    urgent: bool,
) -> Result<NativeOutput> {
    boundary(|| start(env, control(scope, id, panic, urgent)?))
}
// Uses the previous real AsyncTask path with the very same compute method, so
// the finite fixture can establish libuv starvation before testing its remedy.
#[napi]
pub fn test_legacy_query(scope: String, id: u32) -> Result<AsyncTask<NativeTask>> {
    Ok(AsyncTask::new(query(scope, id, false)?))
}
#[napi]
pub fn test_legacy_control(scope: String, id: u32) -> Result<AsyncTask<NativeTask>> {
    Ok(AsyncTask::new(control(scope, id, false, false)?))
}
#[napi]
pub fn test_sync_panic() -> String {
    contained_response(|| panic!("owned synchronous native panic"))
}
#[napi]
pub fn test_control_counts() -> String {
    serde_json::json!({"normal":CONTROL_WORK.load(Ordering::Acquire),"urgent":URGENT_CONTROL_WORK.load(Ordering::Acquire)}).to_string()
}
