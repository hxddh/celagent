//! Bounded, content-type- and wire-encoding-aware body capture.
//!
//! Bodies are never buffered for forwarding unless `--enforce` needs to inspect
//! a JSON/Query request before letting it reach the emulator: [`TeeBody`] wraps
//! the hyper body and records a bounded prefix plus a SHA-256 of every wire
//! byte as it streams through. Forwarding stays byte-exact (so SigV4 signatures
//! remain valid) and has no size limit.

use std::{
    borrow::Cow,
    io::Read,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use anyhow::{Context as _, Result, bail};
use bytes::Bytes;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use hyper::{
    HeaderMap,
    body::{Body, Frame, SizeHint},
};
use sha2::{Digest, Sha256};

use crate::trace::BodyTrace;

/// Default maximum number of body bytes kept for the trace (1 MiB).
pub const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

/// Binary payloads keep an unredacted hex preview of their first 512 bytes.
pub const HEX_PREVIEW_BYTES: usize = 512;

/// Content encodings Lenslet can decode on the captured prefix while forwarding
/// the wire bytes untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Identity,
    Gzip,
    Deflate,
    Zstd,
}

/// The classified `content-type` and wire encoding of a message.
#[derive(Clone, Debug)]
pub struct Content {
    /// Friendly media type without parameters, e.g. `application/xml`.
    pub content_type: Option<String>,
    /// Whether the declared media type is a known text format with a decodable encoding.
    pub text: bool,
    /// Whether an untyped or `octet-stream` payload may be shown as text when it is valid UTF-8.
    pub sniff_utf8: bool,
    /// Whether the wire body is framed with AWS SigV4/trailer `aws-chunked` chunks.
    pub aws_chunked: bool,
    /// Whether the wire body uses `application/vnd.amazon.eventstream` binary framing.
    pub event_stream: bool,
    /// Whether the wire body uses Smithy RPCv2 CBOR (`application/cbor`).
    pub cbor: bool,
    /// Supported compression layer (after unframing `aws-chunked`, when present).
    pub encoding: Option<Encoding>,
    /// Non-identity wire encoding label for trace metadata, e.g. `aws-chunked` or `gzip`.
    pub wire_encoding: Option<String>,
}

/// Parse a `--max-body` byte limit such as `1048576`, `256K`, `4M`, or `unlimited`.
pub fn parse_size(raw: &str) -> Result<usize> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("--max-body requires a byte count (e.g. 1M, 256K) or `unlimited`");
    }
    if trimmed.eq_ignore_ascii_case("unlimited")
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("max")
    {
        return Ok(usize::MAX / 2);
    }
    let upper = trimmed.to_ascii_uppercase();
    let (digits, scale) = if let Some(n) = upper
        .strip_suffix("KIB")
        .or_else(|| upper.strip_suffix("KB"))
        .or_else(|| upper.strip_suffix('K'))
    {
        (n.trim(), 1024usize)
    } else if let Some(n) = upper
        .strip_suffix("MIB")
        .or_else(|| upper.strip_suffix("MB"))
        .or_else(|| upper.strip_suffix('M'))
    {
        (n.trim(), 1024usize * 1024)
    } else if let Some(n) = upper
        .strip_suffix("GIB")
        .or_else(|| upper.strip_suffix("GB"))
        .or_else(|| upper.strip_suffix('G'))
    {
        (n.trim(), 1024usize * 1024 * 1024)
    } else if let Some(n) = upper.strip_suffix('B') {
        (n.trim(), 1usize)
    } else {
        (upper.as_str(), 1usize)
    };
    let count: usize = digits.parse().with_context(|| {
        format!("invalid --max-body `{raw}` (expected bytes, 256K, 4M, or unlimited)")
    })?;
    count
        .checked_mul(scale)
        .with_context(|| format!("--max-body `{raw}` is too large"))
}

