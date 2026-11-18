//! `lenslet.lock`: the AWS access a project's runs are known to need.
//!
//! One permission per line, `action  resource`, sorted, so a pull request
//! that widens access shows up as an added line in review. Comments start
//! with `#`. The file is only ever written by `lenslet run --update`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::{access::Access, summary::AccessUse};

pub const FILE_NAME: &str = "lenslet.lock";

/// `lenslet.lock` in `dir`.
pub fn path_in(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// A lock file: the access it lists, and which of it is test-only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lock {
    pub access: BTreeSet<Access>,
    /// Lines marked `# test`: needed by the tests themselves (fixtures that
    /// create tables or buckets), kept for `--check`, left out of `policy`.
    pub test: BTreeSet<Access>,
}

/// The lock, or `None` when there is no lock file.
pub fn read(path: &Path) -> Result<Option<Lock>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    Ok(Some(parse(&text)))
}

pub fn parse(text: &str) -> Lock {
    let mut lock = Lock::default();
    for line in text.lines() {
        let (entry, comment) = line.split_once('#').unwrap_or((line, ""));
        let mut fields = entry.split_whitespace();
        let Some(action) = fields.next() else {
            continue;
        };
        let access = Access {
            action: action.to_string(),
            resource: fields.next().unwrap_or("*").to_string(),
        };
        if comment
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word == "test")
        {
            lock.test.insert(access.clone());
        }
        lock.access.insert(access);
    }
    lock
}

/// The lock file text. `test` marks carry over from the previous lock.
pub fn render(access: &BTreeMap<Access, AccessUse>, test: &BTreeSet<Access>) -> String {
    let width = access
        .keys()
        .map(|access| access.action.len())
        .max()
        .unwrap_or(0);
    let mut out = String::from(
        "# AWS access observed by `lenslet run`: action, resource.\n\
         # Regenerate with `lenslet run --update -- <tests>`; review changes like code.\n\
         # Mark a line `# test` when only the tests need it; `lenslet policy` leaves it out.\n",
    );
    for (access, used) in access {
        out.push_str(&format!("{:<width$}  {}", access.action, access.resource));
        let mut notes = Vec::new();
        if test.contains(access) {
            notes.push("test");
        }
        if used.implied {
            notes.push("implied");
        }
        if !notes.is_empty() {
            out.push_str(&format!("  # {}", notes.join(", ")));
        }
        out.push('\n');
    }
    out
}

pub fn write(
    path: &Path,
    access: &BTreeMap<Access, AccessUse>,
    test: &BTreeSet<Access>,
) -> Result<()> {
    fs::write(path, render(access, test)).with_context(|| format!("write {}", path.display()))
}

/// An IAM policy document granting the locked access, without `test` lines.
pub fn policy(lock: &Lock) -> serde_json::Value {
    // One statement per resource, actions sorted: compact and reviewable.
    let mut by_resource: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for entry in lock.access.difference(&lock.test) {
        by_resource
            .entry(entry.resource.as_str())
            .or_default()
            .push(entry.action.as_str());
    }
    let statements: Vec<serde_json::Value> = by_resource
        .into_iter()
        .map(|(resource, actions)| {
            serde_json::json!({
                "Effect": "Allow",
                "Action": actions,
                "Resource": resource,
            })
        })
        .collect();
    serde_json::json!({ "Version": "2012-10-17", "Statement": statements })
}

/// What a run's derived permissions are checked or actively enforced against:
/// either `lenslet.lock` or an AWS IAM policy JSON document (`--policy`).
#[derive(Clone, Debug)]
pub struct Gate {
    pub label: String,
    kind: GateKind,
}

#[derive(Clone, Debug)]
enum GateKind {
    Lock(BTreeSet<Access>),
    Policy(Vec<Statement>),
}

#[derive(Clone, Debug)]
struct Statement {
    allow: bool,
    actions: Vec<String>,
    not_actions: Vec<String>,
    resources: Vec<String>,
    not_resources: Vec<String>,
}

impl Gate {
    pub fn from_lock(label: impl Into<String>, access: BTreeSet<Access>) -> Self {
        Gate {
            label: label.into(),
            kind: GateKind::Lock(access),
        }
    }

