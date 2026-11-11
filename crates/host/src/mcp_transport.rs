//! Limits around the SDK's existing JSON-RPC codec, not another protocol.
use rmcp::{
    model::{
        ClientNotification, ClientRequest, ErrorData, JsonRpcMessage, ProtocolVersion, RequestId,
        ServerConfig, ServerResult,
    },
    service::{
        NotificationContext, RequestContext, RoleServer, RxJsonRpcMessage, TxJsonRpcMessage,
    },
    transport::Transport,
};
use serde::Serialize;
use std::{
    borrow::Cow,
    collections::HashMap,
    io,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{io::AsyncWrite, sync::Notify};

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const OUTPUT_DEADLINE: Duration = Duration::from_secs(5);
const MAX_PENDING_RESPONSES: usize = 4;
const MAX_OWNED_FRAMES: usize = 2;

type KeepAlive = Arc<dyn Send + Sync>;
struct PendingResponse {
    serial: usize,
    started: bool,
    reservation: Option<KeepAlive>,
    cancelled: Option<Box<dyn Fn() -> bool + Send + Sync>>,
}

#[derive(Default)]
pub struct Connection {
    failed: AtomicBool,
    changed: Notify,
    cancellation: know_code_core::execution::QueryCancellation,
    responses: Mutex<HashMap<RequestId, PendingResponse>>,
    next_response: AtomicUsize,
    frames: AtomicUsize,
}
impl Connection {
    fn fail(&self) {
        self.cancellation.cancel();
        self.failed.store(true, Ordering::Release);
        self.responses.lock().unwrap().clear();
        self.changed.notify_one();
    }
    fn register_request(&self, id: RequestId) -> io::Result<()> {
        self.check()?;
        let mut responses = self.responses.lock().unwrap();
        self.check()?;
        if responses.contains_key(&id) {
            return Err(io::Error::other("duplicate pending MCP request id"));
        }
        if responses.len() == MAX_PENDING_RESPONSES {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "MCP pending request capacity exhausted",
            ));
        }
        responses.insert(
            id,
            PendingResponse {
                serial: self.next_response.fetch_add(1, Ordering::Relaxed),
                started: false,
                reservation: None,
                cancelled: None,
            },
        );
        Ok(())
    }
    /// A bounded bridge across the SDK's owned response model. The handler
    /// removes unfinished work on Drop; successful handoff is taken by send.
    pub fn begin_response(
        self: &Arc<Self>,
        id: RequestId,
        cancelled: impl Fn() -> bool + Send + Sync + 'static,
    ) -> io::Result<ResponseHandoff> {
        self.check()?;
        let mut responses = self.responses.lock().unwrap();
        self.check()?;
        if responses.get(&id).is_some_and(|entry| entry.started) {
            drop(responses);
            self.fail();
            return Err(io::Error::other("duplicate pending MCP request id"));
        }
        if !responses.contains_key(&id) && responses.len() == MAX_PENDING_RESPONSES {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "MCP responses are awaiting delivery; retry later",
            ));
        }
        let entry = responses
            .entry(id.clone())
            .or_insert_with(|| PendingResponse {
                serial: self.next_response.fetch_add(1, Ordering::Relaxed),
                started: false,
                reservation: None,
                cancelled: None,
            });
        entry.started = true;
        entry.cancelled = Some(Box::new(cancelled));
        let serial = entry.serial;
        Ok(ResponseHandoff {
            connection: self.clone(),
            id,
            serial,
            handed_off: false,
        })
    }
    fn prune_cancelled(&self) {
        self.responses.lock().unwrap().retain(|_, entry| {
            !entry
                .cancelled
                .as_ref()
                .is_some_and(|cancelled| cancelled())
        });
    }
    fn take_response(&self, id: &RequestId) -> Option<KeepAlive> {
        self.responses.lock().unwrap().remove(id)?.reservation
    }
    pub fn retain_response(&self, key: &ResponseKey, reservation: KeepAlive) -> io::Result<()> {
        let mut responses = self.responses.lock().unwrap();
        self.check()?;
        let entry = responses
            .get_mut(&key.id)
            .filter(|entry| entry.started && entry.serial == key.serial)
            .ok_or_else(|| io::Error::other("MCP request ownership is no longer active"))?;
        entry.reservation = Some(reservation);
        Ok(())
    }
    fn begin_frame(self: &Arc<Self>) -> io::Result<FrameOwnership> {
        self.check()?;
        if self
            .frames
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_OWNED_FRAMES).then_some(count + 1)
            })
            .is_err()
        {
            self.fail();
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "MCP output capacity exhausted; connection closed",
            ));
        }
        Ok(FrameOwnership(self.clone()))
    }
    pub fn query_cancellation(&self) -> know_code_core::execution::QueryCancellation {
        self.cancellation.child()
    }
    fn check(&self) -> io::Result<()> {
        if self.failed.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "MCP connection closed",
            ))
        } else {
            Ok(())
        }
    }
}

