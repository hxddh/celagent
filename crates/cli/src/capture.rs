//! Bounded, content-type-aware body capture.
//!
//! Bodies are never buffered for forwarding: [`TeeBody`] wraps the hyper body
//! that is being forwarded and records a bounded prefix plus a SHA-256 of every
//! byte as the bytes stream through. Forwarding therefore stays byte-exact (so
//! SigV4 signatures remain valid) and has no size limit, while memory stays at
//! [`MAX_CAPTURE_BYTES`] per request and per response.

use std::{
    borrow::Cow,
    io::Read,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::Bytes;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};
use hyper::{
    HeaderMap,
    body::{Body, Frame, SizeHint},
};
use sha2::{Digest, Sha256};

use crate::trace::BodyTrace;

/// Maximum number of body bytes kept for the trace (64 KiB).
pub const MAX_CAPTURE_BYTES: usize = 64 * 1024;

/// Content encodings Lenslet can decode on the captured prefix while forwarding
/// the compressed wire bytes untouched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Identity,
    Gzip,
    Deflate,
}

/// The classified `content-type` of a message.
#[derive(Clone, Debug)]
pub struct Content {
    /// Friendly media type without parameters, e.g. `application/xml`.
    pub content_type: Option<String>,
    /// Whether the body is captured as text (JSON, XML, form, or text types).
    pub text: bool,
    /// Supported content encoding for text decoding, when present.
    pub encoding: Option<Encoding>,
}

/// Classify a message's body from its headers.
///
/// Classification is deliberately content-type driven (no sniffing): known text
/// types with identity, `gzip` or `deflate` encoding are decoded into bounded
/// text for error/access inspection; unknown encodings or binary media types
/// are recorded as metadata only.
pub fn content(headers: &HeaderMap) -> Content {
    let media = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());
    let encoding = match headers
        .get("content-encoding")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("") | Some("identity") => Some(Encoding::Identity),
        Some("gzip" | "x-gzip") => Some(Encoding::Gzip),
        Some("deflate") => Some(Encoding::Deflate),
        Some(_) => None,
    };
    let text = media.as_deref().map(is_text).unwrap_or(false) && encoding.is_some();
    Content {
        content_type: media,
        text,
        encoding,
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
        )
}

/// A bounded capture of one body.
pub struct Capture {
    prefix: Vec<u8>,
    keep_bytes: bool,
    bytes: u64,
    hasher: Sha256,
}

impl Capture {
    pub fn new(keep_bytes: bool) -> Self {
        Capture {
            prefix: Vec::new(),
            keep_bytes,
            bytes: 0,
            hasher: Sha256::new(),
        }
    }

    fn feed(&mut self, data: &[u8]) {
        self.bytes = self.bytes.saturating_add(data.len() as u64);
        self.hasher.update(data);
        if !self.keep_bytes {
            return;
        }
        let room = MAX_CAPTURE_BYTES.saturating_sub(self.prefix.len());
        if room > 0 {
            self.prefix.extend_from_slice(&data[..data.len().min(room)]);
        }
    }

    /// The retained prefix of the body (at most [`MAX_CAPTURE_BYTES`]), decoded
    /// when a supported content encoding was used.
    pub fn decoded_prefix(&self, content: &Content) -> Cow<'_, [u8]> {
        match content.encoding {
            Some(Encoding::Identity) | None => Cow::Borrowed(&self.prefix),
            Some(encoding) => decode_compressed(&self.prefix, encoding)
                .map(|(bytes, _)| Cow::Owned(bytes))
                .unwrap_or(Cow::Borrowed(&[])),
        }
    }

    /// Build the trace body, or `None` when no bytes were captured.
    ///
    /// `sha256` always covers every wire byte seen (not just the retained
    /// prefix), so a large, compressed or binary body still has an exact wire
    /// integrity value. `truncated` marks a text capture that holds only a
    /// prefix of the body; binary bodies are never truncated because no bytes
    /// are kept.
    pub fn body(&self, content: &Content) -> Option<BodyTrace> {
        if self.bytes == 0 {
            return None;
        }
        let wire_truncated = self.keep_bytes && self.bytes > self.prefix.len() as u64;
        let decoded = if content.text && self.keep_bytes {
            match content.encoding {
                Some(Encoding::Identity) => Some((
                    String::from_utf8_lossy(&self.prefix).into_owned(),
                    wire_truncated,
                )),
                Some(encoding) => {
                    decode_compressed(&self.prefix, encoding).map(|(bytes, capped)| {
                        (
                            String::from_utf8_lossy(&bytes).into_owned(),
                            wire_truncated || capped,
                        )
                    })
                }
                None => None,
            }
        } else {
            None
        };
        let (text, truncated, binary) = match decoded {
            Some((text, truncated)) => (Some(text), truncated, false),
            None => (None, false, true),
        };
        Some(BodyTrace {
            text,
            bytes: self.bytes,
            truncated,
            binary,
            sha256: hex(&self.hasher.clone().finalize()),
        })
    }
}

