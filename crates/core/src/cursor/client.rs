//! In-process query/page execution with coordinator-owned leases/artifacts.
use super::*;
use crate::{
    control,
    execution::{self, QueryStart, TimedStream},
    protocol::{
        ControlRequest, ControlResponse, CursorReply, CursorRequest, CursorSettings,
        WorkspaceStatus,
    },
    query::{QueryRequest, QueryResult, ReadView},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
};

fn rpc(
    root: &Path,
    database: &str,
    mut request: CursorRequest,
    execution: Execution,
) -> Result<ControlResponse> {
    execution.check()?;
    request.remaining_ms = execution.remaining()?.as_millis().min(u128::from(u32::MAX)) as u32;
    request.deadline_at_ms = execution.deadline_ms()?;
    let stream = execution::connect(&control::socket_path(root), execution.clone()).map_err(|error| match error {
        Error::Io(error) => Error::CoordinatorUnavailable(format!("pagination requires the workspace coordinator; open a Workspace or start the installed Know Code runtime: {error}")),
        other => other,
    })?;
    let response = control::exchange_raw(
        &mut TimedStream::new(stream, execution.clone())?,
        ControlRequest {
            operation: "cursor".into(),
            database: database.into(),
            cursor: Some(request),
            ..Default::default()
        },
    )?;
    execution.check()?;
    if response.cursor_control_version != CONTROL_VERSION {
        return Err(Error::UpdateRequired("update the coordinator and query libraries together; portable cursor control is unavailable or incompatible".into()));
    }
    control::checked_response(response)
}

pub fn settings(
    root: &Path,
    value: Option<CursorLimits>,
    executable: Option<&Path>,
) -> Result<CursorReply> {
    control::ensure_running(root, executable)?;
    let execution = Execution::new(QueryStart::default(), None)?;
    rpc(
        root,
        "",
        CursorRequest {
            action: "settings".into(),
            settings: value.map(|v| CursorSettings {
                max_cursors: v.max_cursors,
                retention_bytes: v.retention_bytes,
            }),
            ..Default::default()
        },
        execution,
    )?
    .cursor
    .ok_or_else(|| Error::Protocol("missing cursor settings response".into()))
}

fn freshness(generation: u64, status: Option<&WorkspaceStatus>) -> &'static str {
    if status.is_some_and(|s| {
        s.available
            && s.phase == "idle"
            && s.observed_epoch == s.published_epoch
            && s.generation == generation
    }) {
        "current"
    } else {
        "stale"
    }
}
fn status(result: &mut QueryResult, state: Option<&WorkspaceStatus>) {
    result.freshness = freshness(result.generation, state);
    result.coordination = state.cloned();
}

