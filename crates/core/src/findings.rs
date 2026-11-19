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

/// How a `leaked` finding's title ends, after the resource it names.
pub const LEAKED_SUFFIX: &str = " created and never deleted";

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
                title: format!("{what}{LEAKED_SUFFIX}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{access::CallAccess, problem::Problem};

    /// A finished call starting `start_ms` into the run and taking `ms`.
    fn call(id: &str, service: &str, operation: &str, start_ms: u64, ms: u64) -> Trace {
        let at = chrono::DateTime::from_timestamp_millis(1_790_000_000_000 + start_ms as i64)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        Trace {
            id: id.into(),
            at,
            method: "POST".into(),
            path: "/".into(),
            host: "127.0.0.1:9297".into(),
            service: service.into(),
            operation: operation.into(),
            status: 200,
            ms,
            region: Some("us-east-1".into()),
            ..Default::default()
        }
    }

    fn failing(mut trace: Trace, status: u16, code: &str, hint: Option<&str>) -> Trace {
        trace.status = status;
        trace.problem = Some(Problem {
            code: code.into(),
            message: Some(format!("{code} message")),
            hint: hint.map(str::to_string),
        });
        trace
    }

    fn with_body(mut trace: Trace, body: &str) -> Trace {
        trace.request.content_type = Some("application/x-amz-json-1.0".into());
        trace.request.body = Some(BodyTrace {
            text: Some(body.into()),
            bytes: body.len() as u64,
            sha256: format!(
                "{:x}",
                body.len() * 7919 + body.bytes().map(usize::from).sum::<usize>()
            ),
            ..Default::default()
        });
        trace
    }

    fn on(mut trace: Trace, action: &str, resource: &str, new: bool) -> Trace {
        trace.access.push(CallAccess {
            action: action.into(),
            resource: resource.into(),
            new,
            unverified: false,
        });
        trace
    }

    fn kinds(findings: &[Finding]) -> Vec<(FindingKind, Severity)> {
        findings.iter().map(|f| (f.kind, f.severity)).collect()
    }

    #[test]
    fn failures_group_by_kind_and_recovered_attempts_are_retries() {
        let mut first = failing(
            call("r1", "sqs", "SendMessage", 0, 5),
            500,
            "InternalError",
            None,
        );
        first.invocation_id = Some("inv".into());
        let mut second = call("r2", "sqs", "SendMessage", 10, 5);
        second.invocation_id = Some("inv".into());
        second.attempt = Some(2);
        let calls = vec![
            first,
            failing(
                call("a1", "dynamodb", "GetItem", 20, 1),
                400,
                "ResourceNotFoundException",
                None,
            ),
            second,
            failing(call("s1", "s3", "GetObject", 30, 1), 404, "NoSuchKey", None),
            failing(
                call("a2", "dynamodb", "GetItem", 40, 1),
                400,
                "ResourceNotFoundException",
                Some("Region mismatch: created in us-west-2."),
            ),
        ];
        let findings = analyze(&calls);
        assert_eq!(
            kinds(&findings),
            [
                (FindingKind::Failure, Severity::Error),
                (FindingKind::Failure, Severity::Error),
                (FindingKind::Retries, Severity::Info),
            ]
        );
        assert_eq!(
            findings[0].title,
            "DYNAMODB GetItem → ResourceNotFoundException ×2"
        );
        assert_eq!(findings[0].calls, ["a1", "a2"]);
        assert_eq!(
            findings[0].detail.as_deref(),
            Some("Region mismatch: created in us-west-2."),
            "the cause one call found explains the group"
        );
        assert_eq!(findings[1].title, "S3 GetObject → NoSuchKey");
        assert_eq!(findings[1].detail.as_deref(), Some("NoSuchKey message"));
        assert_eq!(findings[2].title, "SQS SendMessage retried ×2 → succeeded");
        assert_eq!(
            findings[2].detail.as_deref(),
            Some("Succeeded on attempt 2 after InternalError.")
        );
        assert_eq!(findings[2].calls, ["r1", "r2"]);
    }

    #[test]
    fn retries_that_give_up_are_warnings() {
        let attempts: Vec<Trace> = (1..=3)
            .map(|n| {
                let mut trace = failing(
                    call(&format!("t{n}"), "dynamodb", "PutItem", n * 10, 1),
                    400,
                    "ThrottlingException",
                    None,
                );
                trace.invocation_id = Some("inv".into());
                trace.attempt = (n > 1).then_some(n as u32);
                trace
            })
            .collect();
        let findings = analyze(&attempts);
        assert_eq!(findings[0].calls, ["t1", "t2", "t3"]);
        assert_eq!(findings[0].kind, FindingKind::Failure);
        let retries = findings
            .iter()
            .find(|finding| finding.kind == FindingKind::Retries)
            .unwrap();
        assert_eq!(retries.severity, Severity::Warning);
        assert_eq!(
            retries.title,
            "DYNAMODB PutItem retried ×3 → ThrottlingException"
        );
    }

    #[test]
    fn each_new_permission_is_one_access_finding() {
        let table = "arn:aws:dynamodb:*:*:table/orders";
        let calls = vec![
            on(
                call("d1", "dynamodb", "DeleteItem", 0, 1),
                "dynamodb:DeleteItem",
                table,
                true,
            ),
            on(
                call("g1", "dynamodb", "GetItem", 5, 1),
                "dynamodb:GetItem",
                table,
                false,
            ),
            on(
                call("d2", "dynamodb", "DeleteItem", 10, 1),
                "dynamodb:DeleteItem",
                table,
                true,
            ),
        ];
        let findings = analyze_against(&calls, "infra/aws.lock");
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].title,
            "dynamodb:DeleteItem on table/orders not in infra/aws.lock"
        );
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].calls, ["d1", "d2"]);
        assert_eq!(
            findings[0].detail.as_deref(),
            Some("Needed by DYNAMODB DeleteItem.")
        );
    }

    #[test]
    fn a_serial_burst_of_reads_suggests_the_batch_api() {
        let table = "arn:aws:dynamodb:*:*:table/orders";
        let get = |n: u64, start: u64| {
            on(
                with_body(
                    call(&format!("g{n}"), "dynamodb", "GetItem", start, 3),
                    &format!(r#"{{"TableName":"orders","Key":{{"id":{{"S":"{n}"}}}}}}"#),
                ),
                "dynamodb:GetItem",
                table,
                false,
            )
        };
        // Six one after another (an unrelated call in between), each 1 ms
        // after the previous one ended.
        let mut calls: Vec<Trace> = (0..6).map(|n| get(n, n * 4)).collect();
        calls.insert(3, call("other", "sqs", "ReceiveMessage", 9, 1));
        let findings = analyze(&calls);
        let burst = findings
            .iter()
            .find(|finding| finding.kind == FindingKind::NPlusOne)
            .expect("a burst");
        assert_eq!(burst.title, "DYNAMODB GetItem ×6 in a serial burst");
        assert_eq!(burst.calls, ["g0", "g1", "g2", "g3", "g4", "g5"]);
        assert_eq!(
            burst.detail.as_deref(),
            Some(
                "6 calls on table/orders, one after another in 23 ms: `BatchGetItem` does this in fewer round trips."
            )
        );

        // Concurrent calls are not a serial burst; neither is polling the
        // same request.
        let parallel: Vec<Trace> = (0..6).map(|n| get(n, n)).collect();
        assert!(analyze(&parallel).is_empty());
        let polling: Vec<Trace> = (0..6)
            .map(|n| {
                let mut trace = get(0, n * 4);
                trace.id = format!("p{n}");
                trace
            })
            .collect();
        assert!(analyze(&polling).is_empty());
        // Fewer than five is not worth a finding.
        assert!(analyze(&calls[..4]).is_empty());
    }

    #[test]
    fn resources_created_and_never_deleted_are_leaks() {
        let calls = vec![
            with_body(
                call("c1", "dynamodb", "CreateTable", 0, 1),
                r#"{"TableName":"orders"}"#,
            ),
            with_body(
                call("c2", "sqs", "CreateQueue", 5, 1),
                r#"{"QueueName":"jobs"}"#,
            ),
            failing(
                with_body(
                    call("c3", "sqs", "CreateQueue", 7, 1),
                    r#"{"QueueName":"dupe"}"#,
                ),
                400,
                "QueueAlreadyExists",
                None,
            ),
            with_body(
                call("c4", "sqs", "DeleteQueue", 10, 1),
                r#"{"QueueUrl":"http://127.0.0.1:9297/000000000000/jobs"}"#,
            ),
        ];
        let leaks: Vec<Finding> = analyze(&calls)
            .into_iter()
            .filter(|finding| finding.kind == FindingKind::Leaked)
            .collect();
        assert_eq!(leaks.len(), 1, "{leaks:?}");
        assert_eq!(
            leaks[0].title,
            "DYNAMODB table/orders created and never deleted"
        );
        assert_eq!(leaks[0].severity, Severity::Info);
        assert_eq!(leaks[0].calls, ["c1"]);
    }

    #[test]
    fn essentials_keep_everything_analysis_reads() {
        let mut calls = vec![
            with_body(
                call("c1", "dynamodb", "CreateTable", 0, 1),
                r#"{"TableName":"orders"}"#,
            ),
            on(
                failing(
                    call("d1", "dynamodb", "DeleteItem", 5, 1),
                    400,
                    "AccessDenied",
                    None,
                ),
                "dynamodb:DeleteItem",
                "*",
                true,
            ),
        ];
        calls[1].response.body = Some(BodyTrace {
            text: Some("x".repeat(10_000)),
            ..Default::default()
        });
        let slim: Vec<Trace> = calls.iter().map(essentials).collect();
        assert!(slim[1].response.body.is_none());
        assert!(slim[1].request.body.is_none());
        assert_eq!(analyze(&slim), analyze(&calls));
    }
}
