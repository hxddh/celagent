//! Loopback-only HTTP/1.1 & HTTPS request replayer for the Inspector.
//!
//! While a run is live, a replay goes through the run's own proxy with
//! `x-lenslet-replay-of: <call id>`, so the proxy records it (and gates it,
//! under `--enforce`) like any other call. For a finished run it goes straight
//! to the emulator and is only shown, never recorded. Targets are strictly
//! loopback (`127.0.0.1`, `localhost`, `[::1]`).

use std::{
    fs,
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};
use rustls_pki_types::ServerName;

use crate::model::{Header, Trace};

/// The header that tells the proxy a request replays a recorded call.
pub const REPLAY_OF_HEADER: &str = "x-lenslet-replay-of";

const REPLAY_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REPLAY_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub enum ReplayState {
    Running,
    Done(ReplayOutcome),
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct ReplayOutcome {
    pub status: u16,
    pub ms: u64,
    pub headers: Vec<Header>,
    pub content_type: Option<String>,
    pub body_text: Option<String>,
    pub body_bytes: usize,
    pub raw_bytes: Vec<u8>,
}

/// Whether `trace` can be faithfully replayed against `upstream_url`.
pub fn can_replay(trace: &Trace, upstream_url: Option<&str>) -> bool {
    if trace.pending {
        return false;
    }
    let Some(url) = upstream_url else {
        return false;
    };
    if parse_loopback_target(url).is_none() {
        return false;
    }
    if let Some(body) = trace.request.as_ref().and_then(|r| r.body.as_ref()) {
        let has_lossless_wire = body.raw_b64.is_some() || body.blob.is_some();
        if has_lossless_wire {
            return true;
        }
        if body.binary || body.truncated {
            return false;
        }
        if body
            .encoding
            .as_deref()
            .is_some_and(|e| e.contains("aws-eventstream"))
        {
            return false;
        }
        if body.bytes > 0 && body.text.is_none() {
            return false;
        }
    }
    true
}

#[derive(Clone, Debug)]
struct LoopbackTarget {
    tls: bool,
    host: String,
    port: u16,
}

fn parse_loopback_target(url: &str) -> Option<LoopbackTarget> {
    let trimmed = url.trim();
    let (tls, rest) = if let Some(r) = trimmed.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("http://") {
        (false, r)
    } else if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()) {
        let port: u16 = trimmed.parse().ok().filter(|p| *p > 0)?;
        return Some(LoopbackTarget {
            tls: true,
            host: "127.0.0.1".into(),
            port,
        });
    } else {
        return None;
    };

    let authority = rest.split('/').next().unwrap_or(rest);
    let (host, port_str) = if let Some(after_bracket) = authority.strip_prefix('[') {
        let (ipv6, tail) = after_bracket.split_once(']')?;
        let port = tail.strip_prefix(':')?;
        (ipv6, port)
    } else {
        authority.rsplit_once(':')?
    };
    let port: u16 = port_str.parse().ok().filter(|p| *p > 0)?;
    let clean_host = host.trim();
    // Strictly enforce loopback-only targets so captured headers/tokens never leave localhost.
    if !matches!(clean_host, "127.0.0.1" | "localhost" | "::1") {
        return None;
    }
    Some(LoopbackTarget {
        tls,
        host: clean_host.to_string(),
        port,
    })
}

/// Where a replay goes and what it adds.
#[derive(Clone, Debug)]
pub struct ReplayTarget {
    pub url: String,
    /// Through the run's live proxy (adds [`REPLAY_OF_HEADER`]).
    pub via_proxy: bool,
}

/// An interactive request edited in the Request Composer.
#[derive(Clone, Debug)]
pub struct ComposedRequest {
    pub call_id: Option<String>,
    pub method: String,
    pub path: String,
    pub host: String,
    pub service: String,
    pub region: String,
    pub encoding: Option<String>,
    pub headers: Vec<Header>,
    pub body: String,
    pub resign_sigv4: bool,
}

#[cfg(test)]
pub fn replay_call(
    trace: &Trace,
    replay_target: &ReplayTarget,
    home: &Path,
) -> Result<ReplayOutcome, String> {
    replay_call_with_trace_path(trace, replay_target, home, None)
}