struct Lease<'a> {
    root: &'a Path,
    database: &'a str,
    handle: CursorHandle,
    reply: CursorReply,
    status: Option<WorkspaceStatus>,
    execution: Execution,
    creator: bool,
    health: crate::recovery::IndexReadGuard,
}
impl<'a> Lease<'a> {
    fn from_response(
        root: &'a Path,
        database: &'a str,
        response: ControlResponse,
        execution: Execution,
        creator: bool,
    ) -> Result<Self> {
        let reply = response
            .cursor
            .ok_or_else(|| Error::Protocol("missing cursor lease response".into()))?;
        let handle = CursorHandle::parse_prefix(&reply.cursor)?;
        if reply.owner.as_slice() != handle.owner
            || reply.pin.as_slice() != handle.id
            || reply.generation == 0
            || reply.lease.len() != 32
        {
            return Err(Error::Protocol(
                "invalid cursor generation/lease descriptor".into(),
            ));
        }
        unhex::<16>(&reply.lease)?;
        let execution = execution.cap_at(Duration::from_millis(reply.lease_deadline_ms))?;
        execution.check()?;
        let health = if reply.index_incarnation.is_empty() {
            let health = crate::recovery::IndexReadGuard::open(
                Path::new(database),
                Some(execution.clone()),
            )?;
            if !health.incarnation().is_empty() {
                return Err(Error::Protocol("missing cursor index incarnation".into()));
            }
            health
        } else {
            crate::recovery::IndexReadGuard::open_incarnation(
                Path::new(database),
                &reply.index_incarnation,
                execution.clone(),
            )?
        };
        Ok(Self {
            root,
            database,
            handle,
            reply,
            status: response.workspaces.into_iter().next(),
            execution,
            creator,
            health,
        })
    }
    fn call(&self, mut request: CursorRequest) -> Result<ControlResponse> {
        request.cursor = self.handle.prefix();
        request.lease = self.reply.lease.clone();
        rpc(self.root, self.database, request, self.execution.clone())
    }
    fn finish(mut self, result: Result<String>) -> Result<String> {
        let result = crate::index_result(&self.health, result).and_then(|json| {
            self.call(CursorRequest {
                action: "finish".into(),
                ..Default::default()
            })?;
            self.execution.check()?;
            self.health.check()?;
            Ok(json)
        });
        match result {
            Ok(json) => Ok(json),
            Err(error) => {
                control::report_index_corruption(&error, &self.execution);
                // Cleanup cannot inherit a cancelled flag. It has a separate
                // small budget, never extending the coordinator read lease.
                if let Ok(cleanup) = Execution::new(Default::default(), Some(200))
                    .and_then(|e| e.cap_at(Duration::from_millis(self.reply.lease_deadline_ms)))
                {
                    self.execution = cleanup;
                }
                if self.execution.check().is_ok() {
                    let action = if self.creator {
                        "discard"
                    } else if matches!(error, Error::CursorInvalidated) {
                        "invalidate"
                    } else {
                        "finish"
                    };
                    let _ = self.call(CursorRequest {
                        action: action.into(),
                        ..Default::default()
                    });
                }
                Err(error)
            }
        }
    }
    fn pin(&self) -> GenerationPin {
        GenerationPin {
            database: self.database.into(),
            owner: self.handle.owner,
            id: self.handle.id,
            generation: self.reply.generation,
            health: self.health.clone(),
        }
    }
    fn open_result(&self, reply: &CursorReply) -> Result<File> {
        let expected = self
            .root
            .join("cursors")
            .join(format!("{}.ready", self.handle.key()));
        if Path::new(&reply.result_path) != expected || reply.result_bytes > MAX_RESULT_BYTES {
            return Err(Error::CursorInvalidated);
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&expected)
            .map_err(|_| Error::CursorInvalidated)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.mode() & 0o777 != 0o400
            || metadata.len() != reply.result_bytes
        {
            return Err(Error::CursorInvalidated);
        }
        self.execution.check()?;
        Ok(file)
    }
    fn page(
        &self,
        reply: &CursorReply,
        position: u32,
        maximum: usize,
        state: Option<&WorkspaceStatus>,
    ) -> Result<String> {
        let mut file = self.open_result(reply)?;
        let prefix = self.handle.prefix();
        let value = pages::read(
            &mut file,
            pages::ReadOptions {
                position,
                cursor_prefix: &prefix,
                expected_generation: self.reply.generation,
                response_max_bytes: maximum,
                freshness: freshness(self.reply.generation, state),
                coordination: state,
            },
            self.execution.clone(),
        )?;
        execution::encode(&value, self.execution.clone(), maximum)
    }
}

