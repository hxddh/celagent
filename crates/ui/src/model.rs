//! The window's view of a run file: tolerant row parsing, display helpers,
//! filters, curl export and payload decoding.
//!
//! The run schema itself lives in `lenslet_core::trace`. A view row ([`Trace`])
//! deserializes exactly that schema but tolerates absent fields, and carries
//! UI-only state: where its full line lives on disk (large bodies are shed
//! from memory and hydrated on demand) and a precomputed search haystack.

use std::{
    borrow::Cow,
    fmt::Write as _,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
    sync::Arc,
};

pub use lenslet_core::{
    access::{Access, CallAccess},
    problem::Problem,
    summary::Report,
    trace::{HeaderTrace as Header, RunHeader},
};
use serde::Deserialize;

use crate::fsguard;

pub const MAX_ROWS: usize = 5_000;
/// Bodies larger than this are shed from the in-memory row and hydrated from
/// the run file when the call is inspected.
pub const INLINE_BODY_LIMIT: usize = 8 * 1024;
/// Body text contributed to the search index (per request/response).
const SEARCH_BODY_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Body {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub hex: Option<String>,
    #[serde(default)]
    pub encoding: Option<String>,
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

/// One line of a run file, as the window reads it.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Record {
    Run(RunHeader),
    Call(Box<Trace>),
    End(Box<Report>),
}

/// A run as the window shows it: its header, plus the report once it ended.
#[derive(Clone, Debug, Default)]
pub struct RunInfo {
    pub header: RunHeader,
    pub report: Option<Report>,
}

impl RunInfo {
    pub fn command_line(&self) -> String {
        self.header.command.join(" ")
    }
}

/// Where a row's complete line lives in its run file.
#[derive(Clone, Debug)]
pub struct LineRef {
    pub path: Arc<PathBuf>,
    pub offset: u64,
    pub len: usize,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Trace {
    #[serde(default)]
    pub id: String,
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
    #[serde(default)]
    pub ms: u64,
    #[serde(default)]
    pub authorization: bool,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub invocation_id: Option<String>,
    #[serde(default)]
    pub attempt: Option<u32>,
    #[serde(default)]
    pub max_attempts: Option<u32>,
    /// Still running when written; a later line with the same id replaces it.
    #[serde(default)]
    pub pending: bool,
    /// Proxy-side failure (the emulator was unreachable, ...).
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub problem: Option<Problem>,
    /// The IAM permissions this call needs (several for batches,
    /// transactions and copies).
    #[serde(default)]
    pub access: Vec<CallAccess>,
    /// Permissions the call implies elsewhere (SSE-KMS key, passed role).
    #[serde(default)]
    pub implied: Vec<CallAccess>,
    /// The call used access that is not in the lock file.
    #[serde(default)]
    pub new: bool,
    /// Blocked locally by `--enforce` before reaching the emulator.
    #[serde(default)]
    pub blocked: bool,
    /// Re-sent from the window: the id of the call this replays.
    #[serde(default)]
    pub replay_of: Option<String>,
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
    #[serde(skip)]
    at_ms: Option<i64>,
    #[serde(skip)]
    pub(crate) line_ref: Option<LineRef>,
    #[serde(skip)]
    dehydrated: bool,
}

impl Trace {
    /// Shed bodies over [`INLINE_BODY_LIMIT`] from memory, remembering where
    /// the full line is.
    pub fn dehydrate(&mut self, lref: LineRef) {
        let mut shed = false;
        for ex in [self.request.as_mut(), self.response.as_mut()]
            .into_iter()
            .flatten()
        {
            if let Some(body) = ex.body.as_mut()
                && body
                    .text
                    .as_ref()
                    .is_some_and(|text| text.len() > INLINE_BODY_LIMIT)
            {
                body.text = None;
                shed = true;
            }
        }
        self.line_ref = Some(lref);
        self.dehydrated = shed;
    }

    /// Whether a body was shed and must be read back from disk to inspect.
    pub fn is_dehydrated(&self) -> bool {
        self.dehydrated
    }

    /// The row with its full bodies, read back from the run file. Blocking IO:
    /// call it on a background thread.
    pub fn hydrated(&self) -> Option<Trace> {
        fsguard::assert_io_allowed();
        if !self.dehydrated {
            return Some(self.clone());
        }
        let lref = self.line_ref.as_ref()?;
        let read_at = |offset: u64, len: usize| -> Option<Trace> {
            let mut file = File::open(&*lref.path).ok()?;
            file.seek(SeekFrom::Start(offset)).ok()?;
            let mut buf = vec![0u8; len];
            file.read_exact(&mut buf).ok()?;
            match parse_record(&String::from_utf8_lossy(&buf))? {
                Record::Call(full) if full.id == self.id => Some(*full),
                _ => None,
            }
        };
        read_at(lref.offset, lref.len).or_else(|| self.hydrate_by_scan(&lref.path))
    }

