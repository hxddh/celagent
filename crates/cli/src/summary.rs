//! What a run did: the in-memory tally, the `end` report, and its rendering.
//!
//! The report is one object with one schema everywhere: the last line of the
//! run file, `lenslet run --json`, and `lenslet last --json`.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    access::Access,
    trace::{Trace, trace_access},
};

/// Failed calls listed in the human summary; the report keeps all of them.
const LISTED_FAILURES: usize = 10;
/// Access changes listed in the human summary.
const LISTED_ACCESS: usize = 20;

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub calls: usize,
    pub errors: usize,
    pub unfinished: usize,
    pub services: BTreeMap<String, usize>,
    pub failures: Vec<Failure>,
    pub access: BTreeMap<Access, AccessUse>,
    pending: HashSet<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessUse {
    pub calls: usize,
    pub errors: usize,
    /// Needed by another call rather than called directly (KMS, PassRole).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub implied: bool,
}

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
        *self.services.entry(trace.service.clone()).or_default() += 1;
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
    pub errors: usize,
    pub unfinished: usize,
    pub services: BTreeMap<String, usize>,
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
        out.push_str(&format!(
            "lenslet: {} · {}{} · {}\n",
            plural(self.calls, "call"),
            plural(self.errors, "error"),
            services,
            seconds(self.duration_ms),
        ));
        for failure in self.failures.iter().take(LISTED_FAILURES) {
            out.push_str(&format!(
                "  ✗ {} {}  {}  {}",
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

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
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
    use crate::{problem, trace::Message};

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
            pending: false,
            error: None,
            problem: problem::from_response(status, &[], body),
            access: vec![Access {
                action: format!("{service}:SendMessage"),
                resource: format!("arn:aws:{service}:*:*:jobs"),
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
            errors: summary.errors,
            unfinished: summary.unfinished,
            services: summary.services.clone(),
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
            message: None,
            hint: None,
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
}
