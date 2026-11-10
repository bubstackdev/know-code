//! Limits around the SDK's existing JSON-RPC codec, not another protocol.
use rmcp::{
    model::{ErrorData, JsonRpcMessage},
    service::{RoleServer, RxJsonRpcMessage, TxJsonRpcMessage},
    transport::Transport,
};
use serde::Serialize;
use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
    time::Duration,
};
use tokio::{io::AsyncWrite, sync::Notify};

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const OUTPUT_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Connection {
    failed: AtomicBool,
    changed: Notify,
    cancellation: know_code_core::execution::QueryCancellation,
}
impl Connection {
    fn fail(&self) {
        self.cancellation.cancel();
        self.failed.store(true, Ordering::Release);
        self.changed.notify_one();
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
        let deadline = tokio::time::Instant::now() + self.deadline;
        // Preflight the actual envelope without allocating another copy. An
        // oversized response becomes an explicit small error before any bytes
        // of that response enter the SDK writer.
        let item = if fits(&item, MAX_FRAME_BYTES) {
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
        self.connection.check().ok()?;
        tokio::select! {
            biased;
            _=self.connection.changed.notified()=>None,
            value=self.inner.receive()=>{
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