    /// Fallback when the recorded offset is stale: find the newest line for
    /// this id anywhere in the file.
    fn hydrate_by_scan(&self, path: &std::path::Path) -> Option<Trace> {
        let bytes = std::fs::read(path).ok()?;
        let needle = format!("\"id\":\"{}\"", self.id);
        let mut found = None;
        for line in bytes.split(|byte| *byte == b'\n') {
            if line.len() > needle.len()
                && memfind(line, needle.as_bytes())
                && let Some(Record::Call(full)) = parse_record(&String::from_utf8_lossy(line))
                && full.id == self.id
            {
                found = Some(*full);
            }
        }
        found
    }

    pub(crate) fn finalize(mut self) -> Self {
        if let Some(request) = self.request.as_ref() {
            if self.invocation_id.is_none() {
                self.invocation_id = request
                    .header("amz-sdk-invocation-id")
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string);
            }
            if self.attempt.is_none()
                && let Some(raw) = request.header("amz-sdk-request")
            {
                let (attempt, max) = lenslet_core::wire::parse_sdk_request(raw);
                if attempt.is_some_and(|n| n > 1) {
                    self.attempt = attempt;
                    self.max_attempts = max;
                }
            }
        }
        self.at_ms = parse_rfc3339_ms(&self.at);
        self.service_key = service_id(&self.service);
        self.service_name = service_label(&self.service);
        self.haystack = self.build_haystack();
        self
    }

    pub fn at_ms(&self) -> Option<i64> {
        self.at_ms
    }

    pub fn is_retry(&self) -> bool {
        self.attempt.is_some_and(|attempt| attempt > 1)
    }

    pub fn is_slow(&self) -> bool {
        self.pending || self.ms >= 500
    }

    pub fn retry_badge(&self) -> Option<String> {
        let attempt = self.attempt.filter(|n| *n > 1)?;
        Some(match self.max_attempts {
            Some(max) => format!("RETRY {attempt}/{max}"),
            None => format!("RETRY {attempt}"),
        })
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
        for part in [
            self.service.as_str(),
            &self.operation,
            &self.method,
            &self.path,
            &self.host,
        ] {
            haystack.push_str(part);
            haystack.push('\n');
        }
        let _ = writeln!(haystack, "{} {}", self.status, status_text(self.status));
        if let Some(problem) = self.problem.as_ref() {
            haystack.push_str(&problem.code);
            haystack.push('\n');
            if let Some(message) = problem.message.as_deref() {
                haystack.push_str(message);
                haystack.push('\n');
            }
        }
        for extra in [self.region.as_deref(), self.invocation_id.as_deref()]
            .into_iter()
            .flatten()
        {
            haystack.push_str(extra);
            haystack.push('\n');
        }
        if self.is_retry() {
            haystack.push_str("retry retried\n");
        }
        if self.new {
            haystack.push_str("new\n");
        }
        if self.blocked {
            haystack.push_str("blocked denied enforce\n");
        }
        haystack.push_str(&self.id);
        haystack.push('\n');
        for access in self.access() {
            let _ = writeln!(haystack, "{} {}", access.action, access.resource);
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

    /// A failed call: an HTTP error, or a problem the proxy recorded.
    pub fn is_error(&self) -> bool {
        status_is_error(self.status) || self.problem.is_some()
    }

    /// Signed with a presigned URL rather than an Authorization header.
    pub fn presigned(&self) -> bool {
        self.path.contains("X-Amz-Signature=")
            || (self.path.contains("AWSAccessKeyId=") && self.path.contains("Signature="))
    }

    /// Every permission this call accounts for: its own, then implied ones.
    pub fn access(&self) -> impl Iterator<Item = Access> + '_ {
        self.access
            .iter()
            .chain(self.implied.iter())
            .map(CallAccess::key)
    }

    /// Every permission on this call paired with whether it is implied.
    pub fn call_access(&self) -> impl Iterator<Item = (&CallAccess, bool)> + '_ {
        self.access
            .iter()
            .map(|entry| (entry, false))
            .chain(self.implied.iter().map(|entry| (entry, true)))
    }

    /// Whether a specific permission on this call is marked new (falling back
    /// to `self.new` for single-permission calls from older trace files).
    pub fn is_access_new(&self, entry: &CallAccess) -> bool {
        entry.new || (self.new && self.access.len() + self.implied.len() == 1)
    }