/// Classify a message's body from its headers.
///
/// Known text types (`json`, `xml`, `text/*`, form, ...) are decoded across
/// `identity`, `aws-chunked`, `gzip` and `deflate`. Missing `content-type` or
/// `octet-stream` (the SDK default on S3 `PutObject`/`GetObject` and raw Lambda
/// invocations) also surfaces readable UTF-8 text instead of hiding it as
/// opaque binary, while true binary payloads keep size, SHA-256 and a hex preview.
pub fn content(headers: &HeaderMap) -> Content {
    let media = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());

    let raw_encoding = headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty() && value != "identity");

    let streaming_sha = headers
        .get("x-amz-content-sha256")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim().starts_with("STREAMING-"));

    let mut aws_chunked = streaming_sha;
    let mut compression = Some(Encoding::Identity);
    if let Some(enc) = raw_encoding.as_deref() {
        for layer in enc.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            match layer {
                "identity" => {}
                "aws-chunked" => aws_chunked = true,
                "gzip" | "x-gzip" if compression == Some(Encoding::Identity) => {
                    compression = Some(Encoding::Gzip);
                }
                "deflate" if compression == Some(Encoding::Identity) => {
                    compression = Some(Encoding::Deflate);
                }
                "zstd" if compression == Some(Encoding::Identity) => {
                    compression = Some(Encoding::Zstd);
                }
                _ => {
                    compression = None;
                    break;
                }
            }
        }
    }

    let event_stream = media.as_deref().is_some_and(|m| {
        m == "application/vnd.amazon.eventstream"
            || m.starts_with("application/vnd.amazon.eventstream+")
    });
    let rpcv2_header = headers
        .get("smithy-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("rpc-v2-cbor"));
    let cbor = rpcv2_header
        || media.as_deref().is_some_and(|m| {
            m == "application/cbor"
                || m.starts_with("application/x-amz-cbor")
                || m.ends_with("+cbor")
        });

    let mut enc_parts: Vec<String> = Vec::new();
    if aws_chunked
        && !raw_encoding
            .as_deref()
            .is_some_and(|r| r.contains("aws-chunked"))
    {
        enc_parts.push("aws-chunked".to_string());
    }
    if event_stream {
        enc_parts.push("aws-eventstream".to_string());
    }
    if cbor {
        enc_parts.push("cbor".to_string());
    }
    if let Some(raw) = &raw_encoding {
        enc_parts.push(raw.clone());
    }
    let wire_encoding = (!enc_parts.is_empty()).then(|| enc_parts.join(","));

    let known_text = media.as_deref().is_some_and(is_text) || event_stream || cbor;
    let untyped_or_octet = matches!(
        media.as_deref(),
        None | Some("application/octet-stream" | "binary/octet-stream")
    ) && !cbor;

    Content {
        content_type: media,
        text: known_text && compression.is_some(),
        sniff_utf8: untyped_or_octet && compression.is_some(),
        aws_chunked,
        event_stream,
        cbor,
        encoding: compression,
        wire_encoding,
    }
}

fn is_text(media: &str) -> bool {
    media.contains("json")
        || media.contains("xml")
        || media == "application/x-www-form-urlencoded"
        || media.starts_with("text/")
        || matches!(
            media,
            "application/javascript"
                | "application/x-javascript"
                | "application/yaml"
                | "application/x-yaml"
                | "application/graphql"
                | "application/sql"
                | "application/csv"
        )
}

/// A bounded capture of one body.
pub struct Capture {
    prefix: Vec<u8>,
    limit: usize,
    bytes: u64,
    hasher: Sha256,
}

impl Capture {
    #[cfg(test)]
    pub fn new(keep_bytes: bool) -> Self {
        Self::with_limit(if keep_bytes { MAX_CAPTURE_BYTES } else { 0 })
    }

    pub fn with_limit(limit: usize) -> Self {
        Capture {
            prefix: Vec::new(),
            limit,
            bytes: 0,
            hasher: Sha256::new(),
        }
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.bytes = self.bytes.saturating_add(data.len() as u64);
        self.hasher.update(data);
        if self.limit == 0 {
            return;
        }
        let room = self.limit.saturating_sub(self.prefix.len());
        if room > 0 {
            self.prefix.extend_from_slice(&data[..data.len().min(room)]);
        }
    }

