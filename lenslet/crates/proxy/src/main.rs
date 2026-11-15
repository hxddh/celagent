//! Lenslet proxy: terminate TLS from an AWS SDK, preserve Host/SigV4, forward to
//! AWS Emulator, and append one JSONL trace entry per call for the GPUI app.
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

mod capture;
mod trace;
mod wire;

use std::{
    convert::Infallible,
    fs::{self, File, OpenOptions},
    io::{BufReader, ErrorKind, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
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
use rcgen::{CertifiedKey, generate_simple_self_signed};
use rustls::{RootCertStore, ServerConfig};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::{
    capture::{Capture, Content, MAX_CAPTURE_BYTES, TeeBody},
    trace::{Message, Trace, TraceSink},
};

type Body = BoxBody<Bytes, std::io::Error>;
type HttpsClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Body>;

#[derive(Clone)]
struct State {
    client: HttpsClient,
    /// Upstream base URL without a trailing slash, e.g. `https://127.0.0.1:9298`.
    upstream: String,
    trace: Arc<TraceSink>,
}

/// Everything needed to write one trace entry once the response body is done.
struct Pending {
    state: Arc<State>,
    trace: Trace,
    started: Instant,
    /// Operation advertised by the wire protocol; when `None`, the request body
    /// may still supply one (AWS Query `Action=...`).
    advertised_operation: Option<String>,
    request_capture: Arc<Mutex<Capture>>,
    request_content: Content,
    response_capture: Arc<Mutex<Capture>>,
    response_content: Content,
}

impl Pending {
    fn finish(mut self) {
        self.trace.ms = self.started.elapsed().as_millis();
        self.trace.request.body = snapshot(&self.request_capture, &self.request_content);
        self.trace.response.body = snapshot(&self.response_capture, &self.response_content);
        resolve_operation(
            &mut self.trace,
            self.advertised_operation.as_deref(),
            &self.request_capture,
            &self.request_content,
        );
        self.state.trace.record(&self.trace);
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
    let prefix = capture.prefix();
    let form = content.content_type.as_deref() == Some("application/x-www-form-urlencoded")
        || prefix.trim_ascii_start().starts_with(b"Action=");
    if !form {
        return None;
    }
    wire::form_action(prefix)
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
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}-{sequence:x}")
}

struct Args {
    listen: u16,
    upstream: Upstream,
    emulator_ca: PathBuf,
    cert: PathBuf,
    key: PathBuf,
    trace: PathBuf,
}

impl Args {
    fn validate(&self) -> Result<()> {
        if self.listen == 0 {
            bail!("--listen must be a port between 1 and 65535");
        }
        let loopback = matches!(
            self.upstream.host.as_str(),
            "127.0.0.1" | "localhost" | "::1"
        );
        if loopback && self.upstream.port == self.listen {
            bail!(
                "--listen {} and --upstream {} are the same port; the proxy would forward to itself",
                self.listen,
                self.upstream.port
            );
        }
        Ok(())
    }
}

/// Where the proxy sends requests: `PORT` means `https://127.0.0.1:PORT`.
struct Upstream {
    /// Base URL without a trailing slash, e.g. `https://127.0.0.1:9298`.
    base: String,
    host: String,
    port: u16,
    tls: bool,
}

impl Upstream {
    fn parse(value: &str) -> Result<Self> {
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
            host: authority.host().to_string(),
            port,
            tls: scheme == "https",
        })
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn usage() -> String {
    format!(
        "lenslet-proxy {version}
Usage: lenslet-proxy [options]

Options:
  --listen PORT        HTTPS port for SDK clients (default 9297)
  --upstream ADDRESS   emulator port or http(s) URL (default 9298)
  --emulator-ca PATH   CA used to verify the emulator (default ~/.aws/emulator/9298/ca.pem)
  --cert PATH          proxy certificate (default ~/.lenslet/proxy-cert.pem, generated)
  --key PATH           proxy private key (default ~/.lenslet/proxy-key.pem, generated)
  --trace PATH         JSONL trace file (default ~/.lenslet/trace.jsonl)
  -V, --version        print the version
  -h, --help           print this help

Bodies are forwarded unchanged and unbuffered (SigV4 stays valid); the trace
keeps up to {kib} KiB of each text body, records binary bodies as size and
SHA-256 only, and is written owner-only (0600) on Unix.",
        version = env!("CARGO_PKG_VERSION"),
        kib = MAX_CAPTURE_BYTES / 1024,
    )
}

fn parse_args() -> Result<Option<Args>> {
    parse_args_from(std::env::args().skip(1), &home())
}

fn parse_args_from<I>(arguments: I, home: &Path) -> Result<Option<Args>>
where
    I: IntoIterator<Item = String>,
{
    let mut args = Args {
        listen: 9297,
        upstream: Upstream::parse("9298")?,
        emulator_ca: home.join(".aws/emulator/9298/ca.pem"),
        cert: home.join(".lenslet/proxy-cert.pem"),
        key: home.join(".lenslet/proxy-key.pem"),
        trace: home.join(".lenslet/trace.jsonl"),
    };
    let mut it = arguments.into_iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} requires a value"));
        match arg.as_str() {
            "--listen" => {
                let raw = value()?;
                args.listen = raw
                    .parse::<u16>()
                    .with_context(|| format!("--listen expects a port number, got `{raw}`"))?;
            }
            "--upstream" => args.upstream = Upstream::parse(&value()?)?,
            "--emulator-ca" => args.emulator_ca = PathBuf::from(value()?),
            "--cert" => args.cert = PathBuf::from(value()?),
            "--key" => args.key = PathBuf::from(value()?),
            "--trace" => args.trace = PathBuf::from(value()?),
            "-V" | "--version" => {
                println!("lenslet-proxy {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "-h" | "--help" => {
                println!("{}", usage());
                return Ok(None);
            }
            other => bail!("unknown option `{other}`; try --help"),
        }
    }
    args.validate()?;
    Ok(Some(args))
}

/// Set a filesystem mode, ignoring platforms without POSIX permissions.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn write_file(path: &Path, contents: &[u8], private: bool) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if private { 0o600 } else { 0o644 });
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("write {}", path.display()))?;
    file.write_all(contents)
        .with_context(|| format!("write {}", path.display()))?;
    file.sync_all().ok();
    // A pre-existing file keeps its old mode, so set it explicitly.
    set_mode(path, if private { 0o600 } else { 0o644 })
}

