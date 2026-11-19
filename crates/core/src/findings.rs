//! Findings: everything in a run worth a look, as one ranked list.
//!
//! A finding is a pure function of the run's call records (and, for access
//! findings, of the lock/policy comparison already written on each call). The
//! report carries them (`findings`), and the window computes the same list
//! live from the calls it has, so both always agree.
//!
//! Kinds, most severe first:
//! - `failure`: a failed call, with its cause (root-cause correlation included).
//! - `access`: a call needs access the lock/policy does not grant.
//! - `n_plus_one`: the same read repeated in a tight serial burst that a batch
//!   API would collapse (e.g. many `GetItem` → `BatchGetItem`).
//! - `leaked`: a resource the run created and never deleted.
//! - `retries`: an SDK invocation that needed retries.
//!
//! Severity: failures are errors; access, serial bursts and retries that
//! finally failed are warnings; leaks and recovered retries are info.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::{
    summary::{causal_keys, is_create_operation, is_delete_operation},
    trace::{BodyTrace, Message, Trace},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Failure,
    Access,
    NPlusOne,
    Leaked,
    Retries,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub severity: Severity,
    /// One line, e.g. `DYNAMODB GetItem ×37 in a serial burst`.
    pub title: String,
    /// The why / what to do, one or two sentences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Ids of the calls this finding is about, in call order (first is the
    /// one to jump to).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
}

/// All findings for these calls (finished calls, in arrival order), most
/// severe first, then in call order. Access findings name `lenslet.lock`.
pub fn analyze<'a>(calls: impl IntoIterator<Item = &'a Trace>) -> Vec<Finding> {
    analyze_against(calls, crate::lock::FILE_NAME)
}

/// [`analyze`], naming `gate` (the lock or policy file the run was compared
/// with) in access findings.
pub fn analyze_against<'a>(calls: impl IntoIterator<Item = &'a Trace>, gate: &str) -> Vec<Finding> {
    let calls: Vec<&Trace> = calls.into_iter().filter(|call| !call.pending).collect();
    let mut found = Vec::new();
    failures(&calls, &mut found);
    access(&calls, gate, &mut found);
    n_plus_one(&calls, &mut found);
    leaked(&calls, &mut found);
    retries(&calls, &mut found);
    found.sort_by_key(|(first, finding)| (finding.severity, *first));
    found.into_iter().map(|(_, finding)| finding).collect()
}

/// The parts of a call [`analyze`] reads, without its bodies' text (kept
/// only for create and delete calls, which name their resource in it), so a
/// run's calls can be kept in memory until it ends.
pub fn essentials(trace: &Trace) -> Trace {
    let lifecycle = is_create_operation(&trace.operation) || is_delete_operation(&trace.operation);
    let body = trace.request.body.as_ref().map(|body| BodyTrace {
        text: body.text.as_ref().filter(|_| lifecycle).cloned(),
        hex: None,
        encoding: body.encoding.clone(),
        bytes: body.bytes,
        truncated: body.truncated,
        binary: body.binary,
        sha256: body.sha256.clone(),
    });
    Trace {
        id: trace.id.clone(),
        at: trace.at.clone(),
        method: trace.method.clone(),
        path: trace.path.clone(),
        host: trace.host.clone(),
        service: trace.service.clone(),
        operation: trace.operation.clone(),
        status: trace.status,
        ms: trace.ms,
        authorization: trace.authorization,
        region: trace.region.clone(),
        invocation_id: trace.invocation_id.clone(),
        attempt: trace.attempt,
        max_attempts: trace.max_attempts,
        pending: trace.pending,
        error: trace.error.clone(),
        problem: trace.problem.clone(),
        access: trace.access.clone(),
        implied: trace.implied.clone(),
        new: trace.new,
        blocked: trace.blocked,
        blocked_phase: trace.blocked_phase,
        replay_of: trace.replay_of.clone(),
        request: Message {
            headers: Vec::new(),
            content_type: trace.request.content_type.clone(),
            body,
        },
        response: Message::default(),
    }
}

