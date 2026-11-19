//! The Lenslet proxy: accept SDK calls over plain HTTP on loopback, preserve
//! Host/SigV4, forward to AWS Emulator (HTTPS), and record one trace line per
//! call.
//!
//! Forwarding is streamed, never buffered: request and response bodies pass
//! through unchanged, so SigV4 signatures stay valid and large uploads or
//! downloads are not limited or slowed. Capture is bounded to
//! [`capture::MAX_CAPTURE_BYTES`] per body: text bodies keep a prefix (marked
//! `truncated` when longer), binary bodies keep only size and SHA-256.
//!
//! The trace is a local development artifact and keeps the raw wire values:
//! headers and text bodies are recorded exactly as they arrived, and the trace
//! file is owner-only (0600) on Unix.

use std::{
    collections::BTreeSet,
    convert::Infallible,
    fs,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use chrono::Utc;
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use hyper::{Request, Response, StatusCode, header, server::conn::http1, service::service_fn};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use rustls::RootCertStore;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use crate::{
    access::{self, Access},
    capture::{self, Capture, Content, TeeBody},
    lock::Gate,
    problem,
    trace::{self, Message, RunHeader, Trace, TraceSink},
    wire,
};

type Body = BoxBody<Bytes, std::io::Error>;
type HttpsClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Body>;

/// Upper bound on request body inspection during `--enforce` (64 MiB — 4×
/// AWS DynamoDB's 16 MiB batch limit). Anything larger fails closed.
const MAX_ENFORCE_INSPECT_BYTES: usize = 64 * 1024 * 1024;

/// A call still running after this long gets a `pending` trace line.
const PENDING_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct State {
    client: HttpsClient,
    /// Upstream base URL without a trailing slash, e.g. `https://127.0.0.1:9298`.
    upstream: String,
    pub max_body: usize,
    pub enforce: bool,
    pub trace: Arc<TraceSink>,
}

/// Everything needed to write one trace entry once the response body is done.
struct Pending {
    state: Arc<State>,
    trace: Trace,
    started: Instant,
    finished: Arc<AtomicBool>,
    /// Operation advertised by the wire protocol; when `None`, the request body
    /// may still supply one (AWS Query `Action=...`).
    advertised_operation: Option<String>,
    request_capture: Arc<Mutex<Capture>>,
    request_content: Content,
    /// Full-body snapshot or capture for gate evaluation when `--max-body` is
    /// smaller than [`MAX_ENFORCE_INSPECT_BYTES`].
    enforce_full_body: Option<trace::BodyTrace>,
    gate_capture: Option<Arc<Mutex<Capture>>>,
    response_capture: Arc<Mutex<Capture>>,
    response_content: Content,
}

impl Pending {
    fn finish(mut self) {
        self.finished.store(true, Ordering::SeqCst);
        self.trace.ms = self.started.elapsed().as_millis() as u64;
        self.trace.request.body = snapshot(&self.request_capture, &self.request_content);
        self.trace.response.body = snapshot(&self.response_capture, &self.response_content);
        let op_capture = self
            .gate_capture
            .as_deref()
            .unwrap_or(&self.request_capture);
        resolve_operation(
            &mut self.trace,
            self.advertised_operation.as_deref(),
            op_capture,
            &self.request_content,
        );
        self.trace.problem = problem::from_response(
            self.trace.status,
            &self.trace.response.headers,
            self.trace
                .response
                .body
                .as_ref()
                .and_then(|body| body.text.as_deref()),
        );
        let eval_body = self.enforce_full_body.or_else(|| {
            self.gate_capture
                .as_deref()
                .and_then(|cap| snapshot(cap, &self.request_content))
        });
        if let Some(full) = eval_body {
            let mut eval_trace = self.trace.clone();
            eval_trace.request.body = Some(full);
            self.state
                .trace
                .record_evaluated(self.trace, Some(&eval_trace));
        } else {
            self.state.trace.record(self.trace);
        }
    }
}

/// Fill in the operation from the request body when the wire protocol only
/// advertises it there (AWS Query services send `Action=...` as a form field).
fn resolve_operation(
    trace: &mut Trace,
    advertised: Option<&str>,
    capture: &Mutex<Capture>,
    content: &Content,
) {
    if advertised.is_some() {
        return;
    }
    if let Some(action) = query_action(capture, content) {
        trace.operation = action;
    }
}

/// `Action=` from a captured form body, if the request is a query-protocol call.
fn query_action(capture: &Mutex<Capture>, content: &Content) -> Option<String> {
    let capture = capture.lock().ok()?;
    let prefix = capture.decoded_prefix(content);
    let form = content.content_type.as_deref() == Some("application/x-www-form-urlencoded")
        || prefix.trim_ascii_start().starts_with(b"Action=");
    if !form {
        return None;
    }
    wire::form_action(&prefix)
}

fn snapshot(capture: &Mutex<Capture>, content: &Content) -> Option<trace::BodyTrace> {
    capture
        .lock()
        .map(|capture| capture.body(content))
        .ok()
        .flatten()
}

/// Unique per proxied call: nanosecond timestamp plus a process-local sequence.
fn next_id() -> String {
    use std::sync::atomic::AtomicU64;
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}-{sequence:x}")
}

/// Proxy settings for `lenslet run`.
pub struct Options {
    /// Loopback port for SDK clients; `0` picks a free one.
    pub listen: u16,
    pub upstream: Upstream,
    /// Whether `--upstream` was explicitly passed on the command line.
    pub upstream_given: bool,
    /// CA that signs the emulator's certificate; derived from the upstream
    /// port when not given (`~/.aws/emulator/PORT/ca.pem`).
    pub emulator_ca: Option<PathBuf>,
    /// Fixed trace file; by default each session gets its own run file.
    pub trace: Option<PathBuf>,
    /// Maximum body bytes captured per message (default 1 MiB).
    pub max_body: usize,
    /// Actively block calls whose IAM access is denied by the lock or policy.
    pub enforce: bool,
    pub home: PathBuf,
}

impl Options {
    pub fn new(home: &Path, listen: u16) -> Self {
        Options {
            listen,
            upstream: Upstream::parse("9298").expect("default upstream"),
            upstream_given: false,
            emulator_ca: None,
            trace: None,
            max_body: capture::MAX_CAPTURE_BYTES,
            enforce: false,
            home: home.to_path_buf(),
        }
    }

