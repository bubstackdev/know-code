pub mod control;
pub mod graph;
pub mod live;
pub mod protocol;
pub mod query;
pub mod resource;
pub mod store;
pub mod update;
pub mod workspace;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no published index is available yet")]
    NotReady,
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
    let result = serde_json::from_str(request).map_err(Error::from).and_then(
        |request: query::QueryRequest| {
            let wait = request
                .freshness_wait_ms
                .min(request.deadline_ms.unwrap_or(u32::MAX));
            if request.freshness_wait_ms > 120000 {
                return Err(Error::InvalidRequest(
                    "freshness_wait_ms cannot exceed 120000".into(),
                ));
            }
            let status = control::wait_for_database(
                std::path::Path::new(database),
                wait,
                request.deadline_ms,
            );
            let mut result = query::ReadView::open(database)?.query(request)?;
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
                Err(Error::UpdateRequired(message)) => return Err(Error::UpdateRequired(message)),
                Err(_)
                    if control::root_for_database(std::path::Path::new(database))
                        .is_some_and(|root| root.join("registry.db").exists()) =>
                {
                    result.freshness = "stale"
                }
                _ => {}
            }
            Ok(result)
        },
    );
    response_json(result)
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
