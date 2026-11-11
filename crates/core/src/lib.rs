pub mod admission;
pub mod control;
pub mod cursor;
pub mod execution;
pub mod graph;
pub(crate) mod grouping;
pub mod live;
pub mod protocol;
pub mod query;
pub mod recovery;
pub mod resource;
pub mod settings;
pub mod store;
pub mod testing;
pub mod update;
pub mod workspace;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no published index is available yet")]
    NotReady,
    #[error("query deadline exceeded; restart the request")]
    DeadlineExceeded,
    #[error("query cancelled")]
    QueryCancelled,
    #[error(
        "query response exceeds response_max_bytes; narrow the query or increase the response budget (maximum 2097152 bytes)"
    )]
    ResponseLimitExceeded,
    #[error("cursor is no longer valid; restart the query")]
    CursorInvalidated,
    #[error("snapshot no longer matches the served index; restart lookup")]
    SnapshotInvalidated,
    #[error("cursor capacity is exhausted; retry after an active page finishes")]
    CursorLimit,
    #[error("indexing cancelled; previous generation preserved")]
    Cancelled,
    #[error("{0}")]
    ResourceLimited(String),
    #[error("{0}")]
    Busy(String),
    #[error("derived index is corrupt; recovery required: {message}")]
    IndexCorrupt {
        database: std::path::PathBuf,
        incarnation: String,
        message: String,
    },
    #[error("index recovery is unavailable: {0}")]
    IndexRecovering(String),
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
            Self::QueryCancelled => "cancelled",
            Self::ResponseLimitExceeded => "response_limit_exceeded",
            Self::CursorInvalidated => "cursor_invalidated",
            Self::SnapshotInvalidated => "snapshot_invalidated",
            Self::CursorLimit => "cursor_limit",
            Self::Cancelled => "cancelled",
            Self::ResourceLimited(_) => "resource_limited",
            Self::Busy(_) => "busy",
            Self::IndexCorrupt { .. } => "index_corrupt",
            Self::IndexRecovering(_) => "index_recovering",
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

// Preserve the typed SQLite cause and the exact guarded incarnation. A
// coordinator registry error never crosses this index-connection boundary.
pub(crate) fn index_result<T>(guard: &recovery::IndexReadGuard, result: Result<T>) -> Result<T> {
    match result {
        Ok(value) => {
            guard.check()?;
            Ok(value)
        }
        Err(Error::Sqlite(error)) => {
            let error = guard.map_sqlite(error);
            if matches!(error, Error::IndexCorrupt { .. }) {
                control::index_fault_hint(guard.logical_path());
                Err(error)
            } else {
                guard.check()?;
                Err(error)
            }
        }
        Err(error) => Err(error),
    }
}

pub fn query_json(database: &str, request: &str) -> String {
    query_json_started(database, request, execution::QueryStart::default())
}

pub fn query_json_started(database: &str, request: &str, start: execution::QueryStart) -> String {
    query_json_cancellable(database, request, start, None)
}

pub fn query_json_cancellable(
    database: &str,
    request: &str,
    start: execution::QueryStart,
    cancellation: Option<execution::QueryCancellation>,
) -> String {
    query_json_in_scope(database, request, start, cancellation, None)
}

/// Explicit host scope; the default is shared across the local account.
pub fn query_json_in_scope(
    database: &str,
    request: &str,
    start: execution::QueryStart,
    cancellation: Option<execution::QueryCancellation>,
    scope: Option<&admission::AdmissionScope>,
) -> String {
    let result = (|| {
        if cancellation
            .as_ref()
            .is_some_and(execution::QueryCancellation::is_cancelled)
        {
            return Err(Error::QueryCancelled);
        }
        admission::check_request_bytes(request.len())?;
        let local;
        let scope = match scope {
            Some(scope) => scope,
            None => {
                local = admission::AdmissionScope::local()?;
                &local
            }
        };
        scope.try_reserve()
    })();
    match result {
        Ok(reservation) => {
            query_json_reserved(database, request, start, cancellation, &reservation)
        }
        Err(error) => execution::error_json(
            error.code(),
            &error.to_string(),
            execution::MIN_RESPONSE_BYTES as usize,
        ),
    }
}

/// Consumes one submission from an early host reservation. The host retains
/// the reservation through its own response parsing; nested reads borrow it.
pub fn query_json_reserved(
    database: &str,
    request: &str,
    start: execution::QueryStart,
    cancellation: Option<execution::QueryCancellation>,
    reservation: &admission::Reservation,
) -> String {
    let permit = match reservation.submit() {
        Ok(permit) => permit,
        Err(error) => {
            return execution::error_json(
                error.code(),
                &error.to_string(),
                execution::MIN_RESPONSE_BYTES as usize,
            );
        }
    };
    if cancellation
        .as_ref()
        .is_some_and(execution::QueryCancellation::is_cancelled)
    {
        return execution::error_json(
            "cancelled",
            "query cancelled",
            execution::MIN_RESPONSE_BYTES as usize,
        );
    }
    if let Err(error) = admission::check_request_bytes(request.len()) {
        return execution::error_json(
            error.code(),
            &error.to_string(),
            execution::MIN_RESPONSE_BYTES as usize,
        );
    }
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
        Ok(execution) => execution.with_cancellation(cancellation).with_permit(permit),
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
    let result = if request.page_size.is_some()
        || request.cursor.is_some()
        || request.op == "next_page"
    {
        cursor::client::query(database, request, execution.clone(), maximum)
    } else {
        let mut serving_health = None;
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
                execution.clone(),
            ) {
                Err(error @ Error::UpdateRequired(_)) => return Err(error),
                Err(error @ Error::IndexCorrupt { .. })
                | Err(error @ Error::IndexRecovering(_)) => return Err(error),
                Err(error) if matches!(&error,Error::Remote {code,..} if matches!(code.as_str(),"index_corrupt"|"index_recovering")) =>
                {
                    return Err(error);
                }
                Ok(Some(status)) if status.generation == 0 => return Err(Error::NotReady),
                other => other,
            };
            execution.check()?;
            let view = query::ReadView::open_controlled(database, execution.clone())?;
            serving_health = Some(view.health.clone());
            let mut result = view.query_controlled(request, execution.clone())?;
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
        let encoded = execution.check().and(result).and_then(|value| {
            if let Some(health) = &serving_health {
                health.check()?;
            }
            execution::encode(&value, execution.clone(), maximum)
        });
        if let Some(health) = &serving_health {
            crate::index_result(health, encoded)
        } else {
            encoded
        }
    };
    match result {
        Ok(json) => match execution.check() {
            Ok(()) => json,
            Err(error) => execution::error_json(error.code(), &error.to_string(), maximum),
        },
        Err(error) => {
            control::report_index_corruption(&error, &execution);
            let error = execution.check().err().unwrap_or(error);
            execution::error_json(error.code(), &error.to_string(), maximum)
        }
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
