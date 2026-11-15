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
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result};
use hyper::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::{
    access::{self, Access},
    problem::Problem,
    summary::{Report, Summary},
};

/// Schema version written in every run header.
pub const VERSION: u32 = 3;

/// The first line of a run file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunHeader {
    pub version: u32,
    /// The file stem, e.g. `20261115-054628-15569`.
    pub run: String,
    /// The program run under Lenslet; empty for a bare proxy session.
    #[serde(default)]
    pub command: Vec<String>,
    pub cwd: String,
    pub pid: u32,
    /// What SDKs point at, e.g. `http://127.0.0.1:43761`.
    pub endpoint: String,
    pub upstream: String,
    pub started: String,
    /// The lock file this run is compared with, if one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<String>,
}

/// One line of a run file, for readers.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Record {
    Run(RunHeader),
    // Read by tests; the CLI itself only needs the header and the report.
    #[allow(dead_code)]
    Call(Box<Trace>),
    End(Box<Report>),
}

/// Write `value` as `{"type": kind, ...value}`.
#[derive(Serialize)]
struct Tagged<'a, T: Serialize> {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(flatten)]
    value: &'a T,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trace {
    /// Unique per call within (and across) proxy runs.
    pub id: String,
    /// RFC3339 timestamp of when the request arrived.
    pub at: String,
    pub method: String,
    /// Path and query exactly as sent.
    pub path: String,
    pub host: String,
    pub service: String,
    pub operation: String,
    pub status: u16,
    /// Wall time for the complete exchange, measured after the body finished.
    pub ms: u64,
    pub authorization: bool,
    /// SigV4 signing region, when the request was signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Written for a call that is still running after a second; the final
    /// line for the same `id` replaces it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pending: bool,
    /// Set when the proxy could not complete the exchange.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Error code, message and likely cause of a failed call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<Problem>,
    /// The IAM permissions this call needs (`s3:GetObject` on a bucket):
    /// usually one; several for batches, transactions and copies; none when
    /// the operation is unknown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub access: Vec<Access>,
    /// Permissions the call implies elsewhere (SSE-KMS key, passed role).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implied: Vec<Access>,
    /// The access is not in the lock file: this run grew the footprint.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
    pub request: Message,
    pub response: Message,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Message {
    /// Raw header values in map order, duplicates preserved.
    pub headers: Vec<HeaderTrace>,
    /// Media type without parameters, e.g. `application/xml`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// Absent when the body was empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<BodyTrace>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeaderTrace {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BodyTrace {
    /// Absent for binary (or non-text) bodies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Total bytes of the original body, not just the captured prefix.
    pub bytes: u64,
    /// The capture holds only a prefix of a text body.
    pub truncated: bool,
    pub binary: bool,
    /// SHA-256 of the original bytes (all of them, even when truncated).
    pub sha256: String,
}

/// Raw headers in a stable order with duplicates preserved.
///
/// Order follows the header map (hyper groups repeated names at their first
/// position), so the same header set always produces the same list.
pub fn headers(headers: &HeaderMap) -> Vec<HeaderTrace> {
    headers
        .iter()
        .map(|(name, value)| HeaderTrace {
            name: name.as_str().to_string(),
            value: String::from_utf8_lossy(value.as_bytes()).into_owned(),
        })
        .collect()
}

/// Append-only run file. Writes are serialized; the sink also keeps the
/// in-memory [`Summary`] that becomes the `end` report.
pub struct TraceSink {
    path: PathBuf,
    summary: Mutex<Summary>,
    /// Access recorded in the lock file, when there is one.
    locked: Option<BTreeSet<Access>>,
}

impl TraceSink {
    /// Create the parent directory and the trace file so failures surface at
    /// startup rather than on the first proxied call. On Unix the trace is
    /// owner-only (0600).
    pub fn new(
        path: PathBuf,
        header: &RunHeader,
        locked: Option<BTreeSet<Access>>,
    ) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("create trace directory {}", parent.display()))?;
        }
        open_trace(&path).with_context(|| format!("open trace file {}", path.display()))?;
        set_owner_only(&path)?;
        let sink = TraceSink {
            path,
            summary: Mutex::new(Summary::default()),
            locked,
        };
        sink.write("run", header);
        Ok(sink)
    }

    /// Append the `end` report. Nothing is written after it.
    pub fn end(&self, report: &Report) {
        let _guard = self
            .summary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.write("end", report);
    }

    fn write<T: Serialize>(&self, kind: &'static str, value: &T) {
        let line = match serde_json::to_string(&Tagged { kind, value }) {
            Ok(line) => line,
            Err(error) => {
                eprintln!("lenslet: cannot encode trace: {error}");
                return;
            }
        };
        match open_trace(&self.path) {
            Ok(mut file) => {
                // One write per line, so a reader never sees half a record
                // followed by another writer's line.
                if let Err(error) = file.write_all(format!("{line}\n").as_bytes()) {
                    eprintln!("lenslet: cannot write trace: {error}");
                }
            }
            Err(error) => eprintln!(
                "lenslet: cannot open trace {}: {error}",
                self.path.display()
            ),
        }
    }

    /// Note a call that has started (for the in-flight count).
    pub fn start(&self, id: &str) {
        self.summary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .start(id);
    }

    /// A copy of the calls seen so far.
    pub fn summary(&self) -> Summary {
        self.summary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Write one call. Never fails the caller: trace problems are logged to
    /// stderr and the proxy keeps forwarding.
    pub fn record(&self, mut trace: Trace) {
        if !trace.pending && trace.access.is_empty() {
            let (own, implied) = access::derive(&trace);
            trace.access = own;
            trace.implied = implied;
        }
        if let Some(locked) = self.locked.as_ref() {
            let new = trace_access(&trace).any(|access| !locked.contains(&access));
            trace.new = new;
        }
        // The summary lock also serializes writes, so this check and the
        // write are atomic: a pending line can never land after the call's
        // final line (which would make readers show a finished call as
        // pending).
        let mut summary = self
            .summary
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if trace.pending && !summary.in_flight(&trace.id) {
            return;
        }
        summary.observe(&trace);
        self.write("call", &trace);
    }
}

/// Every permission one call accounts for: its own, then the implied ones.
pub fn trace_access(trace: &Trace) -> impl Iterator<Item = Access> + '_ {
    trace.access.iter().chain(trace.implied.iter()).cloned()
}

fn open_trace(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
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
    use hyper::header::HeaderValue;

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
            pending: false,
            error: None,
            problem: None,
            access: Vec::new(),
            implied: Vec::new(),
            new: false,
            request: Message {
                headers: headers(&request_headers),
                content_type: Some("application/json".to_string()),
                body: Some(BodyTrace {
                    text: Some(r#"{"password":"hunter2","name":"lenslet"}"#.to_string()),
                    bytes: 40,
                    truncated: false,
                    binary: false,
                    sha256: "0".repeat(64),
                }),
            },
            response: Message {
                headers: headers(&response_headers),
                content_type: Some("application/json".to_string()),
                body: None,
            },
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
        sink.record(sample_trace());
        sink.record(sample_trace());
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
        sink.record(done.clone());
        sink.record(late_pending);
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
        sink.record(Trace {
            pending: true,
            ..other.clone()
        });
        assert_eq!(sink.summary().unfinished, 1);
        sink.record(other);
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 4);
        assert_eq!(sink.summary().unfinished, 0);
        let _ = fs::remove_dir_all(&dir);
    }
}
