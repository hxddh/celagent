//! What a run did: the in-memory tally, the `end` report, and its rendering.
//!
//! The report is one object with one schema everywhere: the last line of the
//! run file, `lenslet run --json`, and `lenslet last --json`.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    access::{self, Access},
    findings::{Finding, FindingKind, Severity},
    trace::Trace,
};

/// Failed calls listed in the human summary; the report keeps all of them.
const LISTED_FAILURES: usize = 10;
/// Access changes listed in the human summary.
const LISTED_ACCESS: usize = 20;
/// Inline new-permission rows in PR Markdown before folding into `<details>`.
const MARKDOWN_INLINE_ADDED: usize = 30;
/// Hard ceiling on PR Markdown permission rows so comments stay under 64 KiB.
const MARKDOWN_MAX_ROWS: usize = 120;

fn is_zero(value: &usize) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Default)]
struct ResourceHistory {
    created: Option<(usize, String, Option<String>)>,
    deleted: Option<(usize, String)>,
}

/// Tracks resource creation, deletion, and region placement across calls in one
/// run so missing-resource errors explain exact cross-call root causes.
#[derive(Clone, Debug, Default)]
pub struct CausalTracker {
    resources: BTreeMap<(String, String), ResourceHistory>,
}

impl CausalTracker {
    /// Enrich `trace.problem.hint` when a prior call in this run explains the
    /// failure, then update lifecycle state on successful create/delete calls.
    /// `concrete_own` is the call's concrete own access (body-derived ARNs).
    /// Returns whether the hint changed.
    pub fn observe_and_enrich(
        &mut self,
        trace: &mut Trace,
        concrete_own: &[Access],
        call_no: usize,
    ) -> bool {
        if trace.pending {
            return false;
        }
        let lookup_keys = causal_keys(concrete_own, true);

        if trace.status >= 400
            && !trace.blocked
            && let Some(problem) = trace.problem.as_mut()
            && is_missing_resource_code(&problem.code, trace.status)
        {
            for (svc, stem) in &lookup_keys {
                if let Some(hist) = self.resources.get(&(svc.clone(), stem.clone())) {
                    if let Some((del_call, del_op)) = hist.deleted.as_ref() {
                        problem.hint = Some(format!(
                            "`{stem}` was deleted earlier in this run at call #{del_call} ({del_op}) before this `{}` ran.",
                            trace.operation
                        ));
                        return true;
                    }
                    if !matches!(svc.as_str(), "s3" | "iam")
                        && let Some((cr_call, cr_op, Some(cr_reg))) = hist.created.as_ref()
                        && let Some(req_reg) = trace.region.as_deref().filter(|r| !r.is_empty())
                        && cr_reg != req_reg
                    {
                        problem.hint = Some(format!(
                            "Region mismatch: `{stem}` was created in `{cr_reg}` at call #{cr_call} ({cr_op}), but this `{}` targeted `{req_reg}`.",
                            trace.operation
                        ));
                        return true;
                    }
                }
            }
        }

        if trace.status < 400 {
            let mutate_keys = causal_keys(concrete_own, false);
            if is_create_operation(&trace.operation) {
                for key in mutate_keys {
                    let entry = self.resources.entry(key).or_default();
                    entry.created = Some((call_no, trace.operation.clone(), trace.region.clone()));
                    entry.deleted = None;
                }
            } else if is_delete_operation(&trace.operation) {
                for key in mutate_keys {
                    let entry = self.resources.entry(key).or_default();
                    entry.deleted = Some((call_no, trace.operation.clone()));
                }
            }
        }
        false
    }
}

fn is_missing_resource_code(code: &str, status: u16) -> bool {
    status == 404
        || matches!(
            code,
            "NoSuchBucket"
                | "NoSuchKey"
                | "NotFound"
                | "NotFoundException"
                | "ResourceNotFoundException"
                | "QueueDoesNotExist"
                | "NonExistentQueue"
                | "AWS.SimpleQueueService.NonExistentQueue"
                | "TopicDoesNotExist"
                | "ParameterNotFound"
                | "StateMachineDoesNotExist"
                | "ExecutionDoesNotExist"
                | "RepositoryNotFoundException"
                | "UserPoolAddOnNotEnabledException"
        )
}

pub(crate) fn is_create_operation(op: &str) -> bool {
    matches!(
        op,
        "CreateBucket"
            | "PutObject"
            | "CopyObject"
            | "CompleteMultipartUpload"
            | "CreateTable"
            | "CreateQueue"
            | "CreateTopic"
            | "CreateFunction"
            | "CreateSecret"
            | "PutParameter"
            | "CreateLogGroup"
            | "CreateStream"
            | "CreateDeliveryStream"
            | "CreateEventBus"
            | "CreateStateMachine"
            | "CreateRepository"
            | "CreateUserPool"
            | "CreateSchedule"
    )
}

pub(crate) fn is_delete_operation(op: &str) -> bool {
    matches!(
        op,
        "DeleteBucket"
            | "DeleteObject"
            | "DeleteObjects"
            | "DeleteTable"
            | "DeleteQueue"
            | "DeleteTopic"
            | "DeleteFunction"
            | "DeleteSecret"
            | "DeleteParameter"
            | "DeleteParameters"
            | "DeleteLogGroup"
            | "DeleteStream"
            | "DeleteDeliveryStream"
            | "DeleteEventBus"
            | "DeleteStateMachine"
            | "DeleteRepository"
            | "DeleteUserPool"
            | "DeleteSchedule"
    )
}