    /// Consume one proxy option. Returns `Ok(false)` when `arg` is not one.
    pub fn accept(&mut self, arg: &str, value: &mut dyn FnMut() -> Result<String>) -> Result<bool> {
        match arg {
            "--listen" => {
                let raw = value()?;
                self.listen = raw
                    .parse::<u16>()
                    .with_context(|| format!("--listen expects a port number, got `{raw}`"))?;
            }
            "--upstream" => {
                self.upstream = Upstream::parse(&value()?)?;
                self.upstream_given = true;
            }
            "--emulator-ca" => self.emulator_ca = Some(PathBuf::from(value()?)),
            "--trace" => self.trace = Some(PathBuf::from(value()?)),
            "--max-body" => self.max_body = capture::parse_size(&value()?)?,
            "--enforce" => self.enforce = true,
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub fn validate(&self) -> Result<()> {
        if self.upstream.is_loopback() && self.upstream.port == self.listen {
            bail!(
                "--listen {} and --upstream {} are the same port; the proxy would forward to itself",
                self.listen,
                self.upstream.port
            );
        }
        Ok(())
    }

    /// The emulator's own directory: credentials, config and CA.
    pub fn emulator_dir(&self) -> PathBuf {
        self.home
            .join(".aws/emulator")
            .join(self.upstream.port.to_string())
    }

    pub fn emulator_ca(&self) -> PathBuf {
        self.emulator_ca
            .clone()
            .unwrap_or_else(|| self.emulator_dir().join("ca.pem"))
    }
}

/// Where the proxy sends requests: `PORT` means `https://127.0.0.1:PORT`.
pub struct Upstream {
    /// Base URL without a trailing slash, e.g. `https://127.0.0.1:9298`.
    pub base: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

impl Upstream {
    pub fn parse(value: &str) -> Result<Self> {
        let text = if !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()) {
            format!("https://127.0.0.1:{value}")
        } else {
            value.to_string()
        };
        let uri: hyper::Uri = text.parse().with_context(|| {
            format!("--upstream expects a port or an http(s) URL, got `{value}`")
        })?;
        let scheme = uri.scheme_str().unwrap_or_default();
        if scheme != "http" && scheme != "https" {
            bail!("--upstream `{value}` must use http:// or https://");
        }
        let authority = uri
            .authority()
            .with_context(|| format!("--upstream `{value}` needs a host and port"))?;
        if !uri.path().is_empty() && uri.path() != "/" {
            bail!("--upstream `{value}` must not include a path");
        }
        if uri.query().is_some() {
            bail!("--upstream `{value}` must not include a query");
        }
        let port = authority
            .port_u16()
            .with_context(|| format!("--upstream `{value}` needs an explicit port"))?;
        if port == 0 {
            bail!("--upstream `{value}` needs a port between 1 and 65535");
        }
        Ok(Upstream {
            base: format!("{scheme}://{authority}"),
            host: authority.host().trim_matches(['[', ']']).to_string(),
            port,
            tls: scheme == "https",
        })
    }

    pub fn is_loopback(&self) -> bool {
        matches!(self.host.as_str(), "127.0.0.1" | "localhost" | "::1")
    }

    /// Auto-resolve a local emulator when `--upstream` was omitted:
    /// 1. Inherit a loopback `AWS_ENDPOINT_URL` from the parent environment so
    ///    Lenslet transparently interposes itself in front of `.env` / Compose setups.
    /// 2. If default `:9298` is not listening, probe LocalStack (`:4566`) and Moto
    ///    (`:5000`) with protocol fingerprint verification (avoiding false positives
    ///    on unrelated local HTTP servers such as AirPlay or Flask).
    pub async fn auto_discover(
        env_endpoint: Option<&str>,
        listen_port: u16,
    ) -> Option<(Upstream, &'static str)> {
        if let Some(raw) = env_endpoint.map(str::trim).filter(|s| !s.is_empty())
            && let Ok(parsed) = Upstream::parse(raw)
            && parsed.is_loopback()
            && (listen_port == 0 || parsed.port != listen_port)
        {
            return Some((parsed, "AWS_ENDPOINT_URL"));
        }

        let default_emu = Upstream::parse("9298").ok()?;
        if default_emu.reachable().await {
            return None;
        }

        for (url, path, label) in [
            ("http://127.0.0.1:4566", "/_localstack/health", "LocalStack"),
            ("http://127.0.0.1:5000", "/moto-api/", "Moto"),
        ] {
            let Ok(candidate) = Upstream::parse(url) else {
                continue;
            };
            if listen_port != 0 && candidate.port == listen_port {
                continue;
            }
            if candidate.verify_emulator_signature(path).await {
                return Some((candidate, label));
            }
        }
        None
    }

    async fn verify_emulator_signature(&self, probe_path: &str) -> bool {
        let probe = async {
            let mut stream = TcpStream::connect((self.host.as_str(), self.port))
                .await
                .ok()?;
            let req = format!(
                "GET {probe_path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
                self.port
            );
            stream.write_all(req.as_bytes()).await.ok()?;
            let mut buf = vec![0u8; 2048];
            let n = stream.read(&mut buf).await.ok()?;
            Some(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase())
        };
        let Some(reply) = tokio::time::timeout(Duration::from_millis(120), probe)
            .await
            .ok()
            .flatten()
        else {
            return false;
        };
        if reply.contains("airtunes") || reply.contains("airplay") {
            return false;
        }
        reply.contains("localstack")
            || reply.contains("\"services\"")
            || reply.contains("moto")
            || reply.contains("x-amzn-requestid")
            || reply.contains("<errorresponse")
    }

    /// Whether something accepts TCP connections at the upstream address
    /// (trying normal resolution first, so IPv6-only `localhost` works too).
    pub async fn reachable(&self) -> bool {
        let primary = tokio::time::timeout(
            Duration::from_millis(400),
            TcpStream::connect((self.host.as_str(), self.port)),
        )
        .await
        .is_ok_and(|connected| connected.is_ok());
        if primary || self.host != "localhost" {
            return primary;
        }
        tokio::time::timeout(
            Duration::from_millis(250),
            TcpStream::connect(("127.0.0.1", self.port)),
        )
        .await
        .is_ok_and(|connected| connected.is_ok())
    }

    /// Best-effort fingerprint of the running emulator for `lenslet init`.
    pub async fn detect_emulator(&self) -> Option<String> {
        if self.tls {
            return Some(format!("aws-emulator --telemetry off --port {}", self.port));
        }
        let probe = async {
            let mut stream = TcpStream::connect((self.host.as_str(), self.port))
                .await
                .ok()?;
            stream
                .write_all(
                    b"GET /_localstack/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
                )
                .await
                .ok()?;
            let mut buf = vec![0u8; 2048];
            let n = stream.read(&mut buf).await.ok()?;
            Some(String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase())
        };
        let reply = tokio::time::timeout(Duration::from_millis(300), probe)
            .await
            .ok()
            .flatten()?;
        if reply.contains("server: werkzeug") || reply.contains("moto") {
            Some(format!("moto_server -p {}", self.port))
        } else if reply.contains("localstack") || reply.contains("\"services\"") {
            Some("localstack start -d".to_string())
        } else {
            None
        }
    }
}

/// Set a filesystem mode, ignoring platforms without POSIX permissions.

fn emulator_roots(path: &Path) -> Result<RootCertStore> {
    let pem = fs::read(path).with_context(|| {
        format!(
            "read emulator CA {} (start AWS Emulator first, or pass --emulator-ca)",
            path.display()
        )
    })?;
    let mut roots = RootCertStore::empty();
    let mut count = 0;
    for cert in rustls_pemfile::certs(&mut &pem[..]) {
        roots
            .add(cert.with_context(|| format!("parse emulator CA {}", path.display()))?)
            .with_context(|| format!("trust emulator CA {}", path.display()))?;
        count += 1;
    }
    if count == 0 {
        bail!("no certificates in emulator CA {}", path.display());
    }
    Ok(roots)
}

fn client(roots: RootCertStore) -> HttpsClient {
    let tls = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let https = HttpsConnectorBuilder::new()
        .with_tls_config(tls)
        .https_or_http()
        .enable_http1()
        .build();
    Client::builder(TokioExecutor::new()).build(https)
}

enum CollectedBody {
    Complete(Bytes),
    ExceedsLimit(Bytes),
}

/// Buffer up to `limit` bytes from `body`, returning `ExceedsLimit` as soon as
/// incoming data crosses `limit` so a large or streaming upload never exhausts
/// proxy memory under `--enforce`.
async fn collect_bounded<B>(mut body: B, limit: usize) -> Result<CollectedBody, String>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let cap_hint = body.size_hint().lower().min(limit as u64).min(64 * 1024) as usize;
    let mut buf = Vec::with_capacity(cap_hint);
    while let Some(frame_res) = body.frame().await {
        let frame = frame_res.map_err(|err| err.into().to_string())?;
        if let Ok(chunk) = frame.into_data() {
            if buf.len().saturating_add(chunk.len()) > limit {
                let take = limit.saturating_sub(buf.len());
                buf.extend_from_slice(&chunk[..take]);
                return Ok(CollectedBody::ExceedsLimit(Bytes::from(buf)));
            }
            buf.extend_from_slice(&chunk);
        }
    }
    Ok(CollectedBody::Complete(Bytes::from(buf)))
}

async fn handle<B>(req: Request<B>, state: Arc<State>) -> Result<Response<Body>, Infallible>
where
    B: hyper::body::Body<Data = Bytes> + Send + Sync + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    handle_with_inspect_limit(req, state, MAX_ENFORCE_INSPECT_BYTES).await
}