    pub fn from_policy_file(path: &Path, label: impl Into<String>) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("read policy {}", path.display()))?;
        Self::from_policy_str(&text, label)
            .with_context(|| format!("parse IAM policy {}", path.display()))
    }

    pub fn from_policy_str(text: &str, label: impl Into<String>) -> Result<Self> {
        let doc: serde_json::Value = serde_json::from_str(text).context("invalid JSON")?;
        let raw_statements = match doc.get("Statement") {
            Some(serde_json::Value::Array(items)) => items.clone(),
            Some(single @ serde_json::Value::Object(_)) => vec![single.clone()],
            _ => anyhow::bail!("IAM policy must contain a `Statement` array or object"),
        };
        let mut statements = Vec::with_capacity(raw_statements.len());
        for item in raw_statements {
            let Some(obj) = item.as_object() else {
                anyhow::bail!("each IAM Statement must be a JSON object");
            };
            let effect = obj.get("Effect").and_then(|v| v.as_str()).unwrap_or("");
            let allow = if effect.eq_ignore_ascii_case("allow") {
                true
            } else if effect.eq_ignore_ascii_case("deny") {
                false
            } else {
                anyhow::bail!("IAM Statement Effect must be `Allow` or `Deny`, got `{effect}`");
            };
            let list = |key: &str| -> Vec<String> {
                match obj.get(key) {
                    Some(serde_json::Value::String(s)) => vec![s.clone()],
                    Some(serde_json::Value::Array(arr)) => arr
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect(),
                    _ => Vec::new(),
                }
            };
            statements.push(Statement {
                allow,
                actions: list("Action"),
                not_actions: list("NotAction"),
                resources: list("Resource"),
                not_resources: list("NotResource"),
            });
        }
        Ok(Gate {
            label: label.into(),
            kind: GateKind::Policy(statements),
        })
    }

    pub fn locked_set(&self) -> Option<&BTreeSet<Access>> {
        match &self.kind {
            GateKind::Lock(set) => Some(set),
            GateKind::Policy(_) => None,
        }
    }

    /// Whether `access` is permitted by this lock or IAM policy.
    pub fn allows(&self, access: &Access) -> bool {
        match &self.kind {
            GateKind::Lock(entries) => {
                entries.contains(access)
                    || entries.iter().any(|rule| {
                        iam_action_matches(&rule.action, &access.action)
                            && iam_arn_matches(&rule.resource, &access.resource)
                    })
            }
            GateKind::Policy(statements) => {
                let mut allowed = false;
                for stmt in statements {
                    if !stmt.matches(access) {
                        continue;
                    }
                    if !stmt.allow {
                        return false;
                    }
                    allowed = true;
                }
                allowed
            }
        }
    }
}

impl Statement {
    fn matches(&self, access: &Access) -> bool {
        let action_ok = if !self.actions.is_empty() {
            self.actions
                .iter()
                .any(|p| iam_action_matches(p, &access.action))
        } else if !self.not_actions.is_empty() {
            !self
                .not_actions
                .iter()
                .any(|p| iam_action_matches(p, &access.action))
        } else {
            false
        };
        if !action_ok {
            return false;
        }
        if !self.resources.is_empty() {
            self.resources
                .iter()
                .any(|p| iam_arn_matches(p, &access.resource))
        } else if !self.not_resources.is_empty() {
            !self
                .not_resources
                .iter()
                .any(|p| iam_arn_matches(p, &access.resource))
        } else {
            false
        }
    }
}

fn iam_action_matches(pattern: &str, actual: &str) -> bool {
    glob_match(&pattern.to_ascii_lowercase(), &actual.to_ascii_lowercase())
}

fn iam_arn_matches(pattern: &str, actual: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let (Some(p), Some(a)) = (split_arn(pattern), split_arn(actual)) {
        let partition_ok = p[0] == "*" || a[0] == "*" || glob_match(p[0], a[0]);
        let service_ok = glob_match(&p[1].to_ascii_lowercase(), &a[1].to_ascii_lowercase());
        // Lenslet normalizes region and account to `*`; concrete values in a
        // production IAM policy should still match local normalized ARNs.
        let region_ok = a[2] == "*" || p[2] == "*" || glob_match(p[2], a[2]);
        let account_ok = a[3] == "*" || p[3] == "*" || glob_match(p[3], a[3]);
        let resource_ok = glob_match(p[4], a[4]);
        return partition_ok && service_ok && region_ok && account_ok && resource_ok;
    }
    glob_match(pattern, actual)
}

