pub mod control;
pub mod cursor;
pub mod execution;
pub mod graph;
pub mod live;
pub mod protocol;
pub mod query;
pub mod resource;
pub mod settings;
pub mod store;
pub mod update;
pub mod workspace;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no published index is available yet")]
    NotReady,
    #[error("query deadline exceeded; restart the request")]
    DeadlineExceeded,
    #[error(
        "query response exceeds response_max_bytes; narrow the query or increase the response budget (maximum 2097152 bytes)"
    )]
    ResponseLimitExceeded,
    #[error("cursor is no longer valid; restart the query")]
    CursorInvalidated,
    #[error("cursor capacity is exhausted; retry after an active page finishes")]
    CursorLimit,
    #[error("indexing cancelled; previous generation preserved")]
    Cancelled,
    #[error("{0}")]
    ResourceLimited(String),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("{0}")]
    Protocol(String),
    #[error("{0}")]
    Provider(String),
    #[error("{0}")]
    UpdateRequired(String),
    #[error("{0}")]
    CoordinatorUnavailable(String),
    #[error("{0}")]
    Coordinator(String),
    #[error("{message}")]
    Remote { code: String, message: String },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Decode(#[from] prost::DecodeError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl Error {
    pub fn code(&self) -> &str {
        match self {
            Self::NotReady => "not_ready",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::ResponseLimitExceeded => "response_limit_exceeded",
            Self::CursorInvalidated => "cursor_invalidated",
            Self::CursorLimit => "cursor_limit",
            Self::Cancelled => "cancelled",
            Self::ResourceLimited(_) => "resource_limited",
            Self::InvalidRequest(_) | Self::Json(_) => "invalid_request",
            Self::Protocol(_) | Self::Decode(_) => "protocol_error",
            Self::Provider(_) => "provider_error",
            Self::CoordinatorUnavailable(_) => "coordinator_unavailable",
            Self::Coordinator(_) => "coordinator_error",
            Self::Remote { code, .. } => code,
            Self::UpdateRequired(_) => "update_required",
            Self::Sqlite(_) => "storage_error",
            Self::Io(_) => "io_error",
        }
    }
}
pub type Result<T> = std::result::Result<T, Error>;

pub fn query_json(database: &str, request: &str) -> String {
    query_json_started(database, request, execution::QueryStart::default())
}

pub fn query_json_started(database: &str, request: &str, start: execution::QueryStart) -> String {
    let value: serde_json::Value = match serde_json::from_str(request) {
        Ok(value) => value,
        Err(error) => {
            return execution::error_json(
                "invalid_request",
                &error.to_string(),
                execution::MIN_RESPONSE_BYTES as usize,
            );
        }
    };
    if value.get("op").and_then(|op| op.as_str()) == Some("next_page")
        && value.as_object().is_some_and(|object| {
            object.keys().any(|key| {
                !["op", "cursor", "deadline_ms", "response_max_bytes"].contains(&key.as_str())
            })
        })
    {
        return execution::error_json(
            "invalid_request",
            "next_page accepts only cursor, deadline_ms and response_max_bytes; the original query scope and page size are immutable",
            execution::MIN_RESPONSE_BYTES as usize,
        );
    }
    let request: query::QueryRequest = match serde_json::from_value(value) {
        Ok(request) => request,
        Err(error) => {
            return execution::error_json(
                "invalid_request",
                &error.to_string(),
                execution::MIN_RESPONSE_BYTES as usize,
            );
        }
    };
    let execution = match execution::Execution::new(start, request.deadline_ms) {
        Ok(execution) => execution,
        Err(error) => {
            return execution::error_json(
                error.code(),
                &error.to_string(),
                execution::MIN_RESPONSE_BYTES as usize,
            );
        }
    };
    let maximum = match execution::response_budget(request.response_max_bytes) {
        Ok(maximum) => maximum,
        Err(error) => {
            return execution::error_json(
                error.code(),
                &error.to_string(),
                execution::MIN_RESPONSE_BYTES as usize,
            );
        }
    };
    let result =
        if request.page_size.is_some() || request.cursor.is_some() || request.op == "next_page" {
            cursor::client::query(database, request, execution, maximum)
        } else {
            let result = (|| {
                execution.check()?;
                if request.freshness_wait_ms > 120000 {
                    return Err(Error::InvalidRequest(
                        "freshness_wait_ms cannot exceed 120000".into(),
                    ));
                }
                let status = match control::wait_for_database_controlled(
                    std::path::Path::new(database),
                    request.freshness_wait_ms,
                    execution,
                ) {
                    Err(error @ Error::UpdateRequired(_)) => return Err(error),
                    Ok(Some(status)) if status.generation == 0 => return Err(Error::NotReady),
                    other => other,
                };
                execution.check()?;
                let mut result = query::ReadView::open_controlled(database, execution)?
                    .query_controlled(request, execution)?;
                match status {
                    Ok(Some(status)) => {
                        result.freshness = if status.available
                            && status.phase == "idle"
                            && status.observed_epoch == status.published_epoch
                            && status.generation == result.generation
                        {
                            "current"
                        } else {
                            "stale"
                        };
                        result.coordination = Some(status);
                    }
                    Err(_)
                        if control::root_for_database(std::path::Path::new(database))
                            .is_some_and(|root| root.join("registry.db").exists()) =>
                    {
                        result.freshness = "stale"
                    }
                    _ => {}
                }
                Ok(result)
            })();
            execution
                .check()
                .and(result)
                .and_then(|value| execution::encode(&value, execution, maximum))
        };
    match result {
        Ok(json) => match execution.check() {
            Ok(()) => json,
            Err(error) => execution::error_json(error.code(), &error.to_string(), maximum),
        },
        Err(error) => execution::error_json(error.code(), &error.to_string(), maximum),
    }
}

pub fn workspace_json(request: &str) -> String {
    response_json(
        serde_json::from_str(request)
            .map_err(Error::from)
            .and_then(control::workspace_operation),
    )
}

pub fn error_json(code: &str, message: &str) -> String {
    serde_json::json!({"error":{"code":code,"message":message}}).to_string()
}
fn response_json<T: serde::Serialize>(result: Result<T>) -> String {
    match result {
        Ok(value) => serde_json::json!({"result":value}).to_string(),
        Err(error) => error_json(error.code(), &error.to_string()),
    }
}