    pub fn uses(&self, access: &Access) -> bool {
        self.access
            .iter()
            .chain(self.implied.iter())
            .any(|own| own.action == access.action && own.resource == access.resource)
    }

    /// The failure identity `lenslet` compares across runs.
    pub fn failure_kind(&self) -> Option<lenslet_core::summary::FailureKind> {
        let problem = self.problem.as_ref()?;
        Some(lenslet_core::summary::FailureKind {
            service: self.service.clone(),
            operation: self.operation.clone(),
            code: problem.code.clone(),
        })
    }

    /// The same call as `lenslet_core` sees it, without bodies (for findings).
    pub fn to_core(&self) -> lenslet_core::trace::Trace {
        let message = |exchange: Option<&Exchange>| lenslet_core::trace::Message {
            headers: exchange.map(|e| e.headers.clone()).unwrap_or_default(),
            content_type: exchange.and_then(|e| e.content_type.clone()),
            body: None,
        };
        lenslet_core::trace::Trace {
            id: self.id.clone(),
            at: self.at.clone(),
            method: self.method.clone(),
            path: self.path.clone(),
            host: self.host.clone(),
            service: self.service.clone(),
            operation: self.operation.clone(),
            status: self.status,
            ms: self.ms,
            authorization: self.authorization,
            region: self.region.clone(),
            invocation_id: self.invocation_id.clone(),
            attempt: self.attempt,
            max_attempts: self.max_attempts,
            pending: self.pending,
            error: self.error.clone(),
            problem: self.problem.clone(),
            access: self.access.clone(),
            implied: self.implied.clone(),
            new: self.new,
            blocked: self.blocked,
            replay_of: self.replay_of.clone(),
            request: message(self.request.as_ref()),
            response: message(self.response.as_ref()),
        }
    }
}

fn memfind(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Parse one run-file line. Calls without an id or time are rejected.
pub fn parse_record(line: &str) -> Option<Record> {
    match serde_json::from_str(line.trim()).ok()? {
        Record::Call(trace) => {
            let trace = *trace;
            if trace.id.is_empty() || trace.at.is_empty() {
                return None;
            }
            Some(Record::Call(Box::new(trace.finalize())))
        }
        other => Some(other),
    }
}

/// Parse one `call` line. Other records and malformed lines are `None`.
#[cfg(test)]
pub fn parse_line(line: &str) -> Option<Trace> {
    match parse_record(line)? {
        Record::Call(trace) => Some(*trace),
        _ => None,
    }
}

/// What a chunk of a run file holds.
#[derive(Default)]
pub struct Parsed {
    pub calls: Vec<Trace>,
    pub header: Option<RunHeader>,
    pub report: Option<Report>,
    pub bad: usize,
}

/// Parse complete lines; `base_offset` is where `bytes` starts in the file.
pub fn parse_records_at(bytes: &[u8], base_offset: u64, source: Option<&Arc<PathBuf>>) -> Parsed {
    let mut parsed = Parsed::default();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let rel_end = bytes[pos..]
            .iter()
            .position(|b| *b == b'\n')
            .unwrap_or(bytes.len() - pos);
        let slice = &bytes[pos..pos + rel_end];
        let line_offset = base_offset + pos as u64;
        pos += rel_end + 1;
        if slice.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let line = String::from_utf8_lossy(slice);
        match parse_record(&line) {
            Some(Record::Call(mut trace)) => {
                if let Some(path) = source {
                    trace.dehydrate(LineRef {
                        path: path.clone(),
                        offset: line_offset,
                        len: slice.len(),
                    });
                }
                parsed.calls.push(*trace);
            }
            Some(Record::Run(header)) => parsed.header = Some(header),
            Some(Record::End(report)) => parsed.report = Some(*report),
            None => parsed.bad += 1,
        }
    }
    parsed
}

/// Every call in a run file's text.
pub fn parse_lines(text: &str) -> Vec<Trace> {
    parse_records_at(text.as_bytes(), 0, None).calls
}

/// The run (header and report) in a run file's text.
pub fn parse_run(text: &str) -> RunInfo {
    let parsed = parse_records_at(text.as_bytes(), 0, None);
    RunInfo {
        header: parsed.header.unwrap_or_default(),
        report: parsed.report,
    }
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusFilter {
    #[default]
    All,
    Errors,
    Blocked,
    Retries,
    Slow,
    /// Calls that differ from the baseline (new failures, new access).
    Changed,
}

/// Combinable call filter: status facet AND service AND permission AND time
/// slice AND an explicit id set (a finding's calls).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallFilter {
    pub status: StatusFilter,
    pub service: Option<String>,
    pub access: Option<Access>,
    pub time_window: Option<(i64, i64)>,
    /// Restrict to these call ids, with a label for the chip.
    pub ids: Option<(String, Arc<std::collections::HashSet<String>>)>,
}