async fn handle_with_inspect_limit<B>(
    req: Request<B>,
    state: Arc<State>,
    inspect_limit: usize,
) -> Result<Response<Body>, Infallible>
where
    B: hyper::body::Body<Data = Bytes> + Send + Sync + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let started = Instant::now();
    let method = req.method().clone();
    let raw_path = req
        .uri()
        .path_and_query()
        .map(|path| path.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());
    let host = req
        .headers()
        .get("host")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .or_else(|| req.uri().authority().map(|authority| authority.as_str()))
        .unwrap_or("")
        .to_string();
    let authorization = req.headers().contains_key("authorization");
    let wire::Wire {
        service,
        region,
        operation: advertised_operation,
        invocation_id,
        attempt,
        max_attempts,
    } = wire::wire_info(req.headers(), &raw_path);
    let retry_attempt = attempt.filter(|n| *n > 1);
    let retry_max = max_attempts.filter(|_| retry_attempt.is_some());
    // A protocol-derived operation, or the method/path fallback until (and
    // unless) the request body supplies an AWS Query `Action=...`.
    let operation = advertised_operation.clone().unwrap_or_else(|| {
        wire::fallback_operation(&service, method.as_str(), &raw_path, req.headers())
    });

    let request_content = capture::content(req.headers());
    let request_headers = trace::headers(req.headers());
    let request_capture = Arc::new(Mutex::new(Capture::with_limit(state.max_body)));

    let mut trace = Trace {
        id: next_id(),
        at: Utc::now().to_rfc3339(),
        method: method.to_string(),
        path: raw_path.clone(),
        host,
        service: service.clone(),
        operation,
        status: 0,
        ms: 0,
        authorization,
        region,
        invocation_id,
        attempt: retry_attempt,
        max_attempts: retry_max,
        pending: false,
        error: None,
        problem: None,
        access: Vec::new(),
        implied: Vec::new(),
        new: false,
        blocked: false,
        replay_of: None,
        request: Message {
            headers: request_headers,
            content_type: request_content.content_type.clone(),
            body: None,
        },
        response: Message::default(),
    };

    let (parts, body) = req.into_parts();
    let forward_path = if raw_path.starts_with('/') {
        raw_path.as_str()
    } else if raw_path == "*" {
        "/"
    } else {
        // Guard against authority-form or bare paths lacking a leading `/`.
        return Ok(error_response(
            StatusCode::BAD_REQUEST,
            "invalid request path",
        ));
    };

    // Active IAM enforcement (`--enforce`): deny unauthorized calls locally
    // before a single unauthorized write reaches the emulator. Request bodies
    // are inspected against `MAX_ENFORCE_INSPECT_BYTES` independently of
    // `--max-body`, so a small trace capture limit never hides trailing items.
    let mut enforce_full_body: Option<trace::BodyTrace> = None;
    let mut gate_capture: Option<Arc<Mutex<Capture>>> = None;
    let upstream_body: Body = if state.enforce
        && let Some(gate) = state.trace.gate()
    {
        if access::needs_request_body(
            &service,
            advertised_operation.as_deref(),
            &trace.operation,
            &raw_path,
        ) {
            let (collected, over_limit) = match collect_bounded(body, inspect_limit).await {
                Ok(CollectedBody::Complete(bytes)) => (bytes, false),
                Ok(CollectedBody::ExceedsLimit(prefix)) => (prefix, true),
                Err(detail) => {
                    return Ok(error_response(StatusCode::BAD_REQUEST, &detail));
                }
            };
            if let Ok(mut cap) = request_capture.lock() {
                cap.feed(&collected);
            }
            trace.request.body = snapshot(&request_capture, &request_content);
            if over_limit && let Some(ref mut b) = trace.request.body {
                b.truncated = true;
            }

            let eval_cap = Mutex::new({
                let mut c = Capture::with_limit(inspect_limit);
                c.feed(&collected);
                c
            });
            resolve_operation(
                &mut trace,
                advertised_operation.as_deref(),
                &eval_cap,
                &request_content,
            );
            let mut full_body = snapshot(&eval_cap, &request_content);
            if over_limit {
                match full_body.as_mut() {
                    Some(b) => b.truncated = true,
                    None => {
                        full_body = Some(trace::BodyTrace {
                            text: None,
                            hex: None,
                            encoding: request_content.wire_encoding.clone(),
                            bytes: collected.len() as u64,
                            truncated: true,
                            binary: false,
                            sha256: String::new(),
                        });
                    }
                }
            }
            let mut eval_trace = trace.clone();
            eval_trace.request.body = full_body.clone();
            if over_limit {
                let denied = first_denied(&eval_trace, gate).unwrap_or_else(|| {
                    Access::new(
                        format!("{}:*", trace.service),
                        crate::lock::UNVERIFIED_RESOURCE,
                    )
                });
                state.trace.start(&trace.id);
                return Ok(reject_blocked(
                    trace,
                    &eval_trace,
                    &denied,
                    gate,
                    &state,
                    started,
                ));
            }
            if let Some(denied) = first_denied(&eval_trace, gate) {
                state.trace.start(&trace.id);
                return Ok(reject_blocked(
                    trace,
                    &eval_trace,
                    &denied,
                    gate,
                    &state,
                    started,
                ));
            }
            enforce_full_body = full_body;
            Full::new(collected).map_err(|never| match never {}).boxed()
        } else {
            if let Some(denied) = first_denied(&trace, gate) {
                // Drain small bodies with a strict size cap so keep-alive clients
                // read the 403 cleanly without trusting `size_hint` blindly.
                let small_exact = body
                    .size_hint()
                    .exact()
                    .is_some_and(|len| len <= state.max_body as u64);
                if small_exact
                    && let Ok(CollectedBody::Complete(bytes)) =
                        collect_bounded(body, state.max_body.min(inspect_limit)).await
                {
                    if let Ok(mut cap) = request_capture.lock() {
                        cap.feed(&bytes);
                    }
                    trace.request.body = snapshot(&request_capture, &request_content);
                }
                state.trace.start(&trace.id);
                let eval_trace = trace.clone();
                return Ok(reject_blocked(
                    trace,
                    &eval_trace,
                    &denied,
                    gate,
                    &state,
                    started,
                ));
            }
            TeeBody::new(body, request_capture.clone())
                .map_err(std::io::Error::other)
                .boxed()
        }
    } else if state.trace.gate().is_some() && state.max_body < inspect_limit {
        let gate_cap = Arc::new(Mutex::new(Capture::with_limit(inspect_limit)));
        gate_capture = Some(gate_cap.clone());
        let inner = TeeBody::new(body, request_capture.clone());
        TeeBody::new(inner, gate_cap)
            .map_err(std::io::Error::other)
            .boxed()
    } else {
        TeeBody::new(body, request_capture.clone())
            .map_err(std::io::Error::other)
            .boxed()
    };

    let upstream_uri = format!("{}{}", state.upstream, forward_path);
    let mut upstream = match Request::builder()
        .method(parts.method)
        .uri(upstream_uri)
        .body(upstream_body)
    {
        Ok(request) => request,
        Err(error) => {
            return Ok(error_response(StatusCode::BAD_REQUEST, &error.to_string()));
        }
    };
    // Headers (including Host and Authorization) are forwarded verbatim so
    // SigV4 signatures remain valid.
    *upstream.headers_mut() = parts.headers;

    // A slow call (long poll, large transfer, a hung emulator) shows up as
    // pending—with its full request headers and captured body—instead of
    // being invisible until it ends.
    state.trace.start(&trace.id);
    let finished = Arc::new(AtomicBool::new(false));
    {
        let state = state.clone();
        let finished = finished.clone();
        let pending_capture = request_capture.clone();
        let pending_content = request_content.clone();
        let pending_adv = advertised_operation.clone();
        let mut pending = Trace {
            pending: true,
            ..trace.clone()
        };
        tokio::spawn(async move {
            tokio::time::sleep(PENDING_AFTER).await;
            if !finished.load(Ordering::SeqCst) {
                pending.request.body = snapshot(&pending_capture, &pending_content);
                resolve_operation(
                    &mut pending,
                    pending_adv.as_deref(),
                    &pending_capture,
                    &pending_content,
                );
                state.trace.record(pending);
            }
        });
    }

    match state.client.request(upstream).await {
        Ok(response) => {
            let status = response.status();
            let headers = response.headers().clone();
            let response_headers = trace::headers(&headers);
            let response_content = capture::content(&headers);

            // Enforce response-discovered implied permissions (such as `kms:Decrypt`
            // on an SSE-KMS S3 `GetObject`) before returning the payload.
            if state.enforce
                && let Some(gate) = state.trace.gate()
            {
                let mut check_trace = Trace {
                    status: status.as_u16(),
                    response: Message {
                        headers: response_headers.clone(),
                        content_type: response_content.content_type.clone(),
                        body: None,
                    },
                    ..trace.clone()
                };
                check_trace.request.body = enforce_full_body
                    .clone()
                    .or_else(|| snapshot(&request_capture, &request_content));
                if enforce_full_body.is_none() {
                    resolve_operation(
                        &mut check_trace,
                        advertised_operation.as_deref(),
                        &request_capture,
                        &request_content,
                    );
                }
                if let Some(denied) = first_denied(&check_trace, gate) {
                    finished.store(true, Ordering::SeqCst);
                    drop(response);
                    let mut out_trace = check_trace.clone();
                    out_trace.request.body = snapshot(&request_capture, &request_content);
                    return Ok(reject_blocked(
                        out_trace,
                        &check_trace,
                        &denied,
                        gate,
                        &state,
                        started,
                    ));
                }
            }

            let response_capture = Arc::new(Mutex::new(Capture::with_limit(state.max_body)));
            let pending = Pending {
                state: state.clone(),
                trace: Trace {
                    status: status.as_u16(),
                    response: Message {
                        headers: response_headers,
                        content_type: response_content.content_type.clone(),
                        body: None,
                    },
                    ..trace
                },
                started,
                finished,
                advertised_operation,
                request_capture,
                request_content,
                enforce_full_body,
                gate_capture,
                response_capture: response_capture.clone(),
                response_content,
            };
            // The trace entry is written when the response body ends (or the
            // client goes away), so `ms` covers the whole exchange:
            let body = TeeBody::new(response.into_body(), response_capture)
                .on_end(move || pending.finish())
                .map_err(std::io::Error::other)
                .boxed();
            let mut out = Response::new(body);
            *out.status_mut() = status;
            *out.headers_mut() = headers;
            Ok(out)
        }
        Err(error) => {
            finished.store(true, Ordering::SeqCst);
            let detail = error_chain(&error);
            let mut trace = Trace {
                status: StatusCode::BAD_GATEWAY.as_u16(),
                ms: started.elapsed().as_millis() as u64,
                problem: Some(problem::unreachable(&state.upstream, &detail)),
                error: Some(detail),
                ..trace
            };
            trace.request.body = snapshot(&request_capture, &request_content);
            resolve_operation(
                &mut trace,
                advertised_operation.as_deref(),
                &request_capture,
                &request_content,
            );
            state.trace.record(trace);
            Ok(error_response(StatusCode::BAD_GATEWAY, "upstream error"))
        }
    }
}

