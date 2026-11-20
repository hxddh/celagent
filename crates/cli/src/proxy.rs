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
    collections::BTreeMap,
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
use hyper::{Request, Response, StatusCode, header, service::service_fn};
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto as conn_auto,
};
use rustls::RootCertStore;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::watch,
};

use crate::{
    access,
    capture::{self, Capture, Content, TeeBody},
    lock::{Denial, Evaluation, Gate},
    problem,
    trace::{self, BlockedPhase, Message, RunHeader, Trace, TraceSink},
    wire,
};

type Body = BoxBody<Bytes, std::io::Error>;
type HttpsClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Body>;

/// Upper bound on reading a request body for access derivation (64 MiB, 4×
/// DynamoDB's 16 MiB batch limit), independent of `--max-body`, so a small
/// trace capture never hides a resource named late in the body. Only calls
/// whose access is read from the body get this second look.
const MAX_INSPECT_BYTES: usize = 64 * 1024 * 1024;

/// A blocked call's unread request body is drained up to this much, for up
/// to this long, so the client reads the `AccessDenied` instead of a reset.
const DRAIN_BYTES: usize = 1024 * 1024;
const DRAIN_TIME: Duration = Duration::from_millis(500);

/// A client must send a request's headers within this long.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// A call still running after this long gets a `pending` trace line.
const PENDING_AFTER: Duration = Duration::from_secs(1);

/// A comment line on an idle live event stream, this often.
const KEEPALIVE: Duration = Duration::from_secs(15);

/// Marks a call the window re-sends: the id of the recorded call it replays.
/// Stripped before forwarding, and not recorded among the request headers.
const REPLAY_HEADER: &str = "x-lenslet-replay-of";

#[derive(Clone)]
pub struct State {
    client: HttpsClient,
    /// Default upstream base URL without a trailing slash, e.g. `https://127.0.0.1:9298`.
    upstream: String,
    /// Per-service local upstream overrides (`service` -> base URL), e.g. `s3` -> `http://127.0.0.1:9000`.
    routes: BTreeMap<String, String>,
    /// Bound proxy endpoint URL, e.g. `http://127.0.0.1:9297`.
    endpoint: String,
    pub max_body: usize,
    pub enforce: bool,
    pub trace: Arc<TraceSink>,
}

impl State {
    fn upstream_for(&self, service: &str) -> &str {
        let low = service.to_ascii_lowercase();
        if let Some(target) = self.routes.get(&low) {
            return target.as_str();
        }
        let canonical = match low.as_str() {
            "streams.dynamodb" | "dynamodbstreams" => "dynamodb",
            "email" | "sesv2" => "ses",
            "monitoring" => "cloudwatch",
            "events" => "eventbridge",
            "states" => "stepfunctions",
            other => other,
        };
        self.routes
            .get(canonical)
            .map(String::as_str)
            .unwrap_or(&self.upstream)
    }
}

/// Everything needed to write one trace entry once the response body is done.
struct Pending {
    state: Arc<State>,
    trace: Trace,
    access_key_id: Option<String>,
    started: Instant,
    finished: Arc<AtomicBool>,
    /// Operation advertised by the wire protocol; when `None`, the request body
    /// may still supply one (AWS Query `Action=...`).
    advertised_operation: Option<String>,
    request_capture: Arc<Mutex<Capture>>,
    request_content: Content,
    /// The complete request body, read under `--enforce`, when the trace
    /// capture (`--max-body`) holds less.
    full_body: Option<trace::BodyTrace>,
    /// A second, larger capture of a body-dependent request for derivation,
    /// when `--max-body` is smaller than [`MAX_INSPECT_BYTES`].
    derive_capture: Option<Arc<Mutex<Capture>>>,
    /// The gate evaluation already made for this call (under `--enforce`).
    evaluation: Option<Evaluation>,
    response_capture: Arc<Mutex<Capture>>,
    response_content: Content,
}