pub fn replay_call_with_trace_path(
    trace: &Trace,
    replay_target: &ReplayTarget,
    home: &Path,
    trace_path: Option<&Path>,
) -> Result<ReplayOutcome, String> {
    crate::fsguard::assert_io_allowed();
    let upstream_url = replay_target.url.as_str();
    let extra: Vec<(&str, &str)> = if replay_target.via_proxy {
        vec![(REPLAY_OF_HEADER, trace.id.as_str())]
    } else {
        Vec::new()
    };
    let target = parse_loopback_target(upstream_url).ok_or_else(|| {
        format!("Replay refused: `{upstream_url}` is not a loopback emulator endpoint")
    })?;

    let req_wire = build_http_request(trace, &target, &extra, trace_path)?;
    let is_head = trace.method.eq_ignore_ascii_case("HEAD");
    send_wire(&target, &req_wire, is_head, home)
}

pub fn send_composed(
    req: &ComposedRequest,
    replay_target: &ReplayTarget,
    home: &Path,
) -> Result<ReplayOutcome, String> {
    crate::fsguard::assert_io_allowed();
    let upstream_url = replay_target.url.as_str();
    let target = parse_loopback_target(upstream_url).ok_or_else(|| {
        format!("Replay refused: `{upstream_url}` is not a loopback emulator endpoint")
    })?;

    let method = req.method.trim().to_ascii_uppercase();
    if method.is_empty() || method.bytes().any(|b| !b.is_ascii_alphabetic()) {
        return Err(format!("Invalid HTTP method `{method}`"));
    }
    let path = if req.path.trim().is_empty() {
        "/".to_string()
    } else if req.path.trim().starts_with('/') {
        req.path.trim().replace(['\r', '\n'], "")
    } else {
        format!("/{}", req.path.trim().replace(['\r', '\n'], ""))
    };
    let host_val = if req.host.trim().is_empty() {
        format!("{}:{}", target.host, target.port)
    } else {
        req.host.trim().replace(['\r', '\n'], "")
    };

    let decoded_wire = req.encoding.is_some();
    let mut clean_headers: Vec<Header> = Vec::with_capacity(req.headers.len() + 2);
    let mut saw_host = false;
    for h in &req.headers {
        let low = h.name.trim().to_ascii_lowercase();
        if low.is_empty()
            || matches!(
                low.as_str(),
                "content-length" | "transfer-encoding" | "connection" | "expect"
            )
            || low.eq_ignore_ascii_case(REPLAY_OF_HEADER)
        {
            continue;
        }
        if decoded_wire
            && (matches!(
                low.as_str(),
                "content-encoding"
                    | "x-amz-decoded-content-length"
                    | "x-amz-trailer"
                    | "x-amz-sdk-checksum-algorithm"
                    | "content-md5"
            ) || low.starts_with("x-amz-checksum-"))
        {
            continue;
        }
        if low == "host" {
            saw_host = true;
        }
        let val = if !req.resign_sigv4
            && decoded_wire
            && low == "x-amz-content-sha256"
            && h.value.trim().starts_with("STREAMING-")
        {
            "UNSIGNED-PAYLOAD".to_string()
        } else {
            h.value.replace(['\r', '\n'], " ")
        };
        clean_headers.push(Header {
            name: h.name.trim().replace(['\r', '\n'], ""),
            value: val,
        });
    }
    if !saw_host {
        clean_headers.insert(
            0,
            Header {
                name: "host".into(),
                value: host_val.clone(),
            },
        );
    }

    let cbor_wire = is_cbor_wire(req.encoding.as_deref(), &clean_headers);
    let wire_body = encode_wire_body(&req.body, cbor_wire)?;

    if req.resign_sigv4 {
        let svc = if req.service.trim().is_empty() {
            "s3"
        } else {
            req.service.trim()
        };
        let reg = if req.region.trim().is_empty() {
            "us-east-1"
        } else {
            req.region.trim()
        };
        let existing_ak = clean_headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("authorization"))
            .and_then(|h| crate::model::decode_sigv4_header(&h.value))
            .map(|d| d.access_key_id)
            .filter(|k| !k.is_empty());
        let (ak, sk, tok) = resolve_local_signing_creds(existing_ak.as_deref(), target.port, home);
        if tok.is_some() || existing_ak.as_deref() != Some(ak.as_str()) {
            clean_headers.retain(|h| !h.name.eq_ignore_ascii_case("x-amz-security-token"));
        }
        crate::sigv4::sign_headers(
            &mut clean_headers,
            &wire_body,
            &crate::sigv4::SignParams {
                method: &method,
                path_and_query: &path,
                host: &host_val,
                service: svc,
                region: reg,
                access_key: &ak,
                secret_key: &sk,
                session_token: tok.as_deref(),
            },
        );
    }
    let final_headers = clean_headers;

    let mut wire = Vec::with_capacity(512 + wire_body.len());
    wire.extend_from_slice(format!("{method} {path} HTTP/1.1\r\n").as_bytes());
    for h in &final_headers {
        wire.extend_from_slice(format!("{}: {}\r\n", h.name, h.value).as_bytes());
    }
    if replay_target.via_proxy
        && let Some(id) = req.call_id.as_deref()
    {
        let clean = id.replace(['\r', '\n'], " ");
        wire.extend_from_slice(format!("{REPLAY_OF_HEADER}: {clean}\r\n").as_bytes());
    }
    wire.extend_from_slice(format!("Content-Length: {}\r\n", wire_body.len()).as_bytes());
    wire.extend_from_slice(b"Connection: close\r\n\r\n");
    wire.extend_from_slice(&wire_body);

    let is_head = method.eq_ignore_ascii_case("HEAD");
    send_wire(&target, &wire, is_head, home)
}

