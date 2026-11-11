use napi::{
    Env, JsDeferred, JsValue, Result, Task,
    bindgen_prelude::{Either, PromiseRaw},
};
use napi_derive::napi;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

type NativeOutput = Either<String, PromiseRaw<'static, String>>;
type Completion = JsDeferred<String, Box<dyn FnOnce(Env) -> Result<String> + Send>>;

fn internal_error() -> String {
    know_code_core::error_json(
        "internal_error",
        "Know Code encountered an internal native failure.",
    )
}
fn contained_response(work: impl FnOnce() -> String) -> String {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| internal_error())
}
fn boundary(work: impl FnOnce() -> Result<NativeOutput>) -> Result<NativeOutput> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
        .unwrap_or_else(|_| Ok(Either::A(internal_error())))
}

// Control work does not consume graph slots. Bound it separately through JS
// completion, reserving capacity for status/cancel when waits are occupied.
static CONTROL_WORK: AtomicUsize = AtomicUsize::new(0);
static URGENT_CONTROL_WORK: AtomicUsize = AtomicUsize::new(0);
struct ControlPermit(&'static AtomicUsize);
impl ControlPermit {
    fn acquire(urgent: bool) -> Option<Arc<Self>> {
        let count = if urgent {
            &URGENT_CONTROL_WORK
        } else {
            &CONTROL_WORK
        };
        count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < 2).then_some(current + 1)
            })
            .ok()?;
        Some(Arc::new(Self(count)))
    }
}
impl Drop for ControlPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(feature = "test-hooks")]
mod test_hooks;

#[napi]
pub fn query_contract_version() -> u32 {
    know_code_core::execution::QUERY_CONTRACT_VERSION
}
#[napi]
pub fn query_max_request_bytes() -> u32 {
    know_code_core::admission::MAX_REQUEST_BYTES as u32
}

/// Host ownership extends the same single request's charge through JSON parsing.
#[napi(js_name = "QueryReservation")]
pub struct NativeReservation {
    state: Mutex<ReservationState>,
    error: Option<String>,
}
struct ReservationState {
    reservation: Option<Arc<know_code_core::admission::Reservation>>,
    submitted: bool,
}
#[napi]
impl NativeReservation {
    #[napi(constructor)]
    pub fn new(scope: Option<String>) -> Self {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match scope {
                Some(path) => know_code_core::admission::AdmissionScope::at(path),
                None => know_code_core::admission::AdmissionScope::local(),
            }
            .and_then(|scope| scope.try_reserve())
        }));
        let (reservation, error) = match result {
            Ok(Ok(reservation)) => (Some(Arc::new(reservation)), None),
            Ok(Err(error)) => (
                None,
                Some(know_code_core::error_json(error.code(), &error.to_string())),
            ),
            Err(_) => (None, Some(internal_error())),
        };
        Self {
            state: Mutex::new(ReservationState {
                reservation,
                submitted: false,
            }),
            error,
        }
    }
    #[napi(getter)]
    pub fn error_json(&self) -> Option<String> {
        self.error.clone()
    }
    #[napi]
    pub fn release(&self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .reservation
            .take();
    }
}
impl NativeReservation {
    fn take_submission(
        &self,
    ) -> std::result::Result<Arc<know_code_core::admission::Reservation>, String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let mut state = self.state.lock().map_err(|_| internal_error())?;
        if state.submitted {
            return Err(know_code_core::error_json(
                "invalid_request",
                "query reservation was already submitted",
            ));
        }
        let reservation = state.reservation.as_ref().cloned().ok_or_else(|| {
            know_code_core::error_json("invalid_request", "query reservation was released")
        })?;
        state.submitted = true;
        Ok(reservation)
    }
}