impl Pending {
    fn finish(mut self) {
        self.finished.store(true, Ordering::SeqCst);
        self.trace.ms = self.started.elapsed().as_millis() as u64;
        self.trace.request.body = snapshot(&self.request_capture, &self.request_content);
        self.trace.response.body = snapshot(&self.response_capture, &self.response_content);
        let operation_capture = self
            .derive_capture
            .as_deref()
            .unwrap_or(&self.request_capture);
        resolve_operation(
            &mut self.trace,
            self.advertised_operation.as_deref(),
            operation_capture,
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
        let evaluation = match self.evaluation.take() {
            Some(evaluation) => evaluation,
            None => {
                let mut full = self.full_body.take().or_else(|| {
                    self.derive_capture
                        .as_deref()
                        .and_then(|capture| snapshot(capture, &self.request_content))
                });
                with_body(&mut self.trace, &mut full, |trace| {
                    self.state.trace.evaluate(trace)
                })
            }
        };
        self.state
            .trace
            .record_with_key(self.trace, self.access_key_id.as_deref(), evaluation);
    }
}

/// Run `f` on `trace` as if its request body were `body` (the complete body,
/// when the trace keeps only a prefix), without copying the trace.
fn with_body<R>(
    trace: &mut Trace,
    body: &mut Option<trace::BodyTrace>,
    f: impl FnOnce(&Trace) -> R,
) -> R {
    if body.is_none() {
        return f(trace);
    }
    std::mem::swap(&mut trace.request.body, body);
    let result = f(trace);
    std::mem::swap(&mut trace.request.body, body);
    result
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
    /// Per-service local upstream overrides (`--route s3=http://127.0.0.1:9000`).
    pub routes: BTreeMap<String, Upstream>,
    /// Accept connections from local Docker container bridge interfaces (`0.0.0.0` filtered to local/RFC1918 IPs).
    pub allow_docker: bool,
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
            routes: BTreeMap::new(),
            allow_docker: false,
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
                self.upstream.ensure_local_boundary("--upstream")?;
                self.upstream_given = true;
            }
            "--route" => {
                let raw = value()?;
                let Some((svcs, target)) = raw.split_once('=') else {
                    bail!(
                        "--route expects SERVICE=ADDRESS (e.g. `--route s3=http://127.0.0.1:9000`), got `{raw}`"
                    );
                };
                let parsed = Upstream::parse(target.trim())?;
                parsed.ensure_local_boundary("--route")?;
                let mut added = 0usize;
                for svc in svcs.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    self.routes.insert(svc.to_ascii_lowercase(), parsed.clone());
                    added += 1;
                }
                if added == 0 {
                    bail!("--route `{raw}` must name at least one service before `=`");
                }
            }
            "--allow-docker" => self.allow_docker = true,
            "--emulator-ca" => self.emulator_ca = Some(PathBuf::from(value()?)),
            "--trace" => self.trace = Some(PathBuf::from(value()?)),
            "--max-body" => self.max_body = capture::parse_size(&value()?)?,
            "--enforce" => self.enforce = true,
            _ => return Ok(false),
        }
        Ok(true)
    }

    pub fn validate(&self) -> Result<()> {
        self.upstream.ensure_local_boundary("--upstream")?;
        if self.upstream.is_loopback() && self.upstream.port == self.listen && self.listen != 0 {
            bail!(
                "--listen {} and --upstream {} are the same port; the proxy would forward to itself",
                self.listen,
                self.upstream.port
            );
        }
        for (svc, route) in &self.routes {
            route.ensure_local_boundary("--route")?;
            if route.is_loopback() && route.port == self.listen && self.listen != 0 {
                bail!(
                    "--listen {} and --route {svc}={} are the same port; the proxy would forward to itself",
                    self.listen,
                    route.port
                );
            }
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

/// True only for loopback, link-local, RFC1918 (`10/8`, `172.16/12`, `192.168/16`),
/// or IPv6 ULA (`fc00::/7`) addresses: never public internet IPs.
fn is_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return mapped.is_loopback() || mapped.is_private() || mapped.is_link_local();
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Where the proxy sends requests: `PORT` means `https://127.0.0.1:PORT`.
#[derive(Clone, Debug)]
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

    /// Enforce Lenslet's strict local boundary: never proxy traffic to real AWS
    /// (`*.amazonaws.com`, `*.amazonaws.com.cn`, `*.api.aws`) or public IPs.
    pub fn ensure_local_boundary(&self, flag: &str) -> Result<()> {
        let h = self.host.to_ascii_lowercase();
        if h.ends_with(".amazonaws.com")
            || h == "amazonaws.com"
            || h.ends_with(".amazonaws.com.cn")
            || h.ends_with(".api.aws")
        {
            bail!(
                "{flag} `{}` points at real AWS cloud; Lenslet only proxies to local emulators",
                self.base
            );
        }
        if let Ok(ip) = self.host.parse::<IpAddr>()
            && !is_local_ip(ip)
        {
            bail!(
                "{flag} `{}` is a public IP address; Lenslet only proxies to loopback or local container networks",
                self.base
            );
        }
        Ok(())
    }

    /// Auto-resolve a local emulator when `--upstream` was omitted:
    /// 1. Inherit a loopback `AWS_ENDPOINT_URL` from the parent environment.
    /// 2. If default `:9298` is not listening, probe LocalStack (`:4566`), Moto
    ///    (`:5000`), MinIO (`:9000`), DynamoDB Local (`:8000`), and ElasticMQ (`:9324`).
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
            ("http://127.0.0.1:9000", "/minio/health/live", "MinIO"),
            ("http://127.0.0.1:8000", "/", "DynamoDB Local"),
            ("http://127.0.0.1:9324", "/", "ElasticMQ"),
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

    /// Auto-discover standalone per-service emulators (MinIO `:9000` for `s3`,
    /// DynamoDB Local `:8000` for `dynamodb`, ElasticMQ `:9324` for `sqs`) when
    /// multiple standalone emulators are running side-by-side.
    pub async fn auto_discover_service_routes(
        listen_port: u16,
        primary_port: u16,
    ) -> BTreeMap<String, Upstream> {
        let mut routes = BTreeMap::new();
        for (svc, url, path) in [
            ("s3", "http://127.0.0.1:9000", "/minio/health/live"),
            ("dynamodb", "http://127.0.0.1:8000", "/"),
            ("sqs", "http://127.0.0.1:9324", "/"),
        ] {
            let Ok(candidate) = Upstream::parse(url) else {
                continue;
            };
            if candidate.port == primary_port || (listen_port != 0 && candidate.port == listen_port)
            {
                continue;
            }
            if candidate.verify_emulator_signature(path).await {
                routes.insert(svc.to_string(), candidate);
            }
        }
        routes
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
            || reply.contains("minio")
            || reply.contains("x-amz-request-id")
            || reply.contains("x-amzn-requestid")
            || reply.contains("missingauthenticationtoken")
            || reply.contains("elasticmq")
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
        .enable_http2()
        .build();
    Client::builder(TokioExecutor::new()).build(https)
}

/// A request body read ahead for inspection.
struct Collected {
    data: Bytes,
    /// The body's trailers, when it ended with some.
    trailers: Option<hyper::HeaderMap>,
    /// The body ended: nothing is left unread.
    complete: bool,
}

/// Read `body` until it ends or more than `limit` bytes have arrived, so a
/// large or streaming upload never exhausts proxy memory. The rest stays in
/// `body`, unread.
async fn collect_bounded<B>(body: &mut B, limit: usize) -> Result<Collected, String>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let cap_hint = body.size_hint().lower().min(limit as u64).min(64 * 1024) as usize;
    let mut buf = Vec::with_capacity(cap_hint);
    let mut trailers = None;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|err| err.into().to_string())?;
        match frame.into_data() {
            Ok(chunk) => {
                buf.extend_from_slice(&chunk);
                if buf.len() > limit {
                    return Ok(Collected {
                        data: Bytes::from(buf),
                        trailers: None,
                        complete: false,
                    });
                }
            }
            Err(frame) => {
                if let Ok(map) = frame.into_trailers() {
                    trailers = Some(map);
                }
            }
        }
    }
    Ok(Collected {
        data: Bytes::from(buf),
        trailers,
        complete: true,
    })
}

/// The bytes read ahead, then whatever of the body is still unread (or the
/// trailers it ended with): forwards a request exactly as it arrived.
struct ReadAhead<B> {
    data: Option<Bytes>,
    trailers: Option<hyper::HeaderMap>,
    rest: Option<B>,
}