    /// The retained prefix of the body, unframed and decompressed when a
    /// supported wire encoding was used.
    pub fn decoded_prefix(&self, content: &Content) -> Cow<'_, [u8]> {
        if self.limit == 0 || self.prefix.is_empty() {
            return Cow::Borrowed(&[]);
        }
        if !content.aws_chunked
            && !content.event_stream
            && !content.cbor
            && matches!(content.encoding, Some(Encoding::Identity) | None)
        {
            return Cow::Borrowed(&self.prefix);
        }
        decode_payload(&self.prefix, content, self.limit)
            .map(|(bytes, _)| Cow::Owned(bytes))
            .unwrap_or(Cow::Borrowed(&[]))
    }

    /// Build the trace body, or `None` when no bytes were captured.
    ///
    /// `sha256` always covers every wire byte seen (not just the retained
    /// prefix), so a large, compressed or binary body still has an exact wire
    /// integrity value.
    pub fn body(&self, content: &Content) -> Option<BodyTrace> {
        if self.bytes == 0 {
            return None;
        }
        let sha256 = hex(&self.hasher.clone().finalize());
        if self.limit == 0 {
            return Some(BodyTrace {
                text: None,
                hex: None,
                encoding: content.wire_encoding.clone(),
                bytes: self.bytes,
                truncated: false,
                binary: true,
                sha256,
            });
        }

        let wire_truncated = self.bytes > self.prefix.len() as u64;
        let decoded = decode_payload(&self.prefix, content, self.limit);

        if let Some((ref bytes, capped)) = decoded {
            let truncated = wire_truncated || capped;
            if content.text {
                let text = lossy_trimmed_utf8(bytes, truncated);
                return Some(BodyTrace {
                    text: Some(text),
                    hex: None,
                    encoding: content.wire_encoding.clone(),
                    bytes: self.bytes,
                    truncated,
                    binary: false,
                    sha256,
                });
            }
            if content.sniff_utf8
                && !bytes.is_empty()
                && let Some(text) = clean_utf8(bytes, truncated)
            {
                return Some(BodyTrace {
                    text: Some(text),
                    hex: None,
                    encoding: content.wire_encoding.clone(),
                    bytes: self.bytes,
                    truncated,
                    binary: false,
                    sha256,
                });
            }
            if content.aws_chunked && bytes.is_empty() && !truncated {
                return Some(BodyTrace {
                    text: Some(String::new()),
                    hex: None,
                    encoding: content.wire_encoding.clone(),
                    bytes: self.bytes,
                    truncated: false,
                    binary: false,
                    sha256,
                });
            }
        }

        let preview_source: &[u8] = match decoded.as_ref() {
            Some((bytes, _)) if !bytes.is_empty() => bytes,
            _ => &self.prefix,
        };
        let preview_len = preview_source.len().min(HEX_PREVIEW_BYTES);
        let hex_preview = (preview_len > 0).then(|| hex(&preview_source[..preview_len]));

        Some(BodyTrace {
            text: None,
            hex: hex_preview,
            encoding: content.wire_encoding.clone(),
            bytes: self.bytes,
            truncated: wire_truncated,
            binary: true,
            sha256,
        })
    }
}

/// Unframe `aws-chunked` / `vnd.amazon.eventstream` and decompress `gzip`/`deflate` up to `limit`.
fn decode_payload(prefix: &[u8], content: &Content, limit: usize) -> Option<(Vec<u8>, bool)> {
    let encoding = content.encoding?;
    let (unframed, chunk_capped): (Cow<'_, [u8]>, bool) = if content.aws_chunked {
        let (bytes, capped) = decode_aws_chunked(prefix, limit)?;
        (Cow::Owned(bytes), capped)
    } else {
        (Cow::Borrowed(prefix), false)
    };
    let (decompressed, comp_capped): (Cow<'_, [u8]>, bool) = match encoding {
        Encoding::Identity => (unframed, chunk_capped),
        other => {
            let (bytes, capped) = decode_compressed(&unframed, other, limit)?;
            (Cow::Owned(bytes), chunk_capped || capped)
        }
    };
    if content.event_stream {
        let (events, ev_capped) = decode_eventstream(&decompressed, limit)?;
        return Some((events, comp_capped || ev_capped));
    }
    if content.cbor {
        let json_text = lenslet_core::cbor::decode_cbor_pretty(&decompressed)?;
        let mut bytes = json_text.into_bytes();
        let cbor_capped = bytes.len() > limit;
        if cbor_capped {
            bytes.truncate(limit);
        }
        return Some((bytes, comp_capped || cbor_capped));
    }
    Some((decompressed.into_owned(), comp_capped))
}

/// Unframe AWS `application/vnd.amazon.eventstream` binary messages into readable
/// newline-delimited JSON objects (used by Bedrock `ConverseStream` /
/// `InvokeModelWithResponseStream`, S3 `SelectObjectContent`, Lambda response streams).
fn decode_eventstream(input: &[u8], limit: usize) -> Option<(Vec<u8>, bool)> {
    let mut pos = 0usize;
    let mut out = Vec::new();
    let mut capped = false;
    let mut frames = 0usize;

    while pos < input.len() {
        let rem = &input[pos..];
        if rem.len() < 16 {
            return (frames > 0).then_some((out, true));
        }
        let total_len = u32::from_be_bytes([rem[0], rem[1], rem[2], rem[3]]) as usize;
        let headers_len = u32::from_be_bytes([rem[4], rem[5], rem[6], rem[7]]) as usize;
        let Some(min_total) = 12usize
            .checked_add(headers_len)
            .and_then(|n| n.checked_add(4))
        else {
            return (frames > 0).then_some((out, true));
        };
        if total_len < min_total {
            return (frames > 0).then_some((out, true));
        }
        if rem.len() < total_len {
            return (frames > 0).then_some((out, true));
        }

        let headers_buf = &rem[12..12 + headers_len];
        let payload_buf = &rem[12 + headers_len..total_len - 4];
        let (event_type, message_type, exception_type) = parse_eventstream_headers(headers_buf)?;

        let label = exception_type
            .or(event_type)
            .or(message_type)
            .unwrap_or_else(|| "event".to_string());
        let line_json = format_eventstream_frame(&label, payload_buf);
        let line_bytes = line_json.as_bytes();
        let needed = line_bytes.len() + usize::from(!out.is_empty());
        if out.len().saturating_add(needed) > limit {
            capped = true;
            break;
        }
        if !out.is_empty() {
            out.push(b'\n');
        }
        out.extend_from_slice(line_bytes);
        frames += 1;
        pos += total_len;
    }

    (frames > 0).then_some((out, capped))
}

