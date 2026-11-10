use napi::{Env, Result, Task, bindgen_prelude::AsyncTask};
use napi_derive::napi;

#[napi]
pub fn query_contract_version() -> u32 {
    know_code_core::execution::QUERY_CONTRACT_VERSION
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
}
impl Task for NativeTask {
    type Output = String;
    type JsValue = String;
    fn compute(&mut self) -> Result<String> {
        Ok(match &self.database {
            Some(database) => know_code_core::query_json_cancellable(
                database,
                &self.request,
                self.start,
                self.cancellation.clone(),
            ),
            None => know_code_core::workspace_json(&self.request),
        })
    }
    fn resolve(&mut self, _env: Env, output: String) -> Result<String> {
        Ok(output)
    }
}
#[napi]
pub fn query_json(database: String, request: String) -> AsyncTask<NativeTask> {
    query_json_elapsed(database, request, 0)
}
#[napi]
pub fn query_json_elapsed(
    database: String,
    request: String,
    elapsed_ms: u32,
) -> AsyncTask<NativeTask> {
    AsyncTask::new(NativeTask {
        database: Some(database),
        request,
        start: know_code_core::execution::QueryStart::with_elapsed_ms(u64::from(elapsed_ms)),
        cancellation: None,
    })
}
#[napi]
pub fn query_json_cancellable(
    database: String,
    request: String,
    elapsed_ms: u32,
    cancellation: &NativeCancellation,
) -> AsyncTask<NativeTask> {
    AsyncTask::new(NativeTask {
        database: Some(database),
        request,
        start: know_code_core::execution::QueryStart::with_elapsed_ms(u64::from(elapsed_ms)),
        cancellation: Some(cancellation.inner.clone()),
    })
}
#[napi]
pub fn workspace_json(request: String) -> AsyncTask<NativeTask> {
    AsyncTask::new(NativeTask {
        database: None,
        request,
        start: Default::default(),
        cancellation: None,
    })
}