impl<B> hyper::body::Body for ReadAhead<B>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        if let Some(data) = this.data.take().filter(|data| !data.is_empty()) {
            return std::task::Poll::Ready(Some(Ok(hyper::body::Frame::data(data))));
        }
        if let Some(rest) = this.rest.as_mut() {
            return std::pin::Pin::new(rest).poll_frame(cx);
        }
        std::task::Poll::Ready(
            this.trailers
                .take()
                .map(|trailers| Ok(hyper::body::Frame::trailers(trailers))),
        )
    }

    fn is_end_stream(&self) -> bool {
        self.data.as_ref().is_none_or(Bytes::is_empty)
            && self.trailers.is_none()
            && self.rest.as_ref().is_none_or(|rest| rest.is_end_stream())
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        let ahead = self.data.as_ref().map_or(0, Bytes::len) as u64;
        match self.rest.as_ref() {
            None => hyper::body::SizeHint::with_exact(ahead),
            Some(rest) => {
                let rest = rest.size_hint();
                let mut hint = hyper::body::SizeHint::new();
                hint.set_lower(ahead + rest.lower());
                if let Some(upper) = rest.upper() {
                    hint.set_upper(ahead + upper);
                }
                hint
            }
        }
    }
}

/// Remove hop-by-hop headers (and any the `Connection` header names): they
/// describe one connection, not the request or response, and are not
/// forwarded. The trace still records them as sent.
fn strip_hop_by_hop(headers: &mut hyper::HeaderMap) {
    let named: Vec<header::HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| header::HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-connection",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ] {
        headers.remove(name);
    }
}

async fn handle<B>(req: Request<B>, state: Arc<State>) -> Result<Response<Body>, Infallible>
where
    B: hyper::body::Body<Data = Bytes> + Send + Sync + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    handle_with_inspect_limit(req, state, MAX_INSPECT_BYTES).await
}

async fn handle_with_inspect_limit<B>(
    mut req: Request<B>,
    state: Arc<State>,
    inspect_limit: usize,
) -> Result<Response<Body>, Infallible>
where
    B: hyper::body::Body<Data = Bytes> + Send + Sync + Unpin + 'static,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if let Some(response) = lenslet_endpoint(&req, &state) {
        return Ok(response);
    }
    let started = Instant::now();
    let replay_of = req
        .headers_mut()
        .remove(REPLAY_HEADER)
        .and_then(|value| value.to_str().ok().map(str::to_string))
        .filter(|id| !id.is_empty());
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
        access_key_id,
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
        host: host.clone(),
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
        blocked_phase: None,
        replay_of,
        caller_role: None,
        target: None,
        request: Message {
            headers: request_headers,
            content_type: request_content.content_type.clone(),
            body: None,
        },
        response: Message::default(),
    };

    let (mut parts, mut body) = req.into_parts();
    strip_hop_by_hop(&mut parts.headers);
    // HTTP/2 `h2c` clients send `:authority` in the URI instead of a `Host`
    // header; populate `Host` so HTTP/1.1 emulators validate SigV4 signatures.
    if !parts.headers.contains_key(header::HOST)
        && !host.is_empty()
        && let Ok(hval) = header::HeaderValue::from_str(&host)
    {
        parts.headers.insert(header::HOST, hval);
    }
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

    // Does this call name its resource (or a role or key) in its body? Then
    // derivation reads up to `inspect_limit` of it, whatever `--max-body` is.
    let needs_body = access::needs_request_body(
        &service,
        advertised_operation.as_deref(),
        &trace.operation,
        &raw_path,
    );
    let mut full_body: Option<trace::BodyTrace> = None;
    let mut derive_capture: Option<Arc<Mutex<Capture>>> = None;
    let mut evaluation: Option<Evaluation> = None;
    // Active IAM enforcement (`--enforce`): deny unauthorized calls locally,
    // before they reach the emulator.
    let enforce_gate = state.trace.gate().filter(|_| state.enforce);
    let upstream_body: Body = if let Some(gate) = enforce_gate {
        if needs_body {
            let collected = match collect_bounded(&mut body, inspect_limit).await {
                Ok(collected) => collected,
                Err(detail) => return Ok(error_response(StatusCode::BAD_REQUEST, &detail)),
            };
            if let Ok(mut capture) = request_capture.lock() {
                capture.feed(&collected.data);
            }
            trace.request.body = snapshot(&request_capture, &request_content);
            let inspect = Mutex::new(Capture::with_limit(inspect_limit));
            if let Ok(mut capture) = inspect.lock() {
                capture.feed(&collected.data);
            }
            resolve_operation(
                &mut trace,
                advertised_operation.as_deref(),
                &inspect,
                &request_content,
            );
            // A body longer than the inspection limit is read as truncated:
            // what it names past the limit stands as `*`.
            full_body = snapshot(&inspect, &request_content).map(|mut body| {
                body.truncated |= !collected.complete;
                body
            });
            let checked = with_body(&mut trace, &mut full_body, |trace| {
                state.trace.evaluate(trace)
            });
            if let Some(denied) = checked.denied.first().cloned() {
                state.trace.start(&trace.id);
                return Ok(reject_blocked(
                    trace,
                    access_key_id.as_deref(),
                    checked,
                    &denied,
                    gate,
                    &state,
                    started,
                    BlockedPhase::Request,
                ));
            }
            evaluation = Some(checked);
            let rest = (!collected.complete).then(|| TeeBody::new(body, request_capture.clone()));
            ReadAhead {
                data: Some(collected.data),
                trailers: collected.trailers,
                rest,
            }
            .map_err(|error| std::io::Error::other(error.into()))
            .boxed()
        } else {
            let checked = state.trace.evaluate(&trace);
            if let Some(denied) = checked.denied.first().cloned() {
                if let Ok(Ok(drained)) =
                    tokio::time::timeout(DRAIN_TIME, collect_bounded(&mut body, DRAIN_BYTES)).await
                    && drained.complete
                {
                    if let Ok(mut capture) = request_capture.lock() {
                        capture.feed(&drained.data);
                    }
                    trace.request.body = snapshot(&request_capture, &request_content);
                }
                state.trace.start(&trace.id);
                return Ok(reject_blocked(
                    trace,
                    access_key_id.as_deref(),
                    checked,
                    &denied,
                    gate,
                    &state,
                    started,
                    BlockedPhase::Request,
                ));
            }
            evaluation = Some(checked);
            TeeBody::new(body, request_capture.clone())
                .map_err(|error| std::io::Error::other(error.into()))
                .boxed()
        }
    } else if needs_body && state.max_body < inspect_limit {
        let capture = Arc::new(Mutex::new(Capture::with_limit(inspect_limit)));
        derive_capture = Some(capture.clone());
        TeeBody::new(TeeBody::new(body, request_capture.clone()), capture)
            .map_err(|error| std::io::Error::other(error.into()))
            .boxed()
    } else {
        TeeBody::new(body, request_capture.clone())
            .map_err(|error| std::io::Error::other(error.into()))
            .boxed()
    };

    let target_base = state.upstream_for(&service).to_string();
    let upstream_uri = format!("{target_base}{forward_path}");
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
    // End-to-end headers (including Host and Authorization) are forwarded
    // verbatim so SigV4 signatures remain valid.
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
                state.trace.record_pending(&pending);
            }
        });
    }

    match state.client.request(upstream).await {
        Ok(response) => {
            let status = response.status();
            let mut headers = response.headers().clone();
            trace.status = status.as_u16();
            trace.response = Message {
                headers: trace::headers(&headers),
                content_type: capture::content(&headers).content_type,
                body: None,
            };
            let response_content = capture::content(&headers);
            strip_hop_by_hop(&mut headers);

            // Only an S3 response reveals more access (the SSE-KMS key of
            // the object read or written): check it before returning the
            // payload. The emulator has run the call by now.
            if let Some(gate) = enforce_gate
                && service == "s3"
            {
                let checked = with_body(&mut trace, &mut full_body, |trace| {
                    state.trace.evaluate(trace)
                });
                if let Some(denied) = checked.denied.first().cloned() {
                    finished.store(true, Ordering::SeqCst);
                    drop(response);
                    trace.request.body = snapshot(&request_capture, &request_content);
                    return Ok(reject_blocked(
                        trace,
                        access_key_id.as_deref(),
                        checked,
                        &denied,
                        gate,
                        &state,
                        started,
                        BlockedPhase::Response,
                    ));
                }
                evaluation = Some(checked);
            }

            let response_capture = Arc::new(Mutex::new(Capture::with_limit(state.max_body)));
            let pending = Pending {
                state: state.clone(),
                trace,
                access_key_id,
                started,
                finished,
                advertised_operation,
                request_capture,
                request_content,
                full_body,
                derive_capture,
                evaluation,
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
            trace.status = StatusCode::BAD_GATEWAY.as_u16();
            trace.ms = started.elapsed().as_millis() as u64;
            trace.problem = Some(problem::unreachable(&target_base, &detail));
            trace.error = Some(detail);
            trace.request.body = snapshot(&request_capture, &request_content);
            let operation_capture = derive_capture.as_deref().unwrap_or(&request_capture);
            resolve_operation(
                &mut trace,
                advertised_operation.as_deref(),
                operation_capture,
                &request_content,
            );
            let evaluation = evaluation.unwrap_or_else(|| {
                let mut full = derive_capture
                    .as_deref()
                    .and_then(|capture| snapshot(capture, &request_content));
                with_body(&mut trace, &mut full, |trace| state.trace.evaluate(trace))
            });
            state
                .trace
                .record_with_key(trace, access_key_id.as_deref(), evaluation);
            Ok(error_response(StatusCode::BAD_GATEWAY, "upstream error"))
        }
    }
}

