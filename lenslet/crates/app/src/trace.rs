//! Trace model, tolerant JSONL parsing, live tailing, filtering and display helpers.
//!
//! The proxy writes one JSON object per line. Everything here treats absent or
//! partial fields as normal: the UI must keep rendering while the proxy changes.

use std::{
    borrow::Cow,
    collections::HashMap,
    fmt::Write as _,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::PathBuf,
};

use serde::{
    Deserialize, Deserializer,
    de::{self, Visitor},
};

pub const MAX_ROWS: usize = 500;
/// Only read the last slice of a large trace file on the first load / reload.
const INITIAL_TAIL_BYTES: u64 = 1_500_000;
/// Never let a partially written line grow without bound.
const MAX_CARRY_BYTES: usize = 256 * 1024;
/// Body text contributed to the search index (per request/response).
const SEARCH_BODY_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Header {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Body {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub bytes: usize,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub sha256: Option<String>,
}

impl Body {
    pub fn size(&self) -> usize {
        if self.bytes > 0 {
            self.bytes
        } else {
            self.text.as_ref().map_or(0, |text| text.len())
        }
    }

    pub fn is_empty(&self) -> bool {
        self.size() == 0 && self.text.as_deref().unwrap_or("").is_empty()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Exchange {
    #[serde(default)]
    pub headers: Vec<Header>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub body: Option<Body>,
}

impl Exchange {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case(name))
            .map(|header| header.value.as_str())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Trace {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub at: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub operation: String,
    #[serde(default)]
    pub status: u16,
    #[serde(default, alias = "duration_ms", deserialize_with = "deserialize_ms")]
    pub ms: u64,
    #[serde(default, alias = "auth")]
    pub authorization: bool,
    #[serde(default)]
    pub request: Option<Exchange>,
    #[serde(default)]
    pub response: Option<Exchange>,
    #[serde(skip)]
    service_key: String,
    #[serde(skip)]
    service_name: String,
    #[serde(skip)]
    haystack: String,
}

impl Trace {
    fn finalize(mut self) -> Self {
        self.service_key = service_id(&self.service);
        self.service_name = service_label(&self.service);
        self.haystack = self.build_haystack();
        self
    }

    /// Normalized service identity (e.g. `dynamodb`), used by dynamic filters.
    pub fn service_key(&self) -> &str {
        &self.service_key
    }

    /// Display label for the service (e.g. `DYNAMODB`, `KINESIS`).
    pub fn service_name(&self) -> &str {
        &self.service_name
    }

    fn build_haystack(&self) -> String {
        let mut haystack = String::with_capacity(256 + self.path.len());
        haystack.push_str(&self.service);
        haystack.push('\n');
        haystack.push_str(&self.operation);
        haystack.push('\n');
        haystack.push_str(&self.method);
        haystack.push('\n');
        haystack.push_str(&self.path);
        haystack.push('\n');
        haystack.push_str(&self.host);
        haystack.push('\n');
        haystack.push_str(&self.status.to_string());
        haystack.push(' ');
        haystack.push_str(status_text(self.status));
        haystack.push('\n');
        if let Some(id) = self.id.as_deref() {
            haystack.push_str(id);
            haystack.push('\n');
        }
        for exchange in [self.request.as_ref(), self.response.as_ref()]
            .into_iter()
            .flatten()
        {
            if let Some(content_type) = exchange.content_type.as_deref() {
                haystack.push_str(content_type);
                haystack.push('\n');
            }
            if let Some(body) = exchange.body.as_ref()
                && let Some(text) = body.text.as_deref()
            {
                haystack.push_str(clip_utf8(text, SEARCH_BODY_BYTES));
                haystack.push('\n');
            }
        }
        haystack.to_lowercase()
    }

    fn searchable(&self) -> Cow<'_, str> {
        if self.haystack.is_empty() {
            Cow::Owned(self.build_haystack())
        } else {
            Cow::Borrowed(&self.haystack)
        }
    }

    /// Stable identity used to keep a selection across reloads.
    pub fn key(&self) -> String {
        self.id.clone().unwrap_or_else(|| {
            format!(
                "{}|{}|{}|{}|{}",
                self.at, self.method, self.path, self.operation, self.status
            )
        })
    }
}

fn deserialize_ms<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    struct MsVisitor;

    impl<'de> Visitor<'de> for MsVisitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a non-negative duration in milliseconds")
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
            Ok(value)
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
            Ok(value.max(0) as u64)
        }

        fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
            if value.is_finite() && value > 0.0 {
                Ok(value.round().min(u64::MAX as f64) as u64)
            } else {
                Ok(0)
            }
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            Ok(value
                .trim()
                .parse::<f64>()
                .map(|v| v.max(0.0) as u64)
                .unwrap_or(0))
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(0)
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(0)
        }
    }

    deserializer.deserialize_any(MsVisitor)
}