impl CallFilter {
    pub fn is_active(&self) -> bool {
        self.status != StatusFilter::All
            || self.service.is_some()
            || self.access.is_some()
            || self.time_window.is_some()
            || self.ids.is_some()
    }

    /// `changed` says whether the call differs from the baseline.
    pub fn matches(&self, trace: &Trace, run_start_ms: Option<i64>, changed: bool) -> bool {
        let status_ok = match self.status {
            StatusFilter::All => true,
            StatusFilter::Errors => trace.is_error(),
            StatusFilter::Blocked => trace.blocked,
            StatusFilter::Retries => trace.is_retry(),
            StatusFilter::Slow => trace.is_slow(),
            StatusFilter::Changed => changed,
        };
        if !status_ok {
            return false;
        }
        if let Some(svc) = self.service.as_deref()
            && trace.service_key() != svc
        {
            return false;
        }
        if let Some(access) = self.access.as_ref()
            && !trace.uses(access)
        {
            return false;
        }
        if let Some((_, ids)) = self.ids.as_ref()
            && !ids.contains(&trace.id)
        {
            return false;
        }
        if let (Some((win_start, win_end)), Some(run_start), Some(call_at)) =
            (self.time_window, run_start_ms, trace.at_ms())
        {
            let rel_start = (call_at - run_start).max(0);
            let rel_end = rel_start + trace.ms as i64;
            if rel_end < win_start || rel_start > win_end {
                return false;
            }
        }
        true
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

/// Indented JSON or XML for display, or `None` when the body is neither (or
/// is a truncated JSON prefix that does not parse).
pub fn pretty_body(text: &str, content_type: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("json") || trimmed.starts_with('{') || trimmed.starts_with('[') {
        let value: serde_json::Value = serde_json::from_str(text).ok()?;
        let pretty = serde_json::to_string_pretty(&value).ok()?;
        return (pretty != text).then_some(pretty);
    }
    if content_type.contains("xml") || trimmed.starts_with('<') {
        let pretty = indent_xml(trimmed)?;
        return (pretty != text).then_some(pretty);
    }
    None
}

/// A small, forgiving XML indenter: one element per line, `<a>text</a>` kept
/// together. Not a parser; it only re-flows whitespace between tags.
fn indent_xml(text: &str) -> Option<String> {
    let mut tokens: Vec<&str> = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.starts_with('<') {
            let end = rest.find('>')? + 1;
            tokens.push(&rest[..end]);
            rest = &rest[end..];
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let chunk = rest[..end].trim();
            if !chunk.is_empty() {
                tokens.push(chunk);
            }
            rest = &rest[end..];
        }
    }
    let opening = |token: &str| {
        token.starts_with('<')
            && !token.starts_with("</")
            && !token.starts_with("<?")
            && !token.starts_with("<!")
            && !token.ends_with("/>")
    };
    let mut out = String::new();
    let mut depth = 0usize;
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        let indent = "  ".repeat(depth);
        if opening(token) {
            let inline = tokens
                .get(index + 1)
                .is_some_and(|next| !next.starts_with('<'))
                && tokens
                    .get(index + 2)
                    .is_some_and(|close| close.starts_with("</"));
            if inline {
                let _ = writeln!(
                    out,
                    "{indent}{token}{}{}",
                    tokens[index + 1],
                    tokens[index + 2]
                );
                index += 3;
                continue;
            }
            let _ = writeln!(out, "{indent}{token}");
            depth += 1;
        } else if token.starts_with("</") {
            depth = depth.saturating_sub(1);
            let _ = writeln!(out, "{}{token}", "  ".repeat(depth));
        } else {
            let _ = writeln!(out, "{indent}{token}");
        }
        index += 1;
    }
    Some(out.trim_end().to_string())
}

