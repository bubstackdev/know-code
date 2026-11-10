use napi::{Env, Result, Task, bindgen_prelude::AsyncTask};
use napi_derive::napi;

pub struct NativeTask {
    database: Option<String>,
    request: String,
    start: know_code_core::execution::QueryStart,
}
impl Task for NativeTask {
    type Output = String;
    type JsValue = String;
    fn compute(&mut self) -> Result<String> {
        Ok(match &self.database {
            Some(database) => {
                know_code_core::query_json_started(database, &self.request, self.start)
            }
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
    })
}
#[napi]
pub fn workspace_json(request: String) -> AsyncTask<NativeTask> {
    AsyncTask::new(NativeTask {
        database: None,
        request,
        start: Default::default(),
    })
}