fn is_cbor_wire(encoding: Option<&str>, headers: &[Header]) -> bool {
    if encoding.is_some_and(|e| e.to_ascii_lowercase().contains("cbor")) {
        return true;
    }
    headers.iter().any(|h| {
        (h.name.eq_ignore_ascii_case("content-type")
            || h.name.eq_ignore_ascii_case("smithy-protocol"))
            && h.value.to_ascii_lowercase().contains("cbor")
    })
}

fn encode_wire_body(text: &str, cbor: bool) -> Result<Vec<u8>, String> {
    if !cbor {
        return Ok(text.as_bytes().to_vec());
    }
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let val: serde_json::Value = serde_json::from_str(text)
        .map_err(|e| format!("CBOR request body must be valid JSON: {e}"))?;
    Ok(lenslet_core::cbor::encode_cbor(&val))
}

fn resolve_local_signing_creds(
    existing_ak: Option<&str>,
    port: u16,
    home: &Path,
) -> (String, String, Option<String>) {
    // 1. Check AWS Emulator credentials files (`~/.aws/emulator/<port>/emulator_credentials`),
    // matching `existing_ak` when present or consulting the target/default emulator port.
    let emu_root = home.join(".aws/emulator");
    let mut fallback_emu_creds = None;
    for candidate_port in [port, 9298] {
        let cred_file = emu_root
            .join(candidate_port.to_string())
            .join("emulator_credentials");
        if let Ok(text) = fs::read_to_string(&cred_file)
            && let Some(creds) = parse_ini_creds(&text)
        {
            if existing_ak.is_none_or(|ak| ak == creds.0) {
                return creds;
            }
            if fallback_emu_creds.is_none() {
                fallback_emu_creds = Some(creds);
            }
        }
    }
    if let Some(target_ak) = existing_ak
        && let Ok(entries) = fs::read_dir(&emu_root)
    {
        for entry in entries.flatten() {
            let cred_file = entry.path().join("emulator_credentials");
            if let Ok(text) = fs::read_to_string(&cred_file)
                && let Some(creds) = parse_ini_creds(&text)
                && creds.0 == target_ak
            {
                return creds;
            }
        }
    }

    // 2. Match well-known local placeholder / emulator credential pairs.
    match existing_ak {
        Some("lenslet-local") => ("lenslet-local".into(), "lenslet-local".into(), None),
        Some("test") | None => ("test".into(), "test".into(), None),
        Some("minioadmin") => ("minioadmin".into(), "minioadmin".into(), None),
        Some("dummy") => ("dummy".into(), "dummy".into(), None),
        Some("x") => ("x".into(), "x".into(), None),
        Some("AKIAIOSFODNN7EXAMPLE") => (
            "AKIAIOSFODNN7EXAMPLE".into(),
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            None,
        ),
        // 3. Unknown or ephemeral STS key without a local secret: use discovered
        // emulator credentials or fall back to a coherent `lenslet-local` pair.
        Some(_) => fallback_emu_creds
            .unwrap_or_else(|| ("lenslet-local".into(), "lenslet-local".into(), None)),
    }
}

fn parse_ini_creds(text: &str) -> Option<(String, String, Option<String>)> {
    let mut ak = None;
    let mut sk = None;
    let mut tok = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with(';')
            || line.starts_with('[')
        {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim().trim_matches(['"', '\'']).to_string();
            if val.is_empty() {
                continue;
            }
            match key.as_str() {
                "aws_access_key_id" if ak.is_none() => ak = Some(val),
                "aws_secret_access_key" if sk.is_none() => sk = Some(val),
                "aws_session_token" if tok.is_none() => tok = Some(val),
                _ => {}
            }
        }
    }
    Some((ak?, sk?, tok))
}