pub struct ResponseHandoff {
    connection: Arc<Connection>,
    id: RequestId,
    serial: usize,
    handed_off: bool,
}
#[derive(Clone)]
pub struct ResponseKey {
    id: RequestId,
    serial: usize,
}
impl ResponseHandoff {
    fn key(&self) -> ResponseKey {
        ResponseKey {
            id: self.id.clone(),
            serial: self.serial,
        }
    }
    #[cfg(test)]
    pub fn retain(&self, reservation: KeepAlive) {
        if let Some(entry) = self.connection.responses.lock().unwrap().get_mut(&self.id)
            && entry.serial == self.serial
        {
            entry.reservation = Some(reservation);
        }
    }
    pub fn complete(mut self) {
        self.handed_off = true;
    }
}

/// Forward through the SDK's existing dispatcher while owning every response,
/// including ping/list and errors rejected before a tool handler is called.
pub struct ResponseService<S> {
    inner: S,
    connection: Arc<Connection>,
}
impl<S> ResponseService<S> {
    pub fn new(inner: S, connection: Arc<Connection>) -> Self {
        Self { inner, connection }
    }
}
impl<S: rmcp::Service<RoleServer>> rmcp::Service<RoleServer> for ResponseService<S> {
    async fn handle_request(
        &self,
        request: ClientRequest,
        mut context: RequestContext<RoleServer>,
    ) -> Result<ServerResult, ErrorData> {
        let cancellation = context.ct.clone();
        let probe = cancellation.clone();
        let handoff = self
            .connection
            .begin_response(context.id.clone(), move || probe.is_cancelled())
            .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
        context.extensions.insert(handoff.key());
        if cancellation.is_cancelled() {
            return Err(ErrorData::internal_error(
                "request cancelled",
                Some(serde_json::json!({"code":"cancelled"})),
            ));
        }
        let result = self.inner.handle_request(request, context).await;
        if !cancellation.is_cancelled() {
            handoff.complete();
        }
        result
    }
    async fn handle_notification(
        &self,
        notification: ClientNotification,
        context: NotificationContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.inner.handle_notification(notification, context).await
    }
    fn get_info(&self) -> ServerConfig {
        self.inner.get_info()
    }
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        self.inner.supported_protocol_versions()
    }
}
impl Drop for ResponseHandoff {
    fn drop(&mut self) {
        if self.handed_off {
            return;
        }
        let mut responses = self.connection.responses.lock().unwrap();
        if responses
            .get(&self.id)
            .is_some_and(|entry| entry.serial == self.serial)
        {
            responses.remove(&self.id);
        }
    }
}
struct FrameOwnership(Arc<Connection>);
impl Drop for FrameOwnership {
    fn drop(&mut self) {
        self.0.frames.fetch_sub(1, Ordering::AcqRel);
    }
}