/// A finding and the position of its first call, for ordering.
type Found = Vec<(usize, Finding)>;

/// `DYNAMODB GetItem`.
fn call_name(call: &Trace) -> String {
    format!("{} {}", call.service.to_ascii_uppercase(), call.operation)
}

fn times(count: usize) -> String {
    if count > 1 {
        format!(" ×{count}")
    } else {
        String::new()
    }
}

/// One finding per distinct failure (service, operation, error code), with
/// every call that failed that way. A failed attempt that a later retry of
/// the same SDK invocation recovered from is a `retries` finding instead.
fn failures(calls: &[&Trace], found: &mut Found) {
    let mut last_attempt: HashMap<&str, usize> = HashMap::new();
    for (index, call) in calls.iter().enumerate() {
        if let Some(id) = call.invocation_id.as_deref() {
            last_attempt.insert(id, index);
        }
    }
    let mut groups: BTreeMap<(&str, &str, &str), usize> = BTreeMap::new();
    for (index, call) in calls.iter().enumerate() {
        let Some(problem) = call.problem.as_ref() else {
            continue;
        };
        let recovered = call
            .invocation_id
            .as_deref()
            .and_then(|id| last_attempt.get(id))
            .is_some_and(|&last| last > index && calls[last].problem.is_none());
        if recovered {
            continue;
        }
        let key = (
            call.service.as_str(),
            call.operation.as_str(),
            problem.code.as_str(),
        );
        match groups.get(&key) {
            Some(&slot) => found[slot].1.calls.push(call.id.clone()),
            None => {
                groups.insert(key, found.len());
                found.push((
                    index,
                    Finding {
                        kind: FindingKind::Failure,
                        severity: Severity::Error,
                        title: String::new(),
                        detail: None,
                        calls: vec![call.id.clone()],
                    },
                ));
            }
        }
    }
    for (_, slot) in groups {
        let (first, finding) = &found[slot];
        let call = calls[*first];
        let problem = call.problem.as_ref().expect("grouped by problem");
        let title = format!(
            "{} → {}{}",
            call_name(call),
            problem.code,
            times(finding.calls.len())
        );
        // The first call that has a cause explains them all; else the message.
        let grouped = || {
            calls
                .iter()
                .filter(|call| finding.calls.contains(&call.id))
                .filter_map(|call| call.problem.as_ref())
        };
        let detail = grouped()
            .find_map(|problem| problem.hint.clone())
            .or_else(|| grouped().find_map(|problem| problem.message.clone()));
        found[slot].1.title = title;
        found[slot].1.detail = detail;
    }
}

/// One finding per permission the lock or policy does not grant.
fn access(calls: &[&Trace], gate: &str, found: &mut Found) {
    let mut slots: HashMap<(&str, &str), usize> = HashMap::new();
    for (index, call) in calls.iter().enumerate() {
        let own = call.access.iter().map(|item| (item, false));
        let implied = call.implied.iter().map(|item| (item, true));
        for (item, is_implied) in own.chain(implied).filter(|(item, _)| item.new) {
            let key = (item.action.as_str(), item.resource.as_str());
            let slot = *slots.entry(key).or_insert_with(|| {
                found.push((
                    index,
                    Finding {
                        kind: FindingKind::Access,
                        severity: Severity::Warning,
                        title: format!(
                            "{} on {} not in {gate}",
                            item.action,
                            short_resource(&item.resource)
                        ),
                        detail: None,
                        calls: Vec::new(),
                    },
                ));
                found.len() - 1
            });
            let finding = &mut found[slot].1;
            if !finding.calls.contains(&call.id) {
                finding.calls.push(call.id.clone());
            }
            if finding.detail.is_none() {
                let mut detail = format!(
                    "{} by {}.",
                    if is_implied { "Implied" } else { "Needed" },
                    call_name(call)
                );
                if item.unverified {
                    detail.push_str(
                        " Resource unknown: the request body was not readable (binary or truncated).",
                    );
                }
                finding.detail = Some(detail);
            }
        }
    }
}