fn send_wire(
    target: &LoopbackTarget,
    req_wire: &[u8],
    is_head: bool,
    home: &Path,
) -> Result<ReplayOutcome, String> {
    let started = Instant::now();
    let raw_response = if target.tls {
        let stream = connect_loopback(&target.host, target.port)?;
        let tls_cfg = build_tls_config(target.port, home)?;
        let server_name =
            ServerName::try_from("localhost").map_err(|e| format!("TLS server name error: {e}"))?;
        let conn = ClientConnection::new(Arc::new(tls_cfg), server_name)
            .map_err(|e| format!("TLS handshake init failed: {e}"))?;
        let mut tls_stream = StreamOwned::new(conn, stream);
        tls_stream
            .write_all(req_wire)
            .map_err(|e| format!("TLS write failed: {e}"))?;
        tls_stream.flush().ok();
        read_bounded(&mut tls_stream, is_head)?
    } else {
        let mut stream = connect_loopback(&target.host, target.port)?;
        stream
            .write_all(req_wire)
            .map_err(|e| format!("HTTP write failed: {e}"))?;
        stream.flush().ok();
        read_bounded(&mut stream, is_head)?
    };

    let ms = started.elapsed().as_millis() as u64;
    parse_http_response(&raw_response, is_head, ms)
}

fn connect_loopback(host: &str, port: u16) -> Result<TcpStream, String> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("Cannot resolve {host}:{port}: {e}"))?
        .filter(|a| a.ip().is_loopback())
        .collect();
    let addr = addrs
        .first()
        .copied()
        .ok_or_else(|| format!("No loopback address for {host}:{port}"))?;
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2))
        .map_err(|e| format!("Emulator {host}:{port} unreachable: {e}"))?;
    let _ = stream.set_read_timeout(Some(REPLAY_TIMEOUT));
    let _ = stream.set_write_timeout(Some(REPLAY_TIMEOUT));
    Ok(stream)
}

