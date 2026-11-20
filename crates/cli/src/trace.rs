//! The trace: one JSONL file per run, and its on-disk sink.
//!
//! A run file is an event stream. The first line is the `run` header, then one
//! `call` line per proxied call (a slow call may also get an earlier `pending`
//! line with the same id), and the last line is the `end` report. Every fact a
//! reader shows (the error, the IAM action, whether the access is new) is
//! computed here once and written down, so the window, `--json` and agents
//! all see the same thing. The trace keeps raw headers and text bodies and is
//! written owner-only.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result};

use serde::Serialize;
use tokio::sync::watch;

use crate::{
    lock::{self, Evaluation, Gate},
    summary::{Report, Summary},
};

pub use lenslet_core::trace::*;

struct SinkState {
    file: File,
    summary: Summary,
    /// Finished calls (their [`lenslet_core::findings::essentials`]), for
    /// the findings at the end.
    calls: Vec<Trace>,
    /// Set by [`TraceSink::seal`]: nothing but the `end` line is written after.
    sealed: bool,
    /// Bytes written to the file so far.
    written: u64,
}

/// Append-only run file. Writes are serialized through one open file handle;
/// the sink also keeps the in-memory [`Summary`] that becomes the `end` report.
pub struct TraceSink {
    state: Mutex<SinkState>,
    /// Lock file or IAM policy gate this run is compared/enforced against.
    gate: Option<Gate>,
    path: PathBuf,
    run: String,
    /// The file's length after each line: wakes live event streams.
    written: watch::Sender<u64>,
}

impl TraceSink {
    /// Create the parent directory and the trace file (replacing one that
    /// exists) so failures surface at startup rather than on the first
    /// proxied call. On Unix the trace is owner-only (0600).
    pub fn new(path: PathBuf, header: &RunHeader, gate: Option<Gate>) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("create trace directory {}", parent.display()))?;
        }
        let file =
            open_trace(&path).with_context(|| format!("open trace file {}", path.display()))?;
        set_owner_only(&path)?;
        let sink = TraceSink {
            state: Mutex::new(SinkState {
                file,
                summary: Summary::default(),
                calls: Vec::new(),
                sealed: false,
                written: 0,
            }),
            gate,
            run: header.run.clone(),
            path,
            written: watch::Sender::new(0),
        };
        if let Some(line) = encode("run", header) {
            sink.write_line(&mut sink.lock(), &line);
        }
        Ok(sink)
    }

    pub fn gate(&self) -> Option<&Gate> {
        self.gate.as_ref()
    }

    /// The run file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The run id (the file stem).
    pub fn run(&self) -> &str {
        &self.run
    }

    /// Wakes whenever a line has been written; the value is the file length.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.written.subscribe()
    }

    /// Derive what `trace` needs and check it against this run's gate.
    pub fn evaluate(&self, trace: &Trace) -> Evaluation {
        lock::evaluate(trace, self.gate.as_ref())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SinkState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Note a call that has started (for the in-flight count).
    pub fn start(&self, id: &str) {
        self.lock().summary.start(id);
    }

    /// Calls started and not yet recorded.
    pub fn unfinished(&self) -> usize {
        self.lock().summary.unfinished
    }

    /// Live call counters `(calls, errors, blocked)` for `/_lenslet/health`.
    pub fn live_counts(&self) -> (usize, usize, usize) {
        let s = self.lock();
        (s.summary.calls, s.summary.errors, s.summary.blocked)
    }

    /// A copy of the calls seen so far.
    #[cfg(test)]
    pub fn summary(&self) -> Summary {
        self.lock().summary.clone()
    }

    /// Write a `pending` line for a call still in flight. Dropped when the
    /// call has already finished (or the run has been sealed).
    pub fn record_pending(&self, trace: &Trace) {
        let Some(line) = encode("call", trace) else {
            return;
        };
        // The check and the write happen under one lock, so a pending line
        // can never land after the call's final line.
        let mut state = self.lock();
        if state.sealed || !state.summary.in_flight(&trace.id) {
            return;
        }
        self.write_line(&mut state, &line);
    }

    /// Write one finished call with its gate evaluation. Never fails the
    /// caller: trace problems are logged to stderr and the proxy keeps
    /// forwarding. After [`TraceSink::seal`] the call is dropped and stays
    /// counted as unfinished.
    #[cfg(test)]
    pub fn record(&self, trace: Trace, evaluation: Evaluation) {
        self.record_with_key(trace, None, evaluation);
    }

    pub fn record_with_key(
        &self,
        mut trace: Trace,
        access_key_id: Option<&str>,
        evaluation: Evaluation,
    ) {
        trace.pending = false;
        trace.access = evaluation.access;
        trace.implied = evaluation.implied;
        if trace.target.is_none() {
            trace.target = Some(lenslet_core::catalog::target_summary(&trace));
        }
        if self.gate.is_some() {
            trace.new = !evaluation.denied.is_empty();
        }
        // Encode before locking; only a cross-call hint or AssumeRole session re-encodes.
        let mut line = encode("call", &trace);
        let mut essentials = lenslet_core::findings::essentials(&trace);
        let mut state = self.lock();
        if state.sealed {
            return;
        }
        let prev_role = trace.caller_role.clone();
        state
            .summary
            .role_tracker
            .observe_call(access_key_id, &mut trace);
        let role_changed = trace.caller_role != prev_role;
        let call_no = state.summary.calls + 1;
        let hint_changed =
            state
                .summary
                .causal
                .observe_and_enrich(&mut trace, &evaluation.concrete_own, call_no);
        if role_changed || hint_changed {
            line = encode("call", &trace);
            essentials = lenslet_core::findings::essentials(&trace);
        }
        if self.gate.is_some() {
            state.summary.gate_evaluated = true;
            state
                .summary
                .denied_access
                .extend(evaluation.denied.iter().map(|denial| denial.key()));
        }
        state.summary.observe(&trace);
        state.calls.push(essentials);
        if let Some(line) = line {
            self.write_line(&mut state, &line);
        }
    }

    /// Stop recording: calls that finish from now on are dropped (and stay
    /// counted in `unfinished`). Returns the final tally and the calls.
    pub fn seal(&self) -> (Summary, Vec<Trace>) {
        let mut state = self.lock();
        state.sealed = true;
        (state.summary.clone(), std::mem::take(&mut state.calls))
    }

    /// Seal the run and append the `end` report: nothing is written after it.
    pub fn end(&self, report: &Report) {
        let line = encode("end", report);
        let mut state = self.lock();
        state.sealed = true;
        if let Some(line) = line {
            self.write_line(&mut state, &line);
        }
    }

    /// One `write_all` per line, so a reader never sees half a record
    /// followed by another writer's line.
    fn write_line(&self, state: &mut SinkState, line: &str) {
        match state.file.write_all(line.as_bytes()) {
            Ok(()) => {
                state.written += line.len() as u64;
                self.written.send_replace(state.written);
            }
            Err(error) => eprintln!("lenslet: cannot write trace: {error}"),
        }
    }
}