/// The resources a call names, as `(service, name)` keys: `("s3",
/// "s3://bucket/key")`, `("dynamodb", "table/orders")`, `("sqs", "jobs")`.
pub(crate) fn causal_keys(
    concrete_own: &[Access],
    include_parent_bucket: bool,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for acc in concrete_own {
        let res = acc.resource.as_str();
        if res == "*" {
            continue;
        }
        if let Some(s3_rest) = res.strip_prefix("arn:aws:s3:::") {
            if let Some((bucket, key)) = s3_rest.split_once('/') {
                if !key.is_empty() && key != "*" {
                    out.push(("s3".into(), format!("s3://{bucket}/{key}")));
                }
                if include_parent_bucket {
                    out.push(("s3".into(), format!("s3://{bucket}")));
                }
            } else if !s3_rest.is_empty() {
                out.push(("s3".into(), format!("s3://{s3_rest}")));
            }
            continue;
        }
        let mut parts = res.splitn(6, ':');
        let (Some("arn"), Some(_), Some(arn_svc), Some(_), Some(_), Some(rest)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        let stem = if arn_svc == "dynamodb" {
            if let Some((tbl, _)) = rest.split_once("/index/") {
                tbl.to_string()
            } else if let Some((tbl, _)) = rest.split_once("/stream/") {
                tbl.to_string()
            } else {
                rest.to_string()
            }
        } else if arn_svc == "secretsmanager" {
            rest.strip_suffix("-*").unwrap_or(rest).to_string()
        } else {
            rest.to_string()
        };
        let key = (arn_svc.to_string(), stem);
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub calls: usize,
    pub retries: usize,
    pub errors: usize,
    pub blocked: usize,
    pub unfinished: usize,
    pub services: BTreeMap<String, usize>,
    pub unmapped: BTreeMap<String, usize>,
    pub failures: Vec<Failure>,
    pub access: BTreeMap<Access, AccessUse>,
    /// Concrete or fail-closed permissions rejected by the active gate.
    pub denied_access: BTreeSet<Access>,
    /// Footprint keys that had at least one call rejected by the active gate.
    pub denied_footprints: BTreeSet<Access>,
    pub gate_evaluated: bool,
    pub causal: CausalTracker,
    pending: HashSet<String>,
    invocations: HashSet<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Failure {
    pub id: String,
    pub service: String,
    pub operation: String,
    pub status: u16,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocked: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessUse {
    pub calls: usize,
    pub errors: usize,
    /// Needed by another call rather than called directly (KMS, PassRole).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub implied: bool,
    /// The calls that needed it, e.g. `DYNAMODB DeleteItem` (first few).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub via: Vec<String>,
    /// A `*` that stands for a resource some call's request body did not let
    /// Lenslet read (binary or truncated).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unverified: bool,
}

/// Distinct calls kept per permission in `via`.
const VIA_LIMIT: usize = 3;

impl Summary {
    /// Whether a call has started and not yet been recorded as finished.
    pub fn in_flight(&self, id: &str) -> bool {
        self.pending.contains(id)
    }

    pub fn start(&mut self, id: &str) {
        self.pending.insert(id.to_string());
        self.unfinished = self.pending.len();
    }

    pub fn observe(&mut self, trace: &Trace) {
        if trace.pending {
            // A pending line only ever describes a call still in flight.
            return;
        }
        self.pending.remove(&trace.id);
        self.unfinished = self.pending.len();
        self.calls += 1;
        let seen_invocation = trace
            .invocation_id
            .as_ref()
            .is_some_and(|id| !self.invocations.insert(id.clone()));
        let retried = trace.attempt.is_some_and(|attempt| attempt > 1) || seen_invocation;
        if retried {
            self.retries += 1;
        }
        if trace.blocked {
            self.blocked += 1;
        }
        *self.services.entry(trace.service.clone()).or_default() += 1;
        if access::is_unmapped(trace) {
            *self.unmapped.entry(trace.service.clone()).or_default() += 1;
        }
        let failed = trace.problem.is_some();
        for item in trace.access.iter().chain(trace.implied.iter()) {
            if item.new {
                self.denied_footprints.insert(item.key());
            }
        }
        for (index, item) in trace.access.iter().chain(trace.implied.iter()).enumerate() {
            let entry = self.access.entry(item.key()).or_default();
            entry.unverified |= item.unverified;
            entry.calls += 1;
            entry.errors += usize::from(failed);
            // Own permissions come first, then implied ones. An entry is
            // implied only if no call ever needed it directly.
            let own = index < trace.access.len();
            entry.implied = if entry.calls == 1 {
                !own
            } else {
                entry.implied && !own
            };
            let via = format!("{} {}", trace.service.to_ascii_uppercase(), trace.operation);
            if entry.via.len() < VIA_LIMIT && !entry.via.contains(&via) {
                entry.via.push(via);
            }
        }
        if let Some(problem) = trace.problem.as_ref() {
            self.errors += 1;
            self.failures.push(Failure {
                id: trace.id.clone(),
                service: trace.service.clone(),
                operation: trace.operation.clone(),
                status: trace.status,
                code: problem.code.clone(),
                message: problem.message.clone(),
                hint: problem.hint.clone(),
                attempt: trace.attempt.filter(|attempt| *attempt > 1),
                max_attempts: trace.max_attempts.filter(|_| retried),
                blocked: trace.blocked,
            });
        }
    }
}

/// One line of the access table.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessEntry {
    pub action: String,
    pub resource: String,
    #[serde(flatten)]
    pub used: AccessUse,
    /// Not in the lock file.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
}

/// A failure kind, for comparing runs: the same call failing the same way.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct FailureKind {
    pub service: String,
    pub operation: String,
    pub code: String,
}

/// What changed since the previous run of the same command.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Regressions {
    /// Run id it was compared with.
    pub previous_run: String,
    pub previous_exit_code: Option<i32>,
    #[serde(default)]
    pub previous_calls: usize,
    #[serde(default)]
    pub previous_duration_ms: u64,
    /// Failing now, not failing then.
    pub new_failures: Vec<FailureKind>,
    /// Failing then, not failing now.
    pub fixed: Vec<FailureKind>,
    /// Permissions used now that the previous run did not exercise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_access: Vec<Access>,
}

/// The `end` record: everything a person or an agent needs about one run.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    pub run: String,
    #[serde(default)]
    pub command: Vec<String>,
    pub exit_code: Option<i32>,
    pub ended: String,
    pub duration_ms: u64,
    pub calls: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retries: usize,
    pub errors: usize,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blocked: usize,
    pub unfinished: usize,
    pub services: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unmapped: BTreeMap<String, usize>,
    pub failures: Vec<Failure>,
    pub access: Vec<AccessEntry>,
    /// Compared with this lock file; `null` when there is none.
    pub lock: Option<String>,
    /// Compared/enforced with this IAM policy document, when `--policy` was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<String>,
    /// In this run, not in the lock.
    pub access_added: Vec<Access>,
    /// In the lock, not exercised by this run.
    pub access_unused: Vec<Access>,
    pub regressions: Option<Regressions>,
    /// Everything worth a look, most severe first: the one list the terminal,
    /// the PR comment and the window lead with.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<crate::findings::Finding>,
    pub trace: String,
    pub endpoint: String,
    pub upstream: String,
}