fn build_tls_config(port: u16, home: &Path) -> Result<ClientConfig, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut roots = RootCertStore::empty();
    let emu_ca = home
        .join(".aws/emulator")
        .join(port.to_string())
        .join("ca.pem");
    if let Ok(pem_bytes) = fs::read(&emu_ca) {
        let mut reader = std::io::Cursor::new(pem_bytes);
        for cert in rustls_pemfile::certs(&mut reader).flatten() {
            let _ = roots.add(cert);
        }
    }
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    Ok(ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

fn build_http_request(
    trace: &Trace,
    target: &LoopbackTarget,
    extra: &[(&str, &str)],
    trace_path: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let path = if trace.path.starts_with('/') {
        trace.path.clone()
    } else if let Some((_, rest)) = trace.path.split_once("://") {
        match rest.find('/') {
            Some(idx) => rest[idx..].to_string(),
            None => "/".to_string(),
        }
    } else {
        format!("/{}", trace.path)
    };

    let exact_raw_wire = trace
        .request
        .as_ref()
        .and_then(|r| r.body.as_ref())
        .filter(|b| b.binary || b.truncated || b.encoding.is_some())
        .and_then(|b| b.raw_wire_bytes(trace_path));

    let body_encoding = trace
        .request
        .as_ref()
        .and_then(|r| r.body.as_ref())
        .and_then(|b| b.encoding.as_deref());
    // When `exact_raw_wire` is available (from `raw_b64` or sidecar `blob`), we
    // transmit the untouched compressed/binary/CBOR wire payload (preserving original
    // Content-Encoding & SigV4 body hash!), only unframing `aws-chunked` if needed.
    let using_raw_wire =
        exact_raw_wire.is_some() && !body_encoding.is_some_and(|e| e.contains("aws-chunked"));
    let decoded_wire = body_encoding.is_some() && !using_raw_wire;
    let compressed_wire = decoded_wire
        && body_encoding.is_some_and(|enc| {
            let low = enc.to_ascii_lowercase();
            low.contains("gzip") || low.contains("deflate") || low.contains("zstd")
        });
    let cbor_wire = decoded_wire
        && is_cbor_wire(
            body_encoding,
            trace
                .request
                .as_ref()
                .map(|r| r.headers.as_slice())
                .unwrap_or(&[]),
        );
    let owned_body = if let Some(raw) = exact_raw_wire.filter(|_| using_raw_wire) {
        raw
    } else {
        let body_text = trace
            .request
            .as_ref()
            .and_then(|r| r.body.as_ref())
            .and_then(|b| {
                if b.binary || b.truncated {
                    None
                } else {
                    Some(b.text.as_deref().unwrap_or(""))
                }
            })
            .unwrap_or("");
        encode_wire_body(body_text, cbor_wire)?
    };
    let body_bytes: &[u8] = &owned_body;

    let mut req = Vec::with_capacity(512 + body_bytes.len());
    req.extend_from_slice(format!("{} {} HTTP/1.1\r\n", trace.method, path).as_bytes());

    let mut saw_host = false;
    if let Some(exchange) = trace.request.as_ref() {
        for h in &exchange.headers {
            let low = h.name.to_ascii_lowercase();
            // Strip hop-by-hop framing headers plus `Expect` because `build_http_request`
            // transmits the full body eagerly without waiting for `100 Continue`.
            if matches!(
                low.as_str(),
                "content-length" | "transfer-encoding" | "connection" | "expect"
            ) {
                continue;
            }
            if decoded_wire
                && (matches!(
                    low.as_str(),
                    "content-encoding"
                        | "x-amz-decoded-content-length"
                        | "x-amz-trailer"
                        | "x-amz-sdk-checksum-algorithm"
                ) || low.starts_with("x-amz-checksum-")
                    || (compressed_wire && low == "content-md5"))
            {
                continue;
            }
            if low == "host" {
                saw_host = true;
            }
            if extra
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(&low))
            {
                continue;
            }
            let val = if decoded_wire
                && low == "x-amz-content-sha256"
                && h.value.trim().starts_with("STREAMING-")
            {
                "UNSIGNED-PAYLOAD"
            } else {
                h.value.as_str()
            };
            // Guard against CRLF header injection.
            let clean_val = val.replace(['\r', '\n'], " ");
            req.extend_from_slice(format!("{}: {}\r\n", h.name, clean_val).as_bytes());
        }
    }
    if !saw_host {
        let host = if trace.host.is_empty() {
            format!("{}:{}", target.host, target.port)
        } else {
            trace.host.replace(['\r', '\n'], "")
        };
        req.extend_from_slice(format!("Host: {host}\r\n").as_bytes());
    }
    for (name, value) in extra {
        let clean = value.replace(['\r', '\n'], " ");
        req.extend_from_slice(format!("{name}: {clean}\r\n").as_bytes());
    }
    req.extend_from_slice(format!("Content-Length: {}\r\n", body_bytes.len()).as_bytes());
    req.extend_from_slice(b"Connection: close\r\n\r\n");
    req.extend_from_slice(body_bytes);
    Ok(req)
}

fn read_bounded(reader: &mut impl Read, is_head: bool) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    while out.len() < MAX_REPLAY_RESPONSE_BYTES {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                let take = n.min(MAX_REPLAY_RESPONSE_BYTES - out.len());
                out.extend_from_slice(&chunk[..take]);
                if response_complete(&out, is_head) {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if !out.is_empty() => {
                let _ = e;
                break;
            }
            Err(e) => return Err(format!("Read response failed: {e}")),
        }
    }
    if out.is_empty() {
        return Err("Empty HTTP response from emulator".into());
    }
    Ok(out)
}

/// Returns `true` once all leading `1xx` informational responses have been passed
/// and the final HTTP response body is demonstrably complete in `buf`.
fn response_complete(buf: &[u8], is_head: bool) -> bool {
    let mut cursor = 0usize;
    while cursor < buf.len() {
        let mut hdr_slots = [httparse::EMPTY_HEADER; 128];
        let mut resp = httparse::Response::new(&mut hdr_slots);
        let Ok(httparse::Status::Complete(hdr_len)) = resp.parse(&buf[cursor..]) else {
            return false;
        };
        let status = resp.code.unwrap_or(200);
        if (100..200).contains(&status) && status != 101 {
            cursor += hdr_len;
            continue;
        }
        let body_start = cursor + hdr_len;
        if is_head || matches!(status, 204 | 304) {
            return true;
        }
        let mut chunked = false;
        let mut content_len = None;
        for h in resp.headers.iter() {
            if h.name.eq_ignore_ascii_case("transfer-encoding")
                && std::str::from_utf8(h.value)
                    .is_ok_and(|v| v.to_ascii_lowercase().contains("chunked"))
            {
                chunked = true;
            } else if h.name.eq_ignore_ascii_case("content-length")
                && let Ok(s) = std::str::from_utf8(h.value)
                && let Ok(n) = s.trim().parse::<usize>()
            {
                content_len = Some(n);
            }
        }
        let wire_body = &buf[body_start..];
        if chunked {
            return chunked_complete(wire_body);
        }
        if let Some(cl) = content_len {
            return wire_body.len() >= cl;
        }
        return false;
    }
    false
}

