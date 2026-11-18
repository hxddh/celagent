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
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::Deserialize;

pub const MAX_ROWS: usize = 5_000;
/// Bodies larger than this in a tailed file are offloaded from the in-memory
/// stream row and hydrated on demand via byte offset when inspected.
const INLINE_BODY_LIMIT: usize = 8 * 1024;
/// A reload reads backwards in blocks of this size until it has `MAX_ROWS`
/// lines, so traces with large bodies still show a full stream.
const TAIL_BLOCK_BYTES: u64 = 1 << 20;
/// Upper bound on what a reload reads, whatever the line sizes.
const MAX_TAIL_BYTES: u64 = 256 << 20;
/// Never let a partially written line grow without bound. One record holds up
/// to two 1 MiB bodies plus headers, even fully JSON-escaped.
const MAX_CARRY_BYTES: usize = 16 << 20;
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

/// What went wrong with a call: AWS error code, message, likely cause.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Problem {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub hint: Option<String>,
}

/// One permission: IAM action on a resource ARN pattern.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
pub struct Access {
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub resource: String,
}

/// One permission on a `call` trace line, with its own `new` flag.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct CallAccess {
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub resource: String,
    #[serde(default)]
    pub new: bool,
}

impl CallAccess {
    pub fn key(&self) -> Access {
        Access {
            action: self.action.clone(),
            resource: self.resource.clone(),
        }
    }
}

/// The first line of a run file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct RunHeader {
    #[serde(default)]
    pub run: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub upstream: String,
    #[serde(default)]
    pub started: String,
    /// Compared with this lock file.
    #[serde(default)]
    pub lock: Option<String>,
    /// Compared/enforced with this IAM policy document.
    #[serde(default)]
    pub policy: Option<String>,
    /// Whether `--enforce` actively blocked unauthorized calls.
    #[serde(default)]
    pub enforce: bool,
}

/// One line of the run's access table.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct AccessEntry {
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub resource: String,
    #[serde(default)]
    pub calls: usize,
    #[serde(default)]
    pub errors: usize,
    #[serde(default)]
    pub implied: bool,
    #[serde(default)]
    pub via: Vec<String>,
    /// Not in the lock file.
    #[serde(default)]
    pub new: bool,
    /// In the lock file, not exercised by this run.
    #[serde(skip)]
    pub unused: bool,
    /// Marked `# test` in the lock file.
    #[serde(skip)]
    pub test: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
pub struct FailureKind {
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub operation: String,
    #[serde(default)]
    pub code: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Regressions {
    #[serde(default)]
    pub previous_run: String,
    #[serde(default)]
    pub previous_calls: usize,
    #[serde(default)]
    pub previous_duration_ms: u64,
    #[serde(default)]
    pub new_failures: Vec<FailureKind>,
    #[serde(default)]
    pub fixed: Vec<FailureKind>,
    #[serde(default)]
    pub new_access: Vec<Access>,
}

/// The last line of a finished run file (the same object as `--json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Report {
    #[serde(default)]
    pub run: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub ended: String,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub calls: usize,
    #[serde(default)]
    pub errors: usize,
    #[serde(default)]
    pub retries: usize,
    #[serde(default)]
    pub blocked: usize,
    #[serde(default)]
    pub services: std::collections::BTreeMap<String, usize>,
    #[serde(default)]
    pub failures: Vec<FailureKind>,
    #[serde(default)]
    pub access: Vec<AccessEntry>,
    #[serde(default)]
    pub lock: Option<String>,
    #[serde(default)]
    pub policy: Option<String>,
    #[serde(default)]
    pub access_added: Vec<Access>,
    #[serde(default)]
    pub access_unused: Vec<Access>,
    #[serde(default)]
    pub unmapped: std::collections::BTreeMap<String, usize>,
    #[serde(default)]
    pub regressions: Option<Regressions>,
}

/// A run as the window shows it: its header, plus the report once it ended.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunInfo {
    pub header: RunHeader,
    pub report: Option<Report>,
}

impl RunInfo {
    /// `host:port` of a URL such as `http://127.0.0.1:43761`.
    pub fn address(url: &str) -> Option<&str> {
        let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
        let rest = rest.split('/').next().unwrap_or(rest);
        (!rest.is_empty()).then_some(rest)
    }

    pub fn command_line(&self) -> String {
        self.header.command.join(" ")
    }

    pub fn ended(&self) -> bool {
        self.report.is_some()
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.report.as_ref().and_then(|report| report.exit_code)
    }
}

/// One line of a run file.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Record {
    Run(RunHeader),
    Call(Box<Trace>),
    End(Box<Report>),
}

#[derive(Clone, Debug)]
pub struct LineRef {
    pub path: Arc<PathBuf>,
    pub offset: u64,
    pub len: usize,
}

#[derive(Clone, Debug, Deserialize)]
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
    line_ref: Option<LineRef>,
    #[serde(skip)]
    dehydrated: bool,
}

impl Trace {
    fn dehydrate(&mut self, lref: LineRef) {
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
        if shed {
            self.line_ref = Some(lref);
            self.dehydrated = true;
        }
    }