fn ensure_cert(cert: &Path, key: &Path) -> Result<()> {
    match (cert.exists(), key.exists()) {
        (true, true) => {
            // Never leak the signing key, even if it was created by hand.
            set_mode(key, 0o600)?;
            return Ok(());
        }
        (false, false) => {}
        _ => bail!(
            "{} and {} must both exist; delete both to regenerate the proxy certificate",
            cert.display(),
            key.display()
        ),
    }
    for path in [cert, key] {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("create directory {}", parent.display()))?;
                set_mode(parent, 0o700)?;
            }
        }
    }
    let CertifiedKey {
        cert: generated,
        signing_key,
    } = generate_simple_self_signed(vec!["127.0.0.1".to_string()])
        .context("generate proxy certificate")?;
    write_file(key, signing_key.serialize_pem().as_bytes(), true)?;
    write_file(cert, generated.pem().as_bytes(), false)?;
    println!("generated proxy certificate: {}", cert.display());
    Ok(())
}

fn server_config(cert: &Path, key: &Path) -> Result<Arc<ServerConfig>> {
    let certs = rustls_pemfile::certs(&mut BufReader::new(
        File::open(cert).with_context(|| format!("open certificate {}", cert.display()))?,
    ))
    .collect::<std::result::Result<Vec<_>, _>>()
    .with_context(|| format!("parse certificate {}", cert.display()))?;
    if certs.is_empty() {
        bail!("no certificates in {}", cert.display());
    }
    let key = rustls_pemfile::private_key(&mut BufReader::new(
        File::open(key).with_context(|| format!("open private key {}", key.display()))?,
    ))
    .with_context(|| format!("parse private key {}", key.display()))?
    .with_context(|| format!("no private key in {}", key.display()))?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("certificate and key do not form a TLS pair")?;
    Ok(Arc::new(config))
}