/// `client error (Connect): tcp connect error: Connection refused` instead of
/// just the outermost `client error (Connect)`.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.contains(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

/// Return the first derived permission on `trace` that `gate` does not allow.
fn first_denied(trace: &Trace, gate: &Gate) -> Option<Access> {
    let req_ctx = crate::lock::RequestContext::from_trace(trace);
    let (own, implied) = access::derive_for_gate(trace, gate.is_policy());
    if let Some(denied) = own
        .into_iter()
        .find(|entry| !gate.allows_in_context(entry, Some(&req_ctx)))
    {
        return Some(denied);
    }
    implied.into_iter().find(|entry| {
        let imp_ctx = req_ctx.for_implied(&trace.service, trace.region.as_deref(), &entry.action);
        !gate.allows_in_context(entry, Some(&imp_ctx))
    })
}

/// Record a blocked call in the trace and return a protocol-accurate AWS
/// `AccessDenied` / `AccessDeniedException` response.
fn reject_blocked(
    mut trace: Trace,
    eval_trace: &Trace,
    denied: &Access,
    gate: &Gate,
    state: &State,
    started: Instant,
) -> Response<Body> {
    let wire = problem::blocked(
        &trace.service,
        &trace.request.headers,
        trace.request.content_type.as_deref(),
        denied,
        &gate.label,
        &trace.id,
    );
    let mut resp_headers = hyper::HeaderMap::new();
    resp_headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(wire.content_type),
    );
    for (name, value) in &wire.headers {
        if let (Ok(hname), Ok(hval)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            header::HeaderValue::from_str(value),
        ) {
            resp_headers.insert(hname, hval);
        }
    }
    let body_bytes = wire.body.as_bytes();
    let sha256 = {
        use sha2::{Digest, Sha256};
        capture::hex(&Sha256::digest(body_bytes))
    };
    trace.status = wire.status;
    trace.ms = started.elapsed().as_millis() as u64;
    trace.blocked = true;
    trace.problem = Some(wire.problem);
    trace.response = Message {
        headers: trace::headers(&resp_headers),
        content_type: Some(
            wire.content_type
                .split(';')
                .next()
                .unwrap_or(wire.content_type)
                .to_string(),
        ),
        body: Some(trace::BodyTrace {
            text: Some(wire.body.clone()),
            hex: None,
            encoding: None,
            bytes: body_bytes.len() as u64,
            truncated: false,
            binary: false,
            sha256,
        }),
    };
    state.trace.record_evaluated(trace, Some(eval_trace));

    let mut response = Response::new(
        Full::new(Bytes::from(wire.body))
            .map_err(|never| match never {})
            .boxed(),
    );
    *response.status_mut() = StatusCode::from_u16(wire.status).unwrap_or(StatusCode::FORBIDDEN);
    *response.headers_mut() = resp_headers;
    response
}

fn error_response(status: StatusCode, message: &str) -> Response<Body> {
    let body = Full::new(Bytes::from(message.to_string()))
        .map_err(|never| match never {})
        .boxed();
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

/// A bound, running proxy.
pub struct Server {
    pub addr: SocketAddr,
    pub state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
    v6_task: Option<tokio::task::JoinHandle<()>>,
}

impl Server {
    #[allow(dead_code)]
    pub async fn start(
        options: &Options,
        trace_path: PathBuf,
        header: RunHeader,
        locked: Option<BTreeSet<Access>>,
    ) -> Result<Server> {
        Self::start_with_gate(
            options,
            trace_path,
            header,
            locked.map(|set| Gate::from_lock("lenslet.lock", set)),
        )
        .await
    }

    /// Validate everything, bind loopback (IPv4 + best-effort IPv6 so
    /// `localhost` works on either stack), write the run header, and start
    /// serving in the background.
    pub async fn start_with_gate(
        options: &Options,
        trace_path: PathBuf,
        mut header: RunHeader,
        gate: Option<Gate>,
    ) -> Result<Server> {
        options.validate()?;
        // rustls needs a crypto provider before any TLS config is built.
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        // Read and validate everything before creating any files.
        let roots = if options.upstream.tls {
            emulator_roots(&options.emulator_ca())?
        } else {
            // A plain-HTTP upstream has no TLS certificate to trust.
            RootCertStore::empty()
        };
        let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), options.listen))
            .await
            .with_context(|| format!("bind 127.0.0.1:{}", options.listen))?;
        let addr = listener.local_addr().context("read the bound address")?;
        let v6_listener = TcpListener::bind((IpAddr::V6(Ipv6Addr::LOCALHOST), addr.port()))
            .await
            .ok();
        header.endpoint = format!("http://{addr}");
        header.upstream = options.upstream.base.clone();
        let trace = Arc::new(TraceSink::with_gate(trace_path, &header, gate)?);
        let state = Arc::new(State {
            client: client(roots),
            upstream: options.upstream.base.clone(),
            max_body: options.max_body,
            enforce: options.enforce,
            trace,
        });
        let task = tokio::spawn(serve(listener, state.clone()));
        let v6_task = v6_listener.map(|v6| tokio::spawn(serve(v6, state.clone())));
        Ok(Server {
            addr,
            state,
            task,
            v6_task,
        })
    }

    /// The endpoint SDKs should use.
    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stop accepting, after giving in-flight calls up to `grace` to finish.
    pub async fn stop(self, grace: Duration) {
        let deadline = Instant::now() + grace;
        while self.state.trace.summary().unfinished > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.task.abort();
        if let Some(v6_task) = self.v6_task {
            v6_task.abort();
        }
    }
}