impl Report {
    pub fn failure_kinds(&self) -> BTreeSet<FailureKind> {
        self.failures
            .iter()
            .map(|failure| FailureKind {
                service: failure.service.clone(),
                operation: failure.operation.clone(),
                code: failure.code.clone(),
            })
            .collect()
    }

    pub fn access_set(&self) -> BTreeSet<Access> {
        self.access
            .iter()
            .map(|entry| Access::new(entry.action.clone(), entry.resource.clone()))
            .collect()
    }

    /// Fill in the access table and compare against either a lock file or an IAM policy gate.
    pub fn set_access_with_gate(&mut self, summary: &Summary, gate: Option<&crate::lock::Gate>) {
        let policy_evaluated = gate.is_some_and(|g| g.is_policy()) && summary.gate_evaluated;
        self.access = summary
            .access
            .iter()
            .map(|(access, used)| AccessEntry {
                action: access.action.clone(),
                resource: access.resource.clone(),
                used: used.clone(),
                new: if policy_evaluated {
                    summary.denied_footprints.contains(access)
                } else {
                    summary.denied_footprints.contains(access)
                        || gate.is_some_and(|gate| !gate.allows(access))
                },
            })
            .collect();
        if let Some(gate) = gate {
            if policy_evaluated {
                self.access_added = summary.denied_access.iter().cloned().collect();
            } else {
                let mut added: BTreeSet<Access> = summary
                    .access
                    .keys()
                    .filter(|access| !gate.allows(access))
                    .cloned()
                    .collect();
                added.extend(summary.denied_access.iter().cloned());
                self.access_added = added.into_iter().collect();
            }
            if let Some(locked) = gate.locked_set() {
                self.access_unused = locked
                    .iter()
                    .filter(|access| !summary.access.contains_key(*access))
                    .cloned()
                    .collect();
            }
        }
    }

    pub fn compare_with(&mut self, previous: &Report) {
        let now = self.failure_kinds();
        let then = previous.failure_kinds();
        let now_access = self.access_set();
        let then_access = previous.access_set();
        self.regressions = Some(Regressions {
            previous_run: previous.run.clone(),
            previous_exit_code: previous.exit_code,
            previous_calls: previous.calls,
            previous_duration_ms: previous.duration_ms,
            new_failures: now.difference(&then).cloned().collect(),
            fixed: then.difference(&now).cloned().collect(),
            new_access: now_access.difference(&then_access).cloned().collect(),
        });
    }

    /// The human report, printed to stderr after the command exits.
    pub fn render(&self, trace: &str) -> String {
        let mut out = String::new();
        let services = if self.services.is_empty() {
            String::new()
        } else {
            let names: Vec<&str> = self.services.keys().map(String::as_str).collect();
            format!(
                " · {} ({})",
                plural(self.services.len(), "service"),
                names.join(", ")
            )
        };
        let retries = retries_label(self.retries);
        let blocked_summary = if self.blocked > 0 {
            format!(" ({} blocked)", self.blocked)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "lenslet: {}{retries} · {}{blocked_summary}{} · {}\n",
            plural(self.calls, "call"),
            plural(self.errors, "error"),
            services,
            seconds(self.duration_ms),
        ));
        if self.findings.is_empty() {
            // A report written before findings existed.
            for failure in self.failures.iter().take(LISTED_FAILURES) {
                out.push_str(&failure_lines(failure, 1));
            }
            if self.failures.len() > LISTED_FAILURES {
                out.push_str(&format!(
                    "  … and {} more\n",
                    self.failures.len() - LISTED_FAILURES
                ));
            }
        }
        // Access findings are the `access:` list below; the rest lead.
        let listed: Vec<&Finding> = self.listed_findings().collect();
        for finding in listed.iter().take(LISTED_FAILURES) {
            match (finding.kind, self.failure_of(finding)) {
                (FindingKind::Failure, Some(failure)) => {
                    out.push_str(&failure_lines(failure, finding.calls.len()));
                }
                _ => {
                    let glyph = match finding.severity {
                        Severity::Error => '✗',
                        Severity::Warning => '!',
                        Severity::Info => '·',
                    };
                    out.push_str(&format!("  {glyph} {}\n", finding.title));
                    if let Some(detail) = finding.detail.as_deref() {
                        out.push_str(&format!("      → {detail}\n"));
                    }
                }
            }
        }
        if listed.len() > LISTED_FAILURES {
            out.push_str(&format!(
                "  … and {} more\n",
                listed.len() - LISTED_FAILURES
            ));
        }
        if let Some(regressions) = self.regressions.as_ref()
            && (!regressions.new_failures.is_empty()
                || !regressions.fixed.is_empty()
                || !regressions.new_access.is_empty())
        {
            let new = regressions.new_failures.len();
            let mut parts = Vec::new();
            if new > 0 || !regressions.fixed.is_empty() {
                parts.push(format!(
                    "{new} new {}, {} fixed",
                    if new == 1 { "failure" } else { "failures" },
                    regressions.fixed.len()
                ));
            }
            if !regressions.new_access.is_empty() {
                parts.push(format!(
                    "+{} vs previous run",
                    plural(regressions.new_access.len(), "permission")
                ));
            }
            out.push_str(&format!(
                "  since run {}: {}\n",
                regressions.previous_run,
                parts.join(" · ")
            ));
            for kind in &regressions.new_failures {
                out.push_str(&format!(
                    "    new   {} {} {}\n",
                    kind.service.to_ascii_uppercase(),
                    kind.operation,
                    kind.code
                ));
            }
        }
        let gate_name = self.policy.as_deref().or(self.lock.as_deref());
        match gate_name {
            Some(gate) if !self.access_added.is_empty() => {
                out.push_str(&format!(
                    "  access: {} not in {gate}\n",
                    plural(self.access_added.len(), "permission")
                ));
                for access in self.access_added.iter().take(LISTED_ACCESS) {
                    out.push_str(&format!(
                        "    + {:<32} {}{}\n",
                        access.action,
                        access.resource,
                        if self.unverified(access) {
                            "  (resource unknown: request body not readable)"
                        } else {
                            ""
                        }
                    ));
                }
                if self.access_added.len() > LISTED_ACCESS {
                    out.push_str(&format!(
                        "    … and {} more\n",
                        self.access_added.len() - LISTED_ACCESS
                    ));
                }
            }
            Some(gate) => out.push_str(&format!(
                "  access: within {gate} ({} used)\n",
                plural(self.access.len(), "permission")
            )),
            None if !self.access.is_empty() => out.push_str(&format!(
                "  access: {} observed · `lenslet run --update -- …` writes lenslet.lock\n",
                plural(self.access.len(), "permission")
            )),
            None => {}
        }
        if let Some(lock) = self.lock.as_deref()
            && !self.access_unused.is_empty()
        {
            out.push_str(&format!(
                "  unused: {} in {lock} not exercised by this run\n",
                plural(self.access_unused.len(), "permission")
            ));
            for access in self.access_unused.iter().take(LISTED_ACCESS) {
                out.push_str(&format!(
                    "    - {:<32} {}\n",
                    access.action, access.resource
                ));
            }
            if self.access_unused.len() > LISTED_ACCESS {
                out.push_str(&format!(
                    "    … and {} more\n",
                    self.access_unused.len() - LISTED_ACCESS
                ));
            }
        }
        if !self.unmapped.is_empty() {
            let total: usize = self.unmapped.values().sum();
            let names: Vec<&str> = self.unmapped.keys().map(String::as_str).collect();
            out.push_str(&format!(
                "  note: {} ({}) had no recognizable operation and {} not in the access list\n",
                plural(total, "call"),
                names.join(", "),
                if total == 1 { "is" } else { "are" }
            ));
        }
        if self.unfinished > 0 {
            out.push_str(&format!(
                "  {} still in flight when the command exited\n",
                plural(self.unfinished, "call")
            ));
        }
        if self.calls == 0 && self.unfinished == 0 {
            out.push_str(
                "  no AWS calls reached Lenslet: does the program honor AWS_ENDPOINT_URL?\n",
            );
        }
        out.push_str(&format!(
            "lenslet: trace {trace} · `lenslet ui` to inspect\n"
        ));
        out
    }
}