#[napi(js_name = "QueryCancellation")]
pub struct NativeCancellation {
    inner: know_code_core::execution::QueryCancellation,
}
impl Default for NativeCancellation {
    fn default() -> Self {
        Self::new()
    }
}
#[napi]
impl NativeCancellation {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self {
            inner: Default::default(),
        }
    }
    #[napi]
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

pub struct NativeTask {
    database: Option<String>,
    request: String,
    start: know_code_core::execution::QueryStart,
    cancellation: Option<know_code_core::execution::QueryCancellation>,
    reservation: Option<Arc<know_code_core::admission::Reservation>>,
    control: Option<Arc<ControlPermit>>,
    #[cfg(feature = "test-hooks")]
    hook: Option<test_hooks::Hook>,
}
impl Task for NativeTask {
    type Output = String;
    type JsValue = String;
    fn compute(&mut self) -> Result<String> {
        Ok(contained_response(|| {
            #[cfg(feature = "test-hooks")]
            if let Some(hook) = &self.hook {
                hook.enter(self.cancellation.as_ref());
            }
            if self.cancellation.as_ref().is_some_and(|c| c.is_cancelled()) {
                return know_code_core::error_json("cancelled", "query cancelled");
            }
            match &self.database {
                Some(database) => know_code_core::query_json_reserved(
                    database,
                    &self.request,
                    self.start,
                    self.cancellation.clone(),
                    self.reservation
                        .as_ref()
                        .expect("query task owns reservation"),
                ),
                None => know_code_core::workspace_json(&self.request),
            }
        }))
    }
    fn resolve(&mut self, _env: Env, output: String) -> Result<String> {
        Ok(output)
    }
}
// napi's deferred owns its threadsafe completion and pins this module. Its
// teardown hook fences late settlement; only its JS resolver receives an Env.
// No native worker stores or invokes an Env after environment shutdown.
fn start(env: Env, mut task: NativeTask) -> Result<NativeOutput> {
    let cancellation = task
        .cancellation
        .get_or_insert_with(Default::default)
        .clone();
    let retained_query = task.reservation.clone();
    let retained_control = task.control.clone();
    let (send, receive) = std::sync::mpsc::sync_channel::<Completion>(1);
    if std::thread::Builder::new()
        .name("know-code-native".into())
        .spawn(move || {
            let Ok(deferred) = receive.recv() else {
                return;
            };
            let output = task.compute().unwrap_or_else(|_| internal_error());
            deferred.resolve(Box::new(move |_| Ok(output)));
        })
        .is_err()
    {
        return Ok(Either::A(internal_error()));
    }
    let closed = Arc::new(AtomicBool::new(false));
    let closing = Arc::clone(&closed);
    let hook = env.add_env_cleanup_hook(cancellation, move |cancellation| {
        closing.store(true, Ordering::Release);
        cancellation.cancel();
    })?;
    let (mut deferred, promise) =
        match env.create_deferred::<String, Box<dyn FnOnce(Env) -> Result<String> + Send>>() {
            Ok(created) => created,
            Err(error) => {
                let _ = env.remove_env_cleanup_hook(hook);
                return Err(error);
            }
        };
    deferred.set_finalize_callback(Some(Box::new(move |raw_env| {
        // Both leases survive JS string conversion and promise settlement.
        let _query = retained_query;
        let _control = retained_control;
        if !closed.load(Ordering::Acquire) {
            let _ = Env::from_raw(raw_env).remove_env_cleanup_hook(hook);
        }
    })));
    if let Err(failed) = send.send(deferred) {
        failed.0.resolve(Box::new(|_| Ok(internal_error())));
    }
    // The raw promise is returned immediately in this same JS callback scope;
    // all later use is owned by napi's deferred, never this Rust handle.
    Ok(Either::B(PromiseRaw::new(env.raw(), promise.raw())))
}
fn query_task(
    database: String,
    request: String,
    elapsed_ms: u32,
    reservation: &NativeReservation,
    cancellation: Option<&NativeCancellation>,
) -> std::result::Result<NativeTask, String> {
    if cancellation.is_some_and(|c| c.inner.is_cancelled()) {
        return Err(know_code_core::error_json("cancelled", "query cancelled"));
    }
    know_code_core::admission::check_request_bytes(request.len())
        .map_err(|error| know_code_core::error_json(error.code(), &error.to_string()))?;
    let reservation = reservation.take_submission()?;
    Ok(NativeTask {
        database: Some(database),
        request,
        start: know_code_core::execution::QueryStart::with_elapsed_ms(u64::from(elapsed_ms)),
        cancellation: Some(cancellation.map(|c| c.inner.child()).unwrap_or_default()),
        reservation: Some(reservation),
        control: None,
        #[cfg(feature = "test-hooks")]
        hook: None,
    })
}
fn schedule(
    env: Env,
    database: String,
    request: String,
    elapsed_ms: u32,
    reservation: &NativeReservation,
    cancellation: Option<&NativeCancellation>,
) -> Result<NativeOutput> {
    match query_task(database, request, elapsed_ms, reservation, cancellation) {
        Ok(task) => start(env, task),
        Err(error) => Ok(Either::A(error)),
    }
}
#[napi]
pub fn query_json_reserved(
    env: Env,
    database: String,
    request: String,
    elapsed_ms: u32,
    reservation: &NativeReservation,
    cancellation: Option<&NativeCancellation>,
) -> Result<NativeOutput> {
    boundary(|| {
        schedule(
            env,
            database,
            request,
            elapsed_ms,
            reservation,
            cancellation,
        )
    })
}
#[napi]
pub fn query_json(env: Env, database: String, request: String) -> Result<NativeOutput> {
    query_json_elapsed(env, database, request, 0)
}
#[napi]
pub fn query_json_elapsed(
    env: Env,
    database: String,
    request: String,
    elapsed_ms: u32,
) -> Result<NativeOutput> {
    boundary(|| {
        let reservation = NativeReservation::new(None);
        schedule(env, database, request, elapsed_ms, &reservation, None)
    })
}
#[napi]
pub fn query_json_cancellable(
    env: Env,
    database: String,
    request: String,
    elapsed_ms: u32,
    cancellation: &NativeCancellation,
) -> Result<NativeOutput> {
    boundary(|| {
        if cancellation.inner.is_cancelled() {
            return Ok(Either::A(know_code_core::error_json(
                "cancelled",
                "query cancelled",
            )));
        }
        let reservation = NativeReservation::new(None);
        schedule(
            env,
            database,
            request,
            elapsed_ms,
            &reservation,
            Some(cancellation),
        )
    })
}
fn control_task(request: String) -> std::result::Result<NativeTask, String> {
    // The shared byte ceiling applies before allocating/parsing control JSON.
    know_code_core::admission::check_request_bytes(request.len())
        .map_err(|error| know_code_core::error_json(error.code(), &error.to_string()))?;
    let value: serde_json::Value = serde_json::from_str(&request).map_err(|_| {
        know_code_core::error_json("invalid_request", "invalid workspace request JSON")
    })?;
    let urgent = matches!(
        value.get("op").and_then(|op| op.as_str()),
        Some("status" | "cancel")
    );
    let control = ControlPermit::acquire(urgent).ok_or_else(|| {
        know_code_core::error_json(
            "busy",
            "native workspace control capacity is full; retry after an active operation finishes",
        )
    })?;
    Ok(NativeTask {
        database: None,
        request,
        start: Default::default(),
        cancellation: Some(Default::default()),
        reservation: None,
        control: Some(control),
        #[cfg(feature = "test-hooks")]
        hook: None,
    })
}
#[napi]
pub fn workspace_json(env: Env, request: String) -> Result<NativeOutput> {
    boundary(|| match control_task(request) {
        Ok(task) => start(env, task),
        Err(error) => Ok(Either::A(error)),
    })
}