fn posix_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Build an exact, unredacted `curl` command reproducing `trace`'s HTTP request,
/// or `None` when the request carried a binary or truncated body whose complete
/// payload cannot be reconstructed from the trace.
pub fn to_curl(trace: &Trace, endpoint: Option<&str>) -> Option<String> {
    if let Some(body) = trace.request.as_ref().and_then(|req| req.body.as_ref())
        && (body.truncated
            || (body.bytes > 0 && body.text.is_none())
            || body
                .encoding
                .as_deref()
                .is_some_and(|e| e.contains("aws-eventstream")))
    {
        return None;
    }
    let base = endpoint
        .filter(|e| !e.is_empty())
        .map(|e| e.trim_end_matches('/').to_string())
        .or_else(|| (!trace.host.is_empty()).then(|| format!("http://{}", trace.host)))
        .unwrap_or_else(|| "http://127.0.0.1:9297".to_string());
    let path = display_path(trace);
    let url = if path.starts_with('/') {
        format!("{base}{path}")
    } else {
        format!("{base}/{path}")
    };
    let method = if trace.method.trim().is_empty() {
        "GET".to_string()
    } else {
        trace.method.trim().to_ascii_uppercase()
    };
    let mut parts = vec![
        "curl".to_string(),
        "-i".to_string(),
        "-X".to_string(),
        method,
        posix_single_quote(&url),
    ];
    if let Some(req) = trace.request.as_ref() {
        let decoded_wire = req
            .body
            .as_ref()
            .is_some_and(|body| body.encoding.is_some());
        let compressed_wire = req
            .body
            .as_ref()
            .and_then(|body| body.encoding.as_deref())
            .is_some_and(|enc| {
                let low = enc.to_ascii_lowercase();
                low.contains("gzip") || low.contains("deflate")
            });
        for header in &req.headers {
            let low = header.name.to_ascii_lowercase();
            // Skip hop-by-hop framing headers, and when replaying a decoded
            // (`gzip` / `deflate` / `aws-chunked`) plaintext payload, strip
            // wire-encoding & trailing-checksum headers that no longer match
            // the decoded `--data-raw` bytes.
            if matches!(low.as_str(), "content-length" | "transfer-encoding") {
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
            if low == "x-amz-content-sha256"
                && (decoded_wire
                    || header
                        .value
                        .trim()
                        .to_ascii_uppercase()
                        .starts_with("STREAMING-"))
            {
                parts.push(format!(
                    "-H {}",
                    posix_single_quote(&format!("{}: UNSIGNED-PAYLOAD", header.name))
                ));
                continue;
            }
            parts.push(format!(
                "-H {}",
                posix_single_quote(&format!("{}: {}", header.name, header.value))
            ));
        }
        if let Some(body) = req.body.as_ref()
            && let Some(text) = body.text.as_deref()
            && !text.is_empty()
        {
            parts.push(format!("--data-raw {}", posix_single_quote(text)));
        }
    }
    Some(parts.join(" \\\n  "))
}

/// Render a lowercase hex string (`89504e47...`) as a classic 16-byte-per-row
/// Hex + ASCII dump (`0000  89 50 4e 47 ...  |.PNG...|`).
pub fn format_hex_dump(hex_str: &str) -> String {
    let clean: Vec<u8> = hex_str.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
    let mut bytes = Vec::with_capacity(clean.len() / 2);
    for pair in clean.as_chunks::<2>().0 {
        if let Ok(s) = std::str::from_utf8(pair)
            && let Ok(val) = u8::from_str_radix(s, 16)
        {
            bytes.push(val);
        }
    }
    if bytes.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for (row_idx, row) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "{:04x}  ", row_idx * 16);
        for (i, byte) in row.iter().enumerate() {
            if i == 8 {
                out.push(' ');
            }
            let _ = write!(out, "{byte:02x} ");
        }
        for i in row.len()..16 {
            if i == 8 {
                out.push(' ');
            }
            out.push_str("   ");
        }
        out.push_str(" |");
        for &byte in row {
            let ch = if (0x20..=0x7e).contains(&byte) {
                byte as char
            } else {
                '.'
            };
            out.push(ch);
        }
        out.push_str("|\n");
    }
    out.trim_end().to_string()
}

/// Parse an RFC3339 timestamp into epoch milliseconds, for relative waterfall
/// offsets and idle-gap detection.
pub fn parse_rfc3339_ms(at: &str) -> Option<i64> {
    let (date, rest) = at.trim().split_once(['T', ' '])?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let rest = rest.strip_suffix('Z').unwrap_or(rest);
    let (clock, tz_minutes) = if let Some((c, off)) = rest.rsplit_once('+') {
        (c, parse_tz_offset(off)?)
    } else if let Some((c, off)) = rest.rsplit_once('-') {
        (c, -parse_tz_offset(off)?)
    } else {
        (rest, 0)
    };
    let mut t = clock.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let sec_part = t.next()?;
    let (sec_str, frac_str) = sec_part.split_once('.').unwrap_or((sec_part, ""));
    let second: i64 = sec_str.parse().ok()?;
    let mut millis: i64 = 0;
    for (index, byte) in frac_str.bytes().take(3).enumerate() {
        if !byte.is_ascii_digit() {
            return None;
        }
        let scale = match index {
            0 => 100,
            1 => 10,
            _ => 1,
        };
        millis += i64::from(byte - b'0') * scale;
    }
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let total_seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - tz_minutes * 60;
    Some(total_seconds * 1_000 + millis)
}

