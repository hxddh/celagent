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
    conditions: Vec<ConditionClause>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetQuantifier {
    Single,
    ForAnyValue,
    ForAllValues,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BaseOp {
    StringEquals,
    StringNotEquals,
    StringEqualsIgnoreCase,
    StringNotEqualsIgnoreCase,
    StringLike,
    StringNotLike,
    ArnEquals,
    ArnNotEquals,
    ArnLike,
    ArnNotLike,
    NumericEquals,
    NumericNotEquals,
    NumericLessThan,
    NumericLessThanEquals,
    NumericGreaterThan,
    NumericGreaterThanEquals,
    Bool,
    Null,
}

impl BaseOp {
    fn is_negated(self) -> bool {
        matches!(
            self,
            BaseOp::StringNotEquals
                | BaseOp::StringNotEqualsIgnoreCase
                | BaseOp::StringNotLike
                | BaseOp::ArnNotEquals
                | BaseOp::ArnNotLike
                | BaseOp::NumericNotEquals
        )
    }
}

#[derive(Clone, Debug)]
struct ConditionClause {
    quantifier: SetQuantifier,
    op: BaseOp,
    if_exists: bool,
    key: String,
    values: Vec<String>,
}

/// Request-level IAM context keys unambiguously observable on the local wire.
#[derive(Clone, Debug, Default)]
pub struct RequestContext {
    values: BTreeMap<String, Vec<String>>,
}

const SUPPORTED_CONDITION_KEYS: &[&str] = &[
    "aws:requestedregion",
    "s3:prefix",
    "s3:delimiter",
    "s3:max-keys",
    "s3:versionid",
    "s3:x-amz-acl",
    "s3:x-amz-copy-source",
    "s3:x-amz-metadata-directive",
    "s3:x-amz-server-side-encryption",
    "s3:x-amz-server-side-encryption-aws-kms-key-id",
    "s3:x-amz-storage-class",
    "s3:x-amz-tagging-directive",
];

impl RequestContext {
    pub fn from_trace(trace: &crate::trace::Trace) -> Self {
        let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if let Some(region) = trace.region.as_deref().filter(|r| !r.is_empty()) {
            values.insert("aws:requestedregion".into(), vec![region.to_string()]);
        }

        if trace.service == "s3" {
            let list_bucket = matches!(
                trace.operation.as_str(),
                "ListObjects" | "ListObjectsV2" | "ListObjectVersions" | "ListMultipartUploads"
            );
            match crate::wire::query_param(&trace.path, "prefix") {
                Some(prefix) => {
                    values.insert("s3:prefix".into(), vec![prefix]);
                }
                None if list_bucket => {
                    // S3 populates `s3:prefix` with `""` on ListBucket calls when omitted.
                    values.insert("s3:prefix".into(), vec![String::new()]);
                }
                None => {}
            }
            match crate::wire::query_param(&trace.path, "delimiter") {
                Some(delim) => {
                    values.insert("s3:delimiter".into(), vec![delim]);
                }
                None if list_bucket => {
                    values.insert("s3:delimiter".into(), vec![String::new()]);
                }
                None => {}
            }
            if let Some(max_keys) = crate::wire::query_param(&trace.path, "max-keys") {
                values.insert("s3:max-keys".into(), vec![max_keys]);
            }
            if let Some(vid) = crate::wire::query_param(&trace.path, "versionId") {
                values.insert("s3:versionid".into(), vec![vid]);
            }
            for header in &trace.request.headers {
                let low = header.name.to_ascii_lowercase();
                if matches!(
                    low.as_str(),
                    "x-amz-acl"
                        | "x-amz-copy-source"
                        | "x-amz-metadata-directive"
                        | "x-amz-server-side-encryption"
                        | "x-amz-server-side-encryption-aws-kms-key-id"
                        | "x-amz-storage-class"
                        | "x-amz-tagging-directive"
                ) {
                    let trimmed = header.value.trim();
                    let val = if low == "x-amz-copy-source" {
                        let raw_path = trimmed.split('?').next().unwrap_or(trimmed);
                        crate::wire::percent_decode_path(raw_path)
                    } else {
                        trimmed.to_string()
                    };
                    values.entry(format!("s3:{low}")).or_default().push(val);
                }
            }
        }

        RequestContext { values }
    }

    pub fn get(&self, key: &str) -> &[String] {
        self.values
            .get(&key.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }
}

/// Sentinel resource emitted when a request body needed for permission
/// derivation was truncated or unreadable: every gate rejects it (fail closed).
pub const UNVERIFIED_RESOURCE: &str = "<unverified-request-body>";

impl Gate {
    pub fn from_lock(label: impl Into<String>, access: BTreeSet<Access>) -> Self {
        Gate {
            label: label.into(),
            kind: GateKind::Lock(access),
        }
    }

    pub fn is_policy(&self) -> bool {
        matches!(self.kind, GateKind::Policy(_))
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
            let conditions = match obj.get("Condition") {
                None | Some(serde_json::Value::Null) => Vec::new(),
                Some(cond_val) => parse_conditions(cond_val)?,
            };
            if obj.get("NotPrincipal").is_some()
                || obj
                    .get("Principal")
                    .is_some_and(|p| p != "*" && p.get("AWS") != Some(&serde_json::json!("*")))
            {
                anyhow::bail!(
                    "IAM Statement restricts `Principal`, which Lenslet cannot evaluate locally; pass an identity policy without principal restrictions"
                );
            }
            let effect = obj.get("Effect").and_then(|v| v.as_str()).unwrap_or("");
            let allow = if effect.eq_ignore_ascii_case("allow") {
                true
            } else if effect.eq_ignore_ascii_case("deny") {
                false
            } else {
                anyhow::bail!("IAM Statement Effect must be `Allow` or `Deny`, got `{effect}`");
            };
            let list = |key: &str| -> Result<Vec<String>> {
                match obj.get(key) {
                    None => Ok(Vec::new()),
                    Some(serde_json::Value::String(s)) => Ok(vec![s.clone()]),
                    Some(serde_json::Value::Array(arr)) => arr
                        .iter()
                        .map(|v| {
                            v.as_str().map(str::to_string).ok_or_else(|| {
                                anyhow::anyhow!("IAM Statement `{key}` entries must be strings")
                            })
                        })
                        .collect(),
                    _ => {
                        anyhow::bail!("IAM Statement `{key}` must be a string or array of strings")
                    }
                }
            };
            let actions = list("Action")?;
            let not_actions = list("NotAction")?;
            let resources = list("Resource")?;
            let not_resources = list("NotResource")?;
            for res in resources.iter().chain(not_resources.iter()) {
                if res.contains("${") {
                    anyhow::bail!(
                        "IAM policy variables (`${{...}}`) in Resource `{res}` cannot be evaluated locally"
                    );
                }
            }
            if actions.is_empty() && not_actions.is_empty() {
                anyhow::bail!("IAM Statement must specify `Action` or `NotAction`");
            }
            if resources.is_empty() && not_resources.is_empty() {
                anyhow::bail!("IAM Statement must specify `Resource` or `NotResource`");
            }
            statements.push(Statement {
                allow,
                actions,
                not_actions,
                resources,
                not_resources,
                conditions,
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

    /// Whether `access` is permitted by this lock or IAM policy without request context.
    pub fn allows(&self, access: &Access) -> bool {
        self.allows_in_context(access, None)
    }

    /// Whether `access` is permitted under optional wire `ctx`.
    pub fn allows_in_context(&self, access: &Access, ctx: Option<&RequestContext>) -> bool {
        if access.resource == UNVERIFIED_RESOURCE {
            return false;
        }
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
                    if !stmt.matches(access, ctx) {
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
    fn matches(&self, access: &Access, ctx: Option<&RequestContext>) -> bool {
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
        let resource_ok = if !self.resources.is_empty() {
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
        };
        if !resource_ok {
            return false;
        }
        if self.conditions.is_empty() {
            return true;
        }
        let Some(ctx) = ctx else {
            // Fail closed when no request context is supplied: conditional Allow
            // does not grant, conditional Deny still triggers.
            return !self.allow;
        };
        self.conditions.iter().all(|clause| clause.evaluate(ctx))
    }
}

fn parse_conditions(value: &serde_json::Value) -> Result<Vec<ConditionClause>> {
    let Some(ops_map) = value.as_object() else {
        anyhow::bail!("IAM Statement `Condition` must be a JSON object");
    };
    let mut clauses = Vec::new();
    for (raw_op, keys_val) in ops_map {
        let (quantifier, op, if_exists) = parse_condition_operator(raw_op)?;
        let Some(keys_map) = keys_val.as_object() else {
            anyhow::bail!("IAM Condition operator `{raw_op}` must map to a JSON object of keys");
        };
        for (raw_key, expected_val) in keys_map {
            let key = raw_key.trim().to_ascii_lowercase();
            if !SUPPORTED_CONDITION_KEYS.contains(&key.as_str()) {
                anyhow::bail!(
                    "IAM Condition key `{raw_key}` cannot be evaluated locally; supported wire keys: {}",
                    SUPPORTED_CONDITION_KEYS.join(", ")
                );
            }
            let values = parse_condition_values(raw_op, raw_key, op, expected_val)?;
            clauses.push(ConditionClause {
                quantifier,
                op,
                if_exists,
                key,
                values,
            });
        }
    }
    Ok(clauses)
}

fn parse_condition_operator(raw: &str) -> Result<(SetQuantifier, BaseOp, bool)> {
    let (quantifier, rest) = if let Some(r) = raw.strip_prefix("ForAnyValue:") {
        (SetQuantifier::ForAnyValue, r)
    } else if let Some(r) = raw.strip_prefix("ForAllValues:") {
        (SetQuantifier::ForAllValues, r)
    } else {
        (SetQuantifier::Single, raw)
    };
    let (base, if_exists) = match rest.strip_suffix("IfExists") {
        Some(stripped) if stripped != "Null" => (stripped, true),
        _ => (rest, false),
    };
    let op = match base {
        "StringEquals" => BaseOp::StringEquals,
        "StringNotEquals" => BaseOp::StringNotEquals,
        "StringEqualsIgnoreCase" => BaseOp::StringEqualsIgnoreCase,
        "StringNotEqualsIgnoreCase" => BaseOp::StringNotEqualsIgnoreCase,
        "StringLike" => BaseOp::StringLike,
        "StringNotLike" => BaseOp::StringNotLike,
        "ArnEquals" | "ARNEquals" => BaseOp::ArnEquals,
        "ArnNotEquals" | "ARNNotEquals" => BaseOp::ArnNotEquals,
        "ArnLike" | "ARNLike" => BaseOp::ArnLike,
        "ArnNotLike" | "ARNNotLike" => BaseOp::ArnNotLike,
        "NumericEquals" => BaseOp::NumericEquals,
        "NumericNotEquals" => BaseOp::NumericNotEquals,
        "NumericLessThan" => BaseOp::NumericLessThan,
        "NumericLessThanEquals" => BaseOp::NumericLessThanEquals,
        "NumericGreaterThan" => BaseOp::NumericGreaterThan,
        "NumericGreaterThanEquals" => BaseOp::NumericGreaterThanEquals,
        "Bool" => BaseOp::Bool,
        "Null" if quantifier == SetQuantifier::Single => BaseOp::Null,
        _ => {
            anyhow::bail!("IAM Condition operator `{raw}` is not supported locally")
        }
    };
    Ok((quantifier, op, if_exists))
}

fn parse_condition_values(
    raw_op: &str,
    raw_key: &str,
    op: BaseOp,
    val: &serde_json::Value,
) -> Result<Vec<String>> {
    let items: Vec<&serde_json::Value> = match val {
        serde_json::Value::Array(arr) => arr.iter().collect(),
        single => vec![single],
    };
    if items.is_empty() {
        anyhow::bail!("IAM Condition `{raw_op}` on `{raw_key}` must specify at least one value");
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let s = match item {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => anyhow::bail!(
                "IAM Condition `{raw_op}` on `{raw_key}` expects string, boolean, or numeric values"
            ),
        };
        if s.contains("${") {
            anyhow::bail!(
                "IAM policy variables (`${{...}}`) in Condition `{raw_op}` on `{raw_key}` cannot be evaluated locally"
            );
        }
        if matches!(op, BaseOp::Bool | BaseOp::Null)
            && !matches!(s.to_ascii_lowercase().as_str(), "true" | "false")
        {
            anyhow::bail!(
                "IAM Condition `{raw_op}` on `{raw_key}` expects `true` or `false`, got `{s}`"
            );
        }
        if matches!(
            op,
            BaseOp::NumericEquals
                | BaseOp::NumericNotEquals
                | BaseOp::NumericLessThan
                | BaseOp::NumericLessThanEquals
                | BaseOp::NumericGreaterThan
                | BaseOp::NumericGreaterThanEquals
        ) && s.parse::<f64>().map_or(true, |n| !n.is_finite())
        {
            anyhow::bail!(
                "IAM Condition `{raw_op}` on `{raw_key}` expects finite numbers, got `{s}`"
            );
        }
        out.push(s);
    }
    Ok(out)
}

impl ConditionClause {
    fn evaluate(&self, ctx: &RequestContext) -> bool {
        let actuals = ctx.get(&self.key);
        if self.op == BaseOp::Null {
            let is_null = actuals.is_empty();
            return self
                .values
                .iter()
                .any(|v| v.eq_ignore_ascii_case("true") == is_null);
        }
        if actuals.is_empty() {
            return match self.quantifier {
                SetQuantifier::ForAllValues => true,
                SetQuantifier::ForAnyValue => self.if_exists,
                SetQuantifier::Single => self.if_exists || self.op.is_negated(),
            };
        }
        match self.quantifier {
            SetQuantifier::ForAnyValue => actuals.iter().any(|a| self.matches_one(a)),
            SetQuantifier::ForAllValues | SetQuantifier::Single => {
                actuals.iter().all(|a| self.matches_one(a))
            }
        }
    }

    fn matches_one(&self, actual: &str) -> bool {
        match self.op {
            BaseOp::StringEquals => self.values.iter().any(|p| actual == p),
            BaseOp::StringNotEquals => self.values.iter().all(|p| actual != p),
            BaseOp::StringEqualsIgnoreCase => {
                self.values.iter().any(|p| actual.eq_ignore_ascii_case(p))
            }
            BaseOp::StringNotEqualsIgnoreCase => {
                self.values.iter().all(|p| !actual.eq_ignore_ascii_case(p))
            }
            BaseOp::StringLike => self.values.iter().any(|p| glob_match(p, actual)),
            BaseOp::StringNotLike => self.values.iter().all(|p| !glob_match(p, actual)),
            BaseOp::ArnEquals => self.values.iter().any(|p| actual == p),
            BaseOp::ArnNotEquals => self.values.iter().all(|p| actual != p),
            BaseOp::ArnLike => self.values.iter().any(|p| iam_arn_matches(p, actual)),
            BaseOp::ArnNotLike => self.values.iter().all(|p| !iam_arn_matches(p, actual)),
            BaseOp::Bool => {
                let Some(act_bool) = parse_bool_str(actual) else {
                    return false;
                };
                self.values
                    .iter()
                    .any(|p| parse_bool_str(p) == Some(act_bool))
            }
            BaseOp::NumericEquals => self.num_any(actual, |a, b| a == b),
            BaseOp::NumericNotEquals => {
                let Ok(a) = actual.parse::<f64>() else {
                    return false;
                };
                self.values
                    .iter()
                    .all(|p| p.parse::<f64>().is_ok_and(|b| a != b))
            }
            BaseOp::NumericLessThan => self.num_any(actual, |a, b| a < b),
            BaseOp::NumericLessThanEquals => self.num_any(actual, |a, b| a <= b),
            BaseOp::NumericGreaterThan => self.num_any(actual, |a, b| a > b),
            BaseOp::NumericGreaterThanEquals => self.num_any(actual, |a, b| a >= b),
            BaseOp::Null => unreachable!(),
        }
    }

    fn num_any(&self, actual: &str, cmp: impl Fn(f64, f64) -> bool) -> bool {
        let Ok(a) = actual.parse::<f64>() else {
            return false;
        };
        self.values
            .iter()
            .any(|p| p.parse::<f64>().is_ok_and(|b| cmp(a, b)))
    }
}

fn parse_bool_str(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn iam_action_matches(pattern: &str, actual: &str) -> bool {
    glob_match(&pattern.to_ascii_lowercase(), &actual.to_ascii_lowercase())
}

pub(crate) fn arn_matches(pattern: &str, actual: &str) -> bool {
    iam_arn_matches(pattern, actual)
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

    #[test]
    fn policy_gate_evaluates_wire_conditions_and_rejects_unsupported_clauses() {
        let conditional = r#"{
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Effect": "Allow",
                    "Action": "s3:PutObject",
                    "Resource": "arn:aws:s3:::assets/*",
                    "Condition": {
                        "StringEquals": {
                            "aws:RequestedRegion": "us-east-1",
                            "s3:x-amz-server-side-encryption": "aws:kms"
                        }
                    }
                },
                {
                    "Effect": "Allow",
                    "Action": "s3:ListBucket",
                    "Resource": "arn:aws:s3:::assets",
                    "Condition": {
                        "StringLike": { "s3:prefix": ["public/*"] },
                        "NumericLessThanEqualsIfExists": { "s3:max-keys": 100 }
                    }
                }
            ]
        }"#;
        let gate = Gate::from_policy_str(conditional, "cond.json").unwrap();

        let mut base_put = crate::trace::Trace {
            id: "1".into(),
            at: "t".into(),
            method: "PUT".into(),
            path: "/assets/doc.txt?x-id=PutObject".into(),
            host: "127.0.0.1:9297".into(),
            service: "s3".into(),
            operation: "PutObject".into(),
            status: 200,
            ms: 1,
            authorization: true,
            region: Some("us-east-1".into()),
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
            request: crate::trace::Message::default(),
            response: crate::trace::Message::default(),
        };
        let put_access = access("s3:PutObject", "arn:aws:s3:::assets/doc.txt");
        // Missing SSE-KMS header → denied.
        let unencrypted_ctx = RequestContext::from_trace(&base_put);
        assert!(!gate.allows_in_context(&put_access, Some(&unencrypted_ctx)));

        // Present SSE-KMS header in matching region → allowed.
        base_put.request.headers.push(crate::trace::HeaderTrace {
            name: "x-amz-server-side-encryption".into(),
            value: "aws:kms".into(),
        });
        let encrypted_ctx = RequestContext::from_trace(&base_put);
        assert!(gate.allows_in_context(&put_access, Some(&encrypted_ctx)));

        // Wrong region → denied.
        base_put.region = Some("eu-west-1".into());
        let wrong_region_ctx = RequestContext::from_trace(&base_put);
        assert!(!gate.allows_in_context(&put_access, Some(&wrong_region_ctx)));

        // ListBucket with matching prefix and omitted max-keys (`IfExists`) → allowed;
        // ListBucket without prefix → s3:prefix is "" → denied.
        let mut list = base_put.clone();
        list.region = Some("us-east-1".into());
        list.operation = "ListObjectsV2".into();
        list.path = "/assets?list-type=2&prefix=public%2Fdocs".into();
        let list_access = access("s3:ListBucket", "arn:aws:s3:::assets");
        assert!(gate.allows_in_context(&list_access, Some(&RequestContext::from_trace(&list))));
        list.path = "/assets?list-type=2".into();
        assert!(!gate.allows_in_context(&list_access, Some(&RequestContext::from_trace(&list))));

        // Cloud-only runtime condition keys and unsupported operators still fail closed.
        let cloud_cond = r#"{
            "Statement": [{
                "Effect": "Allow",
                "Action": "s3:GetObject",
                "Resource": "*",
                "Condition": { "StringEquals": { "aws:PrincipalOrgID": "o-123456" } }
            }]
        }"#;
        let Err(err) = Gate::from_policy_str(cloud_cond, "org.json") else {
            panic!("Cloud-only condition key must be rejected");
        };
        assert!(err.to_string().contains("aws:PrincipalOrgID"), "{err}");

        // IAM policy variables (`${...}`) fail closed rather than matching literally.
        let var_policy = r#"{
            "Statement": [{
                "Effect": "Allow",
                "Action": "s3:ListBucket",
                "Resource": "arn:aws:s3:::assets",
                "Condition": { "StringLike": { "s3:prefix": ["home/${aws:username}/*"] } }
            }]
        }"#;
        assert!(Gate::from_policy_str(var_policy, "var.json").is_err());

        // `s3:x-amz-copy-source` decodes percent-encoding and strips `?versionId=...`
        // so encoded segments cannot evade a Deny condition.
        let copy_deny = Gate::from_policy_str(
            r#"{
                "Statement": [
                    { "Effect": "Allow", "Action": "s3:*", "Resource": "*" },
                    {
                        "Effect": "Deny",
                        "Action": "s3:PutObject",
                        "Resource": "*",
                        "Condition": { "StringLike": { "s3:x-amz-copy-source": ["*secret*"] } }
                    }
                ]
            }"#,
            "copy.json",
        )
        .unwrap();
        let mut copy_trace = base_put.clone();
        copy_trace.request.headers.push(crate::trace::HeaderTrace {
            name: "x-amz-copy-source".into(),
            value: "/vault/%73%65%63%72%65%74.txt?versionId=99".into(),
        });
        assert!(
            !copy_deny
                .allows_in_context(&put_access, Some(&RequestContext::from_trace(&copy_trace))),
            "percent-encoded x-amz-copy-source must still trigger Deny"
        );

        // Unqualified negated operators (`StringNotEquals`, `StringNotLike`, ...)
        // evaluate to `true` when the context key is absent, matching AWS IAM so a
        // Deny guardrail cannot be bypassed by omitting the header.
        let sse_deny_guardrail = Gate::from_policy_str(
            r#"{
                "Statement": [
                    { "Effect": "Allow", "Action": "s3:PutObject", "Resource": "arn:aws:s3:::assets/*" },
                    {
                        "Effect": "Deny",
                        "Action": "s3:PutObject",
                        "Resource": "arn:aws:s3:::assets/*",
                        "Condition": {
                            "StringNotEquals": { "s3:x-amz-server-side-encryption": "aws:kms" }
                        }
                    }
                ]
            }"#,
            "sse-guardrail.json",
        )
        .unwrap();
        let mut omitted_sse = base_put.clone();
        omitted_sse.request.headers.clear();
        assert!(
            !sse_deny_guardrail
                .allows_in_context(&put_access, Some(&RequestContext::from_trace(&omitted_sse))),
            "omitting x-amz-server-side-encryption must trigger StringNotEquals Deny"
        );
        let mut wrong_sse = omitted_sse.clone();
        wrong_sse.request.headers.push(crate::trace::HeaderTrace {
            name: "x-amz-server-side-encryption".into(),
            value: "AES256".into(),
        });
        assert!(
            !sse_deny_guardrail
                .allows_in_context(&put_access, Some(&RequestContext::from_trace(&wrong_sse))),
            "non-matching x-amz-server-side-encryption must trigger StringNotEquals Deny"
        );
        let mut matching_sse = omitted_sse;
        matching_sse
            .request
            .headers
            .push(crate::trace::HeaderTrace {
                name: "x-amz-server-side-encryption".into(),
                value: "aws:kms".into(),
            });
        assert!(
            sse_deny_guardrail.allows_in_context(
                &put_access,
                Some(&RequestContext::from_trace(&matching_sse))
            ),
            "matching aws:kms header satisfies guardrail"
        );

        let principal_scoped = r#"{
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "AWS": "arn:aws:iam::123456789012:root" },
                "Action": "s3:GetObject",
                "Resource": "arn:aws:s3:::assets/*"
            }]
        }"#;
        let Err(err) = Gate::from_policy_str(principal_scoped, "principal.json") else {
            panic!("Restricted Principal must be rejected");
        };
        let err = err.to_string();
        assert!(err.contains("Principal"), "{err}");

        let admin = Gate::from_policy_str(
            r#"{"Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"#,
            "admin.json",
        )
        .unwrap();
        assert!(
            !admin.allows(&access("dynamodb:BatchWriteItem", UNVERIFIED_RESOURCE)),
            "unverified request bodies always fail closed"
        );
    }
}
