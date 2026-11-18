//! What a run did: the in-memory tally, the `end` report, and its rendering.
//!
//! The report is one object with one schema everywhere: the last line of the
//! run file, `lenslet run --json`, and `lenslet last --json`.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    access::{self, Access},
    trace::{Trace, trace_access},
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
pub struct Summary {
    pub calls: usize,
    pub retries: usize,
    pub errors: usize,
    pub unfinished: usize,
    pub services: BTreeMap<String, usize>,
    pub unmapped: BTreeMap<String, usize>,
    pub failures: Vec<Failure>,
    pub access: BTreeMap<Access, AccessUse>,
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
        *self.services.entry(trace.service.clone()).or_default() += 1;
        if access::is_unmapped(trace) {
            *self.unmapped.entry(trace.service.clone()).or_default() += 1;
        }
        let failed = trace.problem.is_some();
        for (index, access) in trace_access(trace).enumerate() {
            let entry = self.access.entry(access).or_default();
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
    /// Failing now, not failing then.
    pub new_failures: Vec<FailureKind>,
    /// Failing then, not failing now.
    pub fixed: Vec<FailureKind>,
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
    pub unfinished: usize,
    pub services: BTreeMap<String, usize>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unmapped: BTreeMap<String, usize>,
    pub failures: Vec<Failure>,
    pub access: Vec<AccessEntry>,
    /// Compared with this lock file; `null` when there is none.
    pub lock: Option<String>,
    /// In this run, not in the lock.
    pub access_added: Vec<Access>,
    /// In the lock, not exercised by this run.
    pub access_unused: Vec<Access>,
    pub regressions: Option<Regressions>,
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

    /// Fill in the access table and the comparison with a lock file.
    pub fn set_access(&mut self, summary: &Summary, locked: Option<&BTreeSet<Access>>) {
        self.access = summary
            .access
            .iter()
            .map(|(access, used)| AccessEntry {
                action: access.action.clone(),
                resource: access.resource.clone(),
                used: used.clone(),
                new: locked.is_some_and(|locked| !locked.contains(access)),
            })
            .collect();
        if let Some(locked) = locked {
            self.access_added = summary
                .access
                .keys()
                .filter(|access| !locked.contains(*access))
                .cloned()
                .collect();
            self.access_unused = locked
                .iter()
                .filter(|access| !summary.access.contains_key(*access))
                .cloned()
                .collect();
        }
    }

    pub fn compare_with(&mut self, previous: &Report) {
        let now = self.failure_kinds();
        let then = previous.failure_kinds();
        self.regressions = Some(Regressions {
            previous_run: previous.run.clone(),
            previous_exit_code: previous.exit_code,
            new_failures: now.difference(&then).cloned().collect(),
            fixed: then.difference(&now).cloned().collect(),
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
        out.push_str(&format!(
            "lenslet: {}{retries} · {}{} · {}\n",
            plural(self.calls, "call"),
            plural(self.errors, "error"),
            services,
            seconds(self.duration_ms),
        ));
        for failure in self.failures.iter().take(LISTED_FAILURES) {
            let attempt = attempt_label(failure.attempt, failure.max_attempts);
            out.push_str(&format!(
                "  ✗ {} {}  {}  {}{attempt}",
                failure.service.to_ascii_uppercase(),
                failure.operation,
                failure.status,
                failure.code
            ));
            if let Some(message) = failure.message.as_deref() {
                out.push_str("  ");
                out.push_str(&one_line(message, 120));
            }
            out.push('\n');
            if let Some(hint) = failure.hint.as_deref() {
                out.push_str(&format!("      → {hint}\n"));
            }
        }
        if self.failures.len() > LISTED_FAILURES {
            out.push_str(&format!(
                "  … and {} more\n",
                self.failures.len() - LISTED_FAILURES
            ));
        }
        if let Some(regressions) = self.regressions.as_ref()
            && (!regressions.new_failures.is_empty() || !regressions.fixed.is_empty())
        {
            let new = regressions.new_failures.len();
            out.push_str(&format!(
                "  since run {}: {new} new {}, {} fixed\n",
                regressions.previous_run,
                if new == 1 { "failure" } else { "failures" },
                regressions.fixed.len()
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
        match self.lock.as_deref() {
            Some(lock) if !self.access_added.is_empty() => {
                out.push_str(&format!(
                    "  access: {} not in {lock}\n",
                    plural(self.access_added.len(), "permission")
                ));
                for access in self.access_added.iter().take(LISTED_ACCESS) {
                    out.push_str(&format!(
                        "    + {:<32} {}\n",
                        access.action, access.resource
                    ));
                }
                if self.access_added.len() > LISTED_ACCESS {
                    out.push_str(&format!(
                        "    … and {} more\n",
                        self.access_added.len() - LISTED_ACCESS
                    ));
                }
            }
            Some(lock) => out.push_str(&format!(
                "  access: within {lock} ({} used)\n",
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
    /// The report as GitHub-flavored Markdown: what a PR comment or a CI job
    /// summary shows. Access changes come first, each with the calls behind it.
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
        out.push_str(&format!(
            "{}{} · {} · {}\n\n",
            plural(self.calls, "AWS call"),
            retries_label(self.retries),
            plural(self.errors, "error"),
            plural(self.services.len(), "service")
        ));

        let via = |access: &crate::access::Access| {
            self.access
                .iter()
                .find(|entry| entry.action == access.action && entry.resource == access.resource)
                .map(|entry| {
                    let mut text = entry.used.via.join(", ");
                    if entry.used.implied {
                        text.push_str(" (implied)");
                    }
                    text
                })
                .unwrap_or_default()
        };
        match self.lock.as_deref() {
            Some(lock) if !self.access_added.is_empty() => {
                out.push_str(&format!(
                    "**{} not in `{lock}`.** Review them; if they are intended, run `lenslet run --update` and commit `{lock}`.\n\n",
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
            Some(lock) => out.push_str(&format!(
                "AWS access is within `{lock}` ({} used).\n\n",
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
        if !self.failures.is_empty() {
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
                let marker = if new.contains(&kind) { " **new**" } else { "" };
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
            }],
            implied: Vec::new(),
            new: false,
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
        report.set_access(&summary, Some(&locked));
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
        report.set_access(&summary, None);
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
        report.set_access(&summary, Some(&locked));
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
}