impl Report {
    /// Findings shown in their own list: all but access findings, which the
    /// access section lists with the calls behind them.
    fn listed_findings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|finding| finding.kind != FindingKind::Access)
    }

    /// The latest failed call of a failure finding: the one shown for it.
    fn failure_of(&self, finding: &Finding) -> Option<&Failure> {
        let last = finding.calls.last()?;
        self.failures.iter().find(|failure| &failure.id == last)
    }

    /// Whether this permission's resource is a `*` standing for one that a
    /// request body did not let Lenslet read.
    fn unverified(&self, access: &Access) -> bool {
        self.access.iter().any(|entry| {
            entry.used.unverified
                && entry.action == access.action
                && entry.resource == access.resource
        })
    }

    /// The report as GitHub-flavored Markdown: what a PR comment or a CI job
    /// summary shows. Findings lead, then access changes, each with the calls
    /// behind it.
    pub fn markdown(&self) -> String {
        let mut out = String::new();
        let command = if self.command.is_empty() {
            "proxy session".to_string()
        } else {
            format!("`{}`", self.command.join(" ").replace('`', "'"))
        };
        let exit = match self.exit_code {
            Some(0) => "passed".to_string(),
            Some(code) => format!("**failed** (exit {code})"),
            None => "ended".to_string(),
        };
        out.push_str(&format!("### Lenslet · {command} {exit}\n\n"));
        let blocked_md = if self.blocked > 0 {
            format!(" ({} blocked)", self.blocked)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{}{} · {}{blocked_md} · {}\n\n",
            plural(self.calls, "AWS call"),
            retries_label(self.retries),
            plural(self.errors, "error"),
            plural(self.services.len(), "service")
        ));
        self.markdown_findings(&mut out);

        let via = |access: &crate::access::Access| {
            self.access
                .iter()
                .find(|entry| entry.action == access.action && entry.resource == access.resource)
                .map(|entry| {
                    let mut text = entry.used.via.join(", ");
                    if entry.used.implied {
                        text.push_str(" (implied)");
                    }
                    if entry.used.unverified {
                        text.push_str(" (resource unknown: request body not readable)");
                    }
                    text
                })
                .unwrap_or_default()
        };
        let gate_name = self.policy.as_deref().or(self.lock.as_deref());
        match gate_name {
            Some(gate) if !self.access_added.is_empty() => {
                let remedy = if self.policy.is_some() {
                    format!("Review them or grant them in `{gate}`.")
                } else {
                    format!(
                        "Review them; if they are intended, run `lenslet run --update` and commit `{gate}`."
                    )
                };
                out.push_str(&format!(
                    "**{} not in `{gate}`.** {remedy}\n\n",
                    plural(self.access_added.len(), "permission")
                ));
                out.push_str("| | Permission | Resource | Needed by |\n|---|---|---|---|\n");
                for access in self.access_added.iter().take(MARKDOWN_INLINE_ADDED) {
                    out.push_str(&format!(
                        "| + | `{}` | `{}` | {} |\n",
                        access.action,
                        access.resource,
                        via(access)
                    ));
                }
                out.push('\n');
                if self.access_added.len() > MARKDOWN_INLINE_ADDED {
                    let extra = &self.access_added[MARKDOWN_INLINE_ADDED..];
                    let shown = extra.len().min(MARKDOWN_MAX_ROWS - MARKDOWN_INLINE_ADDED);
                    out.push_str(&format!(
                        "<details><summary>… and {} more new permissions</summary>\n\n| | Permission | Resource | Needed by |\n|---|---|---|---|\n",
                        extra.len()
                    ));
                    for access in extra.iter().take(shown) {
                        out.push_str(&format!(
                            "| + | `{}` | `{}` | {} |\n",
                            access.action,
                            access.resource,
                            via(access)
                        ));
                    }
                    if extra.len() > shown {
                        out.push_str(&format!(
                            "\n_… and {} more in the JSON report._\n",
                            extra.len() - shown
                        ));
                    }
                    out.push_str("\n</details>\n\n");
                }
            }
            Some(gate) => out.push_str(&format!(
                "AWS access is within `{gate}` ({} used).\n\n",
                plural(self.access.len(), "permission")
            )),
            None => out.push_str(&format!(
                "{} observed; there is no `lenslet.lock` to compare with yet.\n\n",
                plural(self.access.len(), "permission")
            )),
        }
        if let Some(lock) = self.lock.as_deref()
            && !self.access_unused.is_empty()
        {
            out.push_str(&format!(
                "<details><summary>{} in <code>{lock}</code> not exercised by this run</summary>\n\n",
                plural(self.access_unused.len(), "permission")
            ));
            out.push_str("| | Permission | Resource |\n|---|---|---|\n");
            for access in self.access_unused.iter().take(MARKDOWN_MAX_ROWS) {
                out.push_str(&format!(
                    "| − | `{}` | `{}` |\n",
                    access.action, access.resource
                ));
            }
            if self.access_unused.len() > MARKDOWN_MAX_ROWS {
                out.push_str(&format!(
                    "\n_… and {} more in `{lock}`._\n",
                    self.access_unused.len() - MARKDOWN_MAX_ROWS
                ));
            }
            out.push_str("\n</details>\n\n");
        }
        if !self.unmapped.is_empty() {
            let total: usize = self.unmapped.values().sum();
            let names: Vec<String> = self
                .unmapped
                .keys()
                .map(|service| format!("`{service}`"))
                .collect();
            out.push_str(&format!(
                "> **Note:** {} ({}) had no recognizable AWS operation and {} not included in `lenslet.lock`.\n\n",
                plural(total, "call"),
                names.join(", "),
                if total == 1 { "is" } else { "are" }
            ));
        }
        if !self.failures.is_empty() && self.findings.is_empty() {
            let new: BTreeSet<FailureKind> = self
                .regressions
                .as_ref()
                .map(|regressions| regressions.new_failures.iter().cloned().collect())
                .unwrap_or_default();
            out.push_str(&format!(
                "**{}**\n\n| Call | Status | Error | Likely cause |\n|---|---|---|---|\n",
                plural(self.failures.len(), "failed call")
            ));
            for failure in self.failures.iter().take(LISTED_FAILURES * 2) {
                let kind = FailureKind {
                    service: failure.service.clone(),
                    operation: failure.operation.clone(),
                    code: failure.code.clone(),
                };
                let marker = if failure.blocked {
                    " **blocked**"
                } else if new.contains(&kind) {
                    " **new**"
                } else {
                    ""
                };
                let attempt = attempt_label(failure.attempt, failure.max_attempts);
                out.push_str(&format!(
                    "| {} {}{marker}{attempt} | {} | `{}` {} | {} |\n",
                    failure.service.to_ascii_uppercase(),
                    failure.operation,
                    failure.status,
                    failure.code,
                    one_line(failure.message.as_deref().unwrap_or(""), 100).replace('|', "\\|"),
                    failure.hint.as_deref().unwrap_or("").replace('|', "\\|"),
                ));
            }
            out.push('\n');
        }
        out.push_str(&format!(
            "<sub>Lenslet {} · observed access from this run, not a security review</sub>\n",
            env!("CARGO_PKG_VERSION")
        ));
        out
    }
}