/// `table/orders` from `arn:aws:dynamodb:*:*:table/orders`, `bucket/*` from
/// `arn:aws:s3:::bucket/*`.
fn short_resource(resource: &str) -> &str {
    if !resource.starts_with("arn:") {
        return resource;
    }
    resource
        .splitn(6, ':')
        .nth(5)
        .filter(|rest| !rest.is_empty())
        .unwrap_or(resource)
}

/// Calls in a serial burst before it is worth a finding.
const MIN_BURST: usize = 5;
/// Longest pause between one call's end and the next call's start in a burst.
const MAX_GAP_MICROS: i64 = 100_000;

/// The batch API that serves many of `operation` in one call; `Some(None)`
/// for a repeated read with no batch form; `None` when not tracked.
fn batch_api(service: &str, operation: &str) -> Option<Option<&'static str>> {
    Some(Some(match (service, operation) {
        ("dynamodb", "GetItem") => "BatchGetItem",
        ("dynamodb", "PutItem" | "DeleteItem") => "BatchWriteItem",
        ("sqs", "SendMessage") => "SendMessageBatch",
        ("sqs", "DeleteMessage") => "DeleteMessageBatch",
        ("sqs", "ChangeMessageVisibility") => "ChangeMessageVisibilityBatch",
        ("s3", "DeleteObject") => "DeleteObjects",
        ("ssm", "GetParameter") => "GetParameters",
        ("secretsmanager", "GetSecretValue") => "BatchGetSecretValue",
        ("sns", "Publish") => "PublishBatch",
        ("kinesis", "PutRecord") => "PutRecords",
        ("firehose", "PutRecord") => "PutRecordBatch",
        ("dynamodb", "Query") | ("s3", "GetObject" | "HeadObject") => return Some(None),
        _ => return None,
    }))
}

struct Burst<'a> {
    first: usize,
    ids: Vec<String>,
    start: i64,
    end: i64,
    /// What the last call asked for (path and body hash): a burst repeats an
    /// operation with different keys, not the same request (polling).
    request: (&'a str, Option<&'a str>),
}

/// The same operation on the same resource, called again and again, each
/// call starting after the previous one ended: a loop a batch API (or
/// concurrency) would collapse.
fn n_plus_one(calls: &[&Trace], found: &mut Found) {
    let mut bursts: BTreeMap<(&str, &str, &str), Burst> = BTreeMap::new();
    let mut done: Vec<((&str, &str, &str), Burst)> = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        if batch_api(&call.service, &call.operation).is_none()
            || call.problem.is_some()
            || call.attempt.is_some_and(|attempt| attempt > 1)
        {
            continue;
        }
        let Some(start) = micros(&call.at) else {
            continue;
        };
        let end = start + call.ms as i64 * 1000;
        let resource = call
            .access
            .first()
            .map_or("", |item| item.resource.as_str());
        let key = (call.service.as_str(), call.operation.as_str(), resource);
        let request = (
            call.path.as_str(),
            call.request.body.as_ref().map(|body| body.sha256.as_str()),
        );
        let next = Burst {
            first: index,
            ids: vec![call.id.clone()],
            start,
            end,
            request,
        };
        match bursts.get_mut(&key) {
            Some(burst)
                if start >= burst.end
                    && start - burst.end <= MAX_GAP_MICROS
                    && request != burst.request =>
            {
                burst.ids.push(call.id.clone());
                burst.end = end;
                burst.request = request;
            }
            _ => {
                if let Some(previous) = bursts.insert(key, next) {
                    done.push((key, previous));
                }
            }
        }
    }
    done.extend(bursts);
    for ((service, operation, resource), burst) in done {
        if burst.ids.len() < MIN_BURST {
            continue;
        }
        let count = burst.ids.len();
        let span = (burst.end - burst.start) / 1000;
        let on = if resource.is_empty() {
            String::new()
        } else {
            format!(" on {}", short_resource(resource))
        };
        let advice = match batch_api(service, operation).flatten() {
            Some(batch) => format!("`{batch}` does this in fewer round trips."),
            None => "each call waited for the previous one.".to_string(),
        };
        found.push((
            burst.first,
            Finding {
                kind: FindingKind::NPlusOne,
                severity: Severity::Warning,
                title: format!(
                    "{} {operation} ×{count} in a serial burst",
                    service.to_ascii_uppercase()
                ),
                detail: Some(format!(
                    "{count} calls{on}, one after another in {span} ms: {advice}"
                )),
                calls: burst.ids,
            },
        ));
    }
}

