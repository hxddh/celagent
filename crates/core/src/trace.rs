//! The run-file schema: the records `lenslet run` writes and every reader parses.

use http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::{
    access::{Access, CallAccess},
    problem::Problem,
    summary::Report,
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
    /// The IAM policy document this run is compared/enforced with, when `--policy` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    /// Whether unlocked/denied calls were actively blocked by `--enforce`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enforce: bool,
}

/// One line of a run file, for readers.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Record {
    Run(RunHeader),
    Call(Box<Trace>),
    End(Box<Report>),
}

/// Write `value` as `{"type": kind, ...value}`.
#[derive(Serialize)]
pub struct Tagged<'a, T: Serialize> {
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(flatten)]
    pub value: &'a T,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
    /// SDK invocation identifier (`amz-sdk-invocation-id`), constant across retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation_id: Option<String>,
    /// Retry attempt number (`> 1`) from `amz-sdk-request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    /// Maximum attempts configured for this SDK invocation, when retried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
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
    pub access: Vec<CallAccess>,
    /// Permissions the call implies elsewhere (SSE-KMS key, passed role).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implied: Vec<CallAccess>,
    /// The access is not in the lock file: this run grew the footprint.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
    /// Denied at the proxy by `--enforce` with an AWS `AccessDenied` reply.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocked: bool,
    /// When a blocked call was stopped: `request` (it never reached the
    /// emulator) or `response` (the emulator had already run it, and only its
    /// response revealed the denied permission, e.g. an SSE-KMS key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_phase: Option<BlockedPhase>,
    /// Set when this call was re-sent from the window (`x-lenslet-replay-of`):
    /// the id of the recorded call it replays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_of: Option<String>,
    /// IAM role ARN attributed via an earlier `sts:AssumeRole*` session in the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_role: Option<String>,
    /// Concise semantic target summary (e.g. `table/orders · id="ord-42"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub request: Message,
    pub response: Message,
}

/// See [`Trace::blocked_phase`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlockedPhase {
    Request,
    Response,
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
    /// Hex preview (up to the first 512 bytes) when `binary` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hex: Option<String>,
    /// Wire content encoding when decoded for inspection (`aws-chunked`, `gzip`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
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

/// Every permission one call accounts for: its own, then the implied ones.
pub fn trace_access(trace: &Trace) -> impl Iterator<Item = Access> + '_ {
    trace
        .access
        .iter()
        .chain(trace.implied.iter())
        .map(CallAccess::key)
}