/// `GET /_lenslet/events` and `GET /_lenslet/health`: Lenslet's own
/// endpoints on the proxy address. They never go upstream and are never
/// recorded.
fn lenslet_endpoint<B>(req: &Request<B>, state: &Arc<State>) -> Option<Response<Body>> {
    if req.method() != hyper::Method::GET {
        return None;
    }
    match req.uri().path() {
        "/_lenslet/health" => {
            let (calls, errors, blocked) = state.trace.live_counts();
            let body = serde_json::json!({
                "run": state.trace.run(),
                "pid": std::process::id(),
                "endpoint": state.endpoint,
                "upstream": state.upstream,
                "routes": state.routes,
                "enforce": state.enforce,
                "calls": calls,
                "errors": errors,
                "blocked": blocked,
            })
            .to_string();
            let mut response = Response::new(
                Full::new(Bytes::from(body))
                    .map_err(|never| match never {})
                    .boxed(),
            );
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
            Some(response)
        }
        "/_lenslet/events" => {
            let (sender, receiver) = tokio::sync::mpsc::channel(64);
            tokio::spawn(stream_events(state.trace.clone(), sender));
            let mut response = Response::new(ChannelBody(receiver).boxed());
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("text/event-stream"),
            );
            headers.insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-cache"),
            );
            Some(response)
        }
        _ => None,
    }
}