fn chunked_complete(input: &[u8]) -> bool {
    let mut pos = 0usize;
    while pos < input.len() {
        let rest = &input[pos..];
        let Some(crlf) = rest.windows(2).position(|w| w == b"\r\n") else {
            return false;
        };
        let size_str = std::str::from_utf8(&rest[..crlf])
            .ok()
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            .unwrap_or("");
        let Ok(len) = usize::from_str_radix(size_str, 16) else {
            return false;
        };
        pos += crlf + 2;
        if len == 0 {
            let tail = &input[pos..];
            return tail.starts_with(b"\r\n") || tail.windows(4).any(|w| w == b"\r\n\r\n");
        }
        if pos + len + 2 > input.len() {
            return false;
        }
        pos += len + 2;
    }
    false
}

fn parse_http_response(raw: &[u8], is_head: bool, ms: u64) -> Result<ReplayOutcome, String> {
    let mut cursor = 0usize;
    loop {
        let mut hdr_slots = [httparse::EMPTY_HEADER; 128];
        let mut resp = httparse::Response::new(&mut hdr_slots);
        let hdr_len = match resp
            .parse(&raw[cursor..])
            .map_err(|e| format!("Malformed HTTP response: {e}"))?
        {
            httparse::Status::Complete(offset) => offset,
            httparse::Status::Partial => return Err("Incomplete HTTP response headers".into()),
        };
        let status = resp.code.unwrap_or(502);
        // Skip any leading 1xx informational responses (e.g. `100 Continue`, `103 Early Hints`)
        // so the Inspector always receives the final status, headers, and payload.
        if (100..200).contains(&status) && status != 101 {
            cursor += hdr_len;
            if cursor >= raw.len() {
                return Err("Incomplete HTTP response after 1xx informational status".into());
            }
            continue;
        }

        let mut headers = Vec::new();
        let mut content_type = None;
        let mut transfer_chunked = false;
        let mut content_encoding = None;
        let mut content_length = None;

        for h in resp.headers.iter() {
            let name = h.name.to_string();
            let value = String::from_utf8_lossy(h.value).into_owned();
            if name.eq_ignore_ascii_case("content-type") {
                content_type = value
                    .split(';')
                    .next()
                    .map(|s| s.trim().to_ascii_lowercase());
            } else if name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                transfer_chunked = true;
            } else if name.eq_ignore_ascii_case("content-encoding") {
                content_encoding = Some(value.trim().to_ascii_lowercase());
            } else if name.eq_ignore_ascii_case("content-length")
                && let Ok(n) = value.trim().parse::<usize>()
            {
                content_length = Some(n);
            }
            headers.push(Header { name, value });
        }

        let body_offset = cursor + hdr_len;
        let wire_body = &raw[body_offset..];
        let unchunked = if is_head || matches!(status, 204 | 304) {
            Vec::new()
        } else if transfer_chunked {
            decode_http_chunked(wire_body)
        } else if let Some(cl) = content_length {
            wire_body[..wire_body.len().min(cl)].to_vec()
        } else {
            wire_body.to_vec()
        };
        let decoded_bytes = match content_encoding.as_deref() {
            Some("gzip" | "x-gzip") => {
                decompress_bytes(GzDecoder::new(&unchunked[..])).unwrap_or(unchunked)
            }
            Some("deflate") => decompress_bytes(ZlibDecoder::new(&unchunked[..]))
                .or_else(|| decompress_bytes(DeflateDecoder::new(&unchunked[..])))
                .unwrap_or(unchunked),
            Some("zstd") => decompress_bytes(
                zstd::stream::read::Decoder::new(&unchunked[..])
                    .map_err(|_| ())
                    .unwrap_or_else(|_| zstd::stream::read::Decoder::new(&[][..]).unwrap()),
            )
            .unwrap_or(unchunked),
            _ => unchunked,
        };

        let body_bytes = decoded_bytes.len();
        let body_text = if decoded_bytes.is_empty() {
            None
        } else if content_type
            .as_deref()
            .is_some_and(|ct| ct.contains("cbor"))
            && let Some(json) = lenslet_core::cbor::decode_cbor_pretty(&decoded_bytes)
        {
            Some(json)
        } else if let Ok(valid) = std::str::from_utf8(&decoded_bytes) {
            Some(valid.to_string())
        } else {
            None
        };

        return Ok(ReplayOutcome {
            status,
            ms,
            headers,
            content_type,
            body_text,
            body_bytes,
            raw_bytes: decoded_bytes,
        });
    }
}