    /// Return a `Trace` with full request/response bodies loaded from disk when
    /// large bodies were offloaded during stream indexing.
    pub fn hydrated(&self) -> Cow<'_, Trace> {
        if !self.dehydrated {
            return Cow::Borrowed(self);
        }
        let Some(lref) = self.line_ref.as_ref() else {
            return Cow::Borrowed(self);
        };
        let Ok(mut file) = File::open(&*lref.path) else {
            return Cow::Borrowed(self);
        };
        if file.seek(SeekFrom::Start(lref.offset)).is_err() {
            return Cow::Borrowed(self);
        }
        let mut buf = vec![0u8; lref.len];
        if file.read_exact(&mut buf).is_err() {
            return Cow::Borrowed(self);
        }
        let line = String::from_utf8_lossy(&buf);
        match parse_record(&line) {
            Some(Record::Call(full)) => Cow::Owned(*full),
            _ => Cow::Borrowed(self),
        }
    }
    fn finalize(mut self) -> Self {
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
                let (attempt, max) = parse_sdk_request(raw);
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
        if let Some(problem) = self.problem.as_ref() {
            haystack.push_str(&problem.code);
            haystack.push('\n');
            if let Some(message) = problem.message.as_deref() {
                haystack.push_str(message);
                haystack.push('\n');
            }
        }
        if let Some(region) = self.region.as_deref() {
            haystack.push_str(region);
            haystack.push('\n');
        }
        if let Some(invocation_id) = self.invocation_id.as_deref() {
            haystack.push_str(invocation_id);
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
            haystack.push_str(&access.action);
            haystack.push(' ');
            haystack.push_str(&access.resource);
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

    /// A failed call: an HTTP error, or a problem the proxy recorded.
    pub fn is_error(&self) -> bool {
        status_is_error(self.status) || self.problem.is_some()
    }

    /// Signed with a presigned URL rather than an Authorization header.
    pub fn presigned(&self) -> bool {
        self.path.contains("X-Amz-Signature=")
            || (self.path.contains("AWSAccessKeyId=") && self.path.contains("Signature="))
    }

    /// Stable identity used to keep a selection across reloads.
    pub fn key(&self) -> String {
        self.id.clone()
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
        self.access().any(|own| &own == access)
    }
}

fn parse_record(line: &str) -> Option<Record> {
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

/// Every call in a run file's text.
pub fn parse_lines(text: &str) -> Vec<Trace> {
    parse_records(text).calls
}

/// The run (header and report) in a run file's text.
pub fn parse_run(text: &str) -> RunInfo {
    let parsed = parse_records(text);
    RunInfo {
        header: parsed.header.unwrap_or_default(),
        report: parsed.report,
    }
}

/// What a chunk of a run file holds.
#[derive(Default)]
struct Parsed {
    calls: Vec<Trace>,
    header: Option<RunHeader>,
    report: Option<Report>,
    bad: usize,
}

fn parse_records(text: &str) -> Parsed {
    parse_records_at(text.as_bytes(), 0, None)
}

fn parse_records_at(bytes: &[u8], base_offset: u64, source: Option<&Arc<PathBuf>>) -> Parsed {
    let mut parsed = Parsed::default();
    let mut pos = 0usize;
    while pos < bytes.len() {
        let rel_end = bytes[pos..]
            .iter()
            .position(|b| *b == b'\n')
            .unwrap_or(bytes.len() - pos);
        let slice = &bytes[pos..pos + rel_end];
        let line_offset = base_offset + pos as u64;
        let line_len = slice.len();
        pos += rel_end + 1;

        let line = String::from_utf8_lossy(slice);
        if line.trim().is_empty() {
            continue;
        }
        match parse_record(&line) {
            Some(Record::Call(mut trace)) => {
                if let Some(arc_path) = source {
                    trace.dehydrate(LineRef {
                        path: arc_path.clone(),
                        offset: line_offset,
                        len: line_len,
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

/// What the window follows: one fixed file, or a run in the runs directory
/// (the newest, unless the viewer picked one).
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    File(PathBuf),
    Runs {
        dir: PathBuf,
        pinned: Option<PathBuf>,
    },
}

impl Source {
    fn resolve(&self) -> Option<PathBuf> {
        match self {
            Source::File(path) => Some(path.clone()),
            Source::Runs { dir, pinned } => pinned
                .clone()
                .filter(|path| path.exists())
                .or_else(|| list_runs(dir).pop()),
        }
    }
}

/// Run files in `dir`, oldest first (names sort by start time).
pub fn list_runs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut runs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    runs.sort();
    runs
}

fn read_header(path: &Path) -> Option<RunHeader> {
    use std::io::BufRead;
    let mut line = String::new();
    io::BufReader::new(File::open(path).ok()?)
        .read_line(&mut line)
        .ok()?;
    match parse_record(&line)? {
        Record::Run(header) => Some(header),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct Tailer {
    source: Source,
    /// The file being followed right now.
    path: Option<PathBuf>,
    offset: u64,
    /// Bytes after the last newline; decoded only once the line is complete,
    /// so a read that ends inside a UTF-8 character cannot corrupt it.
    carry: Vec<u8>,
    file_key: u64,
    ready: bool,
    skipped: usize,
    header: Option<RunHeader>,
    report: Option<Report>,
}

impl Tailer {
    #[cfg(test)]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self::follow(Source::File(path.into()))
    }

    pub fn follow(source: Source) -> Self {
        Self {
            source,
            path: None,
            offset: 0,
            carry: Vec::new(),
            file_key: 0,
            ready: false,
            skipped: 0,
            header: None,
            report: None,
        }
    }

    /// Malformed lines skipped since the last full reload.
    pub fn skipped(&self) -> usize {
        self.skipped
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The followed run: its header, and its report once it has ended.
    pub fn run(&self) -> Option<RunInfo> {
        self.header.clone().map(|header| RunInfo {
            header,
            report: self.report.clone(),
        })
    }

    /// One non-blocking-as-possible poll. Intended to run on a background executor.
    pub fn poll(&mut self) -> TailEvent {
        let Some(path) = self.source.resolve() else {
            self.path = None;
            self.header = None;
            self.report = None;
            self.ready = false;
            return TailEvent::Missing;
        };
        if self.path.as_ref() != Some(&path) {
            // A newer (or a picked) run: follow it from the beginning.
            self.path = Some(path.clone());
            self.header = None;
            self.report = None;
            self.ready = false;
        }

        let metadata = match fs::metadata(&path) {
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
            self.reload(&path, len, key)
        } else if len == self.offset {
            TailEvent::Idle
        } else {
            self.read_new(&path)
        }
    }

    fn reload(&mut self, path: &Path, len: u64, key: u64) -> TailEvent {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) => return TailEvent::Error(short_io_error(&error)),
        };
        // Read backwards a block at a time until there are enough lines.
        let mut start = len;
        let mut bytes: Vec<u8> = Vec::new();
        while start > 0 && len - start < MAX_TAIL_BYTES {
            let block = TAIL_BLOCK_BYTES.min(start);
            start -= block;
            let mut chunk = vec![0u8; block as usize];
            if let Err(error) = file
                .seek(SeekFrom::Start(start))
                .and_then(|_| file.read_exact(&mut chunk))
            {
                return TailEvent::Error(short_io_error(&error));
            }
            chunk.extend_from_slice(&bytes);
            bytes = chunk;
            if bytes.iter().filter(|byte| **byte == b'\n').count() > MAX_ROWS {
                break;
            }
        }

        self.offset = len;
        self.file_key = key;
        self.carry.clear();
        self.ready = true;

        // Drop a leading partial line, and keep an unterminated tail for later.
        let mut body: &[u8] = &bytes;
        let mut body_offset = start;
        if start > 0 {
            match body.iter().position(|byte| *byte == b'\n') {
                Some(ix) => {
                    body = &body[ix + 1..];
                    body_offset += (ix + 1) as u64;
                }
                None => body = &[],
            }
        }
        let complete = match body.iter().rposition(|byte| *byte == b'\n') {
            Some(ix) => {
                self.carry.extend_from_slice(&body[ix + 1..]);
                &body[..=ix]
            }
            None => {
                self.carry.extend_from_slice(body);
                &[][..]
            }
        };
        let arc_path = Arc::new(path.to_path_buf());
        let parsed = parse_records_at(complete, body_offset, Some(&arc_path));
        self.skipped = parsed.bad;
        // A long run's header is far before the tail that was read.
        self.header = parsed.header.or_else(|| read_header(path));
        self.report = parsed.report;
        TailEvent::Replaced(cap_tail(parsed.calls))
    }

    fn read_new(&mut self, path: &Path) -> TailEvent {
        let mut file = match File::open(path) {
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

        let carry_before = self.carry.len() as u64;
        let base_offset = self.offset.saturating_sub(carry_before);
        self.offset += bytes.len() as u64;
        self.carry.extend_from_slice(&bytes);

        let Some(newline) = self.carry.iter().rposition(|byte| *byte == b'\n') else {
            if self.carry.len() > MAX_CARRY_BYTES {
                self.carry.clear();
                self.skipped += 1;
            }
            return TailEvent::Idle;
        };
        let complete: Vec<u8> = self.carry.drain(..=newline).collect();
        let arc_path = Arc::new(path.to_path_buf());
        let parsed = parse_records_at(&complete, base_offset, Some(&arc_path));
        self.skipped += parsed.bad;
        if parsed.header.is_some() {
            self.header = parsed.header;
        }
        if parsed.report.is_some() {
            self.report = parsed.report;
        }
        if parsed.calls.is_empty() {
            TailEvent::Idle
        } else {
            TailEvent::Appended(parsed.calls)
        }
    }
}

/// Fold new lines into the stream: a line whose id is already present replaces
/// that row (a pending call that finished), rows stay in request-arrival order,
/// and only the newest [`MAX_ROWS`] are kept.
pub fn merge(rows: &[Arc<Trace>], incoming: Vec<Trace>) -> Vec<Arc<Trace>> {
    let mut next: Vec<Arc<Trace>> = rows.to_vec();
    let mut positions: HashMap<String, usize> = next
        .iter()
        .enumerate()
        .map(|(index, row)| (row.id.clone(), index))
        .collect();
    let mut sorted = true;
    for trace in incoming {
        if let Some(index) = positions.get(&trace.id) {
            next[*index] = Arc::new(trace);
            continue;
        }
        if next.last().is_some_and(|last| last.at > trace.at) {
            sorted = false;
        }
        positions.insert(trace.id.clone(), next.len());
        next.push(Arc::new(trace));
    }
    if !sorted {
        next.sort_by(|left, right| left.at.cmp(&right.at));
    }
    if next.len() > MAX_ROWS {
        next.drain(..next.len() - MAX_ROWS);
    }
    next
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusFilter {
    #[default]
    All,
    Errors,
    Blocked,
    Retries,
    Slow,
}

/// Combinable call-stream filter: status facet AND service facet AND optional IAM permission.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallFilter {
    pub status: StatusFilter,
    pub service: Option<String>,
    pub access: Option<Access>,
}

impl CallFilter {
    pub fn is_active(&self) -> bool {
        self.status != StatusFilter::All || self.service.is_some() || self.access.is_some()
    }

    pub fn matches(&self, trace: &Trace) -> bool {
        let status_ok = match self.status {
            StatusFilter::All => true,
            StatusFilter::Errors => trace.is_error(),
            StatusFilter::Blocked => trace.blocked,
            StatusFilter::Retries => trace.is_retry(),
            StatusFilter::Slow => trace.pending || trace.ms >= 500,
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
        true
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccessFilter {
    #[default]
    All,
    New,
    Implied,
    Unused,
    Test,
}

impl AccessFilter {
    pub fn matches(self, entry: &AccessEntry) -> bool {
        match self {
            AccessFilter::All => true,
            AccessFilter::New => entry.new,
            AccessFilter::Implied => entry.implied,
            AccessFilter::Unused => entry.unused,
            AccessFilter::Test => entry.test,
        }
    }
}

impl AccessEntry {
    pub fn matches_search(&self, tokens: &[String]) -> bool {
        if tokens.is_empty() {
            return true;
        }
        let mut hay = format!("{} {}", self.action, self.resource).to_lowercase();
        for v in &self.via {
            hay.push(' ');
            hay.push_str(&v.to_lowercase());
        }
        if self.new {
            hay.push_str(" new denied");
        }
        if self.implied {
            hay.push_str(" implied");
        }
        if self.unused {
            hay.push_str(" unused");
        }
        if self.test {
            hay.push_str(" test");
        }
        tokens.iter().all(|tok| hay.contains(tok.as_str()))
    }
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Errors,
    Blocked,
    Service(String),
    /// Calls that need this permission (directly or implied).
    Access(Access),
}

#[cfg(test)]
impl Filter {
    pub fn matches(&self, trace: &Trace) -> bool {
        match self {
            Filter::All => true,
            Filter::Errors => trace.is_error(),
            Filter::Blocked => trace.blocked,
            Filter::Service(key) => trace.service_key() == key,
            Filter::Access(access) => trace.uses(access),
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

pub fn headers_to_text(headers: &[Header]) -> String {
    let mut out = String::new();
    for header in headers {
        let _ = writeln!(out, "{}: {}", header.name, header.value);
    }
    out
}

fn posix_single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Build an exact, unredacted `curl` command reproducing `trace`'s HTTP request,
/// or `None` when the request carried a binary or truncated body whose complete
/// payload cannot be reconstructed from the trace.
pub fn to_curl(trace: &Trace, endpoint: Option<&str>) -> Option<String> {
    if let Some(body) = trace.request.as_ref().and_then(|req| req.body.as_ref())
        && (body.truncated || (body.bytes > 0 && body.text.is_none()))
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

/// Parse `attempt=2; max=3` from `amz-sdk-request`.
pub fn parse_sdk_request(value: &str) -> (Option<u32>, Option<u32>) {
    let mut attempt = None;
    let mut max = None;
    for part in value.split(';') {
        let Some((key, raw)) = part.trim().split_once('=') else {
            continue;
        };
        let parsed = raw.trim().parse::<u32>().ok().filter(|n| *n > 0);
        match key.trim() {
            "attempt" => attempt = parsed,
            "max" => max = parsed,
            _ => {}
        }
    }
    (attempt, max)
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

/// Read `# test` marks from a lock file on disk, when reachable.
pub fn read_test_marks(cwd: &str, lock: &str) -> std::collections::BTreeSet<Access> {
    let lock_path = Path::new(lock);
    let resolved = if lock_path.is_absolute() || cwd.is_empty() {
        lock_path.to_path_buf()
    } else {
        Path::new(cwd).join(lock_path)
    };
    let Ok(text) = fs::read_to_string(resolved) else {
        return std::collections::BTreeSet::new();
    };
    let mut out = std::collections::BTreeSet::new();
    for line in text.lines() {
        let Some((entry, comment)) = line.split_once('#') else {
            continue;
        };
        if !comment
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word == "test")
        {
            continue;
        }
        let mut fields = entry.split_whitespace();
        let Some(action) = fields.next() else {
            continue;
        };
        out.insert(Access {
            action: action.to_string(),
            resource: fields.next().unwrap_or("*").to_string(),
        });
    }
    out
}

/// Summary of one recorded run file for the Run Switcher popover and Diff view.
#[derive(Clone, Debug)]
pub struct RunSummary {
    pub path: PathBuf,
    pub id: String,
    pub header: RunHeader,
    pub report: Option<Report>,
}

impl RunSummary {
    pub fn label(&self) -> String {
        if self.header.command.is_empty() {
            "proxy session".to_string()
        } else {
            format!("$ {}", self.header.command.join(" "))
        }
    }
}

fn read_last_report(path: &Path) -> Option<Report> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len == 0 {
        return None;
    }
    let mut start = len;
    let mut tail: Vec<u8> = Vec::new();
    while start > 0 && len - start < (16 << 20) {
        let block = (64 * 1024u64).min(start);
        start -= block;
        let mut chunk = vec![0u8; block as usize];
        file.seek(SeekFrom::Start(start)).ok()?;
        file.read_exact(&mut chunk).ok()?;
        chunk.extend_from_slice(&tail);
        tail = chunk;
        let body = tail.strip_suffix(b"\n").unwrap_or(&tail);
        if body.contains(&b'\n') {
            break;
        }
    }
    let body = tail.strip_suffix(b"\n").unwrap_or(&tail);
    let line = match body.iter().rposition(|b| *b == b'\n') {
        Some(ix) => &body[ix + 1..],
        None if start == 0 => body,
        None => return None,
    };
    match parse_record(&String::from_utf8_lossy(line))? {
        Record::End(report) => Some(*report),
        _ => None,
    }
}

/// Read header + final `end` report of a run file without reading its call stream.
pub fn load_run_summary(path: &Path) -> Option<RunSummary> {
    let header = read_header(path)?;
    let report = read_last_report(path);
    let id = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| header.run.clone());
    Some(RunSummary {
        path: path.to_path_buf(),
        id,
        header,
        report,
    })
}

/// All recorded runs in `dir`, newest first.
pub fn list_run_summaries(dir: &Path) -> Vec<RunSummary> {
    list_runs(dir)
        .into_iter()
        .rev()
        .filter_map(|p| load_run_summary(&p))
        .collect()
}

/// Finished candidate baselines for `current_path`, ordered with earlier runs
/// first (nearest predecessor at index 0) followed by newer finished runs, plus
/// the nearest earlier baseline path (`prev`) if one exists.
pub fn ordered_baselines(
    dir: &Path,
    current_path: Option<&Path>,
) -> (Vec<RunSummary>, Option<PathBuf>) {
    let mut earlier = Vec::new();
    let mut newer = Vec::new();
    for summary in list_run_summaries(dir) {
        if summary.report.is_none() || Some(summary.path.as_path()) == current_path {
            continue;
        }
        if current_path.is_none_or(|cur| summary.path.as_path() < cur) {
            earlier.push(summary);
        } else {
            newer.push(summary);
        }
    }
    let prev_path = earlier.first().map(|s| s.path.clone());
    earlier.extend(newer);
    (earlier, prev_path)
}

/// `(file_count, total_bytes)` for `~/.lenslet/runs`.
pub fn storage_stats(dir: &Path) -> (usize, u64) {
    let files = list_runs(dir);
    let count = files.len();
    let bytes = files
        .iter()
        .filter_map(|p| fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    (count, bytes)
}

/// Delete finished run files in `dir` (preserving `keep_path` and any still-running file).
pub fn clear_finished_runs(dir: &Path, keep_path: Option<&Path>) -> usize {
    let mut removed = 0usize;
    for path in list_runs(dir) {
        if keep_path.is_some_and(|keep| keep == path.as_path()) {
            continue;
        }
        if read_last_report(&path).is_some() && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Build an IAM policy JSON document from exercised non-`# test` permissions.
pub fn export_policy_json(rows: &[AccessEntry]) -> String {
    let mut by_resource: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for entry in rows {
        if entry.unused || entry.test {
            continue;
        }
        by_resource
            .entry(entry.resource.clone())
            .or_default()
            .push(entry.action.clone());
    }
    let statements: Vec<serde_json::Value> = by_resource
        .into_iter()
        .map(|(resource, mut actions)| {
            actions.sort();
            actions.dedup();
            let action = match actions.as_slice() {
                [only] => serde_json::Value::String(only.clone()),
                many => serde_json::json!(many),
            };
            serde_json::json!({
                "Effect": "Allow",
                "Action": action,
                "Resource": resource,
            })
        })
        .collect();
    let doc = serde_json::json!({
        "Version": "2012-10-17",
        "Statement": statements,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
}

/// Single IAM Statement JSON snippet for one `AccessEntry`.
pub fn statement_json(entry: &AccessEntry) -> String {
    let stmt = serde_json::json!({
        "Effect": "Allow",
        "Action": entry.action,
        "Resource": entry.resource,
    });
    serde_json::to_string_pretty(&stmt).unwrap_or_else(|_| "{}".to_string())
}

/// Run-to-run diff model for the desktop `Diff` view.
#[derive(Clone, Debug, Default)]
pub struct UiRunDiff {
    pub base_id: String,
    pub base_cmd: String,
    pub target_id: String,
    pub target_cmd: String,
    pub base_calls: usize,
    pub target_calls: usize,
    pub base_errors: usize,
    pub target_errors: usize,
    pub base_duration_ms: u64,
    pub target_duration_ms: u64,
    pub new_failures: Vec<FailureKind>,
    pub fixed_failures: Vec<FailureKind>,
    pub access_added: Vec<Access>,
    pub access_removed: Vec<Access>,
    pub service_deltas: Vec<(String, usize, usize)>,
}

impl UiRunDiff {
    pub fn from_summaries(
        base: &RunSummary,
        target_run: &RunInfo,
        target_rows: &[Arc<Trace>],
        target_access: &[AccessEntry],
    ) -> Option<Self> {
        let base_rep = base.report.as_ref()?;

        let target_calls = target_run
            .report
            .as_ref()
            .map_or(target_rows.len(), |r| r.calls);
        let target_errors = target_run.report.as_ref().map_or_else(
            || target_rows.iter().filter(|r| r.is_error()).count(),
            |r| r.errors,
        );
        let target_duration_ms = target_run.report.as_ref().map_or_else(
            || match (
                target_rows.first().and_then(|r| r.at_ms()),
                target_rows
                    .last()
                    .and_then(|r| r.at_ms().map(|at| at + r.ms as i64)),
            ) {
                (Some(s), Some(e)) if e >= s => (e - s) as u64,
                _ => 0,
            },
            |r| r.duration_ms,
        );

        let base_access: std::collections::BTreeSet<Access> = base_rep
            .access
            .iter()
            .filter(|e| !e.unused)
            .map(|e| Access {
                action: e.action.clone(),
                resource: e.resource.clone(),
            })
            .collect();
        let now_access: std::collections::BTreeSet<Access> = target_access
            .iter()
            .filter(|e| !e.unused)
            .map(|e| Access {
                action: e.action.clone(),
                resource: e.resource.clone(),
            })
            .collect();

        let (new_failures, fixed_failures) = if let Some(reg) = target_run
            .report
            .as_ref()
            .and_then(|r| r.regressions.as_ref())
            .filter(|reg| reg.previous_run == base.id || reg.previous_run == base.header.run)
        {
            (reg.new_failures.clone(), reg.fixed.clone())
        } else {
            let base_f: std::collections::BTreeSet<FailureKind> =
                base_rep.failures.iter().cloned().collect();
            let now_f: std::collections::BTreeSet<FailureKind> = if let Some(rep) = target_run
                .report
                .as_ref()
                .filter(|r| !r.failures.is_empty() || r.errors == 0)
            {
                rep.failures.iter().cloned().collect()
            } else {
                target_rows
                    .iter()
                    .filter_map(|row| {
                        let p = row.problem.as_ref()?;
                        Some(FailureKind {
                            service: row.service.clone(),
                            operation: row.operation.clone(),
                            code: p.code.clone(),
                        })
                    })
                    .collect()
            };
            (
                now_f.difference(&base_f).cloned().collect(),
                base_f.difference(&now_f).cloned().collect(),
            )
        };

        let mut target_services: std::collections::BTreeMap<String, usize> = if let Some(rep) =
            target_run
                .report
                .as_ref()
                .filter(|r| !r.services.is_empty())
        {
            rep.services.clone()
        } else {
            let mut map = std::collections::BTreeMap::new();
            for row in target_rows {
                *map.entry(row.service_key().to_string()).or_default() += 1;
            }
            map
        };
        let mut all_svcs: std::collections::BTreeSet<String> =
            base_rep.services.keys().cloned().collect();
        all_svcs.extend(target_services.keys().cloned());
        let mut service_deltas = Vec::new();
        for svc in all_svcs {
            let b = base_rep.services.get(&svc).copied().unwrap_or(0);
            let t = target_services.remove(&svc).unwrap_or(0);
            if b != t || b > 0 {
                service_deltas.push((svc, b, t));
            }
        }

        Some(UiRunDiff {
            base_id: if base.header.run.is_empty() {
                base.id.clone()
            } else {
                base.header.run.clone()
            },
            base_cmd: base.label(),
            target_id: target_run.header.run.clone(),
            target_cmd: if target_run.header.command.is_empty() {
                "proxy session".to_string()
            } else {
                format!("$ {}", target_run.command_line())
            },
            base_calls: base_rep.calls,
            target_calls,
            base_errors: base_rep.errors,
            target_errors,
            base_duration_ms: base_rep.duration_ms,
            target_duration_ms,
            new_failures,
            fixed_failures,
            access_added: now_access.difference(&base_access).cloned().collect(),
            access_removed: base_access.difference(&now_access).cloned().collect(),
            service_deltas,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[allow(clippy::too_many_arguments)]
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
            r#"{{"type":"call","id":"{at}{path}","at":"{at}","method":"{method}","path":"{path}","host":"127.0.0.1:9297","service":"{service}","operation":"{operation}","status":{status},"ms":{ms},"authorization":true"#
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
            r#"{"type":"call","id":"a","at":"2026-11-15T02:24:19.540Z","method":"GET","path":"/demo/a.txt","host":"127.0.0.1:9297","service":"s3","operation":"GetObject","status":200,"ms":3,"authorization":false}"#,
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
        let line = r#"{"type":"call","id":"p","at":"2026-11-15T02:24:19.540507869+00:00","method":"PUT","path":"/demo/a.png?x-id=PutObject","host":"127.0.0.1:9297","service":"s3","operation":"PutObject","status":200,"ms":8,"authorization":true,"request":{"headers":[{"name":"authorization","value":"AWS4-HMAC-SHA256 Credential=demo/20261115/s3/aws4_request"}],"content_type":"image/png","body":{"text":"hello","bytes":5,"truncated":true,"binary":false,"sha256":"abc123"}},"response":{"headers":[],"body":{"text":"","bytes":0,"truncated":false,"binary":true}}}"#;
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
            // A call without an id cannot be tracked, so it is skipped.
            r#"{"type":"call","at":"2026-11-15T02:24:20Z","method":"GET","path":"/x"}"#.to_string(),
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
    fn run_files_carry_a_header_calls_and_a_report() {
        let text = [
            r#"{"type":"run","version":3,"run":"r1","command":["npm","test"],"cwd":"/w","pid":1,"endpoint":"http://127.0.0.1:4100","upstream":"https://127.0.0.1:9298","started":"s","lock":"lenslet.lock","future":true}"#,
            r#"{"type":"call","id":"c1","at":"2026-11-15T00:00:01Z","method":"POST","path":"/","service":"sqs","operation":"SendMessage","status":200,"ms":2,"access":[{"action":"sqs:SendMessage","resource":"arn:aws:sqs:*:*:jobs"}],"new":true,"implied":[{"action":"kms:Decrypt","resource":"arn:aws:kms:*:*:key/*"}]}"#,
            r#"{"type":"end","run":"r1","exit_code":3,"ended":"e","access":[{"action":"sqs:SendMessage","resource":"arn:aws:sqs:*:*:jobs","calls":1,"errors":0,"new":true}],"lock":"lenslet.lock","access_added":[{"action":"sqs:SendMessage","resource":"arn:aws:sqs:*:*:jobs"}],"regressions":{"previous_run":"r0","new_failures":[{"service":"sqs","operation":"GetQueueUrl","code":"QueueDoesNotExist"}],"fixed":[]}}"#,
        ]
        .join("\n");
        let calls = parse_lines(&text);
        assert_eq!(calls.len(), 1, "only call lines are rows");
        let call = &calls[0];
        assert!(call.new);
        let access: Vec<Access> = call.access().collect();
        assert_eq!(access.len(), 2);
        assert_eq!(access[1].action, "kms:Decrypt");
        assert!(call.uses(&access[1]));
        assert!(Filter::Access(access[0].clone()).matches(call));
        assert!(matches_search(call, &search_tokens("sqs:sendmessage")));

        let run = parse_run(&text);
        assert_eq!(run.command_line(), "npm test");
        assert_eq!(
            RunInfo::address(&run.header.endpoint),
            Some("127.0.0.1:4100")
        );
        assert_eq!(run.exit_code(), Some(3));
        let report = run.report.unwrap();
        assert_eq!(report.access_added.len(), 1);
        assert_eq!(
            report.regressions.unwrap().new_failures[0].code,
            "QueueDoesNotExist"
        );
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
        let rows = [iam, iam_again, lambda, s3];

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
        let facets = service_facets([&kinesis]);
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
    fn demo_is_a_real_finished_run() {
        let rows = crate::demo::demo_rows();
        let facets = service_facets(rows.iter());
        assert!(facets.iter().any(|facet| facet.id == "s3"));
        assert!(facets.len() >= 5);
        assert!(rows.iter().any(|row| row.problem.is_some()));
        assert!(rows.iter().any(|row| row.new));
        let run = crate::demo::demo_run();
        assert!(run.ended());
        assert!(!run.report.unwrap().access.is_empty());
    }

    fn line(id: &str, at: &str, extra: serde_json::Value) -> String {
        let mut value = serde_json::json!({
            "type": "call", "id": id, "at": at, "method": "POST", "path": "/", "service": "sqs",
            "operation": "SendMessage", "status": 200, "ms": 1,
        });
        if let (Some(target), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
            for (key, field) in extra {
                target.insert(key.clone(), field.clone());
            }
        }
        value.to_string()
    }

    #[test]
    fn tailer_never_splits_a_utf8_character() {
        let path = temp_trace("utf8");
        let text = line(
            "u1",
            "2026-11-15T00:00:01Z",
            serde_json::json!({"response": {"body": {"text": "你好，世界 ✓", "bytes": 20}}}),
        );
        let mut tailer = Tailer::new(&path);
        write_append(&path, "");
        assert!(matches!(tailer.poll(), TailEvent::Replaced(rows) if rows.is_empty()));
        // Cut the line inside the first multi-byte character.
        let cut = text.find('你').unwrap() + 1;
        let bytes = text.as_bytes();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&bytes[..cut]).unwrap();
        assert!(matches!(tailer.poll(), TailEvent::Idle));
        file.write_all(&bytes[cut..]).unwrap();
        file.write_all(b"\n").unwrap();
        match tailer.poll() {
            TailEvent::Appended(rows) => {
                let body = rows[0].response.as_ref().unwrap().body.as_ref().unwrap();
                assert_eq!(body.text.as_deref(), Some("你好，世界 ✓"));
            }
            other => panic!("expected append, got {other:?}"),
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn reload_fills_the_stream_even_with_large_bodies_and_hydrates_on_demand() {
        let path = temp_trace("large");
        let big = "x".repeat(10_000);
        let mut text = String::new();
        for index in 0..MAX_ROWS + 20 {
            text.push_str(&line(
                &format!("id-{index}"),
                &format!(
                    "2026-11-15T{:02}:{:02}:{:02}Z",
                    index / 3600,
                    (index / 60) % 60,
                    index % 60
                ),
                serde_json::json!({"request": {"body": {"text": big, "bytes": 10_000}}}),
            ));
            text.push('\n');
        }
        assert!(text.len() > 2 * 1024 * 1024, "spans several read blocks");
        fs::write(&path, text).unwrap();
        let mut tailer = Tailer::new(&path);
        match tailer.poll() {
            TailEvent::Replaced(rows) => {
                assert_eq!(rows.len(), MAX_ROWS);
                let last = rows.last().unwrap();
                assert_eq!(last.id, format!("id-{}", MAX_ROWS + 19));
                assert!(
                    last.request
                        .as_ref()
                        .unwrap()
                        .body
                        .as_ref()
                        .unwrap()
                        .text
                        .is_none(),
                    "large bodies are offloaded from stream RAM"
                );
                let hydrated = last.hydrated();
                assert_eq!(
                    hydrated
                        .request
                        .as_ref()
                        .unwrap()
                        .body
                        .as_ref()
                        .unwrap()
                        .text
                        .as_deref()
                        .map(str::len),
                    Some(10_000),
                    "selected row hydrates full body on demand"
                );
            }
            other => panic!("expected replace, got {other:?}"),
        }
        assert_eq!(
            tailer.skipped(),
            0,
            "the partial first line is dropped, not counted"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn follows_the_newest_run_or_a_pinned_one() {
        let dir = temp_trace("runs").with_extension("");
        let _ = fs::remove_dir_all(&dir);
        let runs = dir.join("runs");
        let mut tailer = Tailer::follow(Source::Runs {
            dir: runs.clone(),
            pinned: None,
        });
        assert!(matches!(tailer.poll(), TailEvent::Missing));

        fs::create_dir_all(&runs).unwrap();
        let header = |run: &str| {
            format!(
                r#"{{"type":"run","run":"{run}","command":["npm","test"],"endpoint":"http://127.0.0.1:4100","upstream":"u"}}"#
            )
        };
        let first = runs.join("20261115-000001-1.jsonl");
        fs::write(
            &first,
            format!(
                "{}\n{}\n",
                header("first"),
                line("a", "2026-11-15T00:00:01Z", serde_json::json!({}))
            ),
        )
        .unwrap();
        assert!(matches!(tailer.poll(), TailEvent::Replaced(rows) if rows[0].id == "a"));
        let run = tailer.run().unwrap();
        assert_eq!(run.header.run, "first");
        assert!(!run.ended());

        // The end line arrives like any other line.
        write_append(&first, "{\"type\":\"end\",\"exit_code\":3}\n");
        assert!(matches!(tailer.poll(), TailEvent::Idle));
        assert_eq!(tailer.run().unwrap().exit_code(), Some(3));

        // A newer run takes over.
        let second = runs.join("20261115-000002-2.jsonl");
        fs::write(
            &second,
            format!(
                "{}\n{}\n",
                header("second"),
                line("b", "2026-11-15T00:00:02Z", serde_json::json!({}))
            ),
        )
        .unwrap();
        assert!(matches!(tailer.poll(), TailEvent::Replaced(rows) if rows[0].id == "b"));
        assert_eq!(tailer.path(), Some(second.as_path()));
        assert_eq!(tailer.run().unwrap().header.run, "second");
        assert_eq!(list_runs(&runs), vec![first.clone(), second.clone()]);

        // Pinned to an older run, newer runs do not move the view.
        let mut pinned = Tailer::follow(Source::Runs {
            dir: runs.clone(),
            pinned: Some(first.clone()),
        });
        assert!(matches!(pinned.poll(), TailEvent::Replaced(rows) if rows[0].id == "a"));
        assert!(pinned.run().unwrap().ended());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_run_keeps_its_header_when_only_the_tail_is_read() {
        let path = temp_trace("long");
        let mut text = String::from(
            r#"{"type":"run","run":"long","command":["make"],"endpoint":"http://127.0.0.1:1","upstream":"u"}"#,
        );
        text.push('\n');
        let big = "x".repeat(5_000);
        for index in 0..MAX_ROWS + 300 {
            text.push_str(&line(
                &format!("id-{index}"),
                "2026-11-15T00:00:00Z",
                serde_json::json!({"request": {"body": {"text": big, "bytes": 5000}}}),
            ));
            text.push('\n');
        }
        fs::write(&path, text).unwrap();
        let mut tailer = Tailer::new(&path);
        assert!(matches!(tailer.poll(), TailEvent::Replaced(rows) if rows.len() == MAX_ROWS));
        assert_eq!(tailer.run().unwrap().header.run, "long");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn merge_replaces_pending_rows_and_keeps_arrival_order() {
        let pending = parse_line(&line(
            "p",
            "2026-11-15T00:00:01Z",
            serde_json::json!({"pending": true, "status": 0}),
        ))
        .unwrap();
        assert!(pending.pending);
        let rows = merge(&[], vec![pending]);
        let later = parse_line(&line("q", "2026-11-15T00:00:03Z", serde_json::json!({}))).unwrap();
        let done = parse_line(&line(
            "p",
            "2026-11-15T00:00:01Z",
            serde_json::json!({"status": 400, "problem": {"code": "InvalidParameterValue", "message": "bad"}}),
        ))
        .unwrap();
        let earlier =
            parse_line(&line("r", "2026-11-15T00:00:02Z", serde_json::json!({}))).unwrap();
        let rows = merge(&rows, vec![later, done, earlier]);
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["p", "r", "q"], "one row per call, ordered by arrival");
        assert!(!rows[0].pending);
        assert!(rows[0].is_error());
        assert!(matches_search(&rows[0], &search_tokens("invalidparameter")));
        assert!(Filter::Errors.matches(&rows[0]));

        let many: Vec<Trace> = (0..MAX_ROWS + 3)
            .map(|index| {
                parse_line(&line(
                    &format!("m{index}"),
                    "2026-11-16T00:00:00Z",
                    serde_json::json!({}),
                ))
                .unwrap()
            })
            .collect();
        assert_eq!(merge(&rows, many).len(), MAX_ROWS);
    }

    #[test]
    fn proxy_failures_count_as_errors() {
        let trace = parse_line(&line(
            "e",
            "2026-11-15T00:00:01Z",
            serde_json::json!({"status": 502, "error": "connect refused", "problem": {"code": "EmulatorUnreachable"}}),
        ))
        .unwrap();
        assert!(trace.is_error());
        assert_eq!(trace.error.as_deref(), Some("connect refused"));
    }

    #[test]
    fn pretty_prints_json_and_xml_for_reading() {
        let json = pretty_body(r#"{"b":1,"a":{"c":[1,2]}}"#, "application/x-amz-json-1.0").unwrap();
        assert!(
            json.starts_with("{\n  \"b\": 1"),
            "key order is kept: {json}"
        );
        assert!(
            pretty_body(r#"{"b":1,"a":{"c""#, "application/json").is_none(),
            "truncated JSON stays raw"
        );
        assert!(
            pretty_body("{\n  \"a\": 1\n}", "application/json").is_none(),
            "already pretty"
        );

        let xml = pretty_body(
            "<?xml version=\"1.0\"?><Error><Code>NoSuchKey</Code><Detail><Key>a</Key></Detail><Empty/></Error>",
            "application/xml",
        )
        .unwrap();
        assert_eq!(
            xml,
            "<?xml version=\"1.0\"?>\n<Error>\n  <Code>NoSuchKey</Code>\n  <Detail>\n    <Key>a</Key>\n  </Detail>\n  <Empty/>\n</Error>"
        );
        assert!(pretty_body("plain words", "text/plain").is_none());
        assert!(pretty_body("<unterminated", "application/xml").is_none());
    }

    #[test]
    fn recognizes_presigned_urls() {
        let presigned = parse_line(&line(
            "s",
            "2026-11-15T00:00:01Z",
            serde_json::json!({"path": "/b/k?X-Amz-Signature=abc"}),
        ))
        .unwrap();
        assert!(presigned.presigned());
        let legacy = parse_line(&line(
            "v2",
            "2026-11-15T00:00:01Z",
            serde_json::json!({"path": "/b/k?AWSAccessKeyId=a&Signature=b"}),
        ))
        .unwrap();
        assert!(legacy.presigned());
        let plain = parse_line(&line("p", "2026-11-15T00:00:01Z", serde_json::json!({}))).unwrap();
        assert!(!plain.presigned());
    }

    #[test]
    fn parses_timestamps_retries_per_permission_new_and_test_marks() {
        let start = parse_rfc3339_ms("2026-11-15T09:48:42.242Z").unwrap();
        let later = parse_rfc3339_ms("2026-11-15T09:48:42.616763588+00:00").unwrap();
        assert_eq!(later - start, 374);
        assert_eq!(format_offset(later - start), "+374 ms");

        let retried = parse_line(&line(
            "r1",
            "2026-11-15T09:48:43.000Z",
            serde_json::json!({
                "new": true,
                "access": [{"action": "s3:PutObject", "resource": "arn:aws:s3:::b/*"}],
                "implied": [{"action": "kms:GenerateDataKey", "resource": "arn:aws:kms:*:*:key/*", "new": true}],
                "request": {
                    "headers": [
                        {"name": "amz-sdk-invocation-id", "value": "inv-42"},
                        {"name": "amz-sdk-request", "value": "attempt=3; max=4"}
                    ]
                }
            }),
        ))
        .unwrap();
        assert!(retried.is_retry());
        assert_eq!(retried.retry_badge().as_deref(), Some("RETRY 3/4"));
        assert!(matches_search(&retried, &search_tokens("retry")));
        assert!(matches_search(&retried, &search_tokens("inv-42")));
        assert!(!retried.is_access_new(&retried.access[0]));
        assert!(retried.is_access_new(&retried.implied[0]));

        let lock_file = temp_trace("lock");
        fs::write(
            &lock_file,
            "dynamodb:CreateTable  arn:aws:dynamodb:*:*:table/orders  # test\n\
             dynamodb:PutItem      arn:aws:dynamodb:*:*:table/orders\n",
        )
        .unwrap();
        let marks = read_test_marks("", lock_file.to_str().unwrap());
        assert_eq!(marks.len(), 1);
        assert!(marks.contains(&Access {
            action: "dynamodb:CreateTable".into(),
            resource: "arn:aws:dynamodb:*:*:table/orders".into(),
        }));
        let _ = fs::remove_file(&lock_file);
    }

    #[test]
    fn builds_unredacted_curl_and_hex_dump() {
        let call = parse_line(&serde_json::to_string(&serde_json::json!({
            "type": "call",
            "id": "c1",
            "at": "2026-11-18T00:00:00Z",
            "method": "POST",
            "path": "/?x-id=PutItem",
            "host": "127.0.0.1:9297",
            "service": "dynamodb",
            "operation": "PutItem",
            "status": 400,
            "ms": 4,
            "authorization": true,
            "blocked": true,
            "request": {
                "headers": [
                    {"name": "authorization", "value": "AWS4-HMAC-SHA256 Credential=AKID/s, Signature=rawsig"},
                    {"name": "x-amz-security-token", "value": "RAWTOKEN123"},
                    {"name": "content-length", "value": "99"}
                ],
                "content_type": "application/x-amz-json-1.0",
                "body": {"text": "{\"TableName\":\"orders\",\"note\":\"it's raw\"}", "bytes": 40, "truncated": false, "binary": false}
            }
        })).unwrap()).unwrap();

        assert!(call.blocked);
        assert!(Filter::Blocked.matches(&call));
        let curl = to_curl(&call, Some("http://127.0.0.1:41234")).unwrap();
        assert!(
            curl.contains("'http://127.0.0.1:41234/?x-id=PutItem'"),
            "{curl}"
        );
        assert!(
            curl.contains("Signature=rawsig"),
            "headers stay unredacted: {curl}"
        );
        assert!(curl.contains("RAWTOKEN123"), "{curl}");
        assert!(!curl.contains("content-length"), "{curl}");
        assert!(
            curl.contains(r#"it'\''s raw"#),
            "single quotes escaped: {curl}"
        );

        let dump = format_hex_dump("89504e470d0a1a0a");
        assert!(dump.starts_with("0000  89 50 4e 47 0d 0a 1a 0a"), "{dump}");
        assert!(dump.ends_with("|.PNG....|"), "{dump}");

        // Decoded gzip / aws-chunked requests strip wire framing headers so
        // `--data-raw` replays cleanly against the emulator.
        let encoded_call = parse_line(
            &serde_json::to_string(&serde_json::json!({
                "type": "call",
                "id": "c2",
                "at": "2026-11-18T00:00:01Z",
                "method": "PUT",
                "path": "/bucket/key.json?x-id=PutObject",
                "host": "127.0.0.1:9297",
                "service": "s3",
                "operation": "PutObject",
                "status": 200,
                "ms": 3,
                "request": {
                    "headers": [
                        {"name": "content-encoding", "value": "aws-chunked,gzip"},
                        {"name": "x-amz-decoded-content-length", "value": "16"},
                        {"name": "x-amz-trailer", "value": "x-amz-checksum-crc32"},
                        {"name": "x-amz-content-sha256", "value": "STREAMING-AWS4-HMAC-SHA256-PAYLOAD-TRAILER"},
                        {"name": "content-type", "value": "application/json"}
                    ],
                    "content_type": "application/json",
                    "body": {
                        "text": "{\"order\":\"#42\"}",
                        "encoding": "aws-chunked,gzip",
                        "bytes": 98,
                        "truncated": false,
                        "binary": false
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let encoded_curl = to_curl(&encoded_call, Some("http://127.0.0.1:9297")).unwrap();
        assert!(!encoded_curl.contains("content-encoding"), "{encoded_curl}");
        assert!(
            !encoded_curl.contains("x-amz-decoded-content-length"),
            "{encoded_curl}"
        );
        assert!(!encoded_curl.contains("x-amz-trailer"), "{encoded_curl}");
        assert!(
            encoded_curl.contains("x-amz-content-sha256: UNSIGNED-PAYLOAD"),
            "{encoded_curl}"
        );
        assert!(
            encoded_curl.contains("--data-raw '{\"order\":\"#42\"}'"),
            "{encoded_curl}"
        );

        // Binary and truncated request bodies refuse `to_curl` rather than emitting
        // an incomplete or empty write.
        let mut binary_call = encoded_call.clone();
        binary_call.request.as_mut().unwrap().body = Some(Body {
            text: None,
            hex: Some("89504e47".into()),
            encoding: None,
            bytes: 4096,
            truncated: false,
            binary: true,
            sha256: None,
        });
        assert!(to_curl(&binary_call, None).is_none());

        let mut truncated_call = call.clone();
        truncated_call
            .request
            .as_mut()
            .unwrap()
            .body
            .as_mut()
            .unwrap()
            .truncated = true;
        assert!(to_curl(&truncated_call, None).is_none());
    }

    #[test]
    fn combinable_filters_offloading_policy_export_and_run_diff_work() {
        let dir = temp_trace("v6-workbench").with_extension("");
        let _ = fs::remove_dir_all(&dir);
        let runs_dir = dir.join("runs");
        fs::create_dir_all(&runs_dir).unwrap();

        let big_payload = "x".repeat(INLINE_BODY_LIMIT + 2048);
        let base_path = runs_dir.join("20261118T100000Z-100.jsonl");
        fs::write(
            &base_path,
            concat!(
                "{\"type\":\"run\",\"run\":\"base-run\",\"started\":\"2026-11-18T10:00:00Z\",\"command\":[\"pytest\"]}\n",
                "{\"type\":\"call\",\"id\":\"b1\",\"at\":\"2026-11-18T10:00:01Z\",\"method\":\"PUT\",\"path\":\"/b/k\",\"host\":\"localhost:4566\",\"service\":\"s3\",\"operation\":\"PutObject\",\"status\":200,\"ms\":12,\"access\":[{\"action\":\"s3:PutObject\",\"resource\":\"arn:aws:s3:::b/k\"}]}\n",
                "{\"type\":\"call\",\"id\":\"b2\",\"at\":\"2026-11-18T10:00:02Z\",\"method\":\"POST\",\"path\":\"/\",\"host\":\"localhost:4566\",\"service\":\"dynamodb\",\"operation\":\"GetItem\",\"status\":400,\"ms\":8,\"problem\":{\"code\":\"ResourceNotFoundException\",\"message\":\"missing\"},\"access\":[{\"action\":\"dynamodb:GetItem\",\"resource\":\"arn:aws:dynamodb:us-east-1:000000000000:table/orders\"}]}\n",
                "{\"type\":\"end\",\"run\":\"base-run\",\"calls\":2,\"errors\":1,\"blocked\":0,\"duration_ms\":20,\"exit_code\":1,\"services\":{\"s3\":1,\"dynamodb\":1},\"failures\":[{\"service\":\"dynamodb\",\"operation\":\"GetItem\",\"code\":\"ResourceNotFoundException\"}],\"access\":[{\"action\":\"s3:PutObject\",\"resource\":\"arn:aws:s3:::b/k\",\"calls\":1},{\"action\":\"dynamodb:GetItem\",\"resource\":\"arn:aws:dynamodb:us-east-1:000000000000:table/orders\",\"calls\":1}]}\n"
            ),
        )
        .unwrap();

        let target_path = runs_dir.join("20261118T100500Z-200.jsonl");
        let large_call = serde_json::to_string(&serde_json::json!({
            "type": "call",
            "id": "t1",
            "at": "2026-11-18T10:05:01Z",
            "method": "PUT",
            "path": "/b/big.json",
            "host": "localhost:4566",
            "service": "s3",
            "operation": "PutObject",
            "status": 200,
            "ms": 25,
            "access": [{"action": "s3:PutObject", "resource": "arn:aws:s3:::b/big.json"}],
            "request": {
                "content_type": "application/json",
                "body": {
                    "text": big_payload,
                    "bytes": big_payload.len(),
                    "truncated": false,
                    "binary": false
                }
            }
        }))
        .unwrap();
        let blocked_call = serde_json::to_string(&serde_json::json!({
            "type": "call",
            "id": "t2",
            "at": "2026-11-18T10:05:02Z",
            "method": "POST",
            "path": "/",
            "host": "localhost:4566",
            "service": "kms",
            "operation": "Decrypt",
            "status": 403,
            "ms": 4,
            "blocked": true,
            "problem": {"code": "AccessDeniedException", "message": "blocked"},
            "access": [{"action": "kms:Decrypt", "resource": "arn:aws:kms:us-east-1:000000000000:key/k1"}]
        }))
        .unwrap();
        let end_line = serde_json::to_string(&serde_json::json!({
            "type": "end",
            "run": "target-run",
            "calls": 2,
            "errors": 1,
            "blocked": 1,
            "duration_ms": 29,
            "exit_code": 1,
            "services": {"s3": 1, "kms": 1},
            "failures": [{"service": "kms", "operation": "Decrypt", "code": "AccessDeniedException"}],
            "access": [
                {"action": "s3:PutObject", "resource": "arn:aws:s3:::b/big.json", "calls": 1, "new": true},
                {"action": "kms:Decrypt", "resource": "arn:aws:kms:us-east-1:000000000000:key/k1", "calls": 1, "new": true}
            ],
            "access_unused": [
                {"action": "sqs:SendMessage", "resource": "arn:aws:sqs:us-east-1:000000000000:jobs"}
            ]
        }))
        .unwrap();
        fs::write(
            &target_path,
            format!(
                "{{\"type\":\"run\",\"run\":\"target-run\",\"started\":\"2026-11-18T10:05:00Z\",\"command\":[\"pytest\",\"-k\",\"kms\"]}}\n{large_call}\n{blocked_call}\n{end_line}\n"
            ),
        )
        .unwrap();

        let live_path = runs_dir.join("20261118T101000Z-300.jsonl");
        fs::write(
            &live_path,
            "{\"type\":\"run\",\"run\":\"live-run\",\"started\":\"2026-11-18T10:10:00Z\",\"command\":[\"python\",\"app.py\"]}\n",
        )
        .unwrap();

        let mut tailer = Tailer::new(&target_path);
        let rows: Vec<Arc<Trace>> = match tailer.poll() {
            TailEvent::Replaced(rows) => rows.into_iter().map(Arc::new).collect(),
            other => panic!("expected Replaced, got {other:?}"),
        };
        assert_eq!(rows.len(), 2);

        // Large payload (> INLINE_BODY_LIMIT) is offloaded in stream memory and hydrated on demand.
        let stream_t1 = &rows[0];
        assert!(
            stream_t1
                .request
                .as_ref()
                .unwrap()
                .body
                .as_ref()
                .unwrap()
                .text
                .is_none()
        );
        let hydrated_t1 = stream_t1.hydrated();
        assert_eq!(
            hydrated_t1
                .request
                .as_ref()
                .unwrap()
                .body
                .as_ref()
                .unwrap()
                .text
                .as_deref(),
            Some(big_payload.as_str())
        );

        // Combinable CallFilter: Blocked + service = kms matches only t2.
        let filter = CallFilter {
            status: StatusFilter::Blocked,
            service: Some("kms".into()),
            access: None,
        };
        assert!(!filter.matches(&rows[0]));
        assert!(filter.matches(&rows[1]));

        // AccessFilter + search + IAM statement/policy export.
        let run_info = tailer.run().unwrap();
        let report = run_info.report.as_ref().unwrap();
        assert!(AccessFilter::New.matches(&report.access[0]));
        assert!(!AccessFilter::Unused.matches(&report.access[0]));
        assert!(report.access[1].matches_search(&search_tokens("kms")));
        let policy_doc: serde_json::Value =
            serde_json::from_str(&export_policy_json(&report.access)).unwrap();
        assert_eq!(policy_doc["Version"], "2012-10-17");
        assert_eq!(policy_doc["Statement"].as_array().unwrap().len(), 2);
        let single_stmt: serde_json::Value =
            serde_json::from_str(&statement_json(&report.access[1])).unwrap();
        assert_eq!(single_stmt["Action"], "kms:Decrypt");

        // Run-to-Run Diff between base and target.
        let base_summary = load_run_summary(&base_path).unwrap();
        let diff =
            UiRunDiff::from_summaries(&base_summary, &run_info, &rows, &report.access).unwrap();
        assert_eq!(diff.base_id, "base-run");
        assert_eq!(diff.target_id, "target-run");
        assert_eq!(diff.new_failures.len(), 1);
        assert_eq!(diff.new_failures[0].service, "kms");
        assert_eq!(diff.fixed_failures.len(), 1);
        assert_eq!(diff.fixed_failures[0].service, "dynamodb");

        // Historical baseline ordering: for finished runs A < B < C, viewing B
        // defaults `prev` to A (earlier) while keeping C available afterwards,
        // and viewing oldest A yields `prev == None` so Diff never reverses C → A.
        let newer_path = runs_dir.join("20261118T100800Z-250.jsonl");
        fs::write(
            &newer_path,
            concat!(
                "{\"type\":\"run\",\"run\":\"newer-run\",\"started\":\"2026-11-18T10:08:00Z\",\"command\":[\"pytest\"]}\n",
                "{\"type\":\"end\",\"run\":\"newer-run\",\"calls\":0,\"errors\":0,\"blocked\":0,\"duration_ms\":5,\"exit_code\":0}\n"
            ),
        )
        .unwrap();
        let (bases_for_b, prev_for_b) = ordered_baselines(&runs_dir, Some(&target_path));
        assert_eq!(prev_for_b.as_deref(), Some(base_path.as_path()));
        assert_eq!(bases_for_b.len(), 2);
        assert_eq!(bases_for_b[0].path, base_path);
        assert_eq!(bases_for_b[1].path, newer_path);

        let (bases_for_a, prev_for_a) = ordered_baselines(&runs_dir, Some(&base_path));
        assert!(prev_for_a.is_none());
        assert_eq!(bases_for_a.len(), 2);
        fs::remove_file(&newer_path).unwrap();

        // Storage stats & clear_finished_runs preserves current + live runs and removes finished base_path.
        let (count_before, bytes_before) = storage_stats(&runs_dir);
        assert_eq!(count_before, 3);
        assert!(bytes_before > 0);
        let removed = clear_finished_runs(&runs_dir, Some(&target_path));
        assert_eq!(removed, 1);
        assert!(!base_path.exists());
        assert!(target_path.exists());
        assert!(live_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