#[allow(clippy::type_complexity)]
fn parse_eventstream_headers(
    buf: &[u8],
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let mut pos = 0usize;
    let mut event_type = None;
    let mut message_type = None;
    let mut exception_type = None;

    while pos < buf.len() {
        let name_len = *buf.get(pos)? as usize;
        pos += 1;
        let name = std::str::from_utf8(buf.get(pos..pos.checked_add(name_len)?)?).ok()?;
        pos += name_len;
        let val_type = *buf.get(pos)?;
        pos += 1;
        match val_type {
            0 | 1 => {}
            2 => pos = pos.checked_add(1)?,
            3 => pos = pos.checked_add(2)?,
            4 => pos = pos.checked_add(4)?,
            5 | 8 => pos = pos.checked_add(8)?,
            6 => {
                let b_len = u16::from_be_bytes([*buf.get(pos)?, *buf.get(pos + 1)?]) as usize;
                pos = pos.checked_add(2)?.checked_add(b_len)?;
            }
            7 => {
                let s_len = u16::from_be_bytes([*buf.get(pos)?, *buf.get(pos + 1)?]) as usize;
                pos += 2;
                let val = std::str::from_utf8(buf.get(pos..pos.checked_add(s_len)?)?)
                    .ok()?
                    .to_string();
                pos += s_len;
                match name {
                    ":event-type" => event_type = Some(val),
                    ":message-type" => message_type = Some(val),
                    ":exception-type" | ":error-code" => exception_type = Some(val),
                    _ => {}
                }
            }
            9 => pos = pos.checked_add(16)?,
            _ => return None,
        }
        if pos > buf.len() {
            return None;
        }
    }
    Some((event_type, message_type, exception_type))
}

fn format_eventstream_frame(event: &str, payload: &[u8]) -> String {
    if payload.is_empty() {
        return serde_json::json!({ "event": event }).to_string();
    }
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(payload) {
        // Bedrock `InvokeModelWithResponseStream` wraps provider JSON in `{"bytes":"<base64>"}`.
        if let Some(b64) = val.get("bytes").and_then(|v| v.as_str())
            && let Some(decoded) = decode_base64(b64)
        {
            if let Ok(inner_json) = serde_json::from_slice::<serde_json::Value>(&decoded) {
                return serde_json::json!({ "event": event, "data": inner_json }).to_string();
            }
            if let Ok(inner_str) = std::str::from_utf8(&decoded) {
                return serde_json::json!({ "event": event, "text": inner_str }).to_string();
            }
        }
        return serde_json::json!({ "event": event, "data": val }).to_string();
    }
    if let Ok(text) = std::str::from_utf8(payload) {
        return serde_json::json!({ "event": event, "text": text }).to_string();
    }
    serde_json::json!({ "event": event, "bytes": payload.len() }).to_string()
}

