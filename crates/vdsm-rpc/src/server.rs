//! TCP + TLS accept loop with pluggable framing (line-delimited JSON-RPC
//! or STOMP 1.2).
//!
//! One tokio task per connection. Per-connection state lives entirely in
//! the [`serve_lines`] / [`serve_stomp`] functions — no shared mutable
//! state across connections beyond the [`Dispatcher`] (which is itself
//! immutable post-construction).
//!
//! Framing is selected at server-start time via [`ServerConfig::framing`]
//! and applies to every connection. We intentionally don't auto-detect
//! per-connection: ovirt-engine and `openssl s_client` are mutually
//! exclusive consumer scenarios, so the deployment knob is fine.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, warn};

use crate::dispatch::Dispatcher;
use crate::protocol::{JsonRpcError, Request, Response};
use crate::stomp;
use crate::tls;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// Newline-terminated JSON-RPC requests. Easiest to drive from
    /// `openssl s_client` for smoke testing.
    Line,
    /// STOMP 1.2. Engine wraps every JSON-RPC request in a SEND frame
    /// and expects the response back as a MESSAGE on a destination it
    /// previously SUBSCRIBE'd to.
    Stomp,
}

impl Framing {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "stomp" => Framing::Stomp,
            "line" => Framing::Line,
            other => {
                warn!(value = other, "unknown rpc.framing value; defaulting to line");
                Framing::Line
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub bind: String,
    pub port: u16,
    pub tls_enabled: bool,
    pub tls_cert: PathBuf,
    pub tls_key: PathBuf,
    pub framing: Framing,
}

pub struct Server {
    cfg: ServerConfig,
    dispatcher: Dispatcher,
}

impl Server {
    pub fn new(cfg: ServerConfig, dispatcher: Dispatcher) -> Self {
        Self { cfg, dispatcher }
    }

    pub async fn serve(self) -> anyhow::Result<()> {
        let addr = format!("{}:{}", self.cfg.bind, self.cfg.port);
        let listener = TcpListener::bind(&addr).await?;
        let local = listener.local_addr()?;

        let acceptor = if self.cfg.tls_enabled {
            let server_cfg =
                tls::load_server_config(&self.cfg.tls_cert, &self.cfg.tls_key)?;
            Some(TlsAcceptor::from(server_cfg))
        } else {
            warn!("TLS disabled — listener is plain JSON-RPC over TCP");
            None
        };

        info!(
            address = %local,
            tls = self.cfg.tls_enabled,
            framing = ?self.cfg.framing,
            handlers = self.dispatcher.handler_count(),
            "vdsm-rpc listening"
        );

        let dispatcher = Arc::new(self.dispatcher);
        let framing = self.cfg.framing;
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(p) => p,
                Err(e) => {
                    error!(error = %e, "accept failed");
                    continue;
                }
            };
            let dispatcher = Arc::clone(&dispatcher);
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_conn(stream, peer, dispatcher, acceptor, framing).await {
                    debug!(peer = %peer, error = %e, "connection closed");
                }
            });
        }
    }
}

async fn handle_conn(
    stream: TcpStream,
    peer: SocketAddr,
    dispatcher: Arc<Dispatcher>,
    acceptor: Option<TlsAcceptor>,
    framing: Framing,
) -> anyhow::Result<()> {
    debug!(peer = %peer, ?framing, "new connection");
    if let Err(e) = stream.set_nodelay(true) {
        debug!(peer = %peer, error = %e, "set_nodelay failed (ignored)");
    }

    if let Some(acc) = acceptor {
        let tls_stream = acc.accept(stream).await?;
        match framing {
            Framing::Line => serve_lines(tls_stream, peer, dispatcher).await,
            Framing::Stomp => serve_stomp(tls_stream, peer, dispatcher).await,
        }
    } else {
        match framing {
            Framing::Line => serve_lines(stream, peer, dispatcher).await,
            Framing::Stomp => serve_stomp(stream, peer, dispatcher).await,
        }
    }
}

// ---------------------------------------------------------------------
// Line framing (newline-delimited JSON-RPC).
// ---------------------------------------------------------------------

async fn serve_lines<S>(
    stream: S,
    peer: SocketAddr,
    dispatcher: Arc<Dispatcher>,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            return Ok(());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let resp = process_one(trimmed, &dispatcher).await;
        if let Some(resp) = resp {
            let mut buf = serde_json::to_vec(&resp)?;
            buf.push(b'\n');
            write_half.write_all(&buf).await?;
            write_half.flush().await?;
        }
        debug!(peer = %peer, len = n, "request handled (line framing)");
    }
}