fn parse_tz_offset(offset: &str) -> Option<i64> {
    let (h, m) = offset.split_once(':').unwrap_or((offset, "0"));
    let hours: i64 = h.parse().ok()?;
    let mins: i64 = m.parse().ok()?;
    Some(hours * 60 + mins)
}

/// `+142 ms` or `+1.8 s` relative to the start of the run.
pub fn format_offset(delta_ms: i64) -> String {
    let ms = delta_ms.max(0) as u64;
    if ms == 0 {
        "+0 ms".to_string()
    } else {
        format!("+{}", format_latency(ms))
    }
}
/// Smart AWS protocol semantic decoding for the Inspector (`Decoded` mode):
/// - Unwraps DynamoDB AttributeValue wrappers (`{"S": "x", "N": "42"}`) into native JSON;
/// - Decodes `application/x-www-form-urlencoded` AWS Query requests (`Action=...`) with
///   pretty-printed nested JSON values (`MessageBody`, `PolicyDocument`, `Message`, `Detail`);
/// - Pretty-prints `aws-eventstream` newline-delimited event JSON frames;
/// - Unwraps embedded JSON strings (`MessageBody`, `Detail`, `input`, `output`, `Policy`).
///
/// Returns `Some((mode_badge_label, decoded_text, is_form_syntax))` when a richer semantic
/// projection exists beyond standard `pretty_body`.
pub fn decode_aws_semantic(
    text: &str,
    content_type: &str,
    encoding: Option<&str>,
) -> Option<(&'static str, String, bool)> {
    let ct_low = content_type.to_ascii_lowercase();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    // 1. AWS EventStream JSONL (`{"event":"...","data":...}` per line).
    if encoding.is_some_and(|e| e.contains("aws-eventstream")) {
        let mut frames = Vec::new();
        for line in trimmed.lines().filter(|l| !l.trim().is_empty()) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
                frames.push(val);
            }
        }
        if !frames.is_empty() {
            let pretty = serde_json::to_string_pretty(&frames).ok()?;
            return Some(("EventStream", pretty, false));
        }
    }

    // 2. AWS Query `x-www-form-urlencoded` (`Action=SendMessage&...`).
    if (ct_low.contains("x-www-form-urlencoded")
        || (!trimmed.starts_with('{')
            && !trimmed.starts_with('<')
            && trimmed.starts_with("Action=")))
        && let Some(decoded_form) = format_query_form(trimmed)
    {
        return Some(("Query Params", decoded_form, true));
    }

    // 3. JSON payloads: DynamoDB AttributeValue unwrapping or embedded JSON string unwrapping.
    if (ct_low.contains("json") || trimmed.starts_with('{'))
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        let mut ddb_hits = 0usize;
        let unwrapped = unwrap_ddb_and_embedded(&val, false, &mut ddb_hits);
        if ddb_hits > 0 {
            let out = serde_json::to_string_pretty(&unwrapped).ok()?;
            return Some(("DynamoDB Unwrapped", out, false));
        }
        let mut embedded_hits = 0usize;
        let emb = unwrap_embedded_json_strings(&val, &mut embedded_hits);
        if embedded_hits > 0 {
            let out = serde_json::to_string_pretty(&emb).ok()?;
            return Some(("Decoded JSON", out, false));
        }
    }

    None
}

fn format_query_form(raw: &str) -> Option<String> {
    let mut pairs = Vec::new();
    for part in raw.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        let key = percent_decode_form(k);
        let val = percent_decode_form(v);
        pairs.push((key, val));
    }
    if pairs.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (k, v) in pairs {
        let v_trim = v.trim();
        if ((v_trim.starts_with('{') && v_trim.ends_with('}'))
            || (v_trim.starts_with('[') && v_trim.ends_with(']')))
            && let Ok(json_val) = serde_json::from_str::<serde_json::Value>(v_trim)
            && let Ok(pretty) = serde_json::to_string_pretty(&json_val)
        {
            let _ = writeln!(out, "{k} =");
            for line in pretty.lines() {
                let _ = writeln!(out, "  {line}");
            }
            continue;
        }
        let _ = writeln!(out, "{k} = {v}");
    }
    Some(out.trim_end().to_string())
}

