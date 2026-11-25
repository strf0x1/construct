//! The peer's loopback model endpoint for `llm = "hub"` (#371): every model
//! call this process makes goes to `http://127.0.0.1:<port>/v1`, and each
//! `/chat/completions` request is relayed to the hub on the `llm` channel.
//!
//! The design choice: the engine is not told about the hub. It sees one more
//! OpenAI-compatible endpoint, keyless but for a random bearer token that
//! lives only in this process (sandboxed tools get a credential-free
//! environment, so they cannot reach the hub's models through it). The
//! response comes back chunked, so a link lost mid-stream closes the
//! connection before the last chunk and the client reports an interrupted
//! stream instead of a short, complete-looking one. With no link, a call is
//! answered 503 at once, which the client retries with backoff while the
//! peer redials.

use crate::envelope::PINNED_MAX_ENVELOPE;
use crate::keys::random_token;
use crate::llm::{Frame, Msg};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// The largest request head the gateway reads.
const MAX_HEAD: usize = 16 * 1024;
/// The largest request body: what fits in one pinned envelope with its header.
const MAX_BODY: usize = PINNED_MAX_ENVELOPE - 1024;
/// The one path served.
const PATH: &str = "/v1/chat/completions";

/// A running gateway: where to point the engine, and the relay the link
/// loop feeds.
pub struct Gateway {
    /// `http://127.0.0.1:<port>/v1`.
    pub base_url: String,
    /// The bearer token every request must carry.
    pub token: String,
    pub relay: Relay,
}

/// Calls in flight and whether a link carries them. Cloned into each
/// connection; the link loop calls `linked`, `deliver` and `unlinked`.
#[derive(Clone)]
pub struct Relay {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    next: u64,
    linked: bool,
    calls: HashMap<u64, mpsc::UnboundedSender<Frame>>,
    out: mpsc::UnboundedSender<Frame>,
}

impl Relay {
    /// A relay whose frames for the hub go to `out`.
    #[must_use]
    pub fn new(out: mpsc::UnboundedSender<Frame>) -> Self {
        Self { inner: Arc::new(Mutex::new(Inner { next: 1, linked: false, calls: HashMap::new(), out })) }
    }

    #[expect(clippy::unwrap_used, reason = "poisoned lock: a connection task panicked mid-update")]
    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap()
    }

    /// A link to the hub is up (joined): calls go out from now on.
    pub fn linked(&self) {
        self.lock().linked = true;
    }

    /// The link is gone: every call in flight is cut, and new ones are
    /// refused until the next `linked`.
    pub fn unlinked(&self) {
        let mut s = self.lock();
        s.linked = false;
        s.calls.clear();
    }

    /// A frame from the hub, for the call it names.
    pub fn deliver(&self, frame: Frame) {
        let mut s = self.lock();
        let id = frame.msg.id();
        let done = matches!(frame.msg, Msg::End { .. });
        if let Some(tx) = s.calls.get(&id) {
            let _ = tx.send(frame);
        }
        if done {
            s.calls.remove(&id);
        }
    }

    /// Send a request to the hub; `None` when no link carries it.
    fn open(&self, body: Vec<u8>) -> Option<(u64, mpsc::UnboundedReceiver<Frame>)> {
        let mut s = self.lock();
        if !s.linked {
            return None;
        }
        let id = s.next;
        s.next += 1;
        let (tx, rx) = mpsc::unbounded_channel();
        s.calls.insert(id, tx);
        s.out.send(Frame::new(Msg::Request { id }, body)).ok()?;
        Some((id, rx))
    }

    /// The caller of `id` went away: tell the hub, if the call is still live.
    fn cancel(&self, id: u64) {
        let mut s = self.lock();
        if s.calls.remove(&id).is_some() && s.linked {
            let _ = s.out.send(Frame::bare(Msg::Cancel { id }));
        }
    }
}

/// Bind a gateway on loopback and serve it until the process ends.
pub async fn start(relay: Relay) -> std::io::Result<Gateway> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}/v1", listener.local_addr()?);
    let token = random_token(32).map_err(|e| std::io::Error::other(e.to_string()))?;
    let (t, r) = (Arc::<str>::from(token.as_str()), relay.clone());
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            tokio::spawn(serve(stream, t.clone(), r.clone()));
        }
    });
    Ok(Gateway { base_url, token, relay })
}

