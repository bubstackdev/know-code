use napi::{
    Env, Result, Task,
    bindgen_prelude::{AsyncTask, Either},
};
use napi_derive::napi;
use std::sync::{Arc, Mutex};

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
        let result = match scope {
            Some(path) => know_code_core::admission::AdmissionScope::at(path),
            None => know_code_core::admission::AdmissionScope::local(),
        }
        .and_then(|scope| scope.try_reserve());
        let (reservation, error) = match result {
            Ok(reservation) => (Some(Arc::new(reservation)), None),
            Err(error) => (
                None,
                Some(know_code_core::error_json(error.code(), &error.to_string())),
            ),
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
        self.state.lock().unwrap().reservation.take();
    }
}
impl NativeReservation {
    fn take_submission(
        &self,
    ) -> std::result::Result<Arc<know_code_core::admission::Reservation>, String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let mut state = self.state.lock().unwrap();
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
}
impl Task for NativeTask {
    type Output = String;
    type JsValue = String;
    fn compute(&mut self) -> Result<String> {
        Ok(match &self.database {
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
        })
    }
    fn resolve(&mut self, _env: Env, output: String) -> Result<String> {
        Ok(output)
    }
}
fn schedule(
    database: String,
    request: String,
    elapsed_ms: u32,
    reservation: &NativeReservation,
    cancellation: Option<&NativeCancellation>,
) -> Either<String, AsyncTask<NativeTask>> {
    if cancellation.is_some_and(|c| c.inner.is_cancelled()) {
        return Either::A(know_code_core::error_json("cancelled", "query cancelled"));
    }
    if let Err(error) = know_code_core::admission::check_request_bytes(request.len()) {
        return Either::A(know_code_core::error_json(error.code(), &error.to_string()));
    }
    let reservation = match reservation.take_submission() {
        Ok(reservation) => reservation,
        Err(error) => return Either::A(error),
    };
    Either::B(AsyncTask::new(NativeTask {
        database: Some(database),
        request,
        start: know_code_core::execution::QueryStart::with_elapsed_ms(u64::from(elapsed_ms)),
        cancellation: cancellation.map(|c| c.inner.clone()),
        reservation: Some(reservation),
    }))
}
#[napi]
pub fn query_json_reserved(
    database: String,
    request: String,
    elapsed_ms: u32,
    reservation: &NativeReservation,
    cancellation: Option<&NativeCancellation>,
) -> Either<String, AsyncTask<NativeTask>> {
    schedule(database, request, elapsed_ms, reservation, cancellation)
}
#[napi]
pub fn query_json(database: String, request: String) -> Either<String, AsyncTask<NativeTask>> {
    query_json_elapsed(database, request, 0)
}
#[napi]
pub fn query_json_elapsed(
    database: String,
    request: String,
    elapsed_ms: u32,
) -> Either<String, AsyncTask<NativeTask>> {
    let reservation = NativeReservation::new(None);
    schedule(database, request, elapsed_ms, &reservation, None)
}
#[napi]
pub fn query_json_cancellable(
    database: String,
    request: String,
    elapsed_ms: u32,
    cancellation: &NativeCancellation,
) -> Either<String, AsyncTask<NativeTask>> {
    if cancellation.inner.is_cancelled() {
        return Either::A(know_code_core::error_json("cancelled", "query cancelled"));
    }
    let reservation = NativeReservation::new(None);
    schedule(
        database,
        request,
        elapsed_ms,
        &reservation,
        Some(cancellation),
    )
}
#[napi]
pub fn workspace_json(request: String) -> AsyncTask<NativeTask> {
    AsyncTask::new(NativeTask {
        database: None,
        request,
        start: Default::default(),
        cancellation: None,
        reservation: None,
    })
}