impl Report {
    /// The findings table at the top of the Markdown report.
    fn markdown_findings(&self, out: &mut String) {
        let listed: Vec<&Finding> = self.listed_findings().collect();
        if listed.is_empty() {
            return;
        }
        let new: BTreeSet<FailureKind> = self
            .regressions
            .as_ref()
            .map(|regressions| regressions.new_failures.iter().cloned().collect())
            .unwrap_or_default();
        out.push_str("| | Finding | Detail |\n|---|---|---|\n");
        for finding in listed.iter().take(LISTED_FAILURES * 2) {
            let glyph = match finding.severity {
                Severity::Error => "✗",
                Severity::Warning => "!",
                Severity::Info => "·",
            };
            let failure = self
                .failure_of(finding)
                .filter(|_| finding.kind == FindingKind::Failure);
            let marker = match failure {
                Some(failure) if failure.blocked => " **blocked**",
                Some(failure)
                    if new.contains(&FailureKind {
                        service: failure.service.clone(),
                        operation: failure.operation.clone(),
                        code: failure.code.clone(),
                    }) =>
                {
                    " **new**"
                }
                _ => "",
            };
            // A failure shows its message and its likely cause.
            let detail = match failure {
                Some(failure) => [
                    failure
                        .message
                        .as_deref()
                        .map(|message| one_line(message, 100)),
                    failure.hint.clone(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" — "),
                None => finding.detail.clone().unwrap_or_default(),
            };
            out.push_str(&format!(
                "| {glyph} | {}{marker} | {} |\n",
                finding.title.replace('|', "\\|"),
                detail.replace('|', "\\|")
            ));
        }
        if listed.len() > LISTED_FAILURES * 2 {
            out.push_str(&format!(
                "\n_… and {} more in the JSON report._\n",
                listed.len() - LISTED_FAILURES * 2
            ));
        }
        out.push('\n');
    }
}

/// `  ✗ SQS SendMessage  400  Code ×2 (attempt 2/3)  message` and its cause.
fn failure_lines(failure: &Failure, count: usize) -> String {
    let attempt = attempt_label(failure.attempt, failure.max_attempts);
    let blocked_tag = if failure.blocked { " [blocked]" } else { "" };
    let times = if count > 1 {
        format!(" ×{count}")
    } else {
        String::new()
    };
    let mut out = format!(
        "  ✗ {} {}  {}  {}{times}{attempt}{blocked_tag}",
        failure.service.to_ascii_uppercase(),
        failure.operation,
        failure.status,
        failure.code
    );
    if let Some(message) = failure.message.as_deref() {
        out.push_str("  ");
        out.push_str(&one_line(message, 120));
    }
    out.push('\n');
    if let Some(hint) = failure.hint.as_deref() {
        out.push_str(&format!("      → {hint}\n"));
    }
    out
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn retries_label(retries: usize) -> String {
    match retries {
        0 => String::new(),
        1 => " (1 retry)".to_string(),
        n => format!(" ({n} retries)"),
    }
}

fn attempt_label(attempt: Option<u32>, max: Option<u32>) -> String {
    match (attempt.filter(|n| *n > 1), max) {
        (Some(attempt), Some(max)) => format!(" (attempt {attempt}/{max})"),
        (Some(attempt), None) => format!(" (attempt {attempt})"),
        _ => String::new(),
    }
}

fn seconds(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.1} s", ms as f64 / 1000.0)
    }
}

fn one_line(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        let mut cut: String = flat.chars().take(max - 1).collect();
        cut.push('…');
        cut
    }
}

/// Structured comparison between two finished runs (`lenslet diff`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunDiff {
    pub base_run: String,
    pub target_run: String,
    pub base_command: Vec<String>,
    pub target_command: Vec<String>,
    pub base_exit_code: Option<i32>,
    pub target_exit_code: Option<i32>,
    pub base_calls: usize,
    pub target_calls: usize,
    pub base_errors: usize,
    pub target_errors: usize,
    pub base_blocked: usize,
    pub target_blocked: usize,
    pub base_duration_ms: u64,
    pub target_duration_ms: u64,
    pub new_failures: Vec<FailureKind>,
    pub fixed_failures: Vec<FailureKind>,
    pub access_added: Vec<Access>,
    pub access_removed: Vec<Access>,
    pub service_deltas: BTreeMap<String, isize>,
}