/// Decompress up to [`MAX_CAPTURE_BYTES`] of a `gzip` or `deflate` stream.
/// Partial streams still yield whatever prefix decoded before EOF; unreadable
/// data returns `None` so the caller falls back to binary metadata.
fn decode_compressed(prefix: &[u8], encoding: Encoding) -> Option<(Vec<u8>, bool)> {
    if prefix.is_empty() {
        return Some((Vec::new(), false));
    }
    let read_bounded = |mut reader: Box<dyn Read + '_>| -> Vec<u8> {
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        while out.len() <= MAX_CAPTURE_BYTES {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let keep = n.min((MAX_CAPTURE_BYTES + 1).saturating_sub(out.len()));
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
    };
    if out.is_empty() {
        return None;
    }
    let capped = out.len() > MAX_CAPTURE_BYTES;
    if capped {
        out.truncate(MAX_CAPTURE_BYTES);
    }
    Some((out, capped))
}

fn hex(bytes: &[u8]) -> String {
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

        // Gzip and deflate text bodies are decoded for inspection; unknown encodings stay binary.
        let gzip = content(&headers(&[
            ("content-type", "application/json"),
            ("content-encoding", "gzip"),
        ]));
        assert!(gzip.text);
        assert_eq!(gzip.encoding, Some(Encoding::Gzip));

        let br = content(&headers(&[
            ("content-type", "application/json"),
            ("content-encoding", "br"),
        ]));
        assert!(!br.text);
        assert_eq!(br.encoding, None);

        let missing = content(&HeaderMap::new());
        assert!(!missing.text);
        assert!(missing.content_type.is_none());
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

        // Large compressed payloads are capped at MAX_CAPTURE_BYTES when decoded.
        let huge = vec![b'z'; MAX_CAPTURE_BYTES * 4];
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::fast());
        zlib.write_all(&huge).unwrap();
        let deflated = zlib.finish().unwrap();
        let deflate = content(&headers(&[
            ("content-type", "text/plain"),
            ("content-encoding", "deflate"),
        ]));
        let mut big_capture = Capture::new(true);
        big_capture.feed(&deflated);
        let big_body = big_capture.body(&deflate).unwrap();
        assert!(big_body.truncated);
        assert_eq!(big_body.text.as_ref().unwrap().len(), MAX_CAPTURE_BYTES);

        // Corrupt gzip falls back to metadata rather than empty text.
        let mut bad = Capture::new(true);
        bad.feed(b"not-gzip");
        let bad_body = bad.body(&gzip).unwrap();
        assert!(bad_body.binary);
        assert!(bad_body.text.is_none());
    }

    #[test]
    fn binary_body_is_metadata_only() {
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
        let data = vec![b'a'; MAX_CAPTURE_BYTES + 1024];
        let mut capture = Capture::new(true);
        capture.feed(&data[..1024]);
        capture.feed(&data[1024..]);
        let body = capture.body(&text_content()).unwrap();
        assert!(body.truncated);
        assert!(!body.binary);
        assert_eq!(body.bytes as usize, data.len());
        let text = body.text.expect("text bodies keep a prefix");
        assert_eq!(text.len(), MAX_CAPTURE_BYTES);
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
}