/// Microseconds since the epoch of an RFC 3339 timestamp.
fn micros(at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|time| time.timestamp_micros())
}

/// Resources a successful call created that no later call deleted. Objects
/// written into a bucket are data, not resources, and are not tracked.
fn leaked(calls: &[&Trace], found: &mut Found) {
    let mut alive: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (index, call) in calls.iter().enumerate() {
        let create = is_create_operation(&call.operation)
            && !matches!(
                call.operation.as_str(),
                "PutObject" | "CopyObject" | "CompleteMultipartUpload"
            );
        let delete = is_delete_operation(&call.operation);
        if !(create || delete) || call.status >= 400 || call.blocked || call.problem.is_some() {
            continue;
        }
        let concrete = crate::access::derive_call(call).concrete_own;
        for key in causal_keys(&concrete, false) {
            if create {
                alive.insert(key, index);
            } else {
                alive.remove(&key);
            }
        }
    }
    for ((service, name), index) in alive {
        let call = calls[index];
        let what = if service == "s3" {
            name
        } else {
            format!("{} {name}", service.to_ascii_uppercase())
        };
        found.push((
            index,
            Finding {
                kind: FindingKind::Leaked,
                severity: Severity::Info,
                title: format!("{what} created and never deleted"),
                detail: Some(format!(
                    "Created by {}; no later call in this run deleted it.",
                    call_name(call)
                )),
                calls: vec![call.id.clone()],
            },
        ));
    }
}

/// SDK invocations that took more than one attempt.
fn retries(calls: &[&Trace], found: &mut Found) {
    let mut groups: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, call) in calls.iter().enumerate() {
        if let Some(id) = call.invocation_id.as_deref() {
            groups.entry(id).or_default().push(index);
        }
    }
    for indexes in groups.into_values() {
        let last = calls[*indexes.last().expect("non-empty group")];
        let attempts = indexes.len().max(last.attempt.unwrap_or(1) as usize);
        if attempts < 2 {
            continue;
        }
        let first = calls[indexes[0]];
        let (severity, outcome, detail) = match last.problem.as_ref() {
            Some(problem) => (
                Severity::Warning,
                problem.code.clone(),
                format!("Gave up after {attempts} attempts."),
            ),
            None => {
                let mut codes: Vec<&str> = indexes
                    .iter()
                    .filter_map(|&index| Some(calls[index].problem.as_ref()?.code.as_str()))
                    .collect();
                codes.dedup();
                let before = if codes.is_empty() {
                    String::new()
                } else {
                    format!(" after {}", codes.join(", "))
                };
                (
                    Severity::Info,
                    "succeeded".to_string(),
                    format!("Succeeded on attempt {attempts}{before}."),
                )
            }
        };
        found.push((
            indexes[0],
            Finding {
                kind: FindingKind::Retries,
                severity,
                title: format!("{} retried ×{attempts} → {outcome}", call_name(first)),
                detail: Some(detail),
                calls: indexes
                    .iter()
                    .map(|&index| calls[index].id.clone())
                    .collect(),
            },
        ));
    }
}