/// Server-sent events for one client: every line of the run file in order,
/// from the first, then each new one as it is written, as `data: <line>`;
/// the stream ends after the `end` line. The file is the one source of
/// truth, so the backlog and the live tail can neither miss nor repeat a
/// line: the sink signals after each write and the stream reads on from
/// where it was.
async fn stream_events(sink: Arc<TraceSink>, sender: tokio::sync::mpsc::Sender<Bytes>) {
    use tokio::io::AsyncReadExt as _;
    let mut written = sink.subscribe();
    let Ok(mut file) = tokio::fs::File::open(sink.path()).await else {
        return;
    };
    let mut partial: Vec<u8> = Vec::new();
    loop {
        // Mark the signal seen before reading: a write after this point
        // wakes the wait below.
        written.borrow_and_update();
        if file.read_to_end(&mut partial).await.is_err() {
            return;
        }
        let mut start = 0;
        while let Some(newline) = partial[start..].iter().position(|byte| *byte == b'\n') {
            let line = &partial[start..start + newline];
            start += newline + 1;
            let mut event = Vec::with_capacity(line.len() + 8);
            event.extend_from_slice(b"data: ");
            event.extend_from_slice(line);
            event.extend_from_slice(b"\n\n");
            if sender.send(Bytes::from(event)).await.is_err() {
                return;
            }
            if line.starts_with(br#"{"type":"end""#) {
                return;
            }
        }
        partial.drain(..start);
        tokio::select! {
            changed = written.changed() => {
                if changed.is_err() {
                    // The sink is gone: nothing more will be written.
                    return;
                }
            }
            () = tokio::time::sleep(KEEPALIVE) => {
                if sender.send(Bytes::from_static(b": keepalive\n\n")).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// A response body fed by a channel, one data frame per message.
struct ChannelBody(tokio::sync::mpsc::Receiver<Bytes>);

impl hyper::body::Body for ChannelBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
        self.get_mut()
            .0
            .poll_recv(cx)
            .map(|next| next.map(|data| Ok(hyper::body::Frame::data(data))))
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

/// Record a blocked call in the trace and return a protocol-accurate AWS
/// `AccessDenied` / `AccessDeniedException` response.
#[allow(clippy::too_many_arguments)]
fn reject_blocked(
    mut trace: Trace,
    access_key_id: Option<&str>,
    evaluation: Evaluation,
    denied: &Denial,
    gate: &Gate,
    state: &State,
    started: Instant,
    phase: BlockedPhase,
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
    trace.blocked_phase = Some(phase);
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
    state
        .trace
        .record_with_key(trace, access_key_id, evaluation);

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
    shutdown: watch::Sender<bool>,
    /// One accept loop per listener; each owns its connections.
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Server {
    /// Validate everything, bind loopback (IPv4 + best-effort IPv6 so
    /// `localhost` works on either stack), write the run header, and start
    /// serving in the background.
    pub async fn start(
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
        let bind_v4 = if options.allow_docker {
            IpAddr::from([0, 0, 0, 0])
        } else {
            IpAddr::from([127, 0, 0, 1])
        };
        let listener = TcpListener::bind((bind_v4, options.listen))
            .await
            .with_context(|| format!("bind {bind_v4}:{}", options.listen))?;
        let local = listener.local_addr().context("read the bound address")?;
        // Always advertise `127.0.0.1:PORT` for host processes; Docker containers
        // reach `0.0.0.0:PORT` via `host.docker.internal` or `172.17.0.1`.
        let addr = SocketAddr::new(IpAddr::from([127, 0, 0, 1]), local.port());
        let v6_listener = TcpListener::bind((IpAddr::V6(Ipv6Addr::LOCALHOST), addr.port()))
            .await
            .ok();
        let endpoint = format!("http://{addr}");
        header.endpoint = endpoint.clone();
        header.upstream = options.upstream.base.clone();
        let routes: BTreeMap<String, String> = options
            .routes
            .iter()
            .map(|(svc, up)| (svc.clone(), up.base.clone()))
            .collect();
        let trace = Arc::new(TraceSink::new(trace_path, &header, gate)?);
        let state = Arc::new(State {
            client: client(roots),
            upstream: options.upstream.base.clone(),
            routes,
            endpoint,
            max_body: options.max_body,
            enforce: options.enforce,
            trace,
        });
        let shutdown = watch::Sender::new(false);
        let tasks = std::iter::once(listener)
            .chain(v6_listener)
            .map(|listener| tokio::spawn(serve(listener, state.clone(), shutdown.subscribe())))
            .collect();
        Ok(Server {
            addr,
            state,
            shutdown,
            tasks,
        })
    }

    /// The endpoint SDKs should use.
    pub fn endpoint(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Give in-flight calls up to `grace` to finish, then stop accepting
    /// connections and close idle ones. Requests already being served go on
    /// (a call that finishes after the run is sealed is not recorded; a live
    /// event stream ends after the `end` line).
    pub async fn stop(&self, grace: Duration) {
        let deadline = Instant::now() + grace;
        while self.state.trace.unfinished() > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        self.shutdown.send_replace(true);
    }

    /// After [`Server::stop`]: wait up to `grace` for open connections to
    /// finish, then drop the rest.
    pub async fn join(self, grace: Duration) {
        let deadline = Instant::now() + grace;
        for mut task in self.tasks {
            let left = deadline.saturating_duration_since(Instant::now());
            if tokio::time::timeout(left, &mut task).await.is_err() {
                task.abort();
            }
        }
    }
}

/// Accept connections until shut down, then wait for the open ones (each
/// shut down gracefully). Dropping this future drops every connection.
async fn serve(listener: TcpListener, state: Arc<State>, mut shutdown: watch::Receiver<bool>) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, peer)) = accepted {
                    // Enforce strict local/RFC1918 peer boundary even under `--allow-docker`.
                    if !is_local_ip(peer.ip()) {
                        drop(stream);
                        continue;
                    }
                    connections.spawn(serve_connection(stream, state.clone(), shutdown.clone()));
                }
            }
            () = stopped(&mut shutdown) => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    drop(listener);
    while connections.join_next().await.is_some() {}
}

/// Resolves once the server is stopping (or gone).
async fn stopped(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stop| *stop).await;
}

async fn serve_connection(
    stream: TcpStream,
    state: Arc<State>,
    mut shutdown: watch::Receiver<bool>,
) {
    let service = service_fn(move |req| handle(req, state.clone()));
    let mut builder = conn_auto::Builder::new(TokioExecutor::new());
    builder
        .http1()
        .timer(TokioTimer::new())
        .header_read_timeout(HEADER_READ_TIMEOUT);
    builder.http2().timer(TokioTimer::new());
    let connection = builder.serve_connection(TokioIo::new(stream), service);
    tokio::pin!(connection);
    let result = tokio::select! {
        result = connection.as_mut() => result,
        () = stopped(&mut shutdown) => {
            connection.as_mut().graceful_shutdown();
            connection.await
        }
    };
    if let Err(error) = result {
        let msg = error.to_string();
        if !msg.contains("incomplete message") && !msg.contains("timed out") {
            eprintln!("lenslet: connection error: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Access;
    use crate::capture::MAX_CAPTURE_BYTES;
    use http_body_util::BodyExt;
    use hyper::{HeaderMap, body::Incoming};
    use std::collections::BTreeSet;
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
                            let mut response = Response::builder()
                                .status(StatusCode::OK)
                                .header("content-type", content_type)
                                .header("x-request-id", "upstream-1")
                                .header("connection", "keep-alive")
                                .header("keep-alive", "timeout=5");
                            if path.ends_with("/kms") {
                                response = response
                                    .header("x-amz-server-side-encryption", "aws:kms")
                                    .header(
                                        "x-amz-server-side-encryption-aws-kms-key-id",
                                        "arn:aws:kms:us-east-1:000000000000:key/vault",
                                    );
                            }
                            Ok::<_, Infallible>(
                                response.body(Full::new(Bytes::from(body))).unwrap(),
                            )
                        }
                    });
                    let _ = conn_auto::Builder::new(TokioExecutor::new())
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
            routes: BTreeMap::new(),
            endpoint: String::new(),
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
            routes: BTreeMap::new(),
            endpoint: String::new(),
            max_body: MAX_CAPTURE_BYTES,
            enforce: false,
            trace: Arc::new(
                TraceSink::new(
                    temp.trace(),
                    &RunHeader::default(),
                    Some(Gate::from_lock("lenslet.lock", locked)),
                )
                .unwrap(),
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
            routes: BTreeMap::new(),
            endpoint: String::new(),
            max_body: MAX_CAPTURE_BYTES,
            enforce: true,
            trace: Arc::new(
                TraceSink::new(
                    temp.trace(),
                    &RunHeader::default(),
                    Some(Gate::from_lock("lenslet.lock", locked)),
                )
                .unwrap(),
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
            routes: BTreeMap::new(),
            endpoint: String::new(),
            max_body: MAX_CAPTURE_BYTES,
            enforce: true,
            trace: Arc::new(
                TraceSink::new(temp.trace(), &RunHeader::default(), Some(policy.clone())).unwrap(),
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
            routes: BTreeMap::new(),
            endpoint: String::new(),
            max_body: 64,
            enforce: true,
            trace: Arc::new(
                TraceSink::new(
                    temp.trace(),
                    &RunHeader::default(),
                    Some(Gate::from_lock("lenslet.lock", locked)),
                )
                .unwrap(),
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
        // The lock grants the table the body names, not the `*` its unread
        // tail stands for.
        let admin = Gate::from_lock(
            "lenslet.lock",
            [Access::new(
                "dynamodb:BatchWriteItem",
                "arn:aws:dynamodb:*:*:table/orders",
            )]
            .into(),
        );
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let state = Arc::new(State {
            client: client(RootCertStore::empty()),
            upstream: format!("http://127.0.0.1:{port}"),
            routes: BTreeMap::new(),
            endpoint: String::new(),
            max_body: 64,
            enforce: true,
            trace: Arc::new(
                TraceSink::new(temp.trace(), &RunHeader::default(), Some(admin)).unwrap(),
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

    fn lock_state(upstream: u16, trace: PathBuf, locked: &[Access], enforce: bool) -> Arc<State> {
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let gate = Gate::from_lock("lenslet.lock", locked.iter().cloned().collect());
        Arc::new(State {
            client: client(RootCertStore::empty()),
            upstream: format!("http://127.0.0.1:{upstream}"),
            routes: BTreeMap::new(),
            endpoint: String::new(),
            max_body: MAX_CAPTURE_BYTES,
            enforce,
            trace: Arc::new(TraceSink::new(trace, &RunHeader::default(), Some(gate)).unwrap()),
        })
    }

    #[tokio::test]
    async fn hop_by_hop_headers_are_recorded_but_not_forwarded() {
        let (port, mut upstream) = spawn_upstream().await;
        let temp = TempDir::new("hop");
        let state = state(&format!("http://127.0.0.1:{port}"), temp.trace());
        let mut request = sdk_request("GET", "/bucket/key?x-id=GetObject", b"");
        for (name, value) in [
            ("connection", "keep-alive, x-per-hop"),
            ("x-per-hop", "1"),
            ("keep-alive", "timeout=5"),
            ("te", "trailers"),
            ("proxy-authorization", "Basic eA=="),
            ("upgrade", "h2c"),
        ] {
            request.headers_mut().insert(
                header::HeaderName::from_static(name),
                header::HeaderValue::from_static(value),
            );
        }
        let response = handle(request, state.clone()).await.unwrap();
        for name in ["connection", "keep-alive"] {
            assert!(
                !response.headers().contains_key(name),
                "{name} is not passed back to the client"
            );
        }
        let _ = response.into_body().collect().await.unwrap();
        let (forwarded, _) = upstream.recv().await.unwrap();
        for name in [
            "connection",
            "x-per-hop",
            "keep-alive",
            "te",
            "proxy-authorization",
            "upgrade",
        ] {
            assert!(!forwarded.contains_key(name), "{name} is not forwarded");
        }
        assert!(forwarded.contains_key("authorization"));
        let written = traces(&temp.trace());
        let sent: Vec<&str> = written[0]
            .request
            .headers
            .iter()
            .map(|header| header.name.as_str())
            .collect();
        assert!(
            sent.contains(&"x-per-hop") && sent.contains(&"te"),
            "{sent:?}"
        );
        let received: Vec<&str> = written[0]
            .response
            .headers
            .iter()
            .map(|header| header.name.as_str())
            .collect();
        assert!(received.contains(&"keep-alive"), "{received:?}");
    }

    #[tokio::test]
    async fn a_replayed_call_is_recorded_with_what_it_replays() {
        let (port, mut upstream) = spawn_upstream().await;
        let temp = TempDir::new("replay");
        let state = state(&format!("http://127.0.0.1:{port}"), temp.trace());
        let mut request = sdk_request("GET", "/bucket/key?x-id=GetObject", b"");
        request
            .headers_mut()
            .insert(REPLAY_HEADER, header::HeaderValue::from_static("18f-2"));
        let response = handle(request, state.clone()).await.unwrap();
        let _ = response.into_body().collect().await.unwrap();
        let (forwarded, _) = upstream.recv().await.unwrap();
        assert!(
            !forwarded.contains_key(REPLAY_HEADER),
            "stripped before forwarding"
        );
        let written = traces(&temp.trace());
        assert_eq!(written[0].replay_of.as_deref(), Some("18f-2"));
        assert!(
            !written[0]
                .request
                .headers
                .iter()
                .any(|header| header.name == REPLAY_HEADER),
            "the marker is not one of the call's headers"
        );
        assert_eq!(written[0].access[0].action, "s3:GetObject");
    }

    #[tokio::test]
    async fn a_blocked_call_drains_its_body_and_records_the_request_phase() {
        let (port, mut upstream) = spawn_upstream().await;
        let temp = TempDir::new("drain");
        let state = lock_state(port, temp.trace(), &[], true);
        let response = handle(
            sdk_request("PUT", "/bucket/key?x-id=PutObject", b"payload"),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert!(upstream.try_recv().is_err());
        let written = traces(&temp.trace());
        assert!(written[0].blocked);
        assert_eq!(written[0].blocked_phase, Some(BlockedPhase::Request));
        assert_eq!(
            written[0]
                .request
                .body
                .as_ref()
                .and_then(|body| body.text.as_deref()),
            Some("payload"),
            "the body was read before answering"
        );
    }

    #[tokio::test]
    async fn a_key_only_the_response_reveals_blocks_after_the_emulator_ran() {
        let (port, mut upstream) = spawn_upstream().await;
        let temp = TempDir::new("kms");
        let state = lock_state(
            port,
            temp.trace(),
            &[Access::new("s3:GetObject", "arn:aws:s3:::vault/*")],
            true,
        );
        let response = handle(
            sdk_request("GET", "/vault/kms?x-id=GetObject", b""),
            state.clone(),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let _ = response.into_body().collect().await.unwrap();
        assert!(upstream.try_recv().is_ok(), "the emulator saw the call");
        let written = traces(&temp.trace());
        assert!(written[0].blocked);
        assert_eq!(written[0].blocked_phase, Some(BlockedPhase::Response));
        assert_eq!(written[0].implied[0].action, "kms:Decrypt");
        assert!(written[0].implied[0].new);
        assert!(!written[0].access[0].new);
    }

    /// Send one raw HTTP/1.1 request and read until the server closes.
    async fn raw_get(addr: SocketAddr, path: &str) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut reply = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut reply))
            .await
            .expect("the stream ends")
            .unwrap();
        String::from_utf8_lossy(&reply).into_owned()
    }

    #[tokio::test]
    async fn live_events_replay_the_file_then_follow_it_until_the_end() {
        let (port, _upstream) = spawn_upstream().await;
        let temp = TempDir::new("events");
        fs::create_dir_all(&temp.0).unwrap();
        let mut options = Options::new(&temp.0, 0);
        options.upstream = Upstream::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let header = RunHeader {
            run: "20261119-120000-1".into(),
            ..RunHeader::default()
        };
        let server = Server::start(&options, temp.trace(), header, None)
            .await
            .unwrap();
        let addr = server.addr;

        // Health answers on the proxy address, outside the trace.
        let mut health = TcpStream::connect(addr).await.unwrap();
        health
            .write_all(
                format!(
                    "GET /_lenslet/health HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut reply = String::new();
        health.read_to_string(&mut reply).await.unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let json: serde_json::Value =
            serde_json::from_str(reply.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(json["run"], "20261119-120000-1");
        assert_eq!(json["pid"], std::process::id());

        let plain: Client<HttpConnector, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).build_http();
        let call = |path: &str| {
            Request::builder()
                .uri(format!("http://{addr}{path}"))
                .header(
                    "authorization",
                    "AWS4-HMAC-SHA256 Credential=AKID/20261115/us-east-1/s3/aws4_request, Signature=x",
                )
                .body(Full::new(Bytes::new()))
                .unwrap()
        };
        let response = plain
            .request(call("/bucket/one?x-id=GetObject"))
            .await
            .unwrap();
        let _ = response.into_body().collect().await.unwrap();

        // A subscriber that joins mid-run gets the backlog, then the rest.
        let events = tokio::spawn(raw_get(addr, "/_lenslet/events"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        let response = plain
            .request(call("/bucket/two?x-id=GetObject"))
            .await
            .unwrap();
        let _ = response.into_body().collect().await.unwrap();
        // A slow call still running when the run ends is not recorded.
        let slow = tokio::spawn(plain.request(call("/bucket/slow?x-id=GetObject")));
        tokio::time::sleep(Duration::from_millis(100)).await;

        let state = server.state.clone();
        server.stop(Duration::from_millis(50)).await;
        let (summary, _) = state.trace.seal();
        assert_eq!((summary.calls, summary.unfinished), (2, 1));
        state.trace.end(&crate::summary::Report {
            unfinished: summary.unfinished,
            ..Default::default()
        });
        server.join(Duration::from_secs(5)).await;
        let _ = slow.await;

        let stream = events.await.unwrap();
        assert!(stream.starts_with("HTTP/1.1 200"), "{stream}");
        assert!(
            stream.contains("content-type: text/event-stream"),
            "{stream}"
        );
        let file = fs::read_to_string(temp.trace()).unwrap();
        let lines: Vec<&str> = file.lines().collect();
        let kinds: Vec<String> = lines
            .iter()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(kinds, ["run", "call", "call", "end"], "nothing after `end`");
        // Each line, exactly once and in order, as one event.
        let mut at = 0;
        for line in &lines {
            let event = format!("data: {line}\n\n");
            let found = stream[at..].find(&event).expect("every line is an event");
            at += found + event.len();
        }
        assert_eq!(stream.matches("data: ").count(), lines.len());
        assert!(
            !stream.contains("/_lenslet/"),
            "Lenslet's own requests are not calls"
        );
    }

    #[tokio::test]
    async fn derivation_reads_past_max_body_with_or_without_a_gate() {
        let (port, _upstream) = spawn_upstream().await;
        let temp = TempDir::new("derive");
        rustls::crypto::ring::default_provider()
            .install_default()
            .ok();
        let state = Arc::new(State {
            client: client(RootCertStore::empty()),
            upstream: format!("http://127.0.0.1:{port}"),
            routes: BTreeMap::new(),
            endpoint: String::new(),
            max_body: 64,
            enforce: false,
            trace: Arc::new(TraceSink::new(temp.trace(), &RunHeader::default(), None).unwrap()),
        });
        let payload = br#"{"RequestItems":{"orders":[{"PutRequest":{"Item":{"id":{"S":"order-padding-0000000000000000000000000000"}}}}],"secret_orders":[{"PutRequest":{"Item":{"id":{"S":"2"}}}}]}}"#;
        let request = Request::builder()
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
        let response = handle(request, state.clone()).await.unwrap();
        let _ = response.into_body().collect().await.unwrap();
        let written = traces(&temp.trace());
        assert!(written[0].request.body.as_ref().unwrap().truncated);
        let resources: Vec<&str> = written[0]
            .access
            .iter()
            .map(|item| item.resource.as_str())
            .collect();
        // The same footprint `--update` and `--check` see: no `*` for a tail
        // the trace capture cut off.
        assert_eq!(
            resources,
            [
                "arn:aws:dynamodb:*:*:table/orders",
                "arn:aws:dynamodb:*:*:table/secret_orders"
            ]
        );
    }

    #[tokio::test]
    async fn an_unreadable_body_is_enforced_against_the_wildcard_lock_line() {
        let (port, mut upstream) = spawn_upstream().await;
        let cbor = |state: &Arc<State>| {
            let request = Request::builder()
                .method("POST")
                .uri("/")
                .header("host", "127.0.0.1:9297")
                .header(
                    "authorization",
                    "AWS4-HMAC-SHA256 Credential=AKID/20261115/us-east-1/sqs/aws4_request, SignedHeaders=host, Signature=x",
                )
                .header("x-amz-target", "AmazonSQS.SendMessage")
                .header("content-type", "application/cbor")
                .body(Full::new(Bytes::from_static(&[0xa2, 0x00, 0xff, 0x13, 0x88])))
                .unwrap();
            handle(request, state.clone())
        };

        let temp = TempDir::new("unverified-blocked");
        let state = lock_state(
            port,
            temp.trace(),
            &[Access::new("sqs:SendMessage", "arn:aws:sqs:*:*:jobs")],
            true,
        );
        let response = cbor(&state).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert!(
            String::from_utf8_lossy(&body).contains("could not be read from the request body"),
            "{}",
            String::from_utf8_lossy(&body)
        );
        assert!(upstream.try_recv().is_err());
        let written = traces(&temp.trace());
        assert_eq!(written[0].access.len(), 1);
        assert_eq!(written[0].access[0].resource, "*");
        assert!(written[0].access[0].unverified && written[0].access[0].new);

        // Accepting the `*` line (what `--update` writes) lets it through.
        let temp = TempDir::new("unverified-allowed");
        let state = lock_state(
            port,
            temp.trace(),
            &[Access::new("sqs:SendMessage", "*")],
            true,
        );
        let response = cbor(&state).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let _ = response.into_body().collect().await.unwrap();
        assert!(upstream.try_recv().is_ok());
        let written = traces(&temp.trace());
        assert!(!written[0].new && written[0].access[0].unverified);
    }

    #[tokio::test]
    async fn routes_services_to_multiple_local_emulators_and_serves_http2_h2c() {
        // Strict local boundary rejects real AWS endpoints and public IPs.
        assert!(options(&["--upstream", "https://s3.amazonaws.com:443"]).is_err());
        assert!(options(&["--route", "s3=http://8.8.8.8:9000"]).is_err());
        assert!(options(&["--route", "s3=http://127.0.0.1:9000"]).is_ok());

        let (default_port, mut default_rx) = spawn_upstream().await;
        let (minio_port, mut minio_rx) = spawn_upstream().await;
        let temp = TempDir::new("routes-h2c");
        fs::create_dir_all(&temp.0).unwrap();
        let mut opts = Options::new(&temp.0, 0);
        opts.upstream = Upstream::parse(&format!("http://127.0.0.1:{default_port}")).unwrap();
        opts.routes.insert(
            "s3".into(),
            Upstream::parse(&format!("http://127.0.0.1:{minio_port}")).unwrap(),
        );
        let server = Server::start(&opts, temp.trace(), RunHeader::default(), None)
            .await
            .unwrap();
        let addr = server.addr;

        // Connect an explicit HTTP/2 `h2c` client to the Lenslet proxy.
        let tcp = TcpStream::connect(addr).await.unwrap();
        let (mut h2_sender, h2_conn) =
            hyper::client::conn::http2::handshake(TokioExecutor::new(), TokioIo::new(tcp))
                .await
                .unwrap();
        tokio::spawn(async move {
            let _ = h2_conn.await;
        });

        let s3_req = Request::builder()
            .method("PUT")
            .uri(format!("http://{addr}/assets/logo.svg?x-id=PutObject"))
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKID/20261120/us-east-1/s3/aws4_request, SignedHeaders=host, Signature=x",
            )
            .body(Full::new(Bytes::from_static(b"<svg/>")))
            .unwrap();
        let resp = h2_sender.send_request(s3_req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let _ = resp.into_body().collect().await.unwrap();
        assert!(
            minio_rx.try_recv().is_ok(),
            "S3 call routed to MinIO upstream over HTTP/2 h2c"
        );
        assert!(
            default_rx.try_recv().is_err(),
            "default upstream did not receive S3 call"
        );

        let sqs_req = Request::builder()
            .method("POST")
            .uri(format!("http://{addr}/"))
            .header(
                "authorization",
                "AWS4-HMAC-SHA256 Credential=AKID/20261120/us-east-1/sqs/aws4_request, SignedHeaders=host, Signature=x",
            )
            .header("x-amz-target", "AmazonSQS.SendMessage")
            .header("content-type", "application/x-amz-json-1.0")
            .body(Full::new(Bytes::from_static(
                br#"{"QueueUrl":"http://127.0.0.1:9297/000000000000/jobs","MessageBody":"ok"}"#,
            )))
            .unwrap();
        let resp2 = h2_sender.send_request(sqs_req).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);
        let _ = resp2.into_body().collect().await.unwrap();
        assert!(
            default_rx.try_recv().is_ok(),
            "SQS call routed to default upstream"
        );

        server.stop(Duration::from_millis(200)).await;
        server.join(Duration::from_secs(2)).await;

        let written = traces(&temp.trace());
        assert_eq!(written.len(), 2);
        assert_eq!(written[0].target.as_deref(), Some("s3://assets/logo.svg"));
        assert_eq!(written[1].target.as_deref(), Some("queue/jobs"));
    }
}