async fn serve(listener: TcpListener, state: Arc<State>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let state = state.clone();
        tokio::spawn(async move {
            let service = service_fn(move |req| handle(req, state.clone()));
            if let Err(error) = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await
                && !error.is_incomplete_message()
            {
                eprintln!("lenslet: connection error: {error}");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::MAX_CAPTURE_BYTES;
    use http_body_util::BodyExt;
    use hyper::{HeaderMap, body::Incoming};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn options(arguments: &[&str]) -> Result<Options> {
        let mut options = Options::new(Path::new("/home/tester"), 9297);
        let mut it = arguments.iter().map(|argument| argument.to_string());
        while let Some(arg) = it.next() {
            let mut value = || it.next().context("needs a value");
            if !options.accept(&arg, &mut value)? {
                bail!("unknown {arg}");
            }
        }
        options.validate()?;
        Ok(options)
    }

    #[test]
    fn validates_ports_and_upstream_addresses() {
        let parsed = options(&["--listen", "9300", "--upstream", "9400"]).unwrap();
        assert_eq!(parsed.listen, 9300);
        assert_eq!(parsed.upstream.base, "https://127.0.0.1:9400");
        assert_eq!(
            parsed.emulator_ca(),
            PathBuf::from("/home/tester/.aws/emulator/9400/ca.pem"),
            "the emulator CA follows the upstream port"
        );
        assert!(options(&["--listen", "not-a-port"]).is_err());
        assert!(options(&["--listen", "9297", "--upstream", "9297"]).is_err());
        assert!(options(&["--upstream", "0"]).is_err());
        assert!(options(&["--upstream", "ftp://127.0.0.1:21"]).is_err());
        assert!(options(&["--upstream", "https://127.0.0.1"]).is_err());
        assert!(options(&["--listen"]).is_err());

        let plain = options(&["--upstream", "http://127.0.0.1:8080"]).unwrap();
        assert_eq!(plain.upstream.base, "http://127.0.0.1:8080");
        assert_eq!(plain.upstream.port, 8080);
        assert!(!plain.upstream.tls);
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::AtomicUsize;
            static SEQ: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "lenslet-proxy-{tag}-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                SEQ.fetch_add(1, Ordering::Relaxed),
            ));
            TempDir(path)
        }

        fn trace(&self) -> PathBuf {
            self.0.join("trace.jsonl")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A plain-HTTP upstream that records what it received and answers with
    /// JSON, or with 100 KiB of text when the path ends in `/big`.
    async fn spawn_upstream() -> (u16, mpsc::Receiver<(HeaderMap, Bytes)>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let tx = tx.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |req: Request<Incoming>| {
                        let tx = tx.clone();
                        async move {
                            let (parts, body) = req.into_parts();
                            let path = parts.uri.path().to_string();
                            let big = path.ends_with("/big");
                            if path.ends_with("/slow") {
                                tokio::time::sleep(Duration::from_millis(1400)).await;
                            }
                            let received = body
                                .collect()
                                .await
                                .map(|collected| collected.to_bytes())
                                .unwrap_or_default();
                            let _ = tx.send((parts.headers, received)).await;
                            if path.ends_with("/missing") {
                                return Ok::<_, Infallible>(
                                    Response::builder()
                                        .status(StatusCode::NOT_FOUND)
                                        .header("content-type", "application/xml")
                                        .body(Full::new(Bytes::from_static(
                                            b"<Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>",
                                        )))
                                        .unwrap(),
                                );
                            }
                            let (content_type, body) = if big {
                                ("text/plain", "x".repeat(MAX_CAPTURE_BYTES + 36 * 1024))
                            } else {
                                (
                                    "application/json",
                                    r#"{"ok":true,"Secret":"s3cr3t-value"}"#.to_string(),
                                )
                            };
                            Ok::<_, Infallible>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .header("content-type", content_type)
                                    .header("x-request-id", "upstream-1")
                                    .body(Full::new(Bytes::from(body)))
                                    .unwrap(),
                            )
                        }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        (port, rx)
    }

    fn state(upstream: &str, trace: PathBuf) -> Arc<State> {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        Arc::new(State {
            client: client(RootCertStore::empty()),
            upstream: upstream.to_string(),
            max_body: MAX_CAPTURE_BYTES,
            enforce: false,
            trace: Arc::new(TraceSink::new(trace, &RunHeader::default(), None).unwrap()),
        })
    }

    fn sdk_request(method: &str, uri: &str, body: &'static [u8]) -> Request<Full<Bytes>> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:9297")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261115/us-east-1/s3/aws4_request, SignedHeaders=host, Signature=deadbeef",
            )
            .header("x-amz-security-token", "SESSIONTOKEN123")
            .header("x-amz-content-sha256", "UNSIGNED-PAYLOAD")
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from_static(body)))
            .unwrap()
    }

    /// An AWS Query protocol request: form body, no `x-amz-target`, no `x-id`.
    fn query_request(
        uri: &str,
        service: &str,
        target: Option<&str>,
        body: &'static [u8],
    ) -> Request<Full<Bytes>> {
        let mut builder = Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "127.0.0.1:9297")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261115/us-east-1/{service}/aws4_request, SignedHeaders=host, Signature=deadbeef"
                ),
            )
            .header("content-type", "application/x-www-form-urlencoded");
        if let Some(target) = target {
            builder = builder.header("x-amz-target", target);
        }
        builder.body(Full::new(Bytes::from_static(body))).unwrap()
    }

    fn traces(path: &Path) -> Vec<Trace> {
        fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| match serde_json::from_str(line).expect("a record") {
                trace::Record::Call(trace) => Some(*trace),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn proxies_bodies_intact_and_traces_raw_copies() {
        let (port, mut upstream) = spawn_upstream().await;
        let temp = TempDir::new("bodies");
        let trace_path = temp.trace();
        let state = state(&format!("http://127.0.0.1:{port}"), trace_path.clone());

        let request_body = br#"{"password":"hunter2","name":"lenslet"}"#;
        let request = sdk_request(
            "PUT",
            "/bucket/key?x-id=PutObject&X-Amz-Signature=deadbeef",
            request_body,
        );
        let response = handle(request, state.clone()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // The entry is written after the response body completes, not at headers.
        assert!(
            traces(&trace_path).is_empty(),
            "trace must wait for the response body"
        );

        let response_body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            response_body.as_ref(),
            br#"{"ok":true,"Secret":"s3cr3t-value"}"#,
            "response forwarding is byte exact"
        );

        let (headers, forwarded) = upstream.recv().await.expect("upstream received the call");
        assert_eq!(
            forwarded.as_ref(),
            request_body,
            "request body is byte exact"
        );
        assert_eq!(
            headers.get("authorization").unwrap(),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261115/us-east-1/s3/aws4_request, SignedHeaders=host, Signature=deadbeef",
            "SigV4 headers are forwarded verbatim"
        );
        assert_eq!(headers.get("host").unwrap(), "127.0.0.1:9297");
        assert_eq!(
            headers.get("x-amz-content-sha256").unwrap(),
            "UNSIGNED-PAYLOAD"
        );

        let written = traces(&trace_path);
        assert_eq!(written.len(), 1);
        let trace = &written[0];

        assert_eq!(trace.method, "PUT");
        assert_eq!(
            trace.path,
            "/bucket/key?x-id=PutObject&X-Amz-Signature=deadbeef"
        );
        assert_eq!(trace.host, "127.0.0.1:9297");
        assert_eq!(trace.service, "s3");
        assert_eq!(trace.operation, "PutObject");
        assert_eq!(trace.status, 200);
        assert!(trace.authorization);
        assert!(!trace.id.is_empty());
        assert!(trace.at.contains('T'));
        assert!(trace.error.is_none());

        assert_eq!(
            trace.request.content_type.as_deref(),
            Some("application/json")
        );
        let request_body_trace = trace.request.body.as_ref().expect("request body captured");
        assert!(!request_body_trace.binary);
        assert!(!request_body_trace.truncated);
        assert_eq!(request_body_trace.bytes, request_body.len() as u64);
        assert_eq!(request_body_trace.sha256, sha256_hex(request_body));
        let request_text = request_body_trace.text.as_deref().unwrap();
        assert_eq!(request_text, std::str::from_utf8(request_body).unwrap());

        assert_eq!(
            trace.response.content_type.as_deref(),
            Some("application/json")
        );
        let response_body_trace = trace
            .response
            .body
            .as_ref()
            .expect("response body captured");
        assert!(!response_body_trace.binary);
        assert_eq!(
            response_body_trace.text.as_deref(),
            Some(std::str::from_utf8(response_body.as_ref()).unwrap())
        );
        assert_eq!(
            response_body_trace.sha256,
            sha256_hex(response_body.as_ref())
        );

        let auth = trace
            .request
            .headers
            .iter()
            .find(|header| header.name == "authorization")
            .expect("authorization is traced by name");
        assert_eq!(
            auth.value,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261115/us-east-1/s3/aws4_request, SignedHeaders=host, Signature=deadbeef"
        );
        let token = trace
            .request
            .headers
            .iter()
            .find(|header| header.name == "x-amz-security-token")
            .unwrap();
        assert_eq!(token.value, "SESSIONTOKEN123");
        let host = trace
            .request
            .headers
            .iter()
            .find(|header| header.name == "host")
            .unwrap();
        assert_eq!(host.value, "127.0.0.1:9297");

        // The trace file keeps the raw exchange, one line per call.
        let serialized = fs::read_to_string(&trace_path).unwrap();
        for raw in [
            "hunter2",
            "SESSIONTOKEN123",
            "AKIDEXAMPLE",
            "s3cr3t-value",
            "X-Amz-Signature=deadbeef",
        ] {
            assert!(serialized.contains(raw), "{raw} missing from the trace");
        }
        assert_eq!(
            serialized.lines().count(),
            2,
            "the run header, then one line per call"
        );

        // A >1 MiB response keeps forwarding everything and captures a prefix.
        let big_len = MAX_CAPTURE_BYTES + 36 * 1024;
        let request = sdk_request("GET", "/bucket/big", b"");
        let response = handle(request, state.clone()).await.unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.len(), big_len, "large responses are not truncated");

        let written = traces(&trace_path);
        assert_eq!(written.len(), 2);
        let big = written[1].response.body.as_ref().expect("big body traced");
        assert!(big.truncated, "capture is bounded");
        assert!(!big.binary);
        assert_eq!(big.bytes, big_len as u64);
        let text = big.text.as_deref().unwrap();
        assert_eq!(text.len(), MAX_CAPTURE_BYTES);
        assert!(text.bytes().all(|byte| byte == b'x'));
        assert_eq!(big.sha256, sha256_hex(&vec![b'x'; big_len]));
    }

    #[tokio::test]
    async fn records_upstream_failure_as_a_502_trace() {
        // Grab a port and release it so nothing is listening there.
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let temp = TempDir::new("failure");
        let trace_path = temp.trace();
        let state = state(&format!("http://127.0.0.1:{port}"), trace_path.clone());
        let request = sdk_request("POST", "/?x-id=SendMessage", b"{}");
        let response = handle(request, state).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"upstream error");

        let written = traces(&trace_path);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].status, 502);
        assert!(written[0].error.is_some());
        assert!(written[0].response.body.is_none());
        assert_eq!(written[0].operation, "SendMessage");
        let problem = written[0].problem.as_ref().expect("502 explains itself");
        assert_eq!(problem.code, "EmulatorUnreachable");
        assert!(problem.hint.as_deref().unwrap().contains(&port.to_string()));
        assert!(
            written[0].error.as_deref().unwrap().contains("onnect"),
            "the error keeps its cause: {:?}",
            written[0].error
        );
    }

    #[tokio::test]
    async fn body_hook_runs_once_per_call() {
        let (port, _upstream) = spawn_upstream().await;
        let temp = TempDir::new("once");
        let trace_path = temp.trace();
        let state = state(&format!("http://127.0.0.1:{port}"), trace_path.clone());

        for _ in 0..3 {
            let response = handle(sdk_request("GET", "/bucket/key", b""), state.clone())
                .await
                .unwrap();
            let _ = response.into_body().collect().await.unwrap();
        }
        let written = traces(&trace_path);
        assert_eq!(written.len(), 3, "exactly one entry per call");
        let ids: std::collections::HashSet<&str> =
            written.iter().map(|trace| trace.id.as_str()).collect();
        assert_eq!(ids.len(), 3, "ids are unique per call");
    }

    #[tokio::test]
    async fn resolves_query_protocol_action_from_the_request_body() {
        let (port, _upstream) = spawn_upstream().await;
        let temp = TempDir::new("action");
        let trace_path = temp.trace();
        let state = state(&format!("http://127.0.0.1:{port}"), trace_path.clone());

        let body = b"Action=DescribeInstances&Version=2016-11-15";
        let response = handle(query_request("/", "ec2", None, body), state.clone())
            .await
            .unwrap();
        let _ = response.into_body().collect().await.unwrap();

        let written = traces(&trace_path);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].service, "ec2");
        assert_eq!(
            written[0].operation, "DescribeInstances",
            "the Action from the streamed form body names the operation"
        );
        assert_eq!(
            written[0].request.body.as_ref().unwrap().text.as_deref(),
            Some(std::str::from_utf8(body).unwrap())
        );
    }

    #[tokio::test]
    async fn x_amz_target_and_x_id_take_precedence_over_the_body_action() {
        let (port, _upstream) = spawn_upstream().await;
        let temp = TempDir::new("precedence");
        let trace_path = temp.trace();
        let state = state(&format!("http://127.0.0.1:{port}"), trace_path.clone());
        let body = b"Action=DescribeInstances&Version=2016-11-15";

        for (uri, service, target) in [
            ("/", "dynamodb", Some("DynamoDB_20120810.PutItem")),
            ("/?x-id=SendMessage", "sqs", None),
        ] {
            let response = handle(query_request(uri, service, target, body), state.clone())
                .await
                .unwrap();
            let _ = response.into_body().collect().await.unwrap();
        }

        let written = traces(&trace_path);
        assert_eq!(written.len(), 2);
        assert_eq!(written[0].service, "dynamodb");
        assert_eq!(written[0].operation, "PutItem");
        assert_eq!(written[1].service, "sqs");
        assert_eq!(written[1].operation, "SendMessage");
    }

    #[tokio::test]
    async fn falls_back_to_the_method_when_nothing_is_advertised() {
        let (port, _upstream) = spawn_upstream().await;
        let temp = TempDir::new("fallback");
        let trace_path = temp.trace();
        let state = state(&format!("http://127.0.0.1:{port}"), trace_path.clone());

        let response = handle(
            query_request("/", "cloudformation", None, b"Version=2010-05-15"),
            state.clone(),
        )
        .await
        .unwrap();
        let _ = response.into_body().collect().await.unwrap();

        let written = traces(&trace_path);
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].service, "cloudformation");
        assert_eq!(written[0].operation, "POST");
    }

    #[tokio::test]
    async fn failed_calls_carry_their_aws_error() {
        let (port, _upstream) = spawn_upstream().await;
        let temp = TempDir::new("problem");
        let trace_path = temp.trace();
        let state = state(&format!("http://127.0.0.1:{port}"), trace_path.clone());
        let response = handle(
            sdk_request("GET", "/bucket/missing?x-id=GetObject", b""),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let _ = response.into_body().collect().await.unwrap();

        let written = traces(&trace_path);
        let problem = written[0].problem.as_ref().unwrap();
        assert_eq!(problem.code, "NoSuchKey");
        assert_eq!(
            problem.message.as_deref(),
            Some("The specified key does not exist.")
        );
        assert_eq!(written[0].region.as_deref(), Some("us-east-1"));
        let summary = state.trace.summary();
        assert_eq!((summary.calls, summary.errors), (1, 1));
    }

    #[tokio::test]
    async fn slow_calls_are_visible_while_pending() {
        let (port, _upstream) = spawn_upstream().await;
        let temp = TempDir::new("pending");
        let trace_path = temp.trace();
        let state = state(&format!("http://127.0.0.1:{port}"), trace_path.clone());
        let call = tokio::spawn(handle(
            sdk_request("POST", "/q/slow", br#"{"WaitTimeSeconds":20}"#),
            state.clone(),
        ));

        tokio::time::sleep(Duration::from_millis(1200)).await;
        let written = traces(&trace_path);
        assert_eq!(written.len(), 1, "a pending line appears after a second");
        assert!(written[0].pending);
        assert_eq!(written[0].status, 0);
        assert!(
            !written[0].request.headers.is_empty(),
            "pending trace keeps full request headers for live debugging"
        );
        assert_eq!(
            written[0]
                .request
                .body
                .as_ref()
                .and_then(|b| b.text.as_deref()),
            Some(r#"{"WaitTimeSeconds":20}"#),
            "pending trace keeps captured request body"
        );
        assert_eq!(state.trace.summary().unfinished, 1);

        let response = call.await.unwrap().unwrap();
        let _ = response.into_body().collect().await.unwrap();
        let written = traces(&trace_path);
        assert_eq!(written.len(), 2);
        assert!(!written[1].pending);
        assert_eq!(
            written[1].id, written[0].id,
            "the final line replaces the pending one"
        );
        assert_eq!(written[1].status, 200);
        assert_eq!(state.trace.summary().unfinished, 0);

        // Fast calls never write a pending line.
        let response = handle(sdk_request("GET", "/q/fast", b""), state.clone())
            .await
            .unwrap();
        let _ = response.into_body().collect().await.unwrap();
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(traces(&trace_path).len(), 3);
    }

    #[tokio::test]
    async fn server_writes_a_run_header_then_calls() {
        let (port, _upstream) = spawn_upstream().await;
        let temp = TempDir::new("server");
        fs::create_dir_all(&temp.0).unwrap();
        let mut options = Options::new(&temp.0, 0);
        options.upstream = Upstream::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let header = RunHeader {
            run: "r".into(),
            command: vec!["npm".into(), "test".into()],
            ..RunHeader::default()
        };
        let server = Server::start(&options, temp.trace(), header, None)
            .await
            .unwrap();
        assert!(server.endpoint().starts_with("http://127.0.0.1:"));

        let plain: Client<HttpConnector, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).build_http();
        let request = Request::builder()
            .uri(format!("{}/bucket/key?x-id=GetObject", server.endpoint()))
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKID/20261115/us-east-1/s3/aws4_request, Signature=x",
            )
            .body(Full::new(Bytes::new()))
            .unwrap();
        let response = plain.request(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let _ = response.into_body().collect().await.unwrap();

        // A bare TCP probe (as the window does) is not a call.
        drop(TcpStream::connect(server.addr).await.unwrap());

        let state = server.state.clone();
        server.stop(Duration::from_secs(1)).await;
        assert_eq!(state.trace.summary().calls, 1);
        let lines: Vec<serde_json::Value> = fs::read_to_string(temp.trace())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["type"], "run");
        assert_eq!(lines[0]["endpoint"], state_endpoint(&lines[0]));
        assert_eq!(lines[1]["type"], "call");
        assert_eq!(lines[1]["access"][0]["action"], "s3:GetObject");
        assert_eq!(lines[1]["access"][0]["resource"], "arn:aws:s3:::bucket/*");
    }

    fn state_endpoint(header: &serde_json::Value) -> String {
        let endpoint = header["endpoint"].as_str().unwrap();
        assert!(endpoint.starts_with("http://127.0.0.1:"));
        endpoint.to_string()
    }

    #[tokio::test]
    async fn calls_outside_the_lock_are_marked_new() {
        let (port, _upstream) = spawn_upstream().await;
        let temp = TempDir::new("lock");
        let locked: BTreeSet<Access> = [Access {
            action: "s3:GetObject".into(),
            resource: "arn:aws:s3:::bucket/*".into(),
        }]
        .into();
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let state = Arc::new(State {
            client: client(RootCertStore::empty()),
            upstream: format!("http://127.0.0.1:{port}"),
            max_body: MAX_CAPTURE_BYTES,
            enforce: false,
            trace: Arc::new(
                TraceSink::new(temp.trace(), &RunHeader::default(), Some(locked)).unwrap(),
            ),
        });
        for request in [
            sdk_request("GET", "/bucket/key?x-id=GetObject", b""),
            sdk_request("DELETE", "/bucket/key?x-id=DeleteObject", b""),
        ] {
            let response = handle(request, state.clone()).await.unwrap();
            let _ = response.into_body().collect().await.unwrap();
        }
        let written = traces(&temp.trace());
        assert!(!written[0].new);
        assert!(written[1].new);
        assert_eq!(written[1].access[0].action, "s3:DeleteObject");
    }

    #[tokio::test]
    async fn enforce_blocks_unlocked_calls_before_they_reach_the_emulator() {
        let (port, mut upstream) = spawn_upstream().await;
        let temp = TempDir::new("enforce");
        let locked: BTreeSet<Access> = [
            Access::new("s3:GetObject", "arn:aws:s3:::bucket/*"),
            Access::new("dynamodb:GetItem", "arn:aws:dynamodb:*:*:table/orders"),
        ]
        .into();
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let state = Arc::new(State {
            client: client(RootCertStore::empty()),
            upstream: format!("http://127.0.0.1:{port}"),
            max_body: MAX_CAPTURE_BYTES,
            enforce: true,
            trace: Arc::new(
                TraceSink::new(temp.trace(), &RunHeader::default(), Some(locked)).unwrap(),
            ),
        });

        // Allowed S3 call reaches upstream normally.
        let ok = handle(
            sdk_request("GET", "/bucket/key?x-id=GetObject", b""),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let _ = ok.into_body().collect().await.unwrap();
        assert!(upstream.try_recv().is_ok());

        // Unlocked S3 DeleteObject is blocked with XML AccessDenied (403) and never hits upstream.
        let s3_denied = handle(
            sdk_request("DELETE", "/bucket/key?x-id=DeleteObject", b""),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(s3_denied.status(), StatusCode::FORBIDDEN);
        let s3_body = s3_denied.into_body().collect().await.unwrap().to_bytes();
        assert!(
            std::str::from_utf8(&s3_body)
                .unwrap()
                .contains("<Code>AccessDenied</Code>")
        );
        assert!(
            upstream.try_recv().is_err(),
            "blocked S3 call never reached upstream"
        );

        // Unlocked DynamoDB PutItem (table in JSON body) is blocked with JSON AccessDeniedException (400).
        let ddb_req = Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "127.0.0.1:9297")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKID/20261115/us-east-1/dynamodb/aws4_request, SignedHeaders=host, Signature=x",
            )
            .header("x-amz-target", "DynamoDB_20120810.PutItem")
            .header("content-type", "application/x-amz-json-1.0")
            .body(Full::new(Bytes::from_static(br#"{"TableName":"orders","Item":{}}"#)))
            .unwrap();
        let ddb_denied = handle(ddb_req, state.clone()).await.unwrap();
        assert_eq!(ddb_denied.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            ddb_denied.headers().get("x-amzn-errortype").unwrap(),
            "AccessDeniedException"
        );
        let ddb_body = ddb_denied.into_body().collect().await.unwrap().to_bytes();
        assert!(
            std::str::from_utf8(&ddb_body)
                .unwrap()
                .contains("AccessDeniedException")
        );
        assert!(
            upstream.try_recv().is_err(),
            "blocked DynamoDB call never reached upstream"
        );

        let written = traces(&temp.trace());
        assert_eq!(written.len(), 3);
        assert!(!written[0].blocked);
        assert!(written[1].blocked);
        assert!(written[2].blocked);
        assert_eq!(state.trace.summary().blocked, 2);
    }

    #[tokio::test]
    async fn policy_gate_enforces_s3_prefix_allow_and_explicit_deny_on_concrete_keys() {
        let (port, mut upstream) = spawn_upstream().await;
        let temp = TempDir::new("policy-s3-prefix");
        let policy = Gate::from_policy_str(
            r#"{
                "Version": "2012-10-17",
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": ["s3:GetObject", "s3:DeleteObject"],
                        "Resource": "arn:aws:s3:::bucket/public/*"
                    },
                    {
                        "Effect": "Deny",
                        "Action": ["s3:GetObject", "s3:DeleteObject"],
                        "Resource": "arn:aws:s3:::bucket/public/secret/*"
                    }
                ]
            }"#,
            "policy.json",
        )
        .unwrap();
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let state = Arc::new(State {
            client: client(RootCertStore::empty()),
            upstream: format!("http://127.0.0.1:{port}"),
            max_body: MAX_CAPTURE_BYTES,
            enforce: true,
            trace: Arc::new(
                TraceSink::with_gate(temp.trace(), &RunHeader::default(), Some(policy.clone()))
                    .unwrap(),
            ),
        });

        // 1. Concrete key inside allowed prefix (`public/readme.txt`) succeeds and is NOT marked new.
        let ok = handle(
            sdk_request("GET", "/bucket/public/readme.txt?x-id=GetObject", b""),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(ok.status(), StatusCode::OK);
        let _ = ok.into_body().collect().await.unwrap();
        assert!(upstream.try_recv().is_ok());

        // 2. Concrete key matching explicit Deny (`public/secret/keys.txt`) is blocked.
        let denied_sub = handle(
            sdk_request("GET", "/bucket/public/secret/keys.txt?x-id=GetObject", b""),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(denied_sub.status(), StatusCode::FORBIDDEN);
        let _ = denied_sub.into_body().collect().await.unwrap();
        assert!(upstream.try_recv().is_err());

        // 3. Multi-object DeleteObjects naming a denied `<Key>` in XML is blocked.
        let del_xml = br#"<Delete><Object><Key>public/readme.txt</Key></Object><Object><Key>public/secret/keys.txt</Key></Object></Delete>"#;
        let del_req = sdk_request("POST", "/bucket?delete", del_xml);
        let denied_del = handle(del_req, state.clone()).await.unwrap();
        assert_eq!(denied_del.status(), StatusCode::FORBIDDEN);
        let _ = denied_del.into_body().collect().await.unwrap();
        assert!(upstream.try_recv().is_err());

        // 4. Consecutive slashes (`/bucket/public/secret//keys.txt`) preserve empty key segments
        //    so an explicit Deny on `public/secret/*` still blocks it.
        let denied_double_slash = handle(
            sdk_request("GET", "/bucket/public/secret//keys.txt?x-id=GetObject", b""),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(denied_double_slash.status(), StatusCode::FORBIDDEN);
        let _ = denied_double_slash.into_body().collect().await.unwrap();
        assert!(upstream.try_recv().is_err());

        let written = traces(&temp.trace());
        assert_eq!(written.len(), 4);
        assert!(!written[0].new, "allowed prefix call is not marked new");
        assert!(!written[0].access[0].new);
        assert!(written[1].blocked && written[1].new);
        assert!(written[2].blocked && written[2].new);
        assert!(written[3].blocked && written[3].new);

        let mut report = crate::summary::Report::default();
        report.set_access_with_gate(&state.trace.summary(), Some(&policy));
        assert_eq!(
            report.access_added,
            vec![
                Access::new(
                    "s3:DeleteObject",
                    "arn:aws:s3:::bucket/public/secret/keys.txt"
                ),
                Access::new(
                    "s3:GetObject",
                    "arn:aws:s3:::bucket/public/secret//keys.txt"
                ),
                Access::new("s3:GetObject", "arn:aws:s3:::bucket/public/secret/keys.txt"),
            ]
        );
    }

    #[tokio::test]
    async fn enforce_inspects_complete_request_body_beyond_max_body_limit() {
        let (port, mut upstream) = spawn_upstream().await;
        let temp = TempDir::new("enforce-max-body");
        let locked: BTreeSet<Access> = [Access::new(
            "dynamodb:BatchWriteItem",
            "arn:aws:dynamodb:*:*:table/orders",
        )]
        .into();
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        // Tiny 64-byte capture limit: `"orders"` fits in the prefix, while
        // `"secret_orders"` appears past byte 140.
        let state = Arc::new(State {
            client: client(RootCertStore::empty()),
            upstream: format!("http://127.0.0.1:{port}"),
            max_body: 64,
            enforce: true,
            trace: Arc::new(
                TraceSink::new(temp.trace(), &RunHeader::default(), Some(locked)).unwrap(),
            ),
        });

        let payload = br#"{"RequestItems":{"orders":[{"PutRequest":{"Item":{"id":{"S":"order-padding-0000000000000000000000000000"}}}}],"secret_orders":[{"PutRequest":{"Item":{"id":{"S":"2"}}}}]}}"#;
        assert!(payload.len() > 140);
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "127.0.0.1:9297")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKID/20261115/us-east-1/dynamodb/aws4_request, SignedHeaders=host, Signature=x",
            )
            .header("x-amz-target", "DynamoDB_20120810.BatchWriteItem")
            .header("content-type", "application/x-amz-json-1.0")
            .body(Full::new(Bytes::from_static(payload)))
            .unwrap();

        let resp = handle(req, state.clone()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let _ = resp.into_body().collect().await.unwrap();
        assert!(
            upstream.try_recv().is_err(),
            "trailing unauthorized table past --max-body must be blocked before reaching upstream"
        );

        let written = traces(&temp.trace());
        assert_eq!(written.len(), 1);
        assert!(written[0].blocked);
        assert!(
            written[0].request.body.as_ref().unwrap().truncated,
            "trace file still honors --max-body"
        );
        assert_eq!(written[0].access.len(), 2);
        assert!(!written[0].access[0].new); // orders
        assert!(written[0].access[1].new); // secret_orders
    }

    #[tokio::test]
    async fn enforce_bounds_request_collection_and_fails_closed_when_exceeded() {
        use std::pin::Pin;
        use std::sync::atomic::AtomicUsize;
        use std::task::{Context, Poll};

        struct InfiniteStream {
            polled_frames: Arc<AtomicUsize>,
        }
        impl hyper::body::Body for InfiniteStream {
            type Data = Bytes;
            type Error = std::io::Error;
            fn poll_frame(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
                self.polled_frames.fetch_add(1, Ordering::SeqCst);
                Poll::Ready(Some(Ok(hyper::body::Frame::data(Bytes::from_static(
                    br#"{"RequestItems":{"orders":[{"PutRequest":{"Item":{"id":{"S":"chunk"}}}}]}}"#,
                )))))
            }
        }

        let (port, mut upstream) = spawn_upstream().await;
        let temp = TempDir::new("enforce-bounded");
        let admin = Gate::from_policy_str(
            r#"{"Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"#,
            "admin.json",
        )
        .unwrap();
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let state = Arc::new(State {
            client: client(RootCertStore::empty()),
            upstream: format!("http://127.0.0.1:{port}"),
            max_body: 64,
            enforce: true,
            trace: Arc::new(
                TraceSink::with_gate(temp.trace(), &RunHeader::default(), Some(admin)).unwrap(),
            ),
        });

        let polled = Arc::new(AtomicUsize::new(0));
        let req = Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "127.0.0.1:9297")
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKID/20261115/us-east-1/dynamodb/aws4_request, SignedHeaders=host, Signature=x",
            )
            .header("x-amz-target", "DynamoDB_20120810.BatchWriteItem")
            .header("content-type", "application/x-amz-json-1.0")
            .body(InfiniteStream {
                polled_frames: polled.clone(),
            })
            .unwrap();

        // With a 128-byte inspect limit, an infinite streaming body stops after 2
        // frames (74 * 2 = 148 > 128) and fails closed immediately instead of hanging/OOMing.
        let resp = handle_with_inspect_limit(req, state.clone(), 128)
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert_eq!(polled.load(Ordering::SeqCst), 2);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(
            std::str::from_utf8(&body)
                .unwrap()
                .contains("AccessDeniedException")
        );
        assert!(upstream.try_recv().is_err());
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}