fn fits(value: &impl Serialize, maximum: usize) -> bool {
    struct Counter(usize);
    impl io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self
                .0
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("MCP frame exceeds limit"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    // The SDK writes one LF after the serialized JSON-RPC envelope.
    maximum
        .checked_sub(1)
        .is_some_and(|budget| serde_json::to_writer(Counter(budget), value).is_ok())
}

/// Fence the SDK writer itself: queued sends cannot append another frame after
/// a timed-out or failed partial write. Count actual bytes including every LF.
pub struct Output<W> {
    inner: W,
    connection: Arc<Connection>,
    frame_bytes: usize,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
    deadline: Duration,
}
impl<W> Output<W> {
    pub fn new(inner: W, connection: Arc<Connection>) -> Self {
        Self {
            inner,
            connection,
            frame_bytes: 0,
            timer: None,
            deadline: OUTPUT_DEADLINE,
        }
    }
    fn check_output(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        self.connection.check()?;
        let timer = self
            .timer
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(self.deadline)));
        if timer.as_mut().poll(cx).is_ready() {
            self.connection.fail();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "MCP output deadline exceeded",
            ));
        }
        Ok(())
    }
}
impl<W: AsyncWrite + Unpin> AsyncWrite for Output<W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if let Err(error) = this.check_output(cx) {
            return Poll::Ready(Err(error));
        }
        let bytes = &bytes[..bytes.len().min(16 * 1024)];
        let mut count = this.frame_bytes;
        for byte in bytes {
            count += 1;
            if count > MAX_FRAME_BYTES {
                this.connection.fail();
                return Poll::Ready(Err(io::Error::other("serialized MCP frame exceeds 8 MiB")));
            }
            if *byte == b'\n' {
                count = 0;
            }
        }
        match Pin::new(&mut this.inner).poll_write(cx, bytes) {
            Poll::Ready(Ok(written)) => {
                for byte in &bytes[..written] {
                    this.frame_bytes += 1;
                    if *byte == b'\n' {
                        this.frame_bytes = 0;
                    }
                }
                Poll::Ready(Ok(written))
            }
            Poll::Ready(Err(error)) => {
                this.connection.fail();
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Err(error) = this.check_output(cx) {
            return Poll::Ready(Err(error));
        }
        match Pin::new(&mut this.inner).poll_flush(cx) {
            Poll::Ready(Ok(())) => {
                this.timer = None;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => {
                this.connection.fail();
                Poll::Ready(Err(error))
            }
            Poll::Pending => Poll::Pending,
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

pub struct Bounded<T> {
    inner: T,
    connection: Arc<Connection>,
    deadline: Duration,
}
impl<T> Bounded<T> {
    pub fn new(inner: T, connection: Arc<Connection>) -> Self {
        Self {
            inner,
            connection,
            deadline: OUTPUT_DEADLINE,
        }
    }
}
impl<T: Transport<RoleServer, Error = io::Error>> Transport<RoleServer> for Bounded<T> {
    type Error = io::Error;
    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = io::Result<()>> + Send + 'static {
        let frame = self.connection.begin_frame();
        let reservation = match &item {
            JsonRpcMessage::Response(response) => self.connection.take_response(&response.id),
            JsonRpcMessage::Error(error) => error
                .id
                .as_ref()
                .and_then(|id| self.connection.take_response(id)),
            _ => None,
        };
        let deadline = tokio::time::Instant::now() + self.deadline;
        // Preflight the actual envelope without allocating another copy. An
        // oversized response becomes an explicit small error before any bytes
        // of that response enter the SDK writer.
        let item = if let Err(error) = &frame {
            Err(io::Error::new(error.kind(), error.to_string()))
        } else if fits(&item, MAX_FRAME_BYTES) {
            Ok(item)
        } else {
            match item {
                JsonRpcMessage::Response(response) => Ok(JsonRpcMessage::error(
                    ErrorData::internal_error(
                        "MCP response exceeds the 8 MiB serialized frame limit",
                        Some(serde_json::json!({"code":"response_limit_exceeded"})),
                    ),
                    Some(response.id),
                )),
                _ => Err(io::Error::other("outgoing MCP message exceeds frame limit")),
            }
        };
        let send = item.map(|item| self.inner.send(item));
        let connection = self.connection.clone();
        async move {
            let _frame = frame?;
            let _reservation = reservation;
            connection.check()?;
            let result = match send {
                Ok(send) => tokio::time::timeout_at(deadline, send)
                    .await
                    .unwrap_or_else(|_| {
                        Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "MCP output deadline exceeded",
                        ))
                    }),
                Err(error) => Err(error),
            };
            if result.is_err() {
                connection.fail();
            }
            result
        }
    }
    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        // rmcp processes the prior event before asking for another message.
        // Inspect its actual cancellation token, not wire notification ids:
        // modern cancellation can refer to opposite-direction subscriptions.
        self.connection.prune_cancelled();
        self.connection.check().ok()?;
        tokio::select! {
            biased;
            _=self.connection.changed.notified()=>None,
            value=self.inner.receive()=>{
                if let Some(JsonRpcMessage::Request(request)) = &value {
                    // Register before rmcp dispatches a handler. Both query→ping
                    // and ping→query ID collisions must fail even when neither
                    // handler has been polled yet. No unbounded ID registry.
                    if self.connection.register_request(request.id.clone()).is_err() {
                        self.connection.fail();
                        return None;
                    }
                }
                if value.is_none() { self.connection.fail(); }
                value
            },
        }
    }
    async fn close(&mut self) -> io::Result<()> {
        self.connection.fail();
        tokio::time::timeout(OUTPUT_DEADLINE, self.inner.close())
            .await
            .unwrap_or_else(|_| {
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "MCP close deadline exceeded",
                ))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::{
        model::{CallToolResult, ServerResult},
        transport::async_rw::AsyncRwTransport,
    };
    use tokio::io::AsyncReadExt;
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }
    fn message(value: serde_json::Value) -> TxJsonRpcMessage<RoleServer> {
        JsonRpcMessage::response(
            ServerResult::CallToolResult(CallToolResult::structured(value)),
            rmcp::model::NumberOrString::Number(7),
        )
    }
    #[test]
    fn handoff_is_bounded_drop_safe_and_cancelled_work_keeps_its_own_charge() {
        let connection = Arc::new(Connection::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let first = connection
            .begin_response(RequestId::Number(1), move || flag.load(Ordering::Acquire))
            .unwrap();
        let reservation = Arc::new(());
        let alive = Arc::downgrade(&reservation);
        first.retain(reservation.clone());
        // The native task owns the same charge independently of response state.
        let worker = reservation.clone();
        drop(reservation);
        first.complete();
        let second = connection
            .begin_response(RequestId::Number(2), || false)
            .unwrap();
        let third = connection
            .begin_response(RequestId::Number(3), || false)
            .unwrap();
        let fourth = connection
            .begin_response(RequestId::Number(4), || false)
            .unwrap();
        assert!(matches!(
            connection.begin_response(RequestId::Number(5), || false),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock
        ));
        cancelled.store(true, Ordering::Release);
        connection.prune_cancelled();
        assert!(alive.upgrade().is_some());
        drop(worker);
        assert!(alive.upgrade().is_none());
        drop(second);
        drop(third);
        drop(fourth);
        assert!(connection.responses.lock().unwrap().is_empty());
        let abandoned = connection
            .begin_response(RequestId::Number(3), || false)
            .unwrap();
        let reservation = Arc::new(());
        let alive = Arc::downgrade(&reservation);
        abandoned.retain(reservation);
        drop(abandoned);
        assert!(alive.upgrade().is_none());
    }
    #[test]
    fn duplicate_pending_id_closes_without_replacing_a_reservation() {
        let connection = Arc::new(Connection::default());
        let first = connection
            .begin_response(RequestId::Number(1), || false)
            .unwrap();
        let reservation = Arc::new(());
        let alive = Arc::downgrade(&reservation);
        first.retain(reservation);
        first.complete();
        assert!(
            connection
                .begin_response(RequestId::Number(1), || false)
                .is_err()
        );
        assert!(connection.check().is_err());
        assert!(alive.upgrade().is_none());
    }
    #[test]
    fn duplicate_non_tool_request_cannot_take_a_pending_query_handoff() {
        use tokio::io::AsyncWriteExt;
        runtime().block_on(async {
            let (mut input, read) = tokio::io::duplex(256);
            let (write, _output) = tokio::io::duplex(128);
            let connection = Arc::new(Connection::default());
            let handoff = connection
                .begin_response(RequestId::Number(7), || false)
                .unwrap();
            let reservation = Arc::new(());
            let alive = Arc::downgrade(&reservation);
            handoff.retain(reservation);
            handoff.complete();
            let inner = AsyncRwTransport::<RoleServer, _, _>::new(
                read,
                Output::new(write, connection.clone()),
            );
            let mut transport = Bounded::new(inner, connection.clone());
            input
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n")
                .await
                .unwrap();
            assert!(transport.receive().await.is_none());
            assert!(connection.check().is_err());
            assert!(alive.upgrade().is_none());
        });
    }
    #[test]
    fn actual_sdk_duplicate_id_during_delayed_handler_never_admits_query() {
        use rmcp::{ServerHandler, ServiceExt, model::*, service::RequestContext};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        struct Delayed {
            connection: Arc<Connection>,
            entered: Arc<Notify>,
            resume: Arc<Notify>,
            admitted: Arc<AtomicBool>,
        }
        impl ServerHandler for Delayed {
            fn get_info(&self) -> ServerConfig {
                ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            }
            async fn call_tool(
                &self,
                _: CallToolRequestParams,
                context: RequestContext<RoleServer>,
            ) -> Result<CallToolResponse, ErrorData> {
                self.entered.notify_one();
                self.resume.notified().await;
                let token = context.ct.clone();
                if let Ok(handoff) = self
                    .connection
                    .begin_response(context.id, move || token.is_cancelled())
                {
                    self.admitted.store(true, Ordering::Release);
                    handoff.complete();
                }
                Ok(CallToolResult::structured(serde_json::json!({"done":true})).into())
            }
        }
        runtime().block_on(async {
            let (mut input, read) = tokio::io::duplex(4096);
            let (write, output) = tokio::io::duplex(4096);
            let mut output = BufReader::new(output);
            let connection = Arc::new(Connection::default());
            let entered = Arc::new(Notify::new());
            let resume = Arc::new(Notify::new());
            let admitted = Arc::new(AtomicBool::new(false));
            let service = Delayed {
                connection: connection.clone(),
                entered: entered.clone(),
                resume: resume.clone(),
                admitted: admitted.clone(),
            };
            let inner = AsyncRwTransport::<RoleServer, _, _>::new(
                read,
                Output::new(write, connection.clone()),
            );
            let transport = Bounded::new(inner, connection.clone());
            let server = tokio::spawn(async move {
                service.serve(transport).await.unwrap().waiting().await
            });
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"admission-test\",\"version\":\"1\"}}}\n").await.unwrap();
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(2), output.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"delayed\",\"arguments\":{}}}\n").await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), entered.notified())
                .await
                .unwrap();
            assert!(!connection.responses.lock().unwrap()[&RequestId::Number(7)].started);
            input
                .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n")
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while connection.check().is_ok() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            resume.notify_one();
            drop(input);
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(!admitted.load(Ordering::Acquire));
            assert!(connection.responses.lock().unwrap().is_empty());
        });
    }
    #[test]
    fn actual_sdk_repeated_cancel_before_dispatch_releases_non_tool_placeholders() {
        use rmcp::{ServerHandler, ServiceExt, model::*};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        type CancellationProbe = Arc<Mutex<Option<Box<dyn Fn() -> bool + Send + Sync>>>>;
        struct Empty;
        impl ServerHandler for Empty {
            fn get_info(&self) -> ServerConfig {
                ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            }
            async fn list_tools(
                &self,
                _: Option<PaginatedRequestParams>,
                _: RequestContext<RoleServer>,
            ) -> Result<ListToolsResult, ErrorData> {
                Ok(ListToolsResult::with_all_items(vec![]))
            }
        }
        // Only this owned fixture delays the SDK service before the ownership
        // adapter is polled. Production adds no task or waiting queue.
        struct Deferred<S> {
            inner: S,
            entered: Arc<Notify>,
            resume: Arc<Notify>,
            cancelled: CancellationProbe,
        }
        impl<S: rmcp::Service<RoleServer>> rmcp::Service<RoleServer> for Deferred<S> {
            async fn handle_request(
                &self,
                request: ClientRequest,
                context: RequestContext<RoleServer>,
            ) -> Result<ServerResult, ErrorData> {
                if !matches!(&request, ClientRequest::InitializeRequest(_)) {
                    let token = context.ct.clone();
                    *self.cancelled.lock().unwrap() = Some(Box::new(move || token.is_cancelled()));
                    self.entered.notify_one();
                    self.resume.notified().await;
                }
                self.inner.handle_request(request, context).await
            }
            async fn handle_notification(
                &self,
                notification: ClientNotification,
                context: NotificationContext<RoleServer>,
            ) -> Result<(), ErrorData> {
                self.inner.handle_notification(notification, context).await
            }
            fn get_info(&self) -> ServerConfig {
                self.inner.get_info()
            }
        }
        runtime().block_on(async {
            let (mut input, read) = tokio::io::duplex(8192);
            let (write, output) = tokio::io::duplex(8192);
            let mut output = BufReader::new(output);
            let connection = Arc::new(Connection::default());
            let entered = Arc::new(Notify::new());
            let resume = Arc::new(Notify::new());
            let cancelled = Arc::new(Mutex::new(None));
            let service = Deferred {
                inner: ResponseService::new(Empty, connection.clone()),
                entered: entered.clone(),
                resume: resume.clone(),
                cancelled: cancelled.clone(),
            };
            let inner = AsyncRwTransport::<RoleServer, _, _>::new(
                read,
                Output::new(write, connection.clone()),
            );
            let transport = Bounded::new(inner, connection.clone());
            let server = tokio::spawn(async move {
                service.serve(transport).await.unwrap().waiting().await
            });
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"admission-test\",\"version\":\"1\"}}}\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n").await.unwrap();
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(2), output.read_line(&mut line))
                .await.unwrap().unwrap();
            for id in 10..16 {
                let method = if id % 2 == 0 { "ping" } else { "tools/list" };
                input.write_all(format!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"{method}\"}}\n").as_bytes()).await.unwrap();
                tokio::time::timeout(Duration::from_secs(2), entered.notified()).await.unwrap();
                assert!(!connection.responses.lock().unwrap()[&RequestId::Number(id)].started);
                input.write_all(format!("{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{{\"requestId\":{id}}}}}\n").as_bytes()).await.unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !cancelled.lock().unwrap().as_ref().is_some_and(|probe| probe()) {
                        tokio::task::yield_now().await;
                    }
                }).await.unwrap();
                resume.notify_one();
                tokio::time::timeout(Duration::from_secs(2), async {
                    while !connection.responses.lock().unwrap().is_empty() {
                        tokio::task::yield_now().await;
                    }
                }).await.unwrap();
                assert!(connection.check().is_ok());
            }
            // Metadata rejection also passes the adapter before SDK validation.
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":90,\"method\":\"tools/list\",\"params\":{\"_meta\":{\"io.modelcontextprotocol/protocolVersion\":\"2099-01-01\"}}}\n").await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), entered.notified()).await.unwrap();
            resume.notify_one();
            line.clear();
            tokio::time::timeout(Duration::from_secs(2), output.read_line(&mut line)).await.unwrap().unwrap();
            let invalid: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(invalid["id"], 90);
            assert!(invalid.get("error").is_some());
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"ping\"}\n").await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), entered.notified()).await.unwrap();
            resume.notify_one();
            line.clear();
            tokio::time::timeout(Duration::from_secs(2), output.read_line(&mut line)).await.unwrap().unwrap();
            let healthy: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(healthy["id"], 99);
            assert!(healthy.get("result").is_some());
            assert!(connection.responses.lock().unwrap().is_empty());
            assert!(connection.check().is_ok());
            drop(input);
            tokio::time::timeout(Duration::from_secs(2), server).await.unwrap().unwrap().unwrap();
        });
    }
    #[test]
    fn unpolled_send_owns_reservation_and_all_frame_kinds_have_a_shared_bound() {
        runtime().block_on(async {
            let (_input, read) = tokio::io::duplex(32);
            let (write, _output) = tokio::io::duplex(16);
            let connection = Arc::new(Connection::default());
            let handoff = connection
                .begin_response(RequestId::Number(7), || true)
                .unwrap();
            let reservation = Arc::new(());
            let alive = Arc::downgrade(&reservation);
            handoff.retain(reservation);
            handoff.complete();
            let inner = AsyncRwTransport::<RoleServer, _, _>::new(
                read,
                Output::new(write, connection.clone()),
            );
            let mut transport = Bounded::new(inner, connection.clone());
            // Like rmcp's ToSink event, cancellation precedes send, but the
            // synchronous take precedes the next receive/prune and async write.
            let first = transport.send(message(serde_json::json!({"body":"x".repeat(128)})));
            connection.prune_cancelled();
            assert!(alive.upgrade().is_some());
            assert!(connection.responses.lock().unwrap().is_empty());
            let second = transport.send(JsonRpcMessage::error(
                ErrorData::internal_error("busy", None),
                Some(RequestId::Number(8)),
            ));
            assert_eq!(connection.frames.load(Ordering::Acquire), 2);
            let third = transport.send(JsonRpcMessage::error(
                ErrorData::internal_error("status/error also consumes output capacity", None),
                None,
            ));
            assert!(third.await.is_err());
            assert!(connection.check().is_err());
            drop(first);
            assert!(alive.upgrade().is_none());
            drop(second);
            assert_eq!(connection.frames.load(Ordering::Acquire), 0);
        });
    }
    #[test]
    fn actual_sdk_auto_cancel_and_further_receive_keep_success_until_output_finishes() {
        use rmcp::{ServerHandler, ServiceExt, model::*, service::RequestContext};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        type CancellationProbe = Arc<Mutex<Option<Box<dyn Fn() -> bool + Send + Sync>>>>;
        struct Probe {
            connection: Arc<Connection>,
            alive: Arc<Mutex<Option<std::sync::Weak<()>>>>,
            cancelled: CancellationProbe,
        }
        impl ServerHandler for Probe {
            fn get_info(&self) -> ServerConfig {
                ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            }
            async fn call_tool(
                &self,
                _: CallToolRequestParams,
                context: RequestContext<RoleServer>,
            ) -> Result<CallToolResponse, ErrorData> {
                let token = context.ct.clone();
                *self.cancelled.lock().unwrap() = Some(Box::new(move || token.is_cancelled()));
                let reservation = Arc::new(());
                *self.alive.lock().unwrap() = Some(Arc::downgrade(&reservation));
                self.connection
                    .retain_response(
                        context.extensions.get::<ResponseKey>().unwrap(),
                        reservation,
                    )
                    .unwrap();
                Ok(CallToolResult::structured(serde_json::json!({"body":"x".repeat(4096)})).into())
            }
        }
        runtime().block_on(async {
            let (mut input, read) = tokio::io::duplex(4096);
            let (write, output) = tokio::io::duplex(128);
            let mut output = BufReader::new(output);
            let connection = Arc::new(Connection::default());
            let alive = Arc::new(Mutex::new(None));
            let cancelled = Arc::new(Mutex::new(None));
            let service = Probe {
                connection: connection.clone(),
                alive: alive.clone(),
                cancelled: cancelled.clone(),
            };
            let inner = AsyncRwTransport::<RoleServer, _, _>::new(
                read,
                Output::new(write, connection.clone()),
            );
            let transport = Bounded::new(inner, connection.clone());
            let service = ResponseService::new(service, connection.clone());
            let server = tokio::spawn(async move {
                service.serve(transport).await.unwrap().waiting().await
            });
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"admission-test\",\"version\":\"1\"}}}\n").await.unwrap();
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(2), output.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"], 1);
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"tools/call\",\"params\":{\"name\":\"probe\",\"arguments\":{}}}\n").await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while connection.frames.load(Ordering::Acquire) != 1
                    || alive.lock().unwrap().is_none()
                    || !cancelled
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|probe| probe())
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            // rmcp has automatically cancelled the completed request token.
            // A subsequent receive/prune must not release its blocked output.
            assert!(cancelled.lock().unwrap().as_ref().unwrap()());
            assert!(connection.responses.lock().unwrap().is_empty());
            assert!(alive.lock().unwrap().as_ref().unwrap().upgrade().is_some());
            input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":8,\"method\":\"ping\"}\n").await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while connection.frames.load(Ordering::Acquire) != 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(alive.lock().unwrap().as_ref().unwrap().upgrade().is_some());
            for id in [7, 8] {
                line.clear();
                tokio::time::timeout(Duration::from_secs(2), output.read_line(&mut line))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(serde_json::from_str::<serde_json::Value>(&line).unwrap()["id"], id);
            }
            assert!(alive.lock().unwrap().as_ref().unwrap().upgrade().is_none());
            drop(input);
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        });
    }
    #[test]
    fn complete_frame_counts_text_duplication_escaping_id_and_newline() {
        let value = message(serde_json::json!({"source":"\\\"😀\n".repeat(128)}));
        let actual = serde_json::to_vec(&value).unwrap().len() + 1;
        assert!(fits(&value, actual));
        assert!(!fits(&value, actual - 1));
        assert!(actual > 2 * 128 * "\\\"😀\n".len());
    }
    #[test]
    fn oversized_response_becomes_bounded_error_and_connection_stays_usable() {
        runtime().block_on(async {
            let (_input, read) = tokio::io::duplex(32);
            let (write, mut output) = tokio::io::duplex(4096);
            let connection = Arc::new(Connection::default());
            let inner = AsyncRwTransport::<RoleServer, _, _>::new(
                read,
                Output::new(write, connection.clone()),
            );
            let mut transport = Bounded::new(inner, connection);
            transport
                .send(message(
                    serde_json::json!({"source":"x".repeat(MAX_FRAME_BYTES)}),
                ))
                .await
                .unwrap();
            let mut bytes = [0; 4096];
            let n = output.read(&mut bytes).await.unwrap();
            assert!(n < 1024);
            let value: serde_json::Value = serde_json::from_slice(&bytes[..n]).unwrap();
            assert_eq!(value["id"], 7);
            assert_eq!(value["error"]["data"]["code"], "response_limit_exceeded");
            transport
                .send(message(serde_json::json!({"ok":true})))
                .await
                .unwrap();
            let n = output.read(&mut bytes).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes[..n]).unwrap()["id"],
                7
            );
        });
    }
    #[test]
    fn blocked_partial_output_times_out_and_fences_queued_sends_and_receive() {
        runtime().block_on(async {
            let (_input, read) = tokio::io::duplex(32);
            let (write, mut output) = tokio::io::duplex(16);
            let connection = Arc::new(Connection::default());
            let inner = AsyncRwTransport::<RoleServer, _, _>::new(
                read,
                Output::new(write, connection.clone()),
            );
            let mut transport = Bounded::new(inner, connection);
            transport.deadline = Duration::from_millis(20);
            let first = transport.send(message(serde_json::json!({"source":"x".repeat(4096)})));
            let second = transport.send(message(serde_json::json!({"next":true})));
            assert_eq!(first.await.unwrap_err().kind(), io::ErrorKind::TimedOut);
            assert!(second.await.is_err());
            assert!(transport.receive().await.is_none());
            let mut bytes = [0; 64];
            let n = output.read(&mut bytes).await.unwrap();
            assert_eq!(n, 16);
            assert!(!bytes[..n].contains(&b'\n'));
        });
    }
    #[test]
    fn sdk_internal_error_writes_also_have_the_output_deadline() {
        use tokio::io::AsyncWriteExt;
        runtime().block_on(async {
            let (write, _unread) = tokio::io::duplex(8);
            let mut output = Output::new(write, Arc::new(Connection::default()));
            output.deadline = Duration::from_millis(20);
            let error = output.write_all(&[b'x'; 128]).await.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert!(output.write_all(b"later\n").await.is_err());
        });
    }
    #[test]
    fn input_eof_cancels_active_query_tokens_without_waiting_for_sdk_drain() {
        runtime().block_on(async {
            let (input, read) = tokio::io::duplex(32);
            let (write, _output) = tokio::io::duplex(128);
            let connection = Arc::new(Connection::default());
            let query = connection.query_cancellation();
            let inner = AsyncRwTransport::<RoleServer, _, _>::new(
                read,
                Output::new(write, connection.clone()),
            );
            let mut transport = Bounded::new(inner, connection);
            assert!(!query.is_cancelled());
            drop(input);
            assert!(transport.receive().await.is_none());
            assert!(query.is_cancelled());
        });
    }
}