async fn process_one(line: &str, dispatcher: &Dispatcher) -> Option<Response> {
    let req: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            return Some(Response::err(
                None,
                JsonRpcError::parse_error(format!("invalid JSON-RPC envelope: {e}")),
            ));
        }
    };
    let id = req.id.clone();
    let result = dispatcher.invoke(&req.method, req.params).await;
    if id.is_none() {
        return None;
    }
    Some(match result {
        Ok(value) => Response::ok(id, value),
        Err(err) => Response::err(id, err),
    })
}

// ---------------------------------------------------------------------
// STOMP framing (engine-compatible).
// ---------------------------------------------------------------------

/// Per-connection STOMP state machine.
///
/// Wire flow we expect from ovirt-engine:
///
///   1. Engine -> us:  CONNECT (or STOMP) with accept-version: 1.2.
///   2. Us -> engine:  CONNECTED with version, server, session.
///   3. Engine -> us:  SUBSCRIBE id=0 destination=/queue/_local/vdsm/responses
///                     (engine reads our replies from this destination)
///   4. Engine -> us:  SEND destination=/queue/_local/vdsm/requests
///                     content-type=application/json  body=<JSON-RPC request>
///   5. Us -> engine:  MESSAGE destination=<reply-to or first sub destination>
///                     subscription=<sub id>  body=<JSON-RPC response>
///   6. Repeat (4)/(5) for the lifetime of the connection.
///   7. Engine -> us:  DISCONNECT (optionally with receipt:N).
///   8. Us -> engine:  RECEIPT receipt-id:N if requested, then close.
async fn serve_stomp<S>(
    stream: S,
    peer: SocketAddr,
    dispatcher: Arc<Dispatcher>,
) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut read_half, mut write_half) = tokio::io::split(stream);
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = vec![0u8; 8192];
    let mut subscriptions: HashMap<String, String> = HashMap::new();
    let mut connected = false;

    loop {
        let n = read_half.read(&mut chunk).await?;
        if n == 0 {
            debug!(peer = %peer, "stomp peer closed");
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);

        // Parse as many complete frames as the buffer holds.
        loop {
            let (frame, consumed) = match stomp::parse_frame(&buf) {
                Ok(Some((f, rest))) => (f, buf.len() - rest.len()),
                Ok(None) => break,
                Err(e) => {
                    warn!(peer = %peer, error = %e, "stomp parse error; closing");
                    let err = stomp::build::error("malformed frame", &e.to_string());
                    let _ = write_half.write_all(&err.to_bytes()).await;
                    return Ok(());
                }
            };
            buf.drain(..consumed);

            // Reject anything other than CONNECT/STOMP before handshake.
            if !connected
                && frame.command != "CONNECT"
                && frame.command != "STOMP"
            {
                warn!(
                    peer = %peer,
                    cmd = %frame.command,
                    "frame received before CONNECT; closing"
                );
                let err = stomp::build::error("expected CONNECT", "");
                let _ = write_half.write_all(&err.to_bytes()).await;
                return Ok(());
            }

            match frame.command.as_str() {
                "CONNECT" | "STOMP" => {
                    if connected {
                        let err = stomp::build::error("already connected", "");
                        let _ = write_half.write_all(&err.to_bytes()).await;
                        return Ok(());
                    }
                    let session = format!("vdsm-rs-{}", peer);
                    let server_id = format!("vdsm-rs/{}", env!("CARGO_PKG_VERSION"));
                    let connected_frame =
                        stomp::build::connected("1.2", &server_id, Some(&session));
                    write_half.write_all(&connected_frame.to_bytes()).await?;
                    write_half.flush().await?;
                    connected = true;
                    debug!(peer = %peer, "stomp handshake complete");
                }

                "SUBSCRIBE" => {
                    let id = frame.header("id").unwrap_or("0").to_string();
                    let dest = frame
                        .header("destination")
                        .unwrap_or("/queue/_local/vdsm/responses")
                        .to_string();
                    debug!(peer = %peer, sub_id = %id, destination = %dest, "subscribe");
                    subscriptions.insert(id, dest);
                }

                "UNSUBSCRIBE" => {
                    if let Some(id) = frame.header("id") {
                        subscriptions.remove(id);
                    }
                }

                "SEND" => {
                    handle_stomp_send(&frame, &subscriptions, &dispatcher, &mut write_half)
                        .await?;
                }

                "DISCONNECT" => {
                    if let Some(receipt_id) = frame.header("receipt") {
                        let r = stomp::build::receipt(receipt_id);
                        let _ = write_half.write_all(&r.to_bytes()).await;
                        let _ = write_half.flush().await;
                    }
                    debug!(peer = %peer, "stomp disconnect");
                    return Ok(());
                }

                // ACK / NACK / BEGIN / COMMIT / ABORT — engine doesn't
                // use these for vdsm-style request/response; we silently
                // ignore them rather than ERRORing in case some future
                // engine version probes for support.
                other => {
                    debug!(peer = %peer, cmd = other, "ignoring unhandled STOMP frame");
                }
            }
        }
    }
}