/// Parse a single JSONL row. Malformed lines are ignored, never fatal.
pub fn parse_line(line: &str) -> Option<Trace> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let trace: Trace = serde_json::from_str(line).ok()?;
    if trace.at.is_empty() || (trace.method.is_empty() && trace.path.is_empty()) {
        return None;
    }
    Some(trace.finalize())
}

#[cfg(test)]
pub fn parse_lines(text: &str) -> Vec<Trace> {
    parse_counted(text).0
}

fn parse_counted(text: &str) -> (Vec<Trace>, usize) {
    let mut rows = Vec::new();
    let mut bad = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Some(trace) => rows.push(trace),
            None => bad += 1,
        }
    }
    (rows, bad)
}

pub fn cap_tail(mut rows: Vec<Trace>) -> Vec<Trace> {
    if rows.len() > MAX_ROWS {
        rows.drain(..rows.len() - MAX_ROWS);
    }
    rows
}

pub fn clip_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Character-budget truncation with a real ellipsis, for single-line UI cells.
pub fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut shortened: String = text.chars().take(max_chars - 1).collect();
    shortened.push('…');
    shortened
}

// ---------------------------------------------------------------------------
// Live tail
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum TailEvent {
    /// The trace file does not exist (yet).
    Missing,
    /// The trace file exists but could not be read.
    Error(String),
    /// No new data.
    Idle,
    /// New rows were appended.
    Appended(Vec<Trace>),
    /// The file was created, truncated or replaced; this is the new tail.
    Replaced(Vec<Trace>),
}

#[derive(Clone, Debug)]
pub struct Tailer {
    path: PathBuf,
    offset: u64,
    carry: String,
    file_key: u64,
    ready: bool,
    skipped: usize,
}

