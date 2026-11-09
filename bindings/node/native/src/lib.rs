use napi::{Env, Result, Task, bindgen_prelude::AsyncTask};
use napi_derive::napi;

pub struct NativeTask {
    database: Option<String>,
    request: String,
}
impl Task for NativeTask {
    type Output = String;
    type JsValue = String;
    fn compute(&mut self) -> Result<String> {
        Ok(match &self.database {
            Some(database) => know_code_core::query_json(database, &self.request),
            None => know_code_core::workspace_json(&self.request),
        })
    }
    fn resolve(&mut self, _env: Env, output: String) -> Result<String> {
        Ok(output)
    }
}
#[napi]
pub fn query_json(database: String, request: String) -> AsyncTask<NativeTask> {
    AsyncTask::new(NativeTask {
        database: Some(database),
        request,
    })
}
#[napi]
pub fn workspace_json(request: String) -> AsyncTask<NativeTask> {
    AsyncTask::new(NativeTask {
        database: None,
        request,
    })
}