pub(crate) fn query(
    database: &str,
    mut request: QueryRequest,
    execution: Execution,
    maximum: usize,
) -> Result<String> {
    let root = control::root_for_database(Path::new(database)).ok_or_else(|| {
        Error::CoordinatorUnavailable(
            "pagination requires a coordinator-managed workspace; open/register a Workspace first"
                .into(),
        )
    })?;
    if let Some(token) = request.cursor.take() {
        if request.page_size.is_some() {
            return Err(Error::InvalidRequest(
                "a cursor cannot change page size or create a second paged scope".into(),
            ));
        }
        if !matches!(
            request.op.as_str(),
            "next_page" | "source" | "symbol_context"
        ) {
            return Err(Error::InvalidRequest(
                "a cursor supports next_page, source and symbol_context reads only".into(),
            ));
        }
        let (handle, position) = CursorHandle::parse_token(&token)?;
        let response = rpc(
            root,
            database,
            CursorRequest {
                action: "borrow".into(),
                cursor: handle.prefix(),
                ..Default::default()
            },
            execution.clone(),
        )?;
        let lease = Lease::from_response(root, database, response, execution, false)?;
        if lease.handle != handle {
            return Err(Error::CursorInvalidated);
        }
        let result = if request.op == "next_page" {
            lease.page(&lease.reply, position, maximum, lease.status.as_ref())
        } else {
            (|| {
                let mut file = lease.open_result(&lease.reply)?;
                pages::validate(
                    &mut file,
                    position,
                    &handle.prefix(),
                    lease.reply.generation,
                    lease.execution.clone(),
                )?;
                drop(file);
                let mut result =
                    ReadView::open_pinned_controlled(&lease.pin(), lease.execution.clone())?
                        .query_controlled(request, lease.execution.clone())?;
                status(&mut result, lease.status.as_ref());
                execution::encode(&result, lease.execution.clone(), maximum)
            })()
        };
        return lease.finish(result);
    }
    let page_size = request
        .page_size
        .take()
        .ok_or_else(|| Error::InvalidRequest("next_page requires a cursor".into()))?;
    if !(1..=1000).contains(&page_size)
        || !(1..=1000).contains(&request.limit)
        || !matches!(
            request.op.as_str(),
            "symbols"
                | "references"
                | "callers"
                | "callees"
                | "impact"
                | "symbol_context"
                | "coverage"
        )
    {
        return Err(Error::InvalidRequest(
            "page_size must be 1–1000 for a supported array/graph query".into(),
        ));
    }
    if request.freshness_wait_ms > 120000 {
        return Err(Error::InvalidRequest(
            "freshness_wait_ms cannot exceed 120000".into(),
        ));
    }
    match control::wait_for_database_controlled(
        Path::new(database),
        request.freshness_wait_ms,
        execution.clone(),
    ) {
        Ok(Some(s)) if s.generation == 0 => return Err(Error::NotReady),
        Err(error @ Error::UpdateRequired(_)) => return Err(error),
        _ => {}
    }
    let response = rpc(
        root,
        database,
        CursorRequest {
            action: "admit".into(),
            ..Default::default()
        },
        execution.clone(),
    )?;
    let lease = Lease::from_response(root, database, response, execution, true)?;
    let result = (|| {
        let op = request.op.clone();
        let total_limit = request.limit;
        let mut result = ReadView::open_pinned_controlled(&lease.pin(), lease.execution.clone())?
            .query_controlled(request, lease.execution.clone())?;
        status(&mut result, lease.status.as_ref());
        // Bound JSON before converting to a page-planning value; the typed
        // result/read transaction is released before file transfer begins.
        let encoded = execution::write_json(
            &result,
            Some(lease.execution.clone()),
            MAX_RESULT_BYTES as usize,
            Vec::new(),
        )?;
        drop(result);
        lease.execution.check()?;
        let value: Value = serde_json::from_slice(&encoded)?;
        drop(encoded);
        lease.execution.check()?;
        let blob = pages::encode(
            value,
            pages::EncodeOptions {
                op: &op,
                total_limit,
                page_size,
                cursor_prefix: &lease.handle.prefix(),
                response_max_bytes: maximum,
            },
            lease.execution.clone(),
        )?;
        lease.call(CursorRequest {
            action: "reserve".into(),
            bytes: blob.len() as u64,
            ..Default::default()
        })?;
        let mut digest = Sha256::new();
        let mut offset = 0u64;
        for chunk in blob.chunks(MAX_RESULT_CHUNK) {
            lease.execution.check()?;
            digest.update(chunk);
            let response = lease.call(CursorRequest {
                action: "chunk".into(),
                offset,
                chunk: chunk.to_vec(),
                ..Default::default()
            })?;
            offset += chunk.len() as u64;
            if response.cursor.as_ref().is_none_or(|r| r.offset != offset) {
                return Err(Error::Protocol(
                    "cursor upload acknowledgement has the wrong offset".into(),
                ));
            }
        }
        drop(blob);
        let response = lease.call(CursorRequest {
            action: "complete".into(),
            sha256: format!("{:x}", digest.finalize()),
            ..Default::default()
        })?;
        let reply = response
            .cursor
            .as_ref()
            .ok_or_else(|| Error::Protocol("missing completed cursor result".into()))?;
        lease.page(reply, 0, maximum, response.workspaces.first())
    })();
    lease.finish(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{self, Envelope, VERSION, envelope::Payload};
    use std::os::unix::net::UnixListener;
    #[test]
    fn cancelled_upload_discards_partial_file_and_pin_with_uncancelled_cleanup() {
        use crate::{cursor::CursorService, protocol::WorkspaceStatus};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
            .join(".artifacts/qcu")
            .join(format!(
                "{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        let db = root.join("workspaces/w/index.db");
        crate::cursor::lifecycle_tests::publish(&db, "original source");
        let service = CursorService::new(&root, [db.clone()]).unwrap();
        let listener = UnixListener::bind(control::socket_path(&root)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let cancellation = crate::execution::QueryCancellation::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    let (mut stream, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "cursor cleanup never reached the coordinator"
                            );
                            std::thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        Err(error) => panic!("{error}"),
                    };
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let frame = protocol::read_frame(&mut stream).unwrap().unwrap();
                    let Some(Payload::ControlRequest(request)) = frame.payload else {
                        panic!("expected control request");
                    };
                    let mut response = ControlResponse {
                        index_lifecycle_version: crate::protocol::INDEX_LIFECYCLE_VERSION,
                        cursor_control_version: CONTROL_VERSION,
                        resource_control_version: crate::resource::CONTROL_VERSION,
                        analysis_settings_version: crate::settings::VERSION,
                        workspaces: vec![WorkspaceStatus {
                            database: db.to_string_lossy().into_owned(),
                            generation: 1,
                            phase: "idle".into(),
                            available: true,
                            observed_epoch: 1,
                            published_epoch: 1,
                            ..Default::default()
                        }],
                        ..Default::default()
                    };
                    let action = request
                        .cursor
                        .as_ref()
                        .map(|r| r.action.clone())
                        .unwrap_or_default();
                    if let Some(request) = request.cursor {
                        response.cursor =
                            Some(service.request(&db, request, Default::default()).unwrap());
                    }
                    if action == "chunk" {
                        cancellation.cancel();
                    }
                    // The cancelled sender may already have closed its first
                    // socket. Its separate cleanup must still reach discard.
                    let _ = protocol::write_frame(
                        &mut stream,
                        &Envelope {
                            protocol_version: VERSION,
                            payload: Some(Payload::ControlResponse(response)),
                            ..Default::default()
                        },
                    );
                    if action == "discard" {
                        break;
                    }
                }
            });
            let result = crate::query_json_cancellable(
                db.to_str().unwrap(),
                r#"{"op":"symbols","page_size":1}"#,
                Default::default(),
                Some(cancellation.clone()),
            );
            assert_eq!(
                serde_json::from_str::<Value>(&result).unwrap()["error"]["code"],
                "cancelled"
            );
            server.join().unwrap();
        });
        assert_eq!(std::fs::read_dir(root.join("cursors")).unwrap().count(), 0);
        let connection = rusqlite::Connection::open(&db).unwrap();
        let pins: u64 = connection
            .query_row("SELECT count(*) FROM cursor_generation_pins", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(pins, 0);
        let generation: u64 = connection
            .query_row("SELECT current_generation FROM state", [], |row| row.get(0))
            .unwrap();
        assert_eq!(generation, 1);
    }
    #[test]
    fn older_coordinator_requires_update_before_cursor_use() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
            .join(".artifacts/cc")
            .join(format!(
                "{:x}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
        std::fs::create_dir_all(&root).unwrap();
        let listener = UnixListener::bind(control::socket_path(&root)).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = protocol::read_frame(&mut stream).unwrap();
            protocol::write_frame(
                &mut stream,
                &Envelope {
                    protocol_version: VERSION,
                    payload: Some(Payload::ControlResponse(ControlResponse {
                        index_lifecycle_version: crate::protocol::INDEX_LIFECYCLE_VERSION,
                        error_code: "coordinator_error".into(),
                        error_message: "unknown control operation".into(),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            )
            .unwrap();
        });
        let result = rpc(
            &root,
            "",
            CursorRequest {
                action: "settings".into(),
                ..Default::default()
            },
            Execution::new(Default::default(), None).unwrap(),
        );
        assert!(matches!(result, Err(Error::UpdateRequired(_))));
        server.join().unwrap();
    }
}