fn split_arn(arn: &str) -> Option<[&str; 5]> {
    let rest = arn.strip_prefix("arn:")?;
    let (partition, rest) = rest.split_once(':')?;
    let (service, rest) = rest.split_once(':')?;
    let (region, rest) = rest.split_once(':')?;
    let (account, resource) = rest.split_once(':')?;
    Some([partition, service, region, account, resource])
}

/// Standard IAM wildcard matching (`*` matches 0+ chars, `?` matches 1 char).
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            pi += 1;
            star_ti = ti;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(action: &str, resource: &str) -> Access {
        Access {
            action: action.into(),
            resource: resource.into(),
        }
    }

    #[test]
    fn round_trips_through_a_reviewable_text_file() {
        let mut used = BTreeMap::new();
        used.insert(
            access("sqs:SendMessage", "arn:aws:sqs:*:*:jobs"),
            AccessUse::default(),
        );
        used.insert(
            access("dynamodb:CreateTable", "arn:aws:dynamodb:*:*:table/orders"),
            AccessUse::default(),
        );
        used.insert(
            access("kms:Decrypt", "arn:aws:kms:*:*:key/*"),
            AccessUse {
                implied: true,
                ..AccessUse::default()
            },
        );
        let test: BTreeSet<Access> = [access(
            "dynamodb:CreateTable",
            "arn:aws:dynamodb:*:*:table/orders",
        )]
        .into();
        let text = render(&used, &test);
        assert!(
            text.contains("kms:Decrypt           arn:aws:kms:*:*:key/*  # implied\n"),
            "{text}"
        );
        assert!(
            text.contains("dynamodb:CreateTable  arn:aws:dynamodb:*:*:table/orders  # test\n"),
            "{text}"
        );
        let parsed = parse(&text);
        assert_eq!(parsed.access, used.keys().cloned().collect());
        assert_eq!(parsed.test, test, "test marks survive a rewrite");
        assert_eq!(
            parse("s3:ListAllMyBuckets\n# note\n\n").access,
            [access("s3:ListAllMyBuckets", "*")].into()
        );
        assert!(
            parse("s3:GetObject arn  # tested elsewhere\n")
                .test
                .is_empty(),
            "whole word only"
        );
    }

    #[test]
    fn missing_lock_is_none_and_policy_leaves_out_test_lines() {
        let dir = std::env::temp_dir().join(format!("lenslet-lock-{}", std::process::id()));
        assert!(read(&path_in(&dir)).unwrap().is_none());

        let lock = parse(
            "sqs:SendMessage  arn:aws:sqs:*:*:jobs\n\
             sqs:ReceiveMessage  arn:aws:sqs:*:*:jobs\n\
             s3:GetObject  arn:aws:s3:::assets/*\n\
             s3:CreateBucket  arn:aws:s3:::assets  # test\n",
        );
        let policy = policy(&lock);
        let statements = policy["Statement"].as_array().unwrap();
        assert_eq!(statements.len(), 2, "{policy}");
        assert_eq!(statements[0]["Resource"], "arn:aws:s3:::assets/*");
        assert_eq!(statements[1]["Resource"], "arn:aws:sqs:*:*:jobs");
        assert_eq!(
            statements[1]["Action"],
            serde_json::json!(["sqs:ReceiveMessage", "sqs:SendMessage"])
        );
    }

    #[test]
    fn policy_gate_evaluates_allow_deny_wildcards_and_concrete_arns() {
        let json = r#"{
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Effect": "Allow",
                    "Action": ["s3:Get*", "dynamodb:PutItem"],
                    "Resource": [
                        "arn:aws:s3:::assets/*",
                        "arn:aws:dynamodb:us-east-1:123456789012:table/orders"
                    ]
                },
                {
                    "Effect": "Deny",
                    "Action": "s3:GetObjectAcl",
                    "Resource": "*"
                }
            ]
        }"#;
        let gate = Gate::from_policy_str(json, "iam.json").unwrap();
        assert!(gate.allows(&access("s3:GetObject", "arn:aws:s3:::assets/*")));
        assert!(gate.allows(&access(
            "dynamodb:PutItem",
            "arn:aws:dynamodb:*:*:table/orders"
        )));
        assert!(
            !gate.allows(&access("s3:GetObjectAcl", "arn:aws:s3:::assets/*")),
            "explicit Deny overrides Allow"
        );
        assert!(!gate.allows(&access(
            "dynamodb:DeleteTable",
            "arn:aws:dynamodb:*:*:table/orders"
        )));
    }
}
