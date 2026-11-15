//! What a run did, in a few lines: the end-of-run report of `lenslet run`.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

use crate::trace::Trace;

/// Failed calls listed in the human summary; the JSON keeps all of them.
const LISTED_FAILURES: usize = 10;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub calls: usize,
    pub errors: usize,
    /// Calls that started but had not finished when the summary was taken.
    pub unfinished: usize,
    pub services: BTreeMap<String, usize>,
    pub failures: Vec<Failure>,
    #[serde(skip)]
    pending: HashSet<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub id: String,
    pub service: String,
    pub operation: String,
    pub status: u16,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Summary {
    pub fn observe(&mut self, trace: &Trace) {
        if trace.pending {
            self.pending.insert(trace.id.clone());
            self.unfinished = self.pending.len();
            return;
        }
        self.pending.remove(&trace.id);
        self.unfinished = self.pending.len();
        self.calls += 1;
        *self.services.entry(trace.service.clone()).or_default() += 1;
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

    /// Mark a call as started (it is counted as unfinished until recorded).
    pub fn start(&mut self, id: &str) {
        self.pending.insert(id.to_string());
        self.unfinished = self.pending.len();
    }

    /// The human report, printed to stderr after the command exits.
    pub fn render(&self, trace: &str, elapsed_ms: u128) -> String {
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
            seconds(elapsed_ms),
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
            "lenslet: trace {trace} · open it with `lenslet ui`\n"
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

fn seconds(ms: u128) -> String {
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
            request: Message::default(),
            response: Message::default(),
        }
    }

    #[test]
    fn counts_calls_errors_services_and_in_flight() {
        let mut summary = Summary::default();
        summary.start("a");
        summary.start("b");
        assert_eq!(summary.unfinished, 2);
        summary.observe(&trace("a", "sqs", 200, None));
        summary.observe(&trace(
            "b",
            "sqs",
            400,
            Some(r#"{"__type":"InvalidParameterValue","message":"bad\nbody"}"#),
        ));
        summary.observe(&trace("c", "s3", 200, None));
        assert_eq!(summary.calls, 3);
        assert_eq!(summary.errors, 1);
        assert_eq!(summary.unfinished, 0);
        assert_eq!(summary.services["sqs"], 2);

        let text = summary.render("~/.lenslet/runs/x.jsonl", 1500);
        assert!(text.starts_with("lenslet: 3 calls · 1 error · 2 services (s3, sqs) · 1.5 s"));
        assert!(text.contains("✗ SQS SendMessage  400  InvalidParameterValue  bad body"));
        assert!(text.contains("→ The emulator rejected"));

        let json = serde_json::to_value(&summary).unwrap();
        assert_eq!(json["failures"][0]["code"], "InvalidParameterValue");
    }

    #[test]
    fn says_when_nothing_arrived() {
        let text = Summary::default().render("t", 10);
        assert!(text.contains("AWS_ENDPOINT_URL"));
    }
}