impl Tailer {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            carry: String::new(),
            file_key: 0,
            ready: false,
            skipped: 0,
        }
    }

    /// Malformed lines skipped since the last full reload.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    /// One non-blocking-as-possible poll. Intended to run on a background executor.
    pub fn poll(&mut self) -> TailEvent {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.ready = false;
                return TailEvent::Missing;
            }
            Err(error) => return TailEvent::Error(short_io_error(&error)),
        };
        if metadata.is_dir() {
            return TailEvent::Error("trace path is a directory".to_string());
        }

        let len = metadata.len();
        let key = file_key(&metadata);
        let replaced = !self.ready
            || len < self.offset
            || (self.file_key != 0 && key != 0 && self.file_key != key);

        if replaced {
            self.reload(len, key)
        } else if len == self.offset {
            TailEvent::Idle
        } else {
            self.read_new()
        }
    }

    fn reload(&mut self, len: u64, key: u64) -> TailEvent {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) => return TailEvent::Error(short_io_error(&error)),
        };
        let start = len.saturating_sub(INITIAL_TAIL_BYTES);
        if let Err(error) = file.seek(SeekFrom::Start(start)) {
            return TailEvent::Error(short_io_error(&error));
        }
        let mut bytes = Vec::new();
        if let Err(error) = file.read_to_end(&mut bytes) {
            return TailEvent::Error(short_io_error(&error));
        }

        self.offset = start + bytes.len() as u64;
        self.file_key = key;
        self.carry.clear();
        self.ready = true;

        let text = String::from_utf8_lossy(&bytes);
        let text: Cow<'_, str> = if start > 0 {
            match text.find('\n') {
                Some(ix) => Cow::Owned(text[ix + 1..].to_string()),
                None => Cow::Owned(String::new()),
            }
        } else {
            text
        };
        let (rows, bad) = parse_counted(&text);
        self.skipped = bad;
        TailEvent::Replaced(cap_tail(rows))
    }

    fn read_new(&mut self) -> TailEvent {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) => return TailEvent::Error(short_io_error(&error)),
        };
        if let Err(error) = file.seek(SeekFrom::Start(self.offset)) {
            return TailEvent::Error(short_io_error(&error));
        }
        let mut bytes = Vec::new();
        if let Err(error) = file.read_to_end(&mut bytes) {
            return TailEvent::Error(short_io_error(&error));
        }
        if bytes.is_empty() {
            return TailEvent::Idle;
        }

        self.offset += bytes.len() as u64;
        self.carry.push_str(&String::from_utf8_lossy(&bytes));

        let Some(newline) = self.carry.rfind('\n') else {
            if self.carry.len() > MAX_CARRY_BYTES {
                self.carry.clear();
            }
            return TailEvent::Idle;
        };
        let complete: String = self.carry.drain(..=newline).collect();
        let (rows, bad) = parse_counted(&complete);
        self.skipped += bad;
        if rows.is_empty() {
            TailEvent::Idle
        } else {
            TailEvent::Appended(cap_tail(rows))
        }
    }
}

fn short_io_error(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        io::ErrorKind::NotFound => "trace file not found".to_string(),
        _ => error.to_string(),
    }
}

#[cfg(unix)]
fn file_key(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn file_key(_metadata: &fs::Metadata) -> u64 {
    0
}

// ---------------------------------------------------------------------------
// Services, filters and search
// ---------------------------------------------------------------------------

/// Normalize raw SigV4 / endpoint service names into stable filter identities.
/// Unknown services keep their own name, so every service gets its own chip.
pub fn service_id(service: &str) -> String {
    let lowered = service.trim().to_ascii_lowercase();
    let name = lowered
        .strip_prefix("amazon ")
        .or_else(|| lowered.strip_prefix("aws "))
        .unwrap_or(&lowered);
    match name {
        "" | "aws" => "aws".to_string(),
        "ddb" | "dynamo" => "dynamodb".to_string(),
        "monitoring" => "cloudwatch".to_string(),
        other => other.to_string(),
    }
}

/// Human label for any service, known or not.
pub fn service_label(service: &str) -> String {
    match service_id(service).as_str() {
        "s3" => "S3".to_string(),
        "sqs" => "SQS".to_string(),
        "dynamodb" => "DYNAMODB".to_string(),
        "sns" => "SNS".to_string(),
        "lambda" => "LAMBDA".to_string(),
        "cloudwatch" => "CLOUDWATCH".to_string(),
        "sts" => "STS".to_string(),
        "iam" => "IAM".to_string(),
        "aws" => "AWS".to_string(),
        _ => {
            let display = service.trim();
            let display = if display.is_empty() { "AWS" } else { display };
            clip_utf8(display, 28).to_ascii_uppercase()
        }
    }
}

pub fn operation_label(trace: &Trace) -> &str {
    let operation = trace.operation.trim();
    if !operation.is_empty() {
        operation
    } else if !trace.method.trim().is_empty() {
        trace.method.trim()
    } else {
        "—"
    }
}

pub fn display_path(trace: &Trace) -> &str {
    if trace.path.is_empty() {
        "/"
    } else {
        trace.path.as_str()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceFacet {
    pub id: String,
    pub label: String,
    pub count: usize,
}

/// Services actually present in the loaded trace, sorted stably by label.
/// Nothing here assumes a fixed list of AWS services.
pub fn service_facets<'a>(rows: impl IntoIterator<Item = &'a Trace>) -> Vec<ServiceFacet> {
    let mut facets: HashMap<String, ServiceFacet> = HashMap::new();
    for trace in rows {
        let id = trace.service_key();
        if let Some(facet) = facets.get_mut(id) {
            facet.count += 1;
        } else {
            facets.insert(
                id.to_string(),
                ServiceFacet {
                    id: id.to_string(),
                    label: trace.service_name().to_string(),
                    count: 1,
                },
            );
        }
    }
    let mut facets: Vec<ServiceFacet> = facets.into_values().collect();
    facets.sort_by(|left, right| {
        left.label
            .cmp(&right.label)
            .then_with(|| left.id.cmp(&right.id))
    });
    facets
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Errors,
    Service(String),
}

impl Filter {
    pub fn matches(&self, trace: &Trace) -> bool {
        match self {
            Filter::All => true,
            Filter::Errors => status_is_error(trace.status),
            Filter::Service(key) => trace.service_key() == key,
        }
    }
}

pub fn search_tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(|token| token.to_lowercase())
        .collect()
}

pub fn matches_search(trace: &Trace, tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return true;
    }
    let haystack = trace.searchable();
    tokens.iter().all(|token| haystack.contains(token.as_str()))
}