fn emulator_roots(path: &Path) -> Result<RootCertStore> {
    let pem = fs::read(path)
        .with_context(|| format!("read emulator CA {} (pass --emulator-ca)", path.display()))?;
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

async fn handle<B>(req: Request<B>, state: Arc<State>) -> Result<Response<Body>, Infallible>
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
        .unwrap_or("")
        .to_string();
    let authorization = req.headers().contains_key("authorization");
    let wire::Wire {
        service,
        operation: advertised_operation,
    } = wire::wire_info(req.headers(), &raw_path);
    // A protocol-derived operation, or the method/path fallback until (and
    // unless) the request body supplies an AWS Query `Action=...`.
    let operation = advertised_operation
        .clone()
        .unwrap_or_else(|| wire::fallback_operation(method.as_str(), &raw_path));

    let request_content = capture::content(req.headers());
    let request_headers = trace::headers(req.headers());
    let request_capture = Arc::new(Mutex::new(Capture::new(request_content.text)));

    let (parts, body) = req.into_parts();
    let upstream_uri = format!("{}{}", state.upstream, raw_path);
    let mut upstream = match Request::builder()
        .method(parts.method)
        .uri(upstream_uri)
        .body(
            TeeBody::new(body, request_capture.clone())
                .map_err(|error| std::io::Error::new(ErrorKind::Other, error))
                .boxed(),
        ) {
        Ok(request) => request,
        Err(error) => {
            return Ok(error_response(StatusCode::BAD_REQUEST, &error.to_string()));
        }
    };
    // Headers (including Host and Authorization) are forwarded verbatim so
    // SigV4 signatures remain valid.
    *upstream.headers_mut() = parts.headers;

    let trace = Trace {
        id: next_id(),
        at: Utc::now().to_rfc3339(),
        method: method.to_string(),
        path: raw_path.clone(),
        host,
        service,
        operation,
        status: 0,
        ms: 0,
        authorization,
        error: None,
        request: Message {
            headers: request_headers,
            content_type: request_content.content_type.clone(),
            body: None,
        },
        response: Message::default(),
    };

    match state.client.request(upstream).await {
        Ok(response) => {
            let status = response.status();
            let headers = response.headers().clone();
            let response_content = capture::content(&headers);
            let response_capture = Arc::new(Mutex::new(Capture::new(response_content.text)));
            let pending = Pending {
                state: state.clone(),
                trace: Trace {
                    status: status.as_u16(),
                    response: Message {
                        headers: trace::headers(&headers),
                        content_type: response_content.content_type.clone(),
                        body: None,
                    },
                    ..trace
                },
                started,
                advertised_operation,
                request_capture,
                request_content,
                response_capture: response_capture.clone(),
                response_content,
            };
            // The trace entry is written when the response body ends (or the
            // client goes away), so `ms` covers the whole exchange:
            let body = TeeBody::new(response.into_body(), response_capture)
                .on_end(move || pending.finish())
                .map_err(|error| std::io::Error::new(ErrorKind::Other, error))
                .boxed();
            let mut out = Response::new(body);
            *out.status_mut() = status;
            *out.headers_mut() = headers;
            Ok(out)
        }
        Err(error) => {
            let mut trace = Trace {
                status: StatusCode::BAD_GATEWAY.as_u16(),
                ms: started.elapsed().as_millis(),
                error: Some(error.to_string()),
                ..trace
            };
            trace.request.body = snapshot(&request_capture, &request_content);
            resolve_operation(
                &mut trace,
                advertised_operation.as_deref(),
                &request_capture,
                &request_content,
            );
            state.trace.record(&trace);
            eprintln!("upstream error: {error}");
            Ok(error_response(StatusCode::BAD_GATEWAY, "upstream error"))
        }
    }
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

#[tokio::main]
async fn main() -> Result<()> {
    // rustls needs a crypto provider before any TLS config is built.
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();

    let Some(args) = parse_args()? else {
        return Ok(());
    };
    // Read and validate everything before creating any files.
    let roots = if args.upstream.tls {
        emulator_roots(&args.emulator_ca)?
    } else {
        // A plain-HTTP upstream has no TLS certificate to trust.
        RootCertStore::empty()
    };
    let trace = Arc::new(TraceSink::new(args.trace.clone())?);
    ensure_cert(&args.cert, &args.key)?;
    let tls = TlsAcceptor::from(server_config(&args.cert, &args.key)?);
    let state = Arc::new(State {
        client: client(roots),
        upstream: args.upstream.base.clone(),
        trace,
    });

    let listener = TcpListener::bind((IpAddr::from([127, 0, 0, 1]), args.listen))
        .await
        .with_context(|| format!("bind https://127.0.0.1:{}", args.listen))?;
    println!("lenslet-proxy {}", env!("CARGO_PKG_VERSION"));
    println!("  listen    https://127.0.0.1:{}", args.listen);
    println!("  upstream  {}", args.upstream.base);
    println!(
        "  cert      {} (trust this in the SDK)",
        args.cert.display()
    );
    println!("  trace     {}", args.trace.display());
    if args.upstream.tls {
        println!("  emulator  CA {}", args.emulator_ca.display());
    }
    println!(
        "  capture   {} KiB per text body; binary bodies keep size + sha256 only",
        MAX_CAPTURE_BYTES / 1024
    );

    loop {
        let (stream, _) = listener.accept().await.context("accept connection")?;
        let tls = tls.clone();
        let state = state.clone();
        tokio::spawn(async move {
            match tls.accept(stream).await {
                Ok(stream) => {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| handle(req, state.clone()));
                    if let Err(error) = http1::Builder::new().serve_connection(io, service).await {
                        eprintln!("connection error: {error}");
                    }
                }
                Err(error) => eprintln!("tls accept error: {error}"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use hyper::{HeaderMap, body::Incoming};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    fn parse(arguments: &[&str]) -> Result<Option<Args>> {
        parse_args_from(
            arguments.iter().map(|argument| argument.to_string()),
            Path::new("/home/tester"),
        )
    }

    #[test]
    fn version_and_help_exit_without_arguments() {
        assert!(parse(&["--version"]).unwrap().is_none());
        assert!(parse(&["-V"]).unwrap().is_none());
        assert!(parse(&["--help"]).unwrap().is_none());
        assert!(usage().starts_with(&format!("lenslet-proxy {}", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn validates_ports_and_upstream_addresses() {
        let args = parse(&["--listen", "9300", "--upstream", "9400"])
            .unwrap()
            .unwrap();
        assert_eq!(args.listen, 9300);
        assert_eq!(args.upstream.base, "https://127.0.0.1:9400");
        assert!(parse(&["--listen", "0"]).is_err());
        assert!(parse(&["--listen", "not-a-port"]).is_err());
        assert!(parse(&["--listen", "9297", "--upstream", "9297"]).is_err());
        assert!(parse(&["--upstream", "0"]).is_err());
        assert!(parse(&["--upstream", "ftp://127.0.0.1:21"]).is_err());
        assert!(parse(&["--upstream", "https://127.0.0.1"]).is_err());
        assert!(parse(&["--listen"]).is_err());
        assert!(parse(&["--nope"]).is_err());

        let plain = parse(&["--upstream", "http://127.0.0.1:8080"])
            .unwrap()
            .unwrap();
        assert_eq!(plain.upstream.base, "http://127.0.0.1:8080");
        assert_eq!(plain.upstream.port, 8080);
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "lenslet-proxy-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
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
                            let big = parts.uri.path().ends_with("/big");
                            let received = body
                                .collect()
                                .await
                                .map(|collected| collected.to_bytes())
                                .unwrap_or_default();
                            let _ = tx.send((parts.headers, received)).await;
                            let (content_type, body) = if big {
                                ("text/plain", "x".repeat(100 * 1024))
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
            trace: Arc::new(TraceSink::new(trace).unwrap()),
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
            .map(|line| serde_json::from_str(line).expect("trace line is a Trace"))
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
        assert_eq!(serialized.lines().count(), 1, "one line per call");

        // A 100 KiB response keeps forwarding everything and captures a prefix.
        let request = sdk_request("GET", "/bucket/big", b"");
        let response = handle(request, state.clone()).await.unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.len(), 100 * 1024, "large responses are not truncated");

        let written = traces(&trace_path);
        assert_eq!(written.len(), 2);
        let big = written[1].response.body.as_ref().expect("big body traced");
        assert!(big.truncated, "capture is bounded");
        assert!(!big.binary);
        assert_eq!(big.bytes, 100 * 1024);
        let text = big.text.as_deref().unwrap();
        assert_eq!(text.len(), MAX_CAPTURE_BYTES);
        assert!(text.bytes().all(|byte| byte == b'x'));
        assert_eq!(big.sha256, sha256_hex(&vec![b'x'; 100 * 1024]));
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

    #[test]
    fn cert_and_key_must_agree() {
        let temp = TempDir::new("cert");
        fs::create_dir_all(&temp.0).unwrap();
        let cert = temp.0.join("cert.pem");
        let key = temp.0.join("key.pem");
        ensure_cert(&cert, &key).unwrap();
        assert!(cert.exists() && key.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "private key is not readable by others");
        }
        // A usable pair builds a TLS config, and an existing cert is reused
        // instead of being regenerated behind the SDK's trust store.
        server_config(&cert, &key).unwrap();
        let trusted = fs::read(&cert).unwrap();
        ensure_cert(&cert, &key).unwrap();
        assert_eq!(fs::read(&cert).unwrap(), trusted);

        // A half-present pair must not silently replace the trusted cert.
        fs::remove_file(&key).unwrap();
        assert!(ensure_cert(&cert, &key).is_err());
        // A cert without a usable key is an error, not a panic.
        fs::write(&cert, "not a certificate").unwrap();
        assert!(server_config(&cert, &temp.0.join("missing.pem")).is_err());
        assert!(emulator_roots(&temp.0.join("missing-ca.pem")).is_err());
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}