fn decode_base64(input: &str) -> Option<Vec<u8>> {
    fn val(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        match chunk {
            [a, b, c, d] => {
                let (a, b, c, d) = (val(*a)?, val(*b)?, val(*c)?, val(*d)?);
                out.push((a << 2) | (b >> 4));
                out.push((b << 4) | (c >> 2));
                out.push((c << 6) | d);
            }
            [a, b, c] => {
                let (a, b, c) = (val(*a)?, val(*b)?, val(*c)?);
                out.push((a << 2) | (b >> 4));
                out.push((b << 4) | (c >> 2));
            }
            [a, b] => {
                let (a, b) = (val(*a)?, val(*b)?);
                out.push((a << 2) | (b >> 4));
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Strip AWS `aws-chunked` chunk headers (`<hex-size>[;chunk-signature=...]\r\n<data>\r\n`)
/// and trailing checksum headers while keeping at most `limit` payload bytes.
fn decode_aws_chunked(input: &[u8], limit: usize) -> Option<(Vec<u8>, bool)> {
    let mut pos = 0usize;
    let mut out = Vec::new();
    let mut capped = false;

    while pos < input.len() {
        let rest = &input[pos..];
        let Some(rel_end) = rest.windows(2).position(|w| w == b"\r\n") else {
            return if !out.is_empty() {
                Some((out, true))
            } else {
                None
            };
        };
        let header = &rest[..rel_end];
        let size_part = header
            .split(|&b| b == b';')
            .next()
            .unwrap_or(header)
            .trim_ascii();
        let size_str = std::str::from_utf8(size_part).ok()?;
        let chunk_size = usize::from_str_radix(size_str, 16).ok()?;
        pos += rel_end + 2;

        if chunk_size == 0 {
            return Some((out, capped));
        }

        let available = input.len().saturating_sub(pos);
        let take = available.min(chunk_size);
        let room = limit.saturating_sub(out.len());
        if take > room {
            out.extend_from_slice(&input[pos..pos + room]);
            capped = true;
        } else {
            out.extend_from_slice(&input[pos..pos + take]);
        }
        if take < chunk_size {
            return Some((out, true));
        }
        pos += chunk_size;
        if input.get(pos..pos + 2) == Some(b"\r\n") {
            pos += 2;
        }
        if out.len() >= limit && pos < input.len() {
            // Check whether only the terminal 0-chunk remains.
            let rem = &input[pos..];
            if !rem.starts_with(b"0\r\n") && !rem.starts_with(b"0;") {
                capped = true;
            }
            return Some((out, capped));
        }
    }

    Some((out, capped))
}

/// Decompress up to `limit` bytes of a `gzip` or `deflate` stream.
fn decode_compressed(prefix: &[u8], encoding: Encoding, limit: usize) -> Option<(Vec<u8>, bool)> {
    if prefix.is_empty() {
        return Some((Vec::new(), false));
    }
    let ceiling = limit.saturating_add(1);
    let read_bounded = |mut reader: Box<dyn Read + '_>| -> Vec<u8> {
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        while out.len() < ceiling {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let keep = n.min(ceiling.saturating_sub(out.len()));
                    out.extend_from_slice(&chunk[..keep]);
                }
            }
        }
        out
    };
    let mut out = match encoding {
        Encoding::Identity => prefix.to_vec(),
        Encoding::Gzip => read_bounded(Box::new(GzDecoder::new(prefix))),
        Encoding::Deflate => {
            let zlib = read_bounded(Box::new(ZlibDecoder::new(prefix)));
            if !zlib.is_empty() {
                zlib
            } else {
                read_bounded(Box::new(DeflateDecoder::new(prefix)))
            }
        }
        Encoding::Zstd => {
            let decoder = zstd::stream::read::Decoder::new(prefix).ok()?;
            read_bounded(Box::new(decoder))
        }
    };
    if out.is_empty() {
        return None;
    }
    let capped = out.len() > limit;
    if capped {
        out.truncate(limit);
    }
    Some((out, capped))
}

/// Decode declared text, dropping an incomplete trailing multi-byte UTF-8 sequence
/// when the byte stream was truncated mid-codepoint.
fn lossy_trimmed_utf8(bytes: &[u8], truncated: bool) -> String {
    match std::str::from_utf8(bytes) {
        Ok(valid) => valid.to_string(),
        Err(err) if truncated && err.error_len().is_none() => {
            std::str::from_utf8(&bytes[..err.valid_up_to()])
                .unwrap_or_default()
                .to_string()
        }
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Validate that an untyped/`octet-stream` payload is genuine readable UTF-8
/// (allowing tabs, newlines, and all Unicode characters, rejecting NUL/control bytes).
fn clean_utf8(bytes: &[u8], truncated: bool) -> Option<String> {
    let text = match std::str::from_utf8(bytes) {
        Ok(valid) => valid,
        Err(err) if truncated && err.error_len().is_none() && err.valid_up_to() > 0 => {
            std::str::from_utf8(&bytes[..err.valid_up_to()]).ok()?
        }
        Err(_) => return None,
    };
    let clean = !text.bytes().any(|b| {
        matches!(
            b,
            0x00..=0x08 | 0x0B | 0x0C | 0x0E..=0x1F | 0x7F
        )
    });
    clean.then(|| text.to_string())
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Wraps a body that is being forwarded and captures it on the way through.
///
/// The wrapper never fails a transfer: a poisoned capture mutex or a capture
/// problem only costs trace detail. `on_end` runs exactly once, when the body
/// ends, errors, or is dropped.
pub struct TeeBody<B> {
    inner: B,
    capture: Arc<Mutex<Capture>>,
    on_end: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl<B> TeeBody<B> {
    pub fn new(inner: B, capture: Arc<Mutex<Capture>>) -> Self {
        TeeBody {
            inner,
            capture,
            on_end: None,
        }
    }

    /// Run `hook` once, when the body is finished or dropped.
    pub fn on_end(mut self, hook: impl FnOnce() + Send + Sync + 'static) -> Self {
        self.on_end = Some(Box::new(hook));
        self
    }

    fn end(&mut self) {
        if let Some(hook) = self.on_end.take() {
            hook();
        }
    }
}

impl<B> Drop for TeeBody<B> {
    fn drop(&mut self) {
        self.end();
    }
}

impl<B> Body for TeeBody<B>
where
    B: Body<Data = Bytes> + Unpin,
{
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        match &polled {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref()
                    && let Ok(mut capture) = this.capture.lock()
                {
                    capture.feed(data);
                }
            }
            Poll::Ready(Some(Err(_))) | Poll::Ready(None) => this.end(),
            Poll::Pending => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::{BodyExt, Full};
    use hyper::header::HeaderValue;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn text_content() -> Content {
        content(&headers(&[("content-type", "text/plain; charset=utf-8")]))
    }

    #[test]
    fn classifies_content_types_and_encodings() {
        let json = content(&headers(&[("content-type", "application/x-amz-json-1.0")]));
        assert!(json.text);
        assert_eq!(
            json.content_type.as_deref(),
            Some("application/x-amz-json-1.0")
        );

        let xml = content(&headers(&[("content-type", "text/xml; charset=utf-8")]));
        assert!(xml.text);
        assert_eq!(xml.content_type.as_deref(), Some("text/xml"));

        let form = content(&headers(&[(
            "content-type",
            "application/x-www-form-urlencoded",
        )]));
        assert!(form.text);

        let plain = content(&headers(&[("content-type", "text/plain")]));
        assert!(plain.text);

        let binary = content(&headers(&[("content-type", "application/octet-stream")]));
        assert!(!binary.text);
        assert!(binary.sniff_utf8);

        let gzip = content(&headers(&[
            ("content-type", "application/json"),
            ("content-encoding", "gzip"),
        ]));
        assert!(gzip.text);
        assert_eq!(gzip.encoding, Some(Encoding::Gzip));

        let chunked = content(&headers(&[
            ("content-type", "application/json"),
            ("content-encoding", "aws-chunked"),
            ("x-amz-content-sha256", "STREAMING-UNSIGNED-PAYLOAD-TRAILER"),
        ]));
        assert!(chunked.text);
        assert!(chunked.aws_chunked);
        assert_eq!(chunked.wire_encoding.as_deref(), Some("aws-chunked"));

        let br = content(&headers(&[
            ("content-type", "application/json"),
            ("content-encoding", "br"),
        ]));
        assert!(!br.text);
        assert_eq!(br.encoding, None);

        let missing = content(&HeaderMap::new());
        assert!(!missing.text);
        assert!(missing.sniff_utf8);
        assert!(missing.content_type.is_none());
    }

    #[test]
    fn unframes_aws_chunked_and_surfaces_untyped_utf8_and_binary_hex() {
        // Modern boto3 / AWS SDK v3 S3 PutObject with aws-chunked + trailer checksum,
        // and default binary/octet-stream Content-Type.
        let wire = b"1f;chunk-signature=deadbeef\r\n{\"order\":\"#42\",\"note\":\"\xe4\xb8\xad\xe6\x96\x87\"}\r\n0;chunk-signature=feedface\r\nx-amz-checksum-crc32:AAAAAA==\r\n\r\n";
        let meta = content(&headers(&[
            ("content-type", "binary/octet-stream"),
            ("content-encoding", "aws-chunked"),
        ]));
        let mut capture = Capture::new(true);
        capture.feed(wire);
        let body = capture.body(&meta).unwrap();
        assert!(!body.binary, "valid UTF-8 inside aws-chunked is readable");
        assert!(!body.truncated);
        assert_eq!(body.encoding.as_deref(), Some("aws-chunked"));
        assert_eq!(
            body.text.as_deref(),
            Some("{\"order\":\"#42\",\"note\":\"中文\"}")
        );
        assert_eq!(body.bytes as usize, wire.len());
        assert_eq!(body.sha256, hex(&Sha256::digest(wire)));

        // Genuine binary bytes keep binary=true plus an unredacted hex preview.
        let raw_bin = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0xff];
        let png = content(&headers(&[("content-type", "image/png")]));
        let mut bin_cap = Capture::new(true);
        bin_cap.feed(&raw_bin);
        let bin_body = bin_cap.body(&png).unwrap();
        assert!(bin_body.binary);
        assert!(bin_body.text.is_none());
        assert_eq!(bin_body.hex.as_deref(), Some("89504e470d0a1a0a00ff"));
    }

    #[test]
    fn parses_max_body_sizes() {
        assert_eq!(parse_size("4096").unwrap(), 4096);
        assert_eq!(parse_size("64K").unwrap(), 64 * 1024);
        assert_eq!(parse_size("4MB").unwrap(), 4 * 1024 * 1024);
        assert!(parse_size("unlimited").unwrap() > 1024 * 1024 * 1024);
        assert!(parse_size("").is_err());
        assert!(parse_size("nope").is_err());
    }

    #[test]
    fn decodes_gzip_and_deflate_text_within_capture_bounds() {
        use flate2::{Compression, write::GzEncoder, write::ZlibEncoder};
        use std::io::Write as _;

        let json = br#"{"TableName":"orders","__type":"ResourceNotFoundException"}"#;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(json).unwrap();
        let compressed = encoder.finish().unwrap();

        let gzip = content(&headers(&[
            ("content-type", "application/json"),
            ("content-encoding", "gzip"),
        ]));
        let mut capture = Capture::new(true);
        capture.feed(&compressed);
        let body = capture.body(&gzip).unwrap();
        assert!(!body.binary);
        assert!(!body.truncated);
        assert_eq!(body.bytes as usize, compressed.len());
        assert_eq!(body.sha256, hex(&Sha256::digest(&compressed)));
        assert_eq!(
            body.text.as_deref(),
            Some(std::str::from_utf8(json).unwrap())
        );

        // Large compressed payloads are capped at the configured limit when decoded.
        let limit = 64 * 1024;
        let huge = vec![b'z'; limit * 4];
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::fast());
        zlib.write_all(&huge).unwrap();
        let deflated = zlib.finish().unwrap();
        let deflate = content(&headers(&[
            ("content-type", "text/plain"),
            ("content-encoding", "deflate"),
        ]));
        let mut big_capture = Capture::with_limit(limit);
        big_capture.feed(&deflated);
        let big_body = big_capture.body(&deflate).unwrap();
        assert!(big_body.truncated);
        assert_eq!(big_body.text.as_ref().unwrap().len(), limit);

        // Corrupt gzip falls back to binary metadata rather than empty text.
        let mut bad = Capture::new(true);
        bad.feed(b"not-gzip");
        let bad_body = bad.body(&gzip).unwrap();
        assert!(bad_body.binary);
        assert!(bad_body.text.is_none());
    }

    #[test]
    fn binary_body_is_metadata_only_when_limit_is_zero() {
        let binary = content(&headers(&[("content-type", "application/octet-stream")]));
        let mut capture = Capture::new(false);
        capture.feed(b"abc");
        let body = capture.body(&binary).unwrap();
        assert!(body.binary);
        assert!(body.text.is_none());
        assert!(!body.truncated);
        assert_eq!(body.bytes, 3);
        assert_eq!(
            body.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let json = serde_json::to_string(&body).unwrap();
        assert!(!json.contains("text"));
        assert!(!json.contains("abc"));
    }

    #[test]
    fn truncates_large_text_bodies_but_hashes_everything() {
        let limit = 64 * 1024;
        let data = vec![b'a'; limit + 1024];
        let mut capture = Capture::with_limit(limit);
        capture.feed(&data[..1024]);
        capture.feed(&data[1024..]);
        let body = capture.body(&text_content()).unwrap();
        assert!(body.truncated);
        assert!(!body.binary);
        assert_eq!(body.bytes as usize, data.len());
        let text = body.text.expect("text bodies keep a prefix");
        assert_eq!(text.len(), limit);
        assert!(text.bytes().all(|byte| byte == b'a'));
        assert_eq!(body.sha256, hex(&Sha256::digest(&data)));
    }

    #[test]
    fn small_bodies_are_complete_and_empty_ones_are_absent() {
        let mut small = Capture::new(true);
        small.feed(b"partial");
        let body = small.body(&text_content()).unwrap();
        assert!(!body.truncated, "everything that flowed through was kept");
        assert_eq!(body.bytes, 7);
        assert_eq!(body.text.as_deref(), Some("partial"));

        let empty = Capture::new(true);
        assert!(empty.body(&text_content()).is_none());
        assert!(Capture::new(false).body(&text_content()).is_none());
    }

    #[test]
    fn text_capture_keeps_the_body_verbatim() {
        let raw = br#"{"password":"hunter2","name":"lenslet"}"#;
        let mut capture = Capture::new(true);
        capture.feed(raw);
        let body = capture
            .body(&content(&headers(&[("content-type", "application/json")])))
            .unwrap();
        assert_eq!(
            body.text.as_deref(),
            Some(std::str::from_utf8(raw).unwrap())
        );
        assert_eq!(body.bytes as usize, raw.len());
    }

    #[tokio::test]
    async fn tee_body_forwards_every_byte_and_runs_the_hook_once() {
        let capture = Arc::new(Mutex::new(Capture::new(true)));
        let calls = Arc::new(AtomicUsize::new(0));
        let hook = {
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
            }
        };
        let body =
            TeeBody::new(Full::new(Bytes::from_static(b"hello")), capture.clone()).on_end(hook);
        let forwarded = body.collect().await.unwrap().to_bytes();
        assert_eq!(forwarded, Bytes::from_static(b"hello"));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            capture.lock().unwrap().body(&text_content()).unwrap().text,
            Some("hello".to_string())
        );
    }

    #[test]
    fn tee_body_hook_runs_when_the_body_is_dropped_early() {
        let capture = Arc::new(Mutex::new(Capture::new(true)));
        let calls = Arc::new(AtomicUsize::new(0));
        let hook = {
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
            }
        };
        let body =
            TeeBody::new(Full::new(Bytes::from_static(b"abc")), capture.clone()).on_end(hook);
        drop(body);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let capture = capture.lock().unwrap();
        assert!(capture.body(&text_content()).is_none());
    }

    #[test]
    fn unframes_aws_eventstream_messages_and_bedrock_base64_chunks() {
        fn build_frame(event_type: &str, payload: &[u8]) -> Vec<u8> {
            let hdr_name = b":event-type";
            let val_bytes = event_type.as_bytes();
            let mut hdr = Vec::new();
            hdr.push(hdr_name.len() as u8);
            hdr.extend_from_slice(hdr_name);
            hdr.push(7u8); // string header value
            hdr.extend_from_slice(&(val_bytes.len() as u16).to_be_bytes());
            hdr.extend_from_slice(val_bytes);

            let total_len = 12 + hdr.len() + payload.len() + 4;
            let mut frame = Vec::with_capacity(total_len);
            frame.extend_from_slice(&(total_len as u32).to_be_bytes());
            frame.extend_from_slice(&(hdr.len() as u32).to_be_bytes());
            frame.extend_from_slice(&0u32.to_be_bytes());
            frame.extend_from_slice(&hdr);
            frame.extend_from_slice(payload);
            frame.extend_from_slice(&0u32.to_be_bytes());
            frame
        }

        // `{"delta":{"text":"hi"}}` in base64 = `eyJkZWx0YSI6eyJ0ZXh0IjoiaGkifX0=`
        let mut wire_bytes =
            build_frame("chunk", br#"{"bytes":"eyJkZWx0YSI6eyJ0ZXh0IjoiaGkifX0="}"#);
        wire_bytes.extend_from_slice(&build_frame("messageStop", br#"{"stopReason":"end_turn"}"#));

        let mut cap = Capture::new(true);
        cap.feed(&wire_bytes);
        let body = cap
            .body(&content(&headers(&[(
                "content-type",
                "application/vnd.amazon.eventstream",
            )])))
            .unwrap();
        assert!(!body.binary);
        assert_eq!(body.encoding.as_deref(), Some("aws-eventstream"));
        let text = body.text.unwrap();
        assert!(text.contains(r#""text":"hi""#), "{text}");
        assert!(text.contains(r#""event":"messageStop""#), "{text}");
    }

    #[test]
    fn decodes_smithy_rpcv2_cbor_and_zstd_payloads_to_readable_json() {
        // CBOR map `{"Namespace": "Shop/Checkout"}`
        let mut cbor_bytes = vec![0xa1, 0x69];
        cbor_bytes.extend_from_slice(b"Namespace");
        cbor_bytes.push(0x6d);
        cbor_bytes.extend_from_slice(b"Shop/Checkout");

        let compressed = zstd::stream::encode_all(&cbor_bytes[..], 1).unwrap();
        let mut cap = Capture::new(true);
        cap.feed(&compressed);
        let body = cap
            .body(&content(&headers(&[
                ("content-type", "application/cbor"),
                ("smithy-protocol", "rpc-v2-cbor"),
                ("content-encoding", "zstd"),
            ])))
            .unwrap();
        assert!(!body.binary);
        assert_eq!(body.encoding.as_deref(), Some("cbor,zstd"));
        let text = body.text.unwrap();
        assert!(text.contains(r#""Namespace": "Shop/Checkout""#), "{text}");
    }
}