async fn handle_stomp_send<W>(
    frame: &stomp::Frame,
    subscriptions: &HashMap<String, String>,
    dispatcher: &Arc<Dispatcher>,
    write_half: &mut W,
) -> anyhow::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let body_str = match std::str::from_utf8(&frame.body) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "SEND body is not valid UTF-8");
            return Ok(());
        }
    };

    // Parse JSON-RPC envelope. On parse failure we still want to reply
    // (engine will hang waiting for a response otherwise) — emit a
    // -32700 with no id.
    let parsed: Result<Request, _> = serde_json::from_str(body_str);
    let (response, request_id) = match parsed {
        Ok(req) => {
            let id = req.id.clone();
            let result = dispatcher.invoke(&req.method, req.params).await;
            let resp = match result {
                Ok(value) => Response::ok(id.clone(), value),
                Err(err) => Response::err(id.clone(), err),
            };
            (resp, id)
        }
        Err(e) => (
            Response::err(
                None,
                JsonRpcError::parse_error(format!("invalid JSON-RPC envelope: {e}")),
            ),
            None,
        ),
    };

    // JSON-RPC notifications (no id) get no response per spec — even
    // over STOMP, engine doesn't expect a MESSAGE back.
    if request_id.is_none() && response.error.is_none() {
        return Ok(());
    }

    let resp_body = serde_json::to_vec(&response)?;

    // Route the reply:
    //   1. SEND header `reply-to` wins (point-to-point reply queue).
    //   2. Else, fall back to the first SUBSCRIBE'd destination.
    //   3. Else, send to a hardcoded vdsm responses queue (engine
    //      that didn't subscribe at all isn't a real client, but we
    //      still emit the frame so debugging traces show something).
    let reply_dest = frame
        .header("reply-to")
        .map(String::from)
        .or_else(|| subscriptions.values().next().cloned())
        .unwrap_or_else(|| "/queue/_local/vdsm/responses".to_string());

    let sub_id = subscriptions
        .iter()
        .find(|(_, d)| d.as_str() == reply_dest.as_str())
        .map(|(id, _)| id.clone())
        .unwrap_or_else(|| "0".to_string());

    let msg = stomp::build::message(&reply_dest, &sub_id, resp_body);
    let frame_bytes = msg.to_bytes();
    // Wire trace: header section (before \n\n separator) plus first/last 200
    // bytes of body. Confirms Stomp framing and content-length match.
    let sep = frame_bytes.windows(2).position(|w| w == b"\n\n").unwrap_or(0);
    let body_start = sep.saturating_add(2);
    let body_end = frame_bytes.len().saturating_sub(1); // strip trailing NUL
    let body_len = body_end.saturating_sub(body_start);
    let head = String::from_utf8_lossy(&frame_bytes[..sep.min(frame_bytes.len())]).into_owned();
    let body_first = String::from_utf8_lossy(
        &frame_bytes[body_start..body_start.saturating_add(200).min(body_end)],
    )
    .into_owned();
    let body_last_off = body_end.saturating_sub(200).max(body_start);
    let body_last = String::from_utf8_lossy(&frame_bytes[body_last_off..body_end]).into_owned();
    tracing::info!(
        body_len,
        total_bytes = frame_bytes.len(),
        head = %head,
        body_first = %body_first,
        body_last = %body_last,
        "stomp MESSAGE about to write"
    );
    write_half.write_all(&frame_bytes).await?;
    write_half.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// End-to-end smoke test of the STOMP handler over an in-process
    /// duplex pipe. Drives the same wire flow ovirt-engine would — full
    /// CONNECT / SUBSCRIBE / SEND / DISCONNECT — and asserts the
    /// MESSAGE response carries our JSON-RPC reply.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stomp_roundtrip_ping2() {
        let dispatcher = Dispatcher::builder()
            .register("Host.ping2", |_p| async move {
                Ok(json!({"status": {"code": 0, "message": "Done"}}))
            })
            .build();
        let dispatcher = Arc::new(dispatcher);

        let (client, server) = tokio::io::duplex(8192);
        let server_task = tokio::spawn(serve_stomp(
            server,
            "127.0.0.1:0".parse().unwrap(),
            dispatcher,
        ));

        let (mut cr, mut cw) = tokio::io::split(client);

        // 1. CONNECT
        cw.write_all(
            &stomp::Frame::new("CONNECT")
                .with_header("accept-version", "1.2")
                .with_header("host", "test")
                .to_bytes(),
        )
        .await
        .unwrap();

        let frame = read_one_frame(&mut cr).await;
        assert_eq!(frame.command, "CONNECTED");
        assert_eq!(frame.header("version"), Some("1.2"));
        assert!(frame.header("server").unwrap().starts_with("vdsm-rs/"));

        // 2. SUBSCRIBE
        cw.write_all(
            &stomp::Frame::new("SUBSCRIBE")
                .with_header("id", "sub-1")
                .with_header("destination", "/queue/_local/vdsm/responses")
                .to_bytes(),
        )
        .await
        .unwrap();

        // 3. SEND with a Host.ping2 request.
        let body = br#"{"jsonrpc":"2.0","id":42,"method":"Host.ping2","params":{}}"#;
        cw.write_all(
            &stomp::Frame::new("SEND")
                .with_header("destination", "/queue/_local/vdsm/requests")
                .with_header("content-type", "application/json")
                .with_header("content-length", body.len().to_string())
                .with_body(body.to_vec())
                .to_bytes(),
        )
        .await
        .unwrap();

        // 4. Read MESSAGE reply.
        let msg = read_one_frame(&mut cr).await;
        assert_eq!(msg.command, "MESSAGE");
        assert_eq!(msg.header("subscription"), Some("sub-1"));
        assert_eq!(
            msg.header("destination"),
            Some("/queue/_local/vdsm/responses")
        );
        let body_str = std::str::from_utf8(&msg.body).unwrap();
        assert!(
            body_str.contains("\"id\":42"),
            "response missing id: {body_str}"
        );
        assert!(
            body_str.contains("\"Done\""),
            "response missing Done: {body_str}"
        );

        // 5. DISCONNECT with receipt.
        cw.write_all(
            &stomp::Frame::new("DISCONNECT")
                .with_header("receipt", "bye")
                .to_bytes(),
        )
        .await
        .unwrap();

        let receipt = read_one_frame(&mut cr).await;
        assert_eq!(receipt.command, "RECEIPT");
        assert_eq!(receipt.header("receipt-id"), Some("bye"));

        // Server should now have closed; reader returns EOF.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            server_task,
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stomp_rejects_pre_connect_send() {
        let dispatcher = Arc::new(Dispatcher::builder().build());
        let (client, server) = tokio::io::duplex(8192);
        let server_task = tokio::spawn(serve_stomp(
            server,
            "127.0.0.1:0".parse().unwrap(),
            dispatcher,
        ));

        let (mut cr, mut cw) = tokio::io::split(client);

        // Skip CONNECT entirely; engine bug or hostile peer.
        cw.write_all(
            &stomp::Frame::new("SEND")
                .with_header("destination", "/queue/x")
                .with_body(b"{}".to_vec())
                .to_bytes(),
        )
        .await
        .unwrap();

        let err = read_one_frame(&mut cr).await;
        assert_eq!(err.command, "ERROR");
        assert_eq!(err.header("message"), Some("expected CONNECT"));

        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            server_task,
        )
        .await;
    }

    /// Read bytes from `cr` until [`stomp::parse_frame`] returns one
    /// complete frame. Times out after 2 seconds so a hung server fails
    /// the test rather than the whole suite.
    async fn read_one_frame<R>(cr: &mut R) -> stomp::Frame
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let mut buf = Vec::with_capacity(4096);
        let mut chunk = vec![0u8; 4096];
        let timeout = tokio::time::Duration::from_secs(2);
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(Some((frame, _))) = stomp::parse_frame(&buf) {
                return frame;
            }
            let n = tokio::time::timeout_at(deadline, cr.read(&mut chunk))
                .await
                .expect("timed out waiting for frame")
                .expect("read error");
            if n == 0 {
                panic!("connection closed before frame complete");
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }
}