/// `{"type": kind, ...value}` and a newline.
fn encode<T: Serialize>(kind: &'static str, value: &T) -> Option<String> {
    match serde_json::to_string(&Tagged { kind, value }) {
        Ok(line) => Some(line + "\n"),
        Err(error) => {
            eprintln!("lenslet: cannot encode trace: {error}");
            None
        }
    }
}

/// A fresh, owner-only file: `--trace PATH` replaces what was there.
fn open_trace(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Owner-only trace file (no-op off Unix).
#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::Access;
    use hyper::{HeaderMap, header::HeaderValue};
    use std::collections::BTreeSet;

    /// Record a finished call the way the proxy does: evaluate, then write.
    fn record(sink: &TraceSink, trace: Trace) {
        let evaluation = sink.evaluate(&trace);
        sink.record(trace, evaluation);
    }

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "lenslet-sink-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn append(map: &mut HeaderMap, name: &str, value: &str) {
        map.append(
            hyper::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }

    fn sample_trace() -> Trace {
        let mut request_headers = HeaderMap::new();
        append(
            &mut request_headers,
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261115/us-east-1/s3/aws4_request, Signature=deadbeef",
        );
        append(
            &mut request_headers,
            "x-amz-security-token",
            "SESSIONTOKEN123",
        );
        append(&mut request_headers, "cookie", "session=abc123");
        append(&mut request_headers, "content-type", "application/json");
        append(&mut request_headers, "host", "127.0.0.1:9297");

        let mut response_headers = HeaderMap::new();
        append(&mut response_headers, "content-type", "application/json");
        append(&mut response_headers, "set-cookie", "token=xyz");

        Trace {
            id: "abc-1".to_string(),
            at: "2026-11-15T00:00:00+00:00".to_string(),
            method: "PUT".to_string(),
            path: "/bucket/key?x-id=PutObject".to_string(),
            host: "127.0.0.1:9297".to_string(),
            service: "s3".to_string(),
            operation: "PutObject".to_string(),
            status: 200,
            ms: 12,
            authorization: true,
            region: None,
            invocation_id: None,
            attempt: None,
            max_attempts: None,
            pending: false,
            error: None,
            problem: None,
            access: Vec::new(),
            implied: Vec::new(),
            new: false,
            blocked: false,
            replay_of: None,
            blocked_phase: None,
            caller_role: None,
            target: None,
            request: Message {
                headers: headers(&request_headers),
                content_type: Some("application/json".to_string()),
                body: Some(BodyTrace {
                    text: Some(r#"{"password":"hunter2","name":"lenslet"}"#.to_string()),
                    bytes: 40,
                    sha256: "0".repeat(64),
                    ..Default::default()
                }),
                ..Default::default()
            },
            response: Message {
                headers: headers(&response_headers),
                content_type: Some("application/json".to_string()),
                body: None,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn headers_are_raw_and_keep_duplicates() {
        let mut map = HeaderMap::new();
        append(&mut map, "x-amz-meta-tag", "one");
        append(&mut map, "content-type", "application/xml");
        append(&mut map, "x-amz-meta-tag", "two");
        append(
            &mut map,
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE, Signature=deadbeef",
        );

        let traced = headers(&map);
        let names: Vec<&str> = traced.iter().map(|header| header.name.as_str()).collect();
        // hyper groups repeated names at their first position; the order is
        // stable and both values survive.
        assert_eq!(
            names,
            [
                "x-amz-meta-tag",
                "x-amz-meta-tag",
                "content-type",
                "authorization"
            ]
        );
        assert_eq!(traced[0].value, "one");
        assert_eq!(traced[1].value, "two");
        assert_eq!(traced[2].value, "application/xml");
        assert_eq!(
            traced[3].value,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE, Signature=deadbeef"
        );

        // Stable across repeated calls.
        assert_eq!(
            traced.iter().map(|h| h.value.clone()).collect::<Vec<_>>(),
            headers(&map)
                .iter()
                .map(|h| h.value.clone())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn trace_round_trips_and_keeps_raw_values() {
        let trace = sample_trace();
        let line = serde_json::to_string(&trace).unwrap();
        assert!(!line.contains('\n'), "one JSONL entry is one line");
        assert!(line.contains("SESSIONTOKEN123"));
        assert!(line.contains("session=abc123"));

        let back: Trace = serde_json::from_str(&line).expect("trace round-trips");
        assert_eq!(back.id, "abc-1");
        assert_eq!(back.status, 200);
        assert_eq!(back.ms, 12);
        assert_eq!(back.service, "s3");
        assert_eq!(back.operation, "PutObject");
        assert!(back.response.body.is_none());

        let authorization = back
            .request
            .headers
            .iter()
            .find(|header| header.name == "authorization")
            .expect("authorization header is traced");
        assert_eq!(
            authorization.value,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261115/us-east-1/s3/aws4_request, Signature=deadbeef"
        );

        let body = back.request.body.expect("request body survives");
        assert_eq!(body.bytes, 40);
        assert_eq!(
            body.text.as_deref(),
            Some(r#"{"password":"hunter2","name":"lenslet"}"#)
        );
        assert!(!body.binary);
        assert_eq!(body.sha256.len(), 64);
        assert_eq!(
            back.request.content_type.as_deref(),
            Some("application/json")
        );
    }

    #[test]
    fn empty_bodies_and_optional_fields_are_omitted() {
        let sample = sample_trace();
        let trace = Trace {
            request: Message {
                content_type: Some("application/xml".to_string()),
                body: None,
                ..sample.request
            },
            response: Message {
                content_type: None,
                body: None,
                ..sample.response
            },
            ..sample
        };
        let value: serde_json::Value = serde_json::to_value(&trace).unwrap();
        assert!(value["request"].get("body").is_none());
        assert!(value["response"].get("body").is_none());
        assert!(value["response"].get("content_type").is_none());
        assert!(value.get("error").is_none());
    }

    #[test]
    fn sink_appends_one_line_per_entry_with_owner_only_permissions() {
        let dir = std::env::temp_dir().join(format!(
            "lenslet-trace-sink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("nested/trace.jsonl");
        let sink = TraceSink::new(path.clone(), &RunHeader::default(), None).unwrap();
        record(&sink, sample_trace());
        record(&sink, sample_trace());
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().count(),
            3,
            "the run header, then one line per call"
        );
        assert!(text.ends_with('\n'));
        let records: Vec<Record> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(matches!(records[0], Record::Run(_)));
        assert!(matches!(records[1], Record::Call(_)));
        sink.end(&crate::summary::Report::default());
        let last = fs::read_to_string(&path).unwrap();
        let last = last.lines().last().unwrap();
        assert!(matches!(
            serde_json::from_str(last).unwrap(),
            Record::End(_)
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "trace is owner-only");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pending_line_never_follows_the_final_line() {
        let dir = std::env::temp_dir().join(format!("lenslet-sink-{}", std::process::id()));
        let path = dir.join("trace.jsonl");
        let sink = TraceSink::new(path.clone(), &RunHeader::default(), None).unwrap();
        let done = sample_trace();
        let late_pending = Trace {
            pending: true,
            status: 0,
            ..done.clone()
        };

        // The timer lost the race: the call finished first.
        sink.start(&done.id);
        record(&sink, done.clone());
        sink.record_pending(&late_pending);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().count(),
            2,
            "header + call; the late pending line is dropped"
        );
        assert_eq!(sink.summary().unfinished, 0);
        assert_eq!(sink.summary().calls, 1);

        // In the normal order both lines are kept.
        let other = Trace {
            id: "abc-2".to_string(),
            ..done.clone()
        };
        sink.start(&other.id);
        sink.record_pending(&Trace {
            pending: true,
            ..other.clone()
        });
        assert_eq!(sink.summary().unfinished, 1);
        record(&sink, other);
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 4);
        assert_eq!(sink.summary().unfinished, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn marks_only_the_unlocked_permission_as_new() {
        let dir = std::env::temp_dir().join(format!("lenslet-new-attr-{}", std::process::id()));
        let path = dir.join("trace.jsonl");
        let locked: BTreeSet<Access> =
            [Access::new("s3:PutObject", "arn:aws:s3:::bucket/*")].into();
        let sink = TraceSink::new(
            path.clone(),
            &RunHeader::default(),
            Some(Gate::from_lock("lenslet.lock", locked)),
        )
        .unwrap();
        let mut trace = sample_trace();
        trace.request.headers.push(HeaderTrace {
            name: "x-amz-server-side-encryption".into(),
            value: "aws:kms".into(),
        });
        record(&sink, trace);
        let text = fs::read_to_string(&path).unwrap();
        let call: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        assert_eq!(call["new"], true);
        assert!(
            call["access"][0].get("new").is_none(),
            "locked permission stays unmarked: {call}"
        );
        assert_eq!(
            call["implied"][0]["new"], true,
            "only the implied kms permission is new: {call}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_given_trace_path_is_replaced_not_appended_to() {
        let dir = temp_dir("truncate");
        let path = dir.join("fixed.jsonl");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "{\"type\":\"end\"}\nold run\n").unwrap();
        let sink = TraceSink::new(path.clone(), &RunHeader::default(), None).unwrap();
        record(&sink, sample_trace());
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.starts_with("{\"type\":\"run\""), "{text}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "a replaced trace is owner-only too");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn nothing_is_written_after_the_run_is_sealed() {
        let dir = temp_dir("sealed");
        let path = dir.join("trace.jsonl");
        let sink = TraceSink::new(path.clone(), &RunHeader::default(), None).unwrap();
        let mut written = sink.subscribe();
        let late = sample_trace();
        sink.start(&late.id);
        let (summary, calls) = sink.seal();
        assert_eq!((summary.calls, summary.unfinished), (0, 1));
        assert!(calls.is_empty());
        // The late call finishes after the report was taken: it is dropped
        // and stays unfinished.
        record(&sink, late.clone());
        sink.record_pending(&Trace {
            pending: true,
            ..late
        });
        sink.end(&crate::summary::Report::default());
        assert_eq!(sink.summary().unfinished, 1);
        let text = fs::read_to_string(&path).unwrap();
        let kinds: Vec<String> = text
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(kinds, ["run", "end"]);
        // Every write is signalled with the file's new length.
        assert!(written.has_changed().unwrap());
        assert_eq!(*written.borrow_and_update(), text.len() as u64);
        let _ = fs::remove_dir_all(&dir);
    }
}