/// One request: check it, relay it, and write the hub's answer back.
async fn serve(mut stream: TcpStream, token: Arc<str>, relay: Relay) {
    let req = match read_request(&mut stream).await {
        Ok(r) => r,
        Err((status, why)) => return refuse(&mut stream, status, why).await,
    };
    if !same(req.auth.as_deref().unwrap_or_default(), &format!("Bearer {token}")) {
        return refuse(&mut stream, 401, "missing or wrong gateway token").await;
    }
    if req.method != "POST" || req.path != PATH {
        return refuse(&mut stream, 404, "only POST /v1/chat/completions goes to the hub").await;
    }
    let Some((id, mut rx)) = relay.open(req.body) else {
        return refuse(&mut stream, 503, "not connected to the hub; retrying while the peer redials").await;
    };
    let (mut rd, mut wr) = stream.into_split();
    let mut started = false;
    let mut probe = [0u8; 1];
    loop {
        let frame = tokio::select! {
            f = rx.recv() => f,
            // The caller hung up (a cancelled turn): stop the call at the hub.
            _ = rd.read(&mut probe) => return relay.cancel(id),
        };
        let wrote = match frame {
            // The link went away: before the head that is a 502 the client
            // retries; after it, a cut stream.
            None if !started => {
                let _ = wr.write_all(&plain(502, "lost the hub before it answered")).await;
                return;
            }
            None => return,
            Some(Frame { msg: Msg::Head { status, retry_after, .. }, .. }) if !started => {
                started = true;
                wr.write_all(head(status, retry_after).as_bytes()).await
            }
            Some(Frame { msg: Msg::Data { .. }, bytes }) if started && !bytes.is_empty() => {
                let mut chunk = format!("{:x}\r\n", bytes.len()).into_bytes();
                chunk.extend_from_slice(&bytes);
                chunk.extend_from_slice(b"\r\n");
                wr.write_all(&chunk).await
            }
            Some(Frame { msg: Msg::End { error: None, .. }, .. }) if started => {
                let _ = wr.write_all(b"0\r\n\r\n").await;
                return;
            }
            // An error ends the body without its last chunk: an interrupted stream.
            Some(Frame { msg: Msg::End { .. }, .. }) => return,
            Some(_) => Ok(()),
        };
        if wrote.is_err() {
            return relay.cancel(id);
        }
    }
}

struct Request {
    method: String,
    path: String,
    auth: Option<String>,
    body: Vec<u8>,
}

/// Read one HTTP/1.1 request with a `Content-Length` body.
async fn read_request(stream: &mut TcpStream) -> Result<Request, (u16, &'static str)> {
    let mut buf = Vec::with_capacity(4096);
    let end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEAD {
            return Err((431, "request head too large"));
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return Err((400, "connection closed mid-request")),
            Ok(n) => buf.extend_from_slice(chunk.get(..n).unwrap_or_default()),
        }
    };
    let head = String::from_utf8_lossy(buf.get(..end).unwrap_or_default()).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let (method, path) = (first.next().unwrap_or_default().to_string(), first.next().unwrap_or_default().to_string());
    let (mut len, mut auth) = (None, None);
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        match k.trim().to_ascii_lowercase().as_str() {
            "content-length" => len = v.trim().parse::<usize>().ok(),
            "authorization" => auth = Some(v.trim().to_string()),
            "transfer-encoding" => return Err((411, "send a Content-Length body")),
            _ => {}
        }
    }
    let len = len.unwrap_or(0);
    if len > MAX_BODY {
        return Err((413, "request body too large for the peer link"));
    }
    let mut body = buf.get(end + 4..).unwrap_or_default().to_vec();
    while body.len() < len {
        let mut chunk = vec![0u8; (len - body.len()).min(64 * 1024)];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return Err((400, "connection closed mid-body")),
            Ok(n) => body.extend_from_slice(chunk.get(..n).unwrap_or_default()),
        }
    }
    body.truncate(len);
    Ok(Request { method, path, auth, body })
}

/// Equal without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn head(status: u16, retry_after: Option<u64>) -> String {
    let retry = retry_after.map(|s| format!("Retry-After: {s}\r\n")).unwrap_or_default();
    format!("HTTP/1.1 {status} {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n{retry}\r\n", reason(status))
}

/// A complete response with an OpenAI-shaped error body.
fn plain(status: u16, message: &str) -> Vec<u8> {
    let body = serde_json::json!({ "error": { "message": message, "type": "peer_gateway" } }).to_string();
    format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        reason(status),
        body.len()
    )
    .into_bytes()
}

async fn refuse(stream: &mut TcpStream, status: u16, message: &str) {
    let _ = stream.write_all(&plain(status, message)).await;
    let _ = stream.shutdown().await;
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relay_without_a_link_refuses_and_unlinking_cuts_every_call() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let relay = Relay::new(tx);
        assert!(relay.open(b"{}".to_vec()).is_none(), "no link, no call");
        relay.linked();
        let (id, mut call) = relay.open(b"{}".to_vec()).unwrap();
        assert_eq!(rx.try_recv().unwrap().msg, Msg::Request { id });
        relay.deliver(Frame::bare(Msg::Head { id, status: 200, retry_after: None }));
        assert!(matches!(call.try_recv().unwrap().msg, Msg::Head { status: 200, .. }));
        relay.unlinked();
        assert!(call.try_recv().is_err(), "the call's channel is closed: its stream is cut");
        relay.cancel(id);
        assert!(rx.try_recv().is_err(), "a cut call sends no cancel over a link that is gone");
    }

    #[test]
    fn a_cancel_reaches_the_hub_once_and_an_end_retires_the_call() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let relay = Relay::new(tx);
        relay.linked();
        let (a, _ca) = relay.open(b"{}".to_vec()).unwrap();
        let (b, _cb) = relay.open(b"{}".to_vec()).unwrap();
        let _ = (rx.try_recv(), rx.try_recv());
        relay.cancel(a);
        relay.cancel(a);
        assert_eq!(rx.try_recv().unwrap().msg, Msg::Cancel { id: a });
        assert!(rx.try_recv().is_err());
        relay.deliver(Frame::bare(Msg::End { id: b, error: None }));
        relay.cancel(b);
        assert!(rx.try_recv().is_err(), "a finished call is not cancelled");
    }
}