impl RunDiff {
    pub fn compute(base: &Report, target: &Report) -> Self {
        let base_f = base.failure_kinds();
        let target_f = target.failure_kinds();
        let base_a = base.access_set();
        let target_a = target.access_set();

        let mut services: BTreeSet<&String> = base.services.keys().collect();
        services.extend(target.services.keys());
        let mut service_deltas = BTreeMap::new();
        for svc in services {
            let b = base.services.get(svc).copied().unwrap_or(0) as isize;
            let t = target.services.get(svc).copied().unwrap_or(0) as isize;
            if t - b != 0 {
                service_deltas.insert(svc.clone(), t - b);
            }
        }

        RunDiff {
            base_run: base.run.clone(),
            target_run: target.run.clone(),
            base_command: base.command.clone(),
            target_command: target.command.clone(),
            base_exit_code: base.exit_code,
            target_exit_code: target.exit_code,
            base_calls: base.calls,
            target_calls: target.calls,
            base_errors: base.errors,
            target_errors: target.errors,
            base_blocked: base.blocked,
            target_blocked: target.blocked,
            base_duration_ms: base.duration_ms,
            target_duration_ms: target.duration_ms,
            new_failures: target_f.difference(&base_f).cloned().collect(),
            fixed_failures: base_f.difference(&target_f).cloned().collect(),
            access_added: target_a.difference(&base_a).cloned().collect(),
            access_removed: base_a.difference(&target_a).cloned().collect(),
            service_deltas,
        }
    }

    pub fn is_identical(&self) -> bool {
        self.new_failures.is_empty()
            && self.fixed_failures.is_empty()
            && self.access_added.is_empty()
            && self.access_removed.is_empty()
            && self.base_calls == self.target_calls
            && self.base_errors == self.target_errors
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        let call_delta = self.target_calls as isize - self.base_calls as isize;
        let dur_delta = self.target_duration_ms as i64 - self.base_duration_ms as i64;
        out.push_str(&format!(
            "lenslet diff: {} → {}\n",
            self.base_run, self.target_run
        ));
        out.push_str(&format!(
            "  calls: {} → {} ({:+}) · errors: {} → {} · duration: {} → {} ({:+} ms)\n",
            self.base_calls,
            self.target_calls,
            call_delta,
            self.base_errors,
            self.target_errors,
            seconds(self.base_duration_ms),
            seconds(self.target_duration_ms),
            dur_delta,
        ));
        if !self.new_failures.is_empty() {
            out.push_str(&format!("  new failures ({}):\n", self.new_failures.len()));
            for f in &self.new_failures {
                out.push_str(&format!(
                    "    ✗ {} {}  {}\n",
                    f.service.to_ascii_uppercase(),
                    f.operation,
                    f.code
                ));
            }
        }
        if !self.fixed_failures.is_empty() {
            out.push_str(&format!("  fixed ({}):\n", self.fixed_failures.len()));
            for f in &self.fixed_failures {
                out.push_str(&format!(
                    "    ✓ {} {}  {}\n",
                    f.service.to_ascii_uppercase(),
                    f.operation,
                    f.code
                ));
            }
        }
        if !self.access_added.is_empty() {
            out.push_str(&format!("  access added ({}):\n", self.access_added.len()));
            for a in &self.access_added {
                out.push_str(&format!("    + {:<32} {}\n", a.action, a.resource));
            }
        }
        if !self.access_removed.is_empty() {
            out.push_str(&format!(
                "  access not exercised ({}):\n",
                self.access_removed.len()
            ));
            for a in &self.access_removed {
                out.push_str(&format!("    - {:<32} {}\n", a.action, a.resource));
            }
        }
        if self.is_identical() {
            out.push_str("  no changes in calls, failures, or IAM access.\n");
        }
        out
    }