fn decode_http_chunked(input: &[u8]) -> Vec<u8> {
    let mut pos = 0usize;
    let mut out = Vec::new();
    while pos < input.len() {
        let rest = &input[pos..];
        let Some(crlf) = rest.windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let size_str = std::str::from_utf8(&rest[..crlf])
            .ok()
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            .unwrap_or("");
        let Ok(len) = usize::from_str_radix(size_str, 16) else {
            break;
        };
        pos += crlf + 2;
        if len == 0 {
            break;
        }
        let take = len.min(input.len().saturating_sub(pos));
        out.extend_from_slice(&input[pos..pos + take]);
        pos += take;
        if input.get(pos..pos + 2) == Some(b"\r\n") {
            pos += 2;
        }
    }
    out
}

fn decompress_bytes(mut reader: impl Read) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while out.len() < MAX_REPLAY_RESPONSE_BYTES {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n.min(MAX_REPLAY_RESPONSE_BYTES - out.len())]),
            Err(_) => return None,
        }
    }
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse_line;
    use std::net::TcpListener;

    fn direct(url: &str) -> ReplayTarget {
        ReplayTarget {
            url: url.into(),
            via_proxy: false,
        }
    }

    #[test]
    fn replays_over_loopback_and_blocks_non_loopback_targets() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 2048];
            let n = sock.read(&mut req_buf).unwrap();
            let req_text = String::from_utf8_lossy(&req_buf[..n]);
            assert!(req_text.contains("POST / HTTP/1.1"), "{req_text}");
            assert!(
                req_text.contains("{\"TableName\":\"orders\"}"),
                "{req_text}"
            );
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/x-amz-json-1.0\r\nx-amzn-requestid: req-7\r\nContent-Length: 17\r\n\r\n{\"Item\":{\"ok\":1}}",
            )
            .unwrap();
        });

        let call = parse_line(
            r#"{"type":"call","id":"r1","at":"2026-11-19T00:00:00Z","method":"POST","path":"/","host":"127.0.0.1:4566","service":"dynamodb","operation":"GetItem","status":400,"ms":9,"request":{"headers":[{"name":"x-amz-target","value":"DynamoDB_20120810.GetItem"},{"name":"content-type","value":"application/x-amz-json-1.0"}],"content_type":"application/x-amz-json-1.0","body":{"text":"{\"TableName\":\"orders\"}","bytes":22,"truncated":false,"binary":false}}}"#,
        )
        .unwrap();

        // Non-loopback host is strictly refused.
        assert!(!can_replay(
            &call,
            Some("https://dynamodb.us-east-1.amazonaws.com:443")
        ));
        assert!(
            replay_call(
                &call,
                &direct("https://s3.amazonaws.com:443"),
                Path::new(".")
            )
            .is_err()
        );

        // Loopback emulator succeeds and decodes response cleanly.
        let url = format!("http://127.0.0.1:{port}");
        assert!(can_replay(&call, Some(&url)));
        let out = replay_call(&call, &direct(&url), Path::new(".")).unwrap();
        assert_eq!(out.status, 200);
        assert_eq!(out.body_text.as_deref(), Some("{\"Item\":{\"ok\":1}}"));
        server.join().unwrap();
    }

    #[test]
    fn skips_100_continue_informational_responses_and_strips_expect_header() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 2048];
            let n = sock.read(&mut req_buf).unwrap();
            let req_text = String::from_utf8_lossy(&req_buf[..n]);
            assert!(
                !req_text.to_ascii_lowercase().contains("expect:"),
                "{req_text}"
            );
            assert!(
                req_text.contains("x-lenslet-replay-of: r2\r\n"),
                "{req_text}"
            );
            sock.write_all(
                b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nx-amz-request-id: s3-put-1\r\nContent-Length: 5\r\n\r\n<ok/>",
            )
            .unwrap();
        });

        let call = parse_line(
            r#"{"type":"call","id":"r2","at":"2026-11-19T00:00:00Z","method":"PUT","path":"/bucket/key","host":"127.0.0.1:4566","service":"s3","operation":"PutObject","status":200,"ms":5,"request":{"headers":[{"name":"expect","value":"100-continue"},{"name":"content-type","value":"text/plain"}],"content_type":"text/plain","body":{"text":"hello","bytes":5,"truncated":false,"binary":false}}}"#,
        )
        .unwrap();

        let url = format!("http://127.0.0.1:{port}");
        let target = ReplayTarget {
            url,
            via_proxy: true,
        };
        let out = replay_call(&call, &target, Path::new(".")).unwrap();
        assert_eq!(out.status, 200);
        assert_eq!(out.body_text.as_deref(), Some("<ok/>"));
        server.join().unwrap();
    }

    #[test]
    fn send_composed_resigns_sigv4_and_delivers_mutated_payload() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 4096];
            let n = sock.read(&mut req_buf).unwrap();
            let req_text = String::from_utf8_lossy(&req_buf[..n]);
            assert!(
                req_text.contains("AWS4-HMAC-SHA256 Credential=test/"),
                "{req_text}"
            );
            assert!(
                req_text.contains("{\"TableName\":\"mutated\"}"),
                "{req_text}"
            );
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}",
            )
            .unwrap();
        });

        let composed = ComposedRequest {
            call_id: Some("r3".into()),
            method: "POST".into(),
            path: "/".into(),
            host: format!("127.0.0.1:{port}"),
            service: "dynamodb".into(),
            region: "us-east-1".into(),
            encoding: None,
            headers: vec![Header {
                name: "x-amz-target".into(),
                value: "DynamoDB_20120810.PutItem".into(),
            }],
            body: "{\"TableName\":\"mutated\"}".into(),
            resign_sigv4: true,
        };
        let out = send_composed(
            &composed,
            &direct(&format!("http://127.0.0.1:{port}")),
            Path::new("."),
        )
        .unwrap();
        assert_eq!(out.status, 200);
        server.join().unwrap();
    }

    #[test]
    fn send_composed_reencodes_cbor_and_matches_local_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut req_buf = [0u8; 4096];
            let n = sock.read(&mut req_buf).unwrap();
            let split = req_buf[..n]
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .unwrap();
            let hdr_text = String::from_utf8_lossy(&req_buf[..split]);
            let wire_body = &req_buf[split + 4..n];
            assert!(
                hdr_text.contains("AWS4-HMAC-SHA256 Credential=lenslet-local/"),
                "{hdr_text}"
            );
            assert!(
                !hdr_text.to_ascii_lowercase().contains("content-encoding:"),
                "{hdr_text}"
            );
            let decoded = lenslet_core::cbor::decode_cbor(wire_body).expect("wire body is CBOR");
            assert_eq!(decoded["QueueName"], "orders-v2");
            let resp_cbor = lenslet_core::cbor::encode_cbor(&serde_json::json!({"QueueUrl": "ok"}));
            let resp_hdr = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/cbor\r\nContent-Length: {}\r\n\r\n",
                resp_cbor.len()
            );
            sock.write_all(resp_hdr.as_bytes()).unwrap();
            sock.write_all(&resp_cbor).unwrap();
        });

        let composed = ComposedRequest {
            call_id: Some("r4".into()),
            method: "POST".into(),
            path: "/service/AmazonSQS/operation/CreateQueue".into(),
            host: format!("127.0.0.1:{port}"),
            service: "sqs".into(),
            region: "us-east-1".into(),
            encoding: Some("gzip+cbor".into()),
            headers: vec![
                Header {
                    name: "authorization".into(),
                    value: "AWS4-HMAC-SHA256 Credential=lenslet-local/20261119/us-east-1/sqs/aws4_request, SignedHeaders=host, Signature=00".into(),
                },
                Header {
                    name: "content-type".into(),
                    value: "application/cbor".into(),
                },
                Header {
                    name: "smithy-protocol".into(),
                    value: "rpc-v2-cbor".into(),
                },
                Header {
                    name: "content-encoding".into(),
                    value: "gzip".into(),
                },
            ],
            body: "{\"QueueName\":\"orders-v2\"}".into(),
            resign_sigv4: true,
        };
        let out = send_composed(
            &composed,
            &direct(&format!("http://127.0.0.1:{port}")),
            Path::new("."),
        )
        .unwrap();
        assert_eq!(out.status, 200);
        assert!(
            out.body_text
                .as_deref()
                .unwrap()
                .contains("\"QueueUrl\": \"ok\"")
        );
        server.join().unwrap();
    }
}