// ---------------------------------------------------------------------------
// Display helpers
// ---------------------------------------------------------------------------

pub fn status_is_error(status: u16) -> bool {
    status >= 400
}

pub fn status_is_success(status: u16) -> bool {
    (200..400).contains(&status)
}

pub fn status_text(status: u16) -> &'static str {
    match status {
        0 => "unknown",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        411 => "Length Required",
        412 => "Precondition Failed",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "",
    }
}

pub fn status_label(status: u16) -> String {
    if status == 0 {
        "—".to_string()
    } else {
        status.to_string()
    }
}

pub fn format_latency(ms: u64) -> String {
    if ms == 0 {
        "<1 ms".to_string()
    } else if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        trim_zero(format!("{:.2}", ms as f64 / 1_000.0), "s")
    } else {
        let seconds = ms / 1_000;
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}

fn trim_zero(value: String, unit: &str) -> String {
    let value = value
        .strip_suffix(".00")
        .or_else(|| value.strip_suffix('0'))
        .unwrap_or(&value);
    let value = value.strip_suffix('.').unwrap_or(value);
    format!("{value} {unit}")
}

pub fn format_bytes(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if (bytes as f64) < MB {
        trim_zero(format!("{:.1}", bytes as f64 / KB), "KB")
    } else if (bytes as f64) < GB {
        trim_zero(format!("{:.1}", bytes as f64 / MB), "MB")
    } else {
        trim_zero(format!("{:.1}", bytes as f64 / GB), "GB")
    }
}

/// `2026-11-15T02:24:19.540507869+00:00` -> `02:24:19.540`.
pub fn short_clock(at: &str) -> String {
    let time = at
        .split_once('T')
        .or_else(|| at.split_once(' '))
        .map(|(_, time)| time)
        .unwrap_or(at);
    let time = time.split(['+', '-', 'Z']).next().unwrap_or(time).trim();
    if time.is_empty() {
        "—".to_string()
    } else {
        time.chars().take(12).collect()
    }
}

/// `2026-11-15 02:24:19.540`
pub fn full_time(at: &str) -> String {
    match at.split_once('T').or_else(|| at.split_once(' ')) {
        Some((date, time)) => format!("{date} {}", short_clock(time)),
        None => short_clock(at),
    }
}