fn percent_decode_form(input: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn unwrap_ddb_and_embedded(
    val: &serde_json::Value,
    in_ddb_container: bool,
    ddb_hits: &mut usize,
) -> serde_json::Value {
    const DDB_CONTAINERS: &[&str] = &[
        "Item",
        "Items",
        "Key",
        "Keys",
        "Attributes",
        "ExpressionAttributeValues",
        "LastEvaluatedKey",
        "ExclusiveStartKey",
        "Responses",
        "RequestItems",
        "TransactItems",
    ];

    match val {
        serde_json::Value::Object(map) => {
            if in_ddb_container
                && map.len() == 1
                && let Some((tag, inner)) = map.iter().next()
            {
                match (tag.as_str(), inner) {
                    ("S" | "B", serde_json::Value::String(s)) => {
                        *ddb_hits += 1;
                        return serde_json::Value::String(s.clone());
                    }
                    ("N", serde_json::Value::String(n)) => {
                        *ddb_hits += 1;
                        if let Ok(i) = n.parse::<i64>() {
                            return serde_json::Value::Number(i.into());
                        }
                        if let Ok(f) = n.parse::<f64>()
                            && let Some(num) = serde_json::Number::from_f64(f)
                        {
                            return serde_json::Value::Number(num);
                        }
                        return serde_json::Value::String(n.clone());
                    }
                    ("BOOL", serde_json::Value::Bool(b)) => {
                        *ddb_hits += 1;
                        return serde_json::Value::Bool(*b);
                    }
                    ("NULL", serde_json::Value::Bool(true)) => {
                        *ddb_hits += 1;
                        return serde_json::Value::Null;
                    }
                    ("SS" | "BS", serde_json::Value::Array(arr)) => {
                        *ddb_hits += 1;
                        return serde_json::Value::Array(arr.clone());
                    }
                    ("NS", serde_json::Value::Array(arr)) => {
                        *ddb_hits += 1;
                        let nums = arr
                            .iter()
                            .map(|v| {
                                v.as_str()
                                    .and_then(|s| s.parse::<f64>().ok())
                                    .and_then(serde_json::Number::from_f64)
                                    .map_or_else(|| v.clone(), serde_json::Value::Number)
                            })
                            .collect();
                        return serde_json::Value::Array(nums);
                    }
                    ("L", serde_json::Value::Array(arr)) => {
                        *ddb_hits += 1;
                        return serde_json::Value::Array(
                            arr.iter()
                                .map(|item| unwrap_ddb_and_embedded(item, true, ddb_hits))
                                .collect(),
                        );
                    }
                    ("M", serde_json::Value::Object(sub)) => {
                        *ddb_hits += 1;
                        let mut out = serde_json::Map::new();
                        for (k, v) in sub {
                            out.insert(k.clone(), unwrap_ddb_and_embedded(v, true, ddb_hits));
                        }
                        return serde_json::Value::Object(out);
                    }
                    _ => {}
                }
            }
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let child_in_ddb = in_ddb_container || DDB_CONTAINERS.contains(&k.as_str());
                out.insert(
                    k.clone(),
                    unwrap_ddb_and_embedded(v, child_in_ddb, ddb_hits),
                );
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.iter()
                .map(|v| unwrap_ddb_and_embedded(v, in_ddb_container, ddb_hits))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn unwrap_embedded_json_strings(val: &serde_json::Value, hits: &mut usize) -> serde_json::Value {
    const JSON_STRING_FIELDS: &[&str] = &[
        "MessageBody",
        "Body",
        "Message",
        "Detail",
        "Policy",
        "PolicyDocument",
        "AssumeRolePolicyDocument",
        "input",
        "output",
    ];
    match val {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if JSON_STRING_FIELDS.contains(&k.as_str())
                    && let serde_json::Value::String(s) = v
                {
                    let st = s.trim();
                    if ((st.starts_with('{') && st.ends_with('}'))
                        || (st.starts_with('[') && st.ends_with(']')))
                        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(st)
                    {
                        *hits += 1;
                        out.insert(k.clone(), unwrap_embedded_json_strings(&parsed, hits));
                        continue;
                    }
                }
                out.insert(k.clone(), unwrap_embedded_json_strings(v, hits));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.iter()
                .map(|v| unwrap_embedded_json_strings(v, hits))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenslet_core::trace::{BodyTrace, HeaderTrace, Message, Tagged};

    /// The window reads exactly what `lenslet_core` writes.
    #[test]
    fn reads_a_line_serialized_from_the_core_schema() {
        let core = lenslet_core::trace::Trace {
            id: "18e8-7".into(),
            at: "2026-11-15T09:48:42.616+00:00".into(),
            method: "GET".into(),
            path: "/shop-assets/receipts/1002.json".into(),
            host: "127.0.0.1:43865".into(),
            service: "s3".into(),
            operation: "GetObject".into(),
            status: 404,
            ms: 3,
            authorization: true,
            region: Some("us-east-1".into()),
            invocation_id: Some("inv-1".into()),
            attempt: Some(2),
            max_attempts: Some(3),
            problem: Some(Problem {
                code: "NoSuchKey".into(),
                message: Some("The specified key does not exist.".into()),
                hint: None,
            }),
            access: vec![CallAccess {
                action: "s3:GetObject".into(),
                resource: "arn:aws:s3:::shop-assets/*".into(),
                new: true,
            }],
            implied: vec![CallAccess::from(Access::new("kms:Decrypt", "*"))],
            new: true,
            replay_of: Some("18e8-6".into()),
            request: Message {
                headers: vec![HeaderTrace {
                    name: "amz-sdk-request".into(),
                    value: "attempt=2; max=3".into(),
                }],
                content_type: None,
                body: None,
            },
            response: Message {
                headers: Vec::new(),
                content_type: Some("application/xml".into()),
                body: Some(BodyTrace {
                    text: Some("<Error><Code>NoSuchKey</Code></Error>".into()),
                    hex: None,
                    encoding: None,
                    bytes: 37,
                    truncated: false,
                    binary: false,
                    sha256: "ab".repeat(32),
                }),
            },
            ..Default::default()
        };
        let line = serde_json::to_string(&Tagged {
            kind: "call",
            value: &core,
        })
        .unwrap();
        let row = parse_line(&line).expect("the ui parses a core call line");
        assert_eq!(row.id, core.id);
        assert_eq!(row.status, 404);
        assert_eq!(row.attempt, Some(2));
        assert_eq!(row.replay_of.as_deref(), Some("18e8-6"));
        assert_eq!(row.problem.as_ref().unwrap().code, "NoSuchKey");
        assert!(row.is_error() && row.is_retry() && row.new);
        assert!(row.uses(&Access::new("kms:Decrypt", "*")));
        assert_eq!(row.service_name(), "S3");
        let body = row.response.as_ref().unwrap().body.as_ref().unwrap();
        assert_eq!(body.size(), 37);
        assert_eq!(body.sha256.as_deref(), Some("ab".repeat(32).as_str()));

        // And back: the core sees the same call (without bodies).
        let back = row.to_core();
        assert_eq!(back.access, core.access);
        assert_eq!(back.problem, core.problem);
        assert!(back.response.body.is_none());

        // Header and report lines parse with the core types.
        let header = lenslet_core::trace::RunHeader {
            version: 3,
            run: "r".into(),
            ..Default::default()
        };
        let line = serde_json::to_string(&Tagged {
            kind: "run",
            value: &header,
        })
        .unwrap();
        assert!(matches!(parse_record(&line), Some(Record::Run(h)) if h.run == "r"));
        let report = Report {
            calls: 7,
            ..Default::default()
        };
        let line = serde_json::to_string(&Tagged {
            kind: "end",
            value: &report,
        })
        .unwrap();
        assert!(matches!(parse_record(&line), Some(Record::End(r)) if r.calls == 7));
    }

    #[test]
    fn demo_is_a_real_finished_run() {
        let run = crate::demo::demo_run();
        assert_eq!(run.header.command, ["python", "demo.py"]);
        let report = run.report.expect("demo has its report");
        assert_eq!(report.calls, crate::demo::demo_rows().len());
    }

    #[test]
    fn tolerates_missing_fields_and_rejects_calls_without_identity() {
        assert!(parse_line(r#"{"type":"call","id":"a","at":"2026-11-15T00:00:00Z"}"#).is_some());
        assert!(parse_line(r#"{"type":"call","at":"2026-11-15T00:00:00Z"}"#).is_none());
        assert!(parse_line("not json").is_none());
        let call = parse_line(
            r#"{"type":"call","id":"a","at":"t","method":"post","path":"/q?x=1","request":{"headers":[{"name":"content-length","value":"2"},{"name":"x-amz-target","value":"T"}],"body":{"text":"{}","bytes":2}}}"#,
        )
        .unwrap();
        let curl = to_curl(&call, Some("http://127.0.0.1:1/")).unwrap();
        assert!(curl.contains("POST") && curl.contains("'http://127.0.0.1:1/q?x=1'"));
        assert!(curl.contains("x-amz-target: T") && !curl.contains("content-length"));
    }
}