    pub fn markdown(&self) -> String {
        let mut out = format!(
            "### Lenslet Diff: `{}` → `{}`\n\n",
            self.base_run, self.target_run
        );
        out.push_str(&format!(
            "- **Calls**: {} → {} ({:+})\n- **Errors**: {} → {}\n- **Duration**: {} → {}\n\n",
            self.base_calls,
            self.target_calls,
            self.target_calls as isize - self.base_calls as isize,
            self.base_errors,
            self.target_errors,
            seconds(self.base_duration_ms),
            seconds(self.target_duration_ms),
        ));
        if !self.new_failures.is_empty() {
            out.push_str("**New Failures**\n\n| Service | Operation | Error |\n|---|---|---|\n");
            for f in &self.new_failures {
                out.push_str(&format!(
                    "| `{}` | `{}` | `{}` |\n",
                    f.service, f.operation, f.code
                ));
            }
            out.push('\n');
        }
        if !self.fixed_failures.is_empty() {
            out.push_str("**Fixed**\n\n| Service | Operation | Error |\n|---|---|---|\n");
            for f in &self.fixed_failures {
                out.push_str(&format!(
                    "| `{}` | `{}` | `{}` |\n",
                    f.service, f.operation, f.code
                ));
            }
            out.push('\n');
        }
        if !self.access_added.is_empty() || !self.access_removed.is_empty() {
            out.push_str("**IAM Access Changes**\n\n| | Action | Resource |\n|---|---|---|\n");
            for a in &self.access_added {
                out.push_str(&format!("| + | `{}` | `{}` |\n", a.action, a.resource));
            }
            for a in &self.access_removed {
                out.push_str(&format!("| − | `{}` | `{}` |\n", a.action, a.resource));
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{access::CallAccess, problem, trace::Message};

    fn trace(id: &str, service: &str, status: u16, body: Option<&str>) -> Trace {
        Trace {
            id: id.to_string(),
            at: "2026-11-15T00:00:00+00:00".to_string(),
            method: "POST".to_string(),
            path: "/".to_string(),
            host: "127.0.0.1".to_string(),
            service: service.to_string(),
            operation: "SendMessage".to_string(),
            status,
            ms: 3,
            authorization: true,
            region: None,
            invocation_id: None,
            attempt: None,
            max_attempts: None,
            pending: false,
            error: None,
            problem: problem::from_response(status, &[], body),
            access: vec![CallAccess {
                action: format!("{service}:SendMessage"),
                resource: format!("arn:aws:{service}:*:*:jobs"),
                new: false,
                unverified: false,
            }],
            implied: Vec::new(),
            new: false,
            blocked: false,
            replay_of: None,
            blocked_phase: None,
            request: Message::default(),
            response: Message::default(),
        }
    }

    fn access(action: &str, resource: &str) -> Access {
        Access {
            action: action.into(),
            resource: resource.into(),
        }
    }

    fn lock_gate(locked: &BTreeSet<Access>) -> crate::lock::Gate {
        crate::lock::Gate::from_lock("lenslet.lock", locked.clone())
    }

    fn report(summary: &Summary) -> Report {
        Report {
            run: "r2".into(),
            calls: summary.calls,
            retries: summary.retries,
            errors: summary.errors,
            unfinished: summary.unfinished,
            services: summary.services.clone(),
            unmapped: summary.unmapped.clone(),
            failures: summary.failures.clone(),
            duration_ms: 1500,
            ..Report::default()
        }
    }

    #[test]
    fn tallies_calls_errors_access_and_in_flight() {
        let mut summary = Summary::default();
        summary.start("a");
        summary.start("b");
        assert!(summary.in_flight("a"));
        summary.observe(&trace("a", "sqs", 200, None));
        summary.observe(&trace(
            "b",
            "sqs",
            400,
            Some(r#"{"__type":"InvalidParameterValue","message":"bad\nbody"}"#),
        ));
        summary.observe(&trace("c", "s3", 200, None));
        assert_eq!(
            (summary.calls, summary.errors, summary.unfinished),
            (3, 1, 0)
        );
        let sqs = &summary.access[&access("sqs:SendMessage", "arn:aws:sqs:*:*:jobs")];
        assert_eq!((sqs.calls, sqs.errors, sqs.implied), (2, 1, false));

        let mut report = report(&summary);
        let locked: BTreeSet<Access> = [
            access("sqs:SendMessage", "arn:aws:sqs:*:*:jobs"),
            access("sqs:DeleteMessage", "arn:aws:sqs:*:*:jobs"),
        ]
        .into();
        report.lock = Some("lenslet.lock".into());
        report.set_access_with_gate(&summary, Some(&lock_gate(&locked)));
        assert_eq!(
            report.access_added,
            vec![access("s3:SendMessage", "arn:aws:s3:*:*:jobs")]
        );
        assert_eq!(
            report.access_unused,
            vec![access("sqs:DeleteMessage", "arn:aws:sqs:*:*:jobs")]
        );
        assert!(report.access.iter().any(|entry| entry.new));

        let text = report.render("~/.lenslet/runs/x.jsonl");
        assert!(text.starts_with("lenslet: 3 calls · 1 error · 2 services (s3, sqs) · 1.5 s"));
        assert!(text.contains("✗ SQS SendMessage  400  InvalidParameterValue  bad body"));
        assert!(text.contains("access: 1 permission not in lenslet.lock"));
        assert!(text.contains("+ s3:SendMessage"));

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["failures"][0]["code"], "InvalidParameterValue");
        assert_eq!(json["access"][0]["calls"], 1);
        let back: Report = serde_json::from_value(json).unwrap();
        assert_eq!(back.access, report.access);
    }

    #[test]
    fn compares_failures_with_the_previous_run() {
        let failing = |code: &str| Failure {
            id: "x".into(),
            service: "sqs".into(),
            operation: "SendMessage".into(),
            status: 400,
            code: code.into(),
            ..Failure::default()
        };
        let previous = Report {
            run: "r1".into(),
            exit_code: Some(0),
            failures: vec![failing("Throttling")],
            ..Report::default()
        };
        let mut now = Report {
            run: "r2".into(),
            failures: vec![failing("QueueDoesNotExist")],
            ..Report::default()
        };
        now.compare_with(&previous);
        let regressions = now.regressions.clone().unwrap();
        assert_eq!(regressions.new_failures[0].code, "QueueDoesNotExist");
        assert_eq!(regressions.fixed[0].code, "Throttling");
        assert!(
            now.render("t")
                .contains("since run r1: 1 new failure, 1 fixed")
        );
    }

    #[test]
    fn says_when_nothing_arrived_and_how_to_lock() {
        let text = Report::default().render("t");
        assert!(text.contains("AWS_ENDPOINT_URL"));
        let mut summary = Summary::default();
        summary.observe(&trace("a", "sqs", 200, None));
        let mut report = report(&summary);
        report.set_access_with_gate(&summary, None);
        assert!(report.render("t").contains("--update"));
    }

    #[test]
    fn markdown_explains_access_changes_and_failures() {
        let mut summary = Summary::default();
        summary.observe(&trace("a", "sqs", 200, None));
        summary.observe(&trace(
            "b",
            "s3",
            404,
            Some(r#"<Error><Code>NoSuchKey</Code><Message>gone | away</Message></Error>"#),
        ));
        let mut report = report(&summary);
        report.command = vec!["npm".into(), "test".into()];
        report.exit_code = Some(1);
        report.lock = Some("lenslet.lock".into());
        let locked: BTreeSet<Access> = [
            access("sqs:SendMessage", "arn:aws:sqs:*:*:jobs"),
            access("sqs:DeleteMessage", "arn:aws:sqs:*:*:jobs"),
        ]
        .into();
        report.set_access_with_gate(&summary, Some(&lock_gate(&locked)));
        assert_eq!(
            summary.access[&access("s3:SendMessage", "arn:aws:s3:*:*:jobs")].via,
            ["S3 SendMessage"]
        );
        let md = report.markdown();
        assert!(
            md.starts_with("### Lenslet · `npm test` **failed** (exit 1)"),
            "{md}"
        );
        assert!(
            md.contains("**1 permission not in `lenslet.lock`.**"),
            "{md}"
        );
        assert!(
            md.contains("| + | `s3:SendMessage` | `arn:aws:s3:*:*:jobs` | S3 SendMessage |"),
            "{md}"
        );
        assert!(
            md.contains("1 permission in <code>lenslet.lock</code> not exercised"),
            "{md}"
        );
        assert!(md.contains("| − | `sqs:DeleteMessage`"), "{md}");
        assert!(
            md.contains("`NoSuchKey` gone \\| away"),
            "pipes are escaped: {md}"
        );

        let text = report.render("t");
        assert!(text.contains("unused: 1 permission in lenslet.lock not exercised by this run"));
        assert!(text.contains("- sqs:DeleteMessage"));
    }

    #[test]
    fn surfaces_sdk_retries_and_unmapped_calls() {
        let mut summary = Summary::default();
        let mut first = trace("a1", "sqs", 500, Some(r#"{"__type":"InternalError"}"#));
        first.invocation_id = Some("inv-1".into());
        let mut retry = trace("a2", "sqs", 500, Some(r#"{"__type":"InternalError"}"#));
        retry.invocation_id = Some("inv-1".into());
        retry.attempt = Some(2);
        retry.max_attempts = Some(3);
        let mut unmapped = trace("u1", "custom-svc", 200, None);
        unmapped.operation = "POST".into();
        unmapped.access.clear();

        summary.observe(&first);
        summary.observe(&retry);
        summary.observe(&unmapped);
        assert_eq!(summary.retries, 1);
        assert_eq!(summary.unmapped.get("custom-svc"), Some(&1));

        let report = report(&summary);
        let text = report.render("t");
        assert!(text.contains("3 calls (1 retry)"), "{text}");
        assert!(text.contains("InternalError (attempt 2/3)"), "{text}");
        assert!(
            text.contains("note: 1 call (custom-svc) had no recognizable operation"),
            "{text}"
        );
        let md = report.markdown();
        assert!(md.contains("3 AWS calls (1 retry)"), "{md}");
        assert!(md.contains("(attempt 2/3)"), "{md}");
        assert!(md.contains("`custom-svc`"), "{md}");
    }

    #[test]
    fn causal_tracker_explains_region_mismatch_and_prior_deletion() {
        let mut tracker = CausalTracker::default();

        // Call #1: CreateTable `orders` in `us-west-2` succeeds.
        let mut create_tbl = trace("c1", "dynamodb", 200, None);
        create_tbl.operation = "CreateTable".into();
        create_tbl.region = Some("us-west-2".into());
        create_tbl.request.body = Some(crate::trace::BodyTrace {
            text: Some(r#"{"TableName":"orders"}"#.into()),
            hex: None,
            encoding: None,
            bytes: 22,
            truncated: false,
            binary: false,
            sha256: String::new(),
        });
        let snap1 = access::derive_call(&create_tbl).concrete_own;
        tracker.observe_and_enrich(&mut create_tbl, &snap1, 1);

        // Call #2: GetItem on `orders` in `us-east-1` fails with ResourceNotFoundException.
        let mut get_wrong_reg = trace(
            "c2",
            "dynamodb",
            400,
            Some(r#"{"__type":"ResourceNotFoundException","message":"not found"}"#),
        );
        get_wrong_reg.operation = "GetItem".into();
        get_wrong_reg.region = Some("us-east-1".into());
        get_wrong_reg.request.body = create_tbl.request.body.clone();
        let snap2 = access::derive_call(&get_wrong_reg).concrete_own;
        tracker.observe_and_enrich(&mut get_wrong_reg, &snap2, 2);
        let hint = get_wrong_reg
            .problem
            .as_ref()
            .and_then(|p| p.hint.as_deref())
            .unwrap_or("");
        assert!(
            hint.contains("Region mismatch")
                && hint.contains("us-west-2")
                && hint.contains("us-east-1")
                && hint.contains("call #1"),
            "unexpected hint: {hint}"
        );

        // Call #3: DeleteBucket `receipts`, followed by Call #4 PutObject `/receipts/a.txt` → 404 NoSuchBucket.
        let mut del_bucket = trace("c3", "s3", 204, None);
        del_bucket.operation = "DeleteBucket".into();
        del_bucket.path = "/receipts".into();
        let snap3 = access::derive_call(&del_bucket).concrete_own;
        tracker.observe_and_enrich(&mut del_bucket, &snap3, 3);

        let mut put_after_del = trace(
            "c4",
            "s3",
            404,
            Some("<Error><Code>NoSuchBucket</Code><Message>gone</Message></Error>"),
        );
        put_after_del.operation = "PutObject".into();
        put_after_del.path = "/receipts/a.txt".into();
        let snap4 = access::derive_call(&put_after_del).concrete_own;
        tracker.observe_and_enrich(&mut put_after_del, &snap4, 4);
        let del_hint = put_after_del
            .problem
            .as_ref()
            .and_then(|p| p.hint.as_deref())
            .unwrap_or("");
        assert!(
            del_hint.contains("s3://receipts")
                && del_hint.contains("deleted earlier")
                && del_hint.contains("call #3"),
            "unexpected hint: {del_hint}"
        );
    }

    #[test]
    fn findings_lead_the_summary_and_the_pr_comment_without_repeats() {
        use crate::findings::analyze;
        let mut summary = Summary::default();
        let mut calls = Vec::new();
        for id in ["a", "b"] {
            let mut failed = trace(
                id,
                "sqs",
                400,
                Some(r#"{"__type":"QueueDoesNotExist","message":"no queue"}"#),
            );
            failed.access[0].new = true;
            failed.access[0].unverified = true;
            failed.access[0].resource = "*".into();
            summary.observe(&failed);
            calls.push(failed);
        }
        let mut report = report(&summary);
        report.command = vec!["npm".into(), "test".into()];
        report.exit_code = Some(1);
        report.lock = Some("lenslet.lock".into());
        report.findings = analyze(&calls);
        report.set_access_with_gate(&summary, Some(&lock_gate(&BTreeSet::new())));
        assert_eq!(report.findings.len(), 2, "{:?}", report.findings);

        let text = report.render("t");
        assert!(
            text.contains("✗ SQS SendMessage  400  QueueDoesNotExist ×2  no queue"),
            "{text}"
        );
        assert_eq!(text.matches("QueueDoesNotExist").count(), 1, "{text}");
        assert!(
            text.contains("+ sqs:SendMessage                  *  (resource unknown: request body not readable)"),
            "{text}"
        );
        assert_eq!(text.matches("sqs:SendMessage").count(), 1, "{text}");

        let md = report.markdown();
        let findings_at = md.find("| | Finding | Detail |").expect(&md);
        assert!(
            findings_at < md.find("not in `lenslet.lock`").unwrap(),
            "{md}"
        );
        assert!(
            md.contains("| ✗ | SQS SendMessage → QueueDoesNotExist ×2 | no queue — The resource"),
            "{md}"
        );
        assert!(!md.contains("failed call"), "no second failure table: {md}");
        assert!(
            md.contains("(resource unknown: request body not readable)"),
            "{md}"
        );
    }
}