pub fn headers_to_text(headers: &[Header]) -> String {
    let mut out = String::new();
    for header in headers {
        let _ = writeln!(out, "{}: {}", header.name, header.value);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample(
        at: &str,
        method: &str,
        path: &str,
        service: &str,
        operation: &str,
        status: u16,
        ms: u64,
        body: Option<&str>,
    ) -> String {
        let mut line = format!(
            r#"{{"at":"{at}","method":"{method}","path":"{path}","host":"127.0.0.1:9297","service":"{service}","operation":"{operation}","status":{status},"ms":{ms},"authorization":true"#
        );
        if let Some(body) = body {
            line.push_str(&format!(
                r#","request":{{"headers":[{{"name":"content-type","value":"application/json"}}],"content_type":"application/json","body":{{"text":{body:?},"bytes":{},"truncated":false,"binary":false}}}}"#,
                body.len()
            ));
        }
        line.push('}');
        line
    }

    fn trace(status: u16, service: &str, operation: &str, body: Option<&str>) -> Trace {
        parse_line(&sample(
            "2026-11-15T02:24:19.540507869+00:00",
            "POST",
            "/",
            service,
            operation,
            status,
            8,
            body,
        ))
        .expect("sample parses")
    }

    #[test]
    fn parses_minimal_contract() {
        let trace = parse_line(
            r#"{"at":"2026-11-15T02:24:19.540Z","method":"GET","path":"/demo/a.txt","host":"127.0.0.1:9297","service":"s3","operation":"GetObject","status":200,"ms":3,"authorization":false}"#,
        )
        .expect("parses");
        assert_eq!(trace.status, 200);
        assert_eq!(trace.ms, 3);
        assert!(trace.request.is_none());
        assert!(trace.response.is_none());
        assert!(!trace.authorization);
        assert_eq!(trace.service_name(), "S3");
    }

    #[test]
    fn parses_optional_request_and_response() {
        let line = r#"{"at":"2026-11-15T02:24:19.540507869+00:00","method":"PUT","path":"/demo/a.png?x-id=PutObject","host":"127.0.0.1:9297","service":"s3","operation":"PutObject","status":200,"ms":8,"authorization":true,"request":{"headers":[{"name":"authorization","value":"AWS4-HMAC-SHA256 Credential=demo/20261115/s3/aws4_request"}],"content_type":"image/png","body":{"text":"hello","bytes":5,"truncated":true,"binary":false,"sha256":"abc123"}},"response":{"headers":[],"body":{"text":"","bytes":0,"truncated":false,"binary":true}}}"#;
        let trace = parse_line(line).expect("parses");
        let request = trace.request.expect("request");
        assert_eq!(request.headers.len(), 1);
        assert_eq!(
            request.headers[0].value,
            "AWS4-HMAC-SHA256 Credential=demo/20261115/s3/aws4_request"
        );
        assert_eq!(request.content_type.as_deref(), Some("image/png"));
        let body = request.body.expect("body");
        assert_eq!(body.size(), 5);
        assert!(body.truncated);
        assert_eq!(body.sha256.as_deref(), Some("abc123"));
        let response = trace.response.expect("response");
        assert!(response.body.expect("body").binary);
    }

    #[test]
    fn skips_malformed_lines() {
        let text = [
            sample(
                "2026-11-15T02:24:19Z",
                "GET",
                "/ok",
                "s3",
                "GetObject",
                200,
                2,
                None,
            ),
            "not json".to_string(),
            r#"{"status": 200}"#.to_string(),
            r#"{"at": "2026-11-15T02:24:20Z"}"#.to_string(),
            sample(
                "2026-11-15T02:24:20Z",
                "GET",
                "/ok2",
                "s3",
                "GetObject",
                200,
                2,
                None,
            ),
        ]
        .join("\n");
        let rows = parse_lines(&text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].path, "/ok2");
    }

    #[test]
    fn tolerates_legacy_and_extra_fields() {
        let line = r#"{"id":"abc","at":"2026-11-15T02:24:19Z","method":"GET","path":"/","host":"h","service":"sqs","operation":"SendMessage","status":200,"ms":4,"authorization":true,"request":{"headers":[],"body":{"text":"x","bytes":1,"truncated":false,"binary":false,"redacted":true}},"future_field":{"a":1}}"#;
        let trace = parse_line(line).expect("parses");
        assert_eq!(trace.id.as_deref(), Some("abc"));
        assert_eq!(trace.key(), "abc");
        assert_eq!(trace.service_name(), "SQS");
    }

    #[test]
    fn accepts_float_and_string_latency() {
        let line = r#"{"at":"t","method":"GET","path":"/","ms":12.6}"#;
        assert_eq!(parse_line(line).expect("parses").ms, 13);
        let line = r#"{"at":"t","method":"GET","path":"/","ms":"7"}"#;
        assert_eq!(parse_line(line).expect("parses").ms, 7);
    }

    #[test]
    fn search_matches_each_field_and_body() {
        let s3 = trace(404, "s3", "GetObject", Some("NoSuchKey: missing"));
        let sqs = trace(200, "sqs", "SendMessage", Some("thumbnail"));

        let tokens = search_tokens("s3 getobject");
        assert!(matches_search(&s3, &tokens));
        assert!(!matches_search(&sqs, &tokens));

        assert!(matches_search(&s3, &search_tokens("404")));
        assert!(!matches_search(&sqs, &search_tokens("404")));
        assert!(matches_search(&s3, &search_tokens("NOSUCHKEY")));
        assert!(matches_search(&s3, &search_tokens("/")));
        assert!(matches_search(&sqs, &search_tokens("thumbnail")));
        assert!(matches_search(&s3, &[]));
        // Any service name is searchable, including ones with no dedicated chip.
        let kinesis = trace(200, "kinesis", "PutRecord", None);
        assert!(matches_search(
            &kinesis,
            &search_tokens("kinesis putrecord")
        ));
    }

    #[test]
    fn filters_errors_and_services() {
        let s3 = trace(404, "s3", "GetObject", None);
        let sqs = trace(200, "sqs", "SendMessage", None);
        let ddb = trace(200, "dynamodb", "PutItem", None);

        assert!(Filter::All.matches(&s3));
        assert!(Filter::Errors.matches(&s3));
        assert!(!Filter::Errors.matches(&sqs));
        assert!(Filter::Service(service_id("s3")).matches(&s3));
        assert!(!Filter::Service(service_id("s3")).matches(&sqs));
        assert!(Filter::Service(service_id("sqs")).matches(&sqs));
        assert!(Filter::Service(service_id("dynamodb")).matches(&ddb));
    }

    #[test]
    fn filters_any_present_service() {
        // IAM and Lambda are not special-cased by the chip list.
        let iam = trace(200, "iam", "GetUser", None);
        let iam_again = trace(200, "iam", "ListUsers", None);
        let lambda = trace(202, "lambda", "Invoke", None);
        let s3 = trace(200, "s3", "GetObject", None);
        let rows = vec![iam, iam_again, lambda, s3];

        let facets = service_facets(rows.iter());
        let labels: Vec<&str> = facets.iter().map(|facet| facet.label.as_str()).collect();
        assert_eq!(labels, vec!["IAM", "LAMBDA", "S3"]);
        assert_eq!(
            facets
                .iter()
                .find(|facet| facet.id == "iam")
                .map(|facet| facet.count),
            Some(2)
        );

        let iam_filter = Filter::Service(service_id("iam"));
        assert!(iam_filter.matches(&rows[0]));
        assert!(!iam_filter.matches(&rows[2]));
        assert!(Filter::Service(service_id("lambda")).matches(&rows[2]));

        // A service with no dedicated styling still gets a real filter + label.
        let kinesis = trace(200, "kinesis", "PutRecord", None);
        let facets = service_facets([&kinesis].into_iter());
        assert_eq!(facets.len(), 1);
        assert_eq!(facets[0].label, "KINESIS");
        let kinesis_filter = Filter::Service(service_id("kinesis"));
        assert!(kinesis_filter.matches(&kinesis));
        assert!(!kinesis_filter.matches(&rows[3]));
    }

    #[test]
    fn labels_services() {
        assert_eq!(service_label("s3"), "S3");
        assert_eq!(service_label("Amazon S3"), "S3");
        assert_eq!(service_label("dynamodb"), "DYNAMODB");
        assert_eq!(service_label("ddb"), "DYNAMODB");
        assert_eq!(service_label(""), "AWS");
        assert_eq!(service_label("kinesis"), "KINESIS");
        assert_eq!(service_label("monitoring"), "CLOUDWATCH");
        assert_eq!(service_id("monitoring"), "cloudwatch");
        let t = trace(200, "kinesis", "PutRecord", None);
        assert_eq!(t.service_name(), "KINESIS");
    }

    #[test]
    fn ellipsizes_long_cells() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("abcdef", 4), "abc…");
        assert_eq!(ellipsize("héllo wörld", 6), "héllo…");
        assert_eq!(ellipsize("anything", 0), "");
    }

    #[test]
    fn formats_clocks() {
        assert_eq!(
            short_clock("2026-11-15T02:24:19.540507869+00:00"),
            "02:24:19.540"
        );
        assert_eq!(short_clock("2026-11-15T02:24:19Z"), "02:24:19");
        assert_eq!(short_clock("02:24:19.540"), "02:24:19.540");
        assert_eq!(short_clock(""), "—");
        assert_eq!(
            full_time("2026-11-15T02:24:19.540507869+00:00"),
            "2026-11-15 02:24:19.540"
        );
    }

    #[test]
    fn formats_latency() {
        assert_eq!(format_latency(0), "<1 ms");
        assert_eq!(format_latency(8), "8 ms");
        assert_eq!(format_latency(999), "999 ms");
        assert_eq!(format_latency(1_240), "1.24 s");
        assert_eq!(format_latency(1_500), "1.5 s");
        assert_eq!(format_latency(65_000), "1m 05s");
    }

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1_536), "1.5 KB");
        assert_eq!(format_bytes(184_320), "180 KB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2 MB");
    }

    #[test]
    fn formats_status() {
        assert_eq!(status_label(200), "200");
        assert_eq!(status_label(0), "—");
        assert_eq!(status_text(200), "OK");
        assert_eq!(status_text(404), "Not Found");
        assert_eq!(status_text(299), "");
        assert!(status_is_success(204));
        assert!(status_is_success(301));
        assert!(!status_is_success(404));
        assert!(!status_is_error(399));
        assert!(status_is_error(400));
        assert!(status_is_error(500));
    }

    #[test]
    fn parses_multiline_body_search() {
        let t = trace(200, "sqs", "SendMessage", Some("line one\nline two"));
        assert!(matches_search(&t, &search_tokens("line two")));
    }

    #[test]
    fn headers_copy_as_lines() {
        let headers = vec![
            Header {
                name: "host".into(),
                value: "127.0.0.1:9297".into(),
            },
            Header {
                name: "authorization".into(),
                value: "AWS4-HMAC-SHA256 ...".into(),
            },
        ];
        assert_eq!(
            headers_to_text(&headers),
            "host: 127.0.0.1:9297\nauthorization: AWS4-HMAC-SHA256 ...\n"
        );
    }

    // -- tailer -------------------------------------------------------------

    fn temp_trace(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "lenslet-{}-{name}-{nanos}.jsonl",
            std::process::id()
        ))
    }

    fn write_append(path: &Path, text: &str) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open trace");
        file.write_all(text.as_bytes()).expect("write trace");
    }

    #[test]
    fn tailer_loads_appends_and_holds_partial_lines() {
        let path = temp_trace("append");
        let first = sample(
            "2026-11-15T02:24:19Z",
            "GET",
            "/one",
            "s3",
            "GetObject",
            200,
            2,
            None,
        );
        let second = sample(
            "2026-11-15T02:24:20Z",
            "POST",
            "/two",
            "sqs",
            "SendMessage",
            200,
            3,
            None,
        );
        write_append(&path, &format!("{first}\n"));

        let mut tailer = Tailer::new(&path);
        match tailer.poll() {
            TailEvent::Replaced(rows) => assert_eq!(rows.len(), 1),
            other => panic!("expected replace, got {other:?}"),
        }

        // A partial line is withheld until its newline arrives.
        write_append(&path, &second[..20]);
        assert!(matches!(tailer.poll(), TailEvent::Idle));
        write_append(&path, &format!("{}\n", &second[20..]));
        match tailer.poll() {
            TailEvent::Appended(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].path, "/two");
            }
            other => panic!("expected append, got {other:?}"),
        }
        assert!(matches!(tailer.poll(), TailEvent::Idle));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn tailer_detects_truncation() {
        let path = temp_trace("truncate");
        let line = sample(
            "2026-11-15T02:24:19Z",
            "GET",
            "/one",
            "s3",
            "GetObject",
            200,
            2,
            None,
        );
        write_append(&path, &format!("{line}\n{line}\n"));
        let mut tailer = Tailer::new(&path);
        assert!(matches!(tailer.poll(), TailEvent::Replaced(rows) if rows.len() == 2));

        fs::write(&path, format!("{line}\n")).expect("truncate");
        match tailer.poll() {
            TailEvent::Replaced(rows) => assert_eq!(rows.len(), 1),
            other => panic!("expected replace after truncation, got {other:?}"),
        }
        let _ = fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn tailer_detects_replacement() {
        let path = temp_trace("replace");
        let before = sample(
            "2026-11-15T02:24:19Z",
            "GET",
            "/before",
            "s3",
            "GetObject",
            200,
            2,
            None,
        );
        let after = sample(
            "2026-11-15T02:24:20Z",
            "GET",
            "/after",
            "iam",
            "GetUser",
            200,
            2,
            None,
        );
        fs::write(&path, format!("{before}\n")).expect("write");
        let mut tailer = Tailer::new(&path);
        assert!(matches!(tailer.poll(), TailEvent::Replaced(rows) if rows.len() == 1));

        let replacement = temp_trace("replace-new");
        fs::write(&replacement, format!("{after}\n")).expect("write replacement");
        fs::rename(&replacement, &path).expect("replace file");
        match tailer.poll() {
            TailEvent::Replaced(rows) => {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].path, "/after");
            }
            other => panic!("expected replace after inode change, got {other:?}"),
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn tailer_reports_missing_file() {
        let path = temp_trace("missing");
        let mut tailer = Tailer::new(&path);
        assert!(matches!(tailer.poll(), TailEvent::Missing));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn tailer_skips_bad_lines_and_caps_tail() {
        let path = temp_trace("cap");
        let line = sample(
            "2026-11-15T02:24:19Z",
            "GET",
            "/one",
            "s3",
            "GetObject",
            200,
            2,
            None,
        );
        let mut text = String::new();
        for _ in 0..MAX_ROWS + 5 {
            text.push_str(&line);
            text.push('\n');
        }
        text.push_str("garbage\n");
        fs::write(&path, text).expect("write");
        let mut tailer = Tailer::new(&path);
        match tailer.poll() {
            TailEvent::Replaced(rows) => assert_eq!(rows.len(), MAX_ROWS),
            other => panic!("expected replace, got {other:?}"),
        }
        assert_eq!(tailer.skipped(), 1);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn demo_rows_cover_several_services() {
        let rows = crate::demo::demo_rows();
        let facets = service_facets(rows.iter());
        assert!(facets.iter().any(|facet| facet.id == "s3"));
        assert!(facets.iter().any(|facet| facet.id == "kinesis"));
        assert!(facets.len() >= 5);
    }
}
