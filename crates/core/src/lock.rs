//! `lenslet.lock`: the AWS access a project's runs are known to need.
//!
//! One permission per line, `action  resource`, sorted, so a pull request
//! that widens access shows up as an added line in review. Comments start
//! with `#`. The file is written by `lenslet run --update` and by
//! [`accept`] / [`set_test_mark`] (the window), always atomically and always
//! in the canonical [`render`] format.
//!
//! What a rewrite keeps: every `test` mark, and the full-line comments at the
//! top of the file (they replace the generated header). Other comments, and
//! blank lines, are not kept: the entries are re-rendered.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    access::{self, Access, CallAccess},
    summary::AccessUse,
    trace::Trace,
};

pub const FILE_NAME: &str = "lenslet.lock";

/// The header a new lock file gets.
const DEFAULT_HEADER: &str = "# AWS access observed by `lenslet run`: action, resource.\n\
     # Regenerate with `lenslet run --update -- <tests>`; review changes like code.\n\
     # Mark a line `# test` when only the tests need it; `lenslet policy` leaves it out.\n";

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
    /// Lines marked `# implied`: needed by another call (KMS, PassRole).
    pub implied: BTreeSet<Access>,
    /// The full-line comments before the first entry, kept on rewrite.
    pub header: Vec<String>,
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
    let mut in_header = true;
    for line in text.lines() {
        let (entry, comment) = line.split_once('#').unwrap_or((line, ""));
        let mut fields = entry.split_whitespace();
        let Some(action) = fields.next() else {
            if in_header && line.trim_start().starts_with('#') {
                lock.header.push(line.trim_end().to_string());
            }
            continue;
        };
        in_header = false;
        let access = Access {
            action: action.to_string(),
            resource: fields.next().unwrap_or("*").to_string(),
        };
        let words: Vec<&str> = comment
            .split(|c: char| !c.is_ascii_alphanumeric())
            .collect();
        if words.contains(&"test") {
            lock.test.insert(access.clone());
        }
        if words.contains(&"implied") {
            lock.implied.insert(access.clone());
        }
        lock.access.insert(access);
    }
    lock
}

/// The lock file text. `test` marks carry over from the previous lock.
pub fn render(access: &BTreeMap<Access, AccessUse>, test: &BTreeSet<Access>) -> String {
    render_entries(
        DEFAULT_HEADER,
        access
            .iter()
            .map(|(access, used)| (access, test.contains(access), used.implied)),
    )
}

/// The canonical text of a parsed lock, with its own header comments.
fn render_lock(lock: &Lock) -> String {
    render_entries(
        &header_text(&lock.header),
        lock.access.iter().map(|access| {
            (
                access,
                lock.test.contains(access),
                lock.implied.contains(access),
            )
        }),
    )
}

fn header_text(header: &[String]) -> String {
    if header.is_empty() {
        DEFAULT_HEADER.to_string()
    } else {
        header.iter().map(|line| format!("{line}\n")).collect()
    }
}

/// One aligned `action  resource  # notes` line per entry, in order.
fn render_entries<'a>(
    header: &str,
    entries: impl Iterator<Item = (&'a Access, bool, bool)> + Clone,
) -> String {
    let width = entries
        .clone()
        .map(|(access, _, _)| access.action.len())
        .max()
        .unwrap_or(0);
    let mut out = header.to_string();
    for (access, test, implied) in entries {
        out.push_str(&format!("{:<width$}  {}", access.action, access.resource));
        let mut notes = Vec::new();
        if test {
            notes.push("test");
        }
        if implied {
            notes.push("implied");
        }
        if !notes.is_empty() {
            out.push_str(&format!("  # {}", notes.join(", ")));
        }
        out.push('\n');
    }
    out
}

/// Write the lock from a run's access (`lenslet run --update`). The caller
/// passes the `test` marks to keep; the file's header comments are kept.
pub fn write(
    path: &Path,
    access: &BTreeMap<Access, AccessUse>,
    test: &BTreeSet<Access>,
) -> Result<()> {
    let header = read(path)?.map(|lock| lock.header).unwrap_or_default();
    let text = render_entries(
        &header_text(&header),
        access
            .iter()
            .map(|(access, used)| (access, test.contains(access), used.implied)),
    );
    write_atomic(path, &text)
}

/// Add permissions to the lock (creating it if needed), each with its `test`
/// mark. Lines already in the lock are left as they are. Returns how many
/// lines were added; the file is only rewritten when that is not zero.
pub fn accept(path: &Path, additions: &[(Access, bool)]) -> Result<usize> {
    let with_implied: Vec<(Access, bool, bool)> = additions
        .iter()
        .map(|(acc, test)| (acc.clone(), *test, false))
        .collect();
    accept_with_implied(path, &with_implied)
}

/// Like [`accept`], also preserving `# implied` notes when accepting from the UI.
pub fn accept_with_implied(path: &Path, additions: &[(Access, bool, bool)]) -> Result<usize> {
    let mut lock = read(path)?.unwrap_or_default();
    let mut added = 0;
    for (access, test, implied) in additions {
        if lock.access.insert(access.clone()) {
            added += 1;
            if *test {
                lock.test.insert(access.clone());
            }
            if *implied {
                lock.implied.insert(access.clone());
            }
        }
    }
    if added > 0 {
        write_atomic(path, &render_lock(&lock))?;
    }
    Ok(added)
}

/// Mark (or unmark) a lock line `# test`. Returns whether the file changed:
/// `false` when the line is not in the lock or already marked that way.
pub fn set_test_mark(path: &Path, access: &Access, test: bool) -> Result<bool> {
    let Some(mut lock) = read(path)? else {
        return Ok(false);
    };
    if !lock.access.contains(access) || lock.test.contains(access) == test {
        return Ok(false);
    }
    if test {
        lock.test.insert(access.clone());
    } else {
        lock.test.remove(access);
    }
    write_atomic(path, &render_lock(&lock))?;
    Ok(true)
}

/// Replace `path` with `text` atomically: a temporary file in the same
/// directory, synced, given the old file's permissions, then renamed over it.
/// A symlinked lock is written through to its target.
fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| FILE_NAME.to_string());
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let permissions = fs::metadata(&target).ok().map(|meta| meta.permissions());
    let result = (|| -> std::io::Result<()> {
        let mut file = fs::File::create(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        if let Some(permissions) = permissions {
            fs::set_permissions(&temp, permissions)?;
        }
        fs::rename(&temp, &target)?;
        // Make the rename itself durable (best effort; not every platform
        // can open a directory).
        if let Ok(dir) = fs::File::open(dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| format!("write {}", path.display()))
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
    /// `Sid`, or `Statement[i]` (0-based) when the statement has none.
    label: String,
    allow: bool,
    actions: Vec<String>,
    not_actions: Vec<String>,
    resources: Vec<String>,
    not_resources: Vec<String>,
    conditions: Vec<ConditionClause>,
    /// Policy variables (`${...}`) referenced in `Resource` / `NotResource`.
    resource_vars: Vec<String>,
}

/// Why a permission was allowed or denied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Allowed,
    /// A `Deny` statement matched.
    ExplicitDeny,
    /// No `Allow` statement (or lock line) grants it.
    NoAllow,
    /// Matching statement depends on cloud-runtime context keys not supplied locally.
    NeedsContext,
}

/// A gate's verdict on one permission, and the statement that decided it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub allowed: bool,
    /// The deciding statement's `Sid`, or `Statement[i]` (0-based) without
    /// one: the denying statement, or the first allowing one. `None` for a
    /// lock, and when no statement matched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<String>,
    pub reason: Reason,
    /// Cloud context keys or policy variables needed to resolve a conditional statement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_context: Vec<String>,
}

impl Decision {
    pub fn allowed(by: Option<String>) -> Self {
        Decision {
            allowed: true,
            by,
            reason: Reason::Allowed,
            missing_context: Vec::new(),
        }
    }

    pub fn explicit_deny(by: String) -> Self {
        Decision {
            allowed: false,
            by: Some(by),
            reason: Reason::ExplicitDeny,
            missing_context: Vec::new(),
        }
    }

    pub fn no_allow() -> Self {
        Decision {
            allowed: false,
            by: None,
            reason: Reason::NoAllow,
            missing_context: Vec::new(),
        }
    }

    pub fn needs_context(by: String, missing_context: Vec<String>) -> Self {
        Decision {
            allowed: false,
            by: Some(by),
            reason: Reason::NeedsContext,
            missing_context,
        }
    }
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
    raw_key: String,
    values: Vec<String>,
    /// True when `key` is a cloud-runtime IAM context key (not observable on wire).
    cloud_only: bool,
    /// Policy variables (`${...}`) referenced in `values`.
    value_vars: Vec<String>,
}

/// Request-level IAM context keys unambiguously observable on the local wire
/// (plus optional simulation overrides passed via `--context`).
#[derive(Clone, Debug, Default)]
pub struct RequestContext {
    values: BTreeMap<String, Vec<String>>,
}

const SUPPORTED_CONDITION_KEYS: &[&str] = &[
    "aws:requestedregion",
    "aws:tagkeys",
    "dynamodb:leadingkeys",
    "iam:passedtoservice",
    "kms:encryptioncontextkeys",
    "kms:viaservice",
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

fn is_wire_condition_key(key: &str) -> bool {
    SUPPORTED_CONDITION_KEYS.contains(&key)
        || key
            .strip_prefix("aws:requesttag/")
            .is_some_and(|rest| !rest.is_empty())
        || key
            .strip_prefix("kms:encryptioncontext:")
            .is_some_and(|rest| !rest.is_empty())
}

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
                } else if low == "x-amz-tagging" {
                    for pair in header.value.split('&') {
                        let pair = pair.trim();
                        if pair.is_empty() {
                            continue;
                        }
                        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                        let k_dec = crate::wire::percent_decode_query(k);
                        let v_dec = crate::wire::percent_decode_query(v);
                        if !k_dec.is_empty() {
                            push_tag(&mut values, &k_dec, &v_dec);
                        }
                    }
                }
            }
        }

        if let Some(body_text) = trace
            .request
            .body
            .as_ref()
            .and_then(|b| b.text.as_deref())
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            extract_wire_body_context(&trace.service, body_text, &mut values);
        }

        RequestContext { values }
    }

    /// Merge caller-supplied `--context key=val` overrides into this context.
    pub fn extend_overrides(&mut self, overrides: &BTreeMap<String, Vec<String>>) {
        for (k, vals) in overrides {
            self.values
                .insert(k.trim().to_ascii_lowercase(), vals.clone());
        }
    }

    pub fn get(&self, key: &str) -> &[String] {
        self.values
            .get(&key.to_ascii_lowercase())
            .map_or(&[], Vec::as_slice)
    }

    /// Derive the request context seen by an implied permission (`iam:PassRole`
    /// receives `iam:PassedToService`, and implied `kms:*` receives `kms:ViaService`).
    pub fn for_implied(
        &self,
        parent_service: &str,
        region: Option<&str>,
        implied_action: &str,
    ) -> Self {
        let mut next = self.clone();
        if implied_action.eq_ignore_ascii_case("iam:PassRole") {
            let principal = match parent_service {
                "ecs" => "ecs-tasks.amazonaws.com".to_string(),
                "monitoring" => "cloudwatch.amazonaws.com".to_string(),
                "email" | "sesv2" => "ses.amazonaws.com".to_string(),
                "bedrock-runtime" => "bedrock.amazonaws.com".to_string(),
                other => format!("{other}.amazonaws.com"),
            };
            next.values
                .insert("iam:passedtoservice".into(), vec![principal]);
        } else if implied_action.to_ascii_lowercase().starts_with("kms:")
            && let Some(reg) = region.filter(|r| !r.is_empty())
        {
            let via_svc = match parent_service {
                "streams.dynamodb" | "dynamodbstreams" => "dynamodb",
                "email" | "sesv2" => "ses",
                other => other,
            };
            next.values.insert(
                "kms:viaservice".into(),
                vec![format!("{via_svc}.{reg}.amazonaws.com")],
            );
        }
        next
    }
}

fn push_unique(values: &mut BTreeMap<String, Vec<String>>, key: &str, val: &str) {
    let slot = values.entry(key.to_ascii_lowercase()).or_default();
    if !slot.iter().any(|existing| existing == val) {
        slot.push(val.to_string());
    }
}

fn push_tag(values: &mut BTreeMap<String, Vec<String>>, key: &str, val: &str) {
    let key = key.trim();
    if key.is_empty() {
        return;
    }
    push_unique(
        values,
        &format!("aws:requesttag/{}", key.to_ascii_lowercase()),
        val,
    );
    push_unique(values, "aws:tagkeys", key);
}

fn push_tag_key_only(values: &mut BTreeMap<String, Vec<String>>, key: &str) {
    let key = key.trim();
    if !key.is_empty() {
        push_unique(values, "aws:tagkeys", key);
    }
}

fn extract_wire_body_context(
    service: &str,
    body_text: &str,
    values: &mut BTreeMap<String, Vec<String>>,
) {
    if body_text.starts_with('{')
        && let Ok(serde_json::Value::Object(map)) =
            serde_json::from_str::<serde_json::Value>(body_text)
    {
        for field in ["Tags", "tags", "TagList", "tagList"] {
            match map.get(field) {
                Some(serde_json::Value::Array(arr)) => {
                    for entry in arr {
                        let Some(obj) = entry.as_object() else {
                            continue;
                        };
                        let k = obj
                            .get("Key")
                            .or_else(|| obj.get("key"))
                            .and_then(|v| v.as_str());
                        let v = obj
                            .get("Value")
                            .or_else(|| obj.get("value"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if let Some(k) = k {
                            push_tag(values, k, v);
                        }
                    }
                }
                Some(serde_json::Value::Object(tag_map)) => {
                    for (k, v) in tag_map {
                        if let Some(vs) = v.as_str() {
                            push_tag(values, k, vs);
                        }
                    }
                }
                _ => {}
            }
        }
        for field in ["TagKeys", "tagKeys"] {
            if let Some(serde_json::Value::Array(arr)) = map.get(field) {
                for v in arr.iter().filter_map(|v| v.as_str()) {
                    push_tag_key_only(values, v);
                }
            }
        }
        if service == "dynamodb" {
            for field in ["Key", "Item"] {
                if let Some(serde_json::Value::Object(item)) = map.get(field) {
                    extract_ddb_scalars(item, values);
                }
            }
            if let Some(serde_json::Value::Object(req_items)) = map.get("RequestItems") {
                for table_entry in req_items.values() {
                    if let Some(keys) = table_entry.get("Keys").and_then(|v| v.as_array()) {
                        for kobj in keys.iter().filter_map(|v| v.as_object()) {
                            extract_ddb_scalars(kobj, values);
                        }
                    }
                    if let Some(writes) = table_entry.as_array() {
                        for w in writes {
                            if let Some(item) = w
                                .get("PutRequest")
                                .and_then(|p| p.get("Item"))
                                .and_then(|v| v.as_object())
                            {
                                extract_ddb_scalars(item, values);
                            }
                            if let Some(key) = w
                                .get("DeleteRequest")
                                .and_then(|d| d.get("Key"))
                                .and_then(|v| v.as_object())
                            {
                                extract_ddb_scalars(key, values);
                            }
                        }
                    }
                }
            }
        }
        if service == "kms"
            && let Some(serde_json::Value::Object(enc)) = map.get("EncryptionContext")
        {
            for (k, v) in enc {
                if let Some(vs) = v.as_str() {
                    push_unique(
                        values,
                        &format!("kms:encryptioncontext:{}", k.to_ascii_lowercase()),
                        vs,
                    );
                    push_unique(values, "kms:encryptioncontextkeys", k);
                }
            }
        }
        return;
    }

    if body_text.starts_with('<') {
        // S3 PutObjectTagging / PutBucketTagging XML `<Tag><Key>...</Key><Value>...</Value></Tag>`
        let mut rest = body_text;
        while let Some(start) = rest.find("<Tag>") {
            let after_start = &rest[start + 5..];
            let Some(end) = after_start.find("</Tag>") else {
                break;
            };
            let block = &after_start[..end];
            if let Some(k) = xml_element(block, "Key") {
                let v = xml_element(block, "Value").unwrap_or_default();
                push_tag(values, &k, &v);
            }
            rest = &after_start[end + 6..];
        }
        return;
    }

    // AWS Query form-urlencoded: `Tag.1.Key=env&Tag.1.Value=prod` or `Tags.member.1.Key=...`
    if body_text.contains('=') {
        let mut tag_keys_by_idx: BTreeMap<String, String> = BTreeMap::new();
        let mut tag_vals_by_idx: BTreeMap<String, String> = BTreeMap::new();
        for pair in body_text.split('&') {
            let Some((raw_k, raw_v)) = pair.split_once('=') else {
                continue;
            };
            let k = crate::wire::percent_decode_query(raw_k);
            let v = crate::wire::percent_decode_query(raw_v);
            for prefix in ["Tag.", "Tags.member.", "TagSpecification.1.Tag."] {
                if let Some(tail) = k.strip_prefix(prefix) {
                    if let Some(idx) = tail.strip_suffix(".Key") {
                        tag_keys_by_idx.insert(idx.to_string(), v.clone());
                    } else if let Some(idx) = tail.strip_suffix(".Value") {
                        tag_vals_by_idx.insert(idx.to_string(), v.clone());
                    }
                }
            }
            if k.starts_with("TagKeys.member.") {
                push_tag_key_only(values, &v);
            }
        }
        for (idx, tk) in tag_keys_by_idx {
            let tv = tag_vals_by_idx.remove(&idx).unwrap_or_default();
            push_tag(values, &tk, &tv);
        }
    }
}

fn extract_ddb_scalars(
    item: &serde_json::Map<String, serde_json::Value>,
    values: &mut BTreeMap<String, Vec<String>>,
) {
    for attr in item.values().take(8) {
        let Some(typed) = attr.as_object() else {
            continue;
        };
        for ty in ["S", "N", "B"] {
            if let Some(s) = typed.get(ty).and_then(|v| v.as_str()) {
                push_unique(values, "dynamodb:leadingkeys", s);
            }
        }
    }
}

fn xml_element(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(
        xml[start..end]
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'"),
    )
}

fn collect_policy_vars(text: &str) -> Vec<String> {
    let mut vars = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            break;
        };
        let raw_var = after[..end].trim();
        // Strip optional default value `${aws:PrincipalTag/team, 'none'}`
        let key = raw_var
            .split(',')
            .next()
            .unwrap_or(raw_var)
            .trim()
            .to_string();
        if !key.is_empty() && !vars.iter().any(|v: &String| v.eq_ignore_ascii_case(&key)) {
            vars.push(key);
        }
        rest = &after[end + 1..];
    }
    vars
}

fn resolve_policy_template(
    text: &str,
    ctx: Option<&RequestContext>,
    wildcard_unresolved: bool,
) -> String {
    if !text.contains("${") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let raw_var = after[..end].trim();
        let (key_part, default_part) = match raw_var.split_once(',') {
            Some((k, d)) => (
                k.trim(),
                Some(d.trim().trim_matches('\'').trim_matches('"')),
            ),
            None => (raw_var, None),
        };
        if let Some(val) = ctx.and_then(|c| c.get(key_part).first()) {
            out.push_str(val);
        } else if let Some(def) = default_part {
            out.push_str(def);
        } else if wildcard_unresolved {
            out.push('*');
        } else {
            out.push_str(&rest[start..start + 2 + end + 1]);
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[derive(Clone, Debug)]
enum StatementMatch {
    NoMatch,
    Matched,
    NeedsContext(Vec<String>),
}

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

    /// Parse an IAM policy in simulation mode (`lenslet check` and UI Policy Simulator):
    /// cloud-runtime condition keys (`aws:PrincipalOrgID`, `aws:PrincipalTag/*`, etc.)
    /// and `${...}` policy variables are retained instead of rejected at load time,
    /// evaluating to [`Reason::NeedsContext`] unless supplied via `--context key=val`.
    pub fn from_policy_file_simulating(path: &Path, label: impl Into<String>) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("read policy {}", path.display()))?;
        Self::from_policy_str_simulating(&text, label)
            .with_context(|| format!("parse IAM policy {}", path.display()))
    }

    pub fn from_policy_str(text: &str, label: impl Into<String>) -> Result<Self> {
        Self::parse_policy_inner(text, label, false)
    }

    pub fn from_policy_str_simulating(text: &str, label: impl Into<String>) -> Result<Self> {
        Self::parse_policy_inner(text, label, true)
    }

    fn parse_policy_inner(text: &str, label: impl Into<String>, simulating: bool) -> Result<Self> {
        let doc: serde_json::Value = serde_json::from_str(text).context("invalid JSON")?;
        let raw_statements = match doc.get("Statement") {
            Some(serde_json::Value::Array(items)) => items.clone(),
            Some(single @ serde_json::Value::Object(_)) => vec![single.clone()],
            _ => anyhow::bail!("IAM policy must contain a `Statement` array or object"),
        };
        let mut statements = Vec::with_capacity(raw_statements.len());
        for (index, item) in raw_statements.into_iter().enumerate() {
            let Some(obj) = item.as_object() else {
                anyhow::bail!("each IAM Statement must be a JSON object");
            };
            let conditions = match obj.get("Condition") {
                None | Some(serde_json::Value::Null) => Vec::new(),
                Some(cond_val) => parse_conditions(cond_val, simulating)?,
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
            let mut resource_vars = Vec::new();
            for res in resources.iter().chain(not_resources.iter()) {
                let vars = collect_policy_vars(res);
                if !vars.is_empty() && !simulating {
                    anyhow::bail!(
                        "IAM policy variables (`${{...}}`) in Resource `{res}` cannot be evaluated locally"
                    );
                }
                for v in vars {
                    if !resource_vars
                        .iter()
                        .any(|x: &String| x.eq_ignore_ascii_case(&v))
                    {
                        resource_vars.push(v);
                    }
                }
            }
            if actions.is_empty() && not_actions.is_empty() {
                anyhow::bail!("IAM Statement must specify `Action` or `NotAction`");
            }
            if resources.is_empty() && not_resources.is_empty() {
                anyhow::bail!("IAM Statement must specify `Resource` or `NotResource`");
            }
            let label = obj
                .get("Sid")
                .and_then(|sid| sid.as_str())
                .filter(|sid| !sid.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("Statement[{index}]"));
            statements.push(Statement {
                label,
                allow,
                actions,
                not_actions,
                resources,
                not_resources,
                conditions,
                resource_vars,
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
        self.decide(access, None).allowed
    }

    /// Whether `access` is permitted under optional wire `ctx`.
    pub fn allows_in_context(&self, access: &Access, ctx: Option<&RequestContext>) -> bool {
        self.decide(access, ctx).allowed
    }

    /// The verdict on `access`, and which statement decided it.
    pub fn decide(&self, access: &Access, ctx: Option<&RequestContext>) -> Decision {
        match &self.kind {
            GateKind::Lock(entries) if entries.contains(access) => Decision::allowed(None),
            GateKind::Lock(_) => Decision::no_allow(),
            GateKind::Policy(statements) => {
                let mut allowed_by: Option<String> = None;
                let mut needs_context_allow: Option<(String, Vec<String>)> = None;
                let mut needs_context_deny: Option<(String, Vec<String>)> = None;
                for statement in statements {
                    match statement.classify(access, ctx) {
                        StatementMatch::NoMatch => continue,
                        StatementMatch::Matched => {
                            if !statement.allow {
                                return Decision::explicit_deny(statement.label.clone());
                            }
                            allowed_by.get_or_insert_with(|| statement.label.clone());
                        }
                        StatementMatch::NeedsContext(missing) => {
                            if statement.allow {
                                needs_context_allow
                                    .get_or_insert_with(|| (statement.label.clone(), missing));
                            } else {
                                needs_context_deny
                                    .get_or_insert_with(|| (statement.label.clone(), missing));
                            }
                        }
                    }
                }
                if let Some(by) = allowed_by {
                    if let Some((deny_by, missing)) = needs_context_deny {
                        return Decision::needs_context(deny_by, missing);
                    }
                    return Decision::allowed(Some(by));
                }
                if let Some((allow_by, missing)) = needs_context_allow {
                    return Decision::needs_context(allow_by, missing);
                }
                Decision::no_allow()
            }
        }
    }
}

impl Statement {
    fn classify(&self, access: &Access, ctx: Option<&RequestContext>) -> StatementMatch {
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
            return StatementMatch::NoMatch;
        }

        let mut missing_vars: Vec<String> = Vec::new();
        for var in &self.resource_vars {
            if ctx.is_none_or(|c| c.get(var).is_empty()) {
                missing_vars.push(var.clone());
            }
        }

        type Matcher = fn(&str, &str) -> bool;
        let (covers, overlaps): (Matcher, Matcher) = (arn_covers, arn_overlaps);
        let check_resources = |wildcard_unresolved: bool| -> bool {
            if !self.resources.is_empty() {
                let matches = if self.allow { covers } else { overlaps };
                self.resources.iter().any(|p| {
                    let resolved = resolve_policy_template(p, ctx, wildcard_unresolved);
                    matches(&resolved, &access.resource)
                })
            } else if !self.not_resources.is_empty() {
                let excluded = if self.allow { overlaps } else { covers };
                !self.not_resources.iter().any(|p| {
                    let resolved = resolve_policy_template(p, ctx, wildcard_unresolved);
                    excluded(&resolved, &access.resource)
                })
            } else {
                false
            }
        };

        if missing_vars.is_empty() {
            if !check_resources(false) {
                return StatementMatch::NoMatch;
            }
        } else if !check_resources(true) {
            // Even if unresolved `${...}` expanded to `*`, this resource cannot match.
            return StatementMatch::NoMatch;
        }

        if self.conditions.is_empty() {
            return if missing_vars.is_empty() {
                StatementMatch::Matched
            } else {
                StatementMatch::NeedsContext(missing_vars)
            };
        }

        let Some(ctx) = ctx else {
            if !missing_vars.is_empty() {
                return StatementMatch::NeedsContext(missing_vars);
            }
            let cloud_keys: Vec<String> = self
                .conditions
                .iter()
                .filter(|c| c.cloud_only || !c.value_vars.is_empty())
                .map(|c| c.raw_key.clone())
                .collect();
            if !cloud_keys.is_empty() {
                return StatementMatch::NeedsContext(cloud_keys);
            }
            // Fail closed when no request context is supplied: conditional Allow
            // does not grant, conditional Deny still triggers.
            return if !self.allow {
                StatementMatch::Matched
            } else {
                StatementMatch::NoMatch
            };
        };

        for clause in &self.conditions {
            let mut clause_missing = Vec::new();
            if clause.cloud_only && ctx.get(&clause.key).is_empty() && !clause.if_exists {
                clause_missing.push(clause.raw_key.clone());
            }
            for var in &clause.value_vars {
                if ctx.get(var).is_empty()
                    && !clause_missing
                        .iter()
                        .any(|k: &String| k.eq_ignore_ascii_case(var))
                {
                    clause_missing.push(var.clone());
                }
            }
            if clause_missing.is_empty() {
                if !clause.evaluate(ctx) {
                    return StatementMatch::NoMatch;
                }
            } else {
                for key in clause_missing {
                    if !missing_vars.iter().any(|k| k.eq_ignore_ascii_case(&key)) {
                        missing_vars.push(key);
                    }
                }
            }
        }

        if missing_vars.is_empty() {
            StatementMatch::Matched
        } else {
            StatementMatch::NeedsContext(missing_vars)
        }
    }
}

/// A denied permission of one call, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Denial {
    pub action: String,
    pub resource: String,
    /// Needed elsewhere (a KMS key, a passed role) rather than by the call itself.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub implied: bool,
    /// The resource is `*` because the request body could not be read.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unverified: bool,
    #[serde(flatten)]
    pub decision: Decision,
}

impl Denial {
    pub fn key(&self) -> Access {
        Access::new(self.action.clone(), self.resource.clone())
    }
}

/// One call checked against a gate, derived once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Evaluation {
    /// The footprint, each entry marked `new` when the gate denies it.
    pub access: Vec<CallAccess>,
    pub implied: Vec<CallAccess>,
    /// What the gate denied: concrete permissions for a policy, footprint
    /// entries for a lock; own permissions first, then implied ones.
    pub denied: Vec<Denial>,
    /// Concrete own permissions, for cross-call diagnostics.
    pub concrete_own: Vec<Access>,
}

/// Derive what `trace` needs and check it against `gate`. Without a gate
/// nothing is denied and nothing is new.
pub fn evaluate(trace: &Trace, gate: Option<&Gate>) -> Evaluation {
    evaluate_with_context(trace, gate, &BTreeMap::new())
}

/// Derive what `trace` needs and check it against `gate` with optional context overrides.
pub fn evaluate_with_context(
    trace: &Trace,
    gate: Option<&Gate>,
    overrides: &BTreeMap<String, Vec<String>>,
) -> Evaluation {
    let derived = access::derive_call(trace);
    let flag = |entry: &Access| derived.unverified.contains(entry);
    let mut denied = Vec::new();
    if let Some(gate) = gate {
        let (own, implied) = if gate.is_policy() {
            (&derived.concrete_own, &derived.concrete_implied)
        } else {
            (&derived.own, &derived.implied)
        };
        let ctx = gate.is_policy().then(|| {
            let mut c = RequestContext::from_trace(trace);
            if !overrides.is_empty() {
                c.extend_overrides(overrides);
            }
            c
        });
        for (entry, is_implied) in own
            .iter()
            .map(|entry| (entry, false))
            .chain(implied.iter().map(|entry| (entry, true)))
        {
            let entry_ctx = ctx.as_ref().map(|ctx| {
                if is_implied {
                    ctx.for_implied(&trace.service, trace.region.as_deref(), &entry.action)
                } else {
                    ctx.clone()
                }
            });
            let decision = gate.decide(entry, entry_ctx.as_ref());
            if !decision.allowed {
                denied.push(Denial {
                    action: entry.action.clone(),
                    resource: entry.resource.clone(),
                    implied: is_implied,
                    unverified: flag(entry),
                    decision,
                });
            }
        }
    }
    // A footprint entry is new when a denied permission falls under it.
    let mark = |entries: &[Access], is_implied: bool| -> Vec<CallAccess> {
        entries
            .iter()
            .map(|entry| CallAccess {
                new: denied.iter().any(|denial| {
                    denial.implied == is_implied
                        && denial.action == entry.action
                        && arn_covers(&entry.resource, &denial.resource)
                }),
                unverified: flag(entry),
                action: entry.action.clone(),
                resource: entry.resource.clone(),
            })
            .collect()
    };
    Evaluation {
        access: mark(&derived.own, false),
        implied: mark(&derived.implied, true),
        concrete_own: derived.concrete_own.clone(),
        denied,
    }
}

fn parse_conditions(value: &serde_json::Value, simulating: bool) -> Result<Vec<ConditionClause>> {
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
            let wire = is_wire_condition_key(&key);
            if !wire && !simulating {
                anyhow::bail!(
                    "IAM Condition key `{raw_key}` cannot be evaluated locally; supported wire keys: {}, aws:requesttag/*, kms:encryptioncontext:* (use `lenslet check --context {raw_key}=...` to simulate cloud keys)",
                    SUPPORTED_CONDITION_KEYS.join(", ")
                );
            }
            let (values, value_vars) =
                parse_condition_values(raw_op, raw_key, op, expected_val, simulating)?;
            clauses.push(ConditionClause {
                quantifier,
                op,
                if_exists,
                key,
                raw_key: raw_key.trim().to_string(),
                values,
                cloud_only: !wire,
                value_vars,
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
    simulating: bool,
) -> Result<(Vec<String>, Vec<String>)> {
    let items: Vec<&serde_json::Value> = match val {
        serde_json::Value::Array(arr) => arr.iter().collect(),
        single => vec![single],
    };
    if items.is_empty() {
        anyhow::bail!("IAM Condition `{raw_op}` on `{raw_key}` must specify at least one value");
    }
    let mut out = Vec::with_capacity(items.len());
    let mut vars = Vec::new();
    for item in items {
        let s = match item {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => anyhow::bail!(
                "IAM Condition `{raw_op}` on `{raw_key}` expects string, boolean, or numeric values"
            ),
        };
        let item_vars = collect_policy_vars(&s);
        if !item_vars.is_empty() && !simulating {
            anyhow::bail!(
                "IAM policy variables (`${{...}}`) in Condition `{raw_op}` on `{raw_key}` cannot be evaluated locally"
            );
        }
        for v in item_vars {
            if !vars.iter().any(|x: &String| x.eq_ignore_ascii_case(&v)) {
                vars.push(v);
            }
        }
        if vars.is_empty()
            && matches!(op, BaseOp::Bool | BaseOp::Null)
            && !matches!(s.to_ascii_lowercase().as_str(), "true" | "false")
        {
            anyhow::bail!(
                "IAM Condition `{raw_op}` on `{raw_key}` expects `true` or `false`, got `{s}`"
            );
        }
        if vars.is_empty()
            && matches!(
                op,
                BaseOp::NumericEquals
                    | BaseOp::NumericNotEquals
                    | BaseOp::NumericLessThan
                    | BaseOp::NumericLessThanEquals
                    | BaseOp::NumericGreaterThan
                    | BaseOp::NumericGreaterThanEquals
            )
            && s.parse::<f64>().map_or(true, |n| !n.is_finite())
        {
            anyhow::bail!(
                "IAM Condition `{raw_op}` on `{raw_key}` expects finite numbers, got `{s}`"
            );
        }
        out.push(s);
    }
    Ok((out, vars))
}

impl ConditionClause {
    fn evaluate(&self, ctx: &RequestContext) -> bool {
        let actuals = ctx.get(&self.key);
        let resolved_values: Vec<String> = if self.value_vars.is_empty() {
            self.values.clone()
        } else {
            self.values
                .iter()
                .map(|v| resolve_policy_template(v, Some(ctx), false))
                .collect()
        };
        if self.op == BaseOp::Null {
            let is_null = actuals.is_empty();
            return resolved_values
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
            SetQuantifier::ForAnyValue => actuals
                .iter()
                .any(|a| self.matches_one(a, &resolved_values)),
            SetQuantifier::ForAllValues | SetQuantifier::Single => actuals
                .iter()
                .all(|a| self.matches_one(a, &resolved_values)),
        }
    }

    fn matches_one(&self, actual: &str, values: &[String]) -> bool {
        match self.op {
            BaseOp::StringEquals => values.iter().any(|p| actual == p),
            BaseOp::StringNotEquals => values.iter().all(|p| actual != p),
            BaseOp::StringEqualsIgnoreCase => values.iter().any(|p| actual.eq_ignore_ascii_case(p)),
            BaseOp::StringNotEqualsIgnoreCase => {
                values.iter().all(|p| !actual.eq_ignore_ascii_case(p))
            }
            BaseOp::StringLike => values.iter().any(|p| glob_match(p, actual)),
            BaseOp::StringNotLike => values.iter().all(|p| !glob_match(p, actual)),
            BaseOp::ArnEquals => values.iter().any(|p| actual == p),
            BaseOp::ArnNotEquals => values.iter().all(|p| actual != p),
            BaseOp::ArnLike => values.iter().any(|p| arn_covers(p, actual)),
            BaseOp::ArnNotLike => values.iter().all(|p| !arn_covers(p, actual)),
            BaseOp::Bool => {
                let Some(act_bool) = parse_bool_str(actual) else {
                    return false;
                };
                values.iter().any(|p| parse_bool_str(p) == Some(act_bool))
            }
            BaseOp::NumericEquals => Self::num_any(actual, values, |a, b| a == b),
            BaseOp::NumericNotEquals => {
                let Ok(a) = actual.parse::<f64>() else {
                    return false;
                };
                values
                    .iter()
                    .all(|p| p.parse::<f64>().is_ok_and(|b| a != b))
            }
            BaseOp::NumericLessThan => Self::num_any(actual, values, |a, b| a < b),
            BaseOp::NumericLessThanEquals => Self::num_any(actual, values, |a, b| a <= b),
            BaseOp::NumericGreaterThan => Self::num_any(actual, values, |a, b| a > b),
            BaseOp::NumericGreaterThanEquals => Self::num_any(actual, values, |a, b| a >= b),
            BaseOp::Null => unreachable!(),
        }
    }

    fn num_any(actual: &str, values: &[String], cmp: impl Fn(f64, f64) -> bool) -> bool {
        let Ok(a) = actual.parse::<f64>() else {
            return false;
        };
        values
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

/// Whether IAM ARN pattern `pattern` matches every resource `actual` can
/// stand for. A `*` in `actual` is an unknown part, so only a `*` in the
/// pattern covers it.
pub fn arn_covers(pattern: &str, actual: &str) -> bool {
    arn_compare(pattern, actual, glob)
}

/// Whether IAM ARN pattern `pattern` matches at least one resource `actual`
/// can stand for (a `*` in `actual` is an unknown part).
pub fn arn_overlaps(pattern: &str, actual: &str) -> bool {
    arn_compare(pattern, actual, globs_intersect)
}

/// Compare ARNs part by part. Lenslet normalizes region and account to `*`
/// (and the account is never known locally), so a concrete region, account or
/// partition in a production policy still matches local ARNs.
fn arn_compare(pattern: &str, actual: &str, matches: impl Fn(&str, &str) -> bool) -> bool {
    if pattern == "*" {
        return true;
    }
    if let (Some(p), Some(a)) = (split_arn(pattern), split_arn(actual)) {
        let relaxed = |p: &str, a: &str| p == "*" || a == "*" || glob(p, a);
        return relaxed(p[0], a[0])
            && matches(&p[1].to_ascii_lowercase(), &a[1].to_ascii_lowercase())
            && relaxed(p[2], a[2])
            && relaxed(p[3], a[3])
            && matches(p[4], a[4]);
    }
    matches(pattern, actual)
}

fn split_arn(arn: &str) -> Option<[&str; 5]> {
    let rest = arn.strip_prefix("arn:")?;
    let (partition, rest) = rest.split_once(':')?;
    let (service, rest) = rest.split_once(':')?;
    let (region, rest) = rest.split_once(':')?;
    let (account, resource) = rest.split_once(':')?;
    Some([partition, service, region, account, resource])
}

/// IAM wildcard matching of a resource. A `*` in `text` is an unknown run of
/// characters: only a pattern `*` absorbs it.
fn glob(pattern: &str, text: &str) -> bool {
    wildcard_match(pattern, text, true)
}

/// Standard IAM wildcard matching (`*` matches 0+ chars, `?` matches 1 char)
/// of a concrete value (an action, a condition value).
fn glob_match(pattern: &str, text: &str) -> bool {
    wildcard_match(pattern, text, false)
}

fn wildcard_match(pattern: &str, text: &str, star_is_unknown: bool) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            pi += 1;
            star_ti = ti;
        } else if pi < p.len()
            && !(star_is_unknown && t[ti] == '*')
            && (p[pi] == '?' || p[pi] == t[ti])
        {
            pi += 1;
            ti += 1;
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

/// Whether two IAM wildcard patterns match at least one common string: a
/// search over the product of their automata, O(len(a) × len(b)).
fn globs_intersect(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let width = b.len() + 1;
    let mut seen = vec![false; (a.len() + 1) * width];
    let mut stack = vec![(0usize, 0usize)];
    while let Some((i, j)) = stack.pop() {
        if std::mem::replace(&mut seen[i * width + j], true) {
            continue;
        }
        if i == a.len() && j == b.len() {
            return true;
        }
        let star_a = a.get(i) == Some(&'*');
        let star_b = b.get(j) == Some(&'*');
        // A `*` may match nothing.
        if star_a {
            stack.push((i + 1, j));
        }
        if star_b {
            stack.push((i, j + 1));
        }
        // Both consume one common character.
        if let (Some(&x), Some(&y)) = (a.get(i), b.get(j)) {
            let any_x = x == '*' || x == '?';
            let any_y = y == '*' || y == '?';
            if any_x || any_y || x == y {
                let next_i = if star_a { i } else { i + 1 };
                let next_j = if star_b { j } else { j + 1 };
                if (next_i, next_j) != (i, j) {
                    stack.push((next_i, next_j));
                }
            }
        }
    }
    false
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
            request: crate::trace::Message::default(),
            response: crate::trace::Message::default(),
            ..Default::default()
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

        // Implied `iam:PassRole` and `kms:*` receive `iam:PassedToService` and `kms:ViaService`.
        let implied_gate = Gate::from_policy_str(
            r#"{
                "Statement": [
                    {
                        "Effect": "Allow",
                        "Action": "iam:PassRole",
                        "Resource": "arn:aws:iam::*:role/fn-role",
                        "Condition": {
                            "StringEquals": { "iam:PassedToService": "lambda.amazonaws.com" }
                        }
                    },
                    {
                        "Effect": "Allow",
                        "Action": "kms:GenerateDataKey",
                        "Resource": "*",
                        "Condition": {
                            "StringEquals": { "kms:ViaService": "s3.us-east-1.amazonaws.com" }
                        }
                    }
                ]
            }"#,
            "implied.json",
        )
        .unwrap();
        let base_ctx = RequestContext::from_trace(&base_put);
        let pass_role = access("iam:PassRole", "arn:aws:iam::*:role/fn-role");
        assert!(implied_gate.allows_in_context(
            &pass_role,
            Some(&base_ctx.for_implied("lambda", Some("us-east-1"), "iam:PassRole"))
        ));
        assert!(!implied_gate.allows_in_context(
            &pass_role,
            Some(&base_ctx.for_implied("ecs", Some("us-east-1"), "iam:PassRole"))
        ));
        let kms_gdk = access("kms:GenerateDataKey", "arn:aws:kms:us-east-1:*:key/k1");
        assert!(implied_gate.allows_in_context(
            &kms_gdk,
            Some(&base_ctx.for_implied("s3", Some("us-east-1"), "kms:GenerateDataKey"))
        ));
        assert!(!implied_gate.allows_in_context(
            &kms_gdk,
            Some(&base_ctx.for_implied("sqs", Some("us-east-1"), "kms:GenerateDataKey"))
        ));

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
            admin.allows(&access("dynamodb:BatchWriteItem", "*")),
            "`Resource: *` grants a resource that could not be read"
        );
    }

    fn policy_gate(json: &str) -> Gate {
        Gate::from_policy_str(json, "policy.json").unwrap()
    }

    #[test]
    fn an_unknown_resource_fails_closed_for_deny_and_open_only_for_allow_star() {
        let gate = policy_gate(
            r#"{"Statement":[
                {"Effect":"Allow","Action":"dynamodb:*","Resource":"*"},
                {"Sid":"KeepAudit","Effect":"Deny","Action":"dynamodb:DeleteItem","Resource":"arn:aws:dynamodb:*:*:table/audit"}
            ]}"#,
        );
        // The table could be `audit`: the Deny applies.
        let unknown = access("dynamodb:DeleteItem", "*");
        assert_eq!(
            gate.decide(&unknown, None),
            Decision::explicit_deny("KeepAudit".into())
        );
        // A name that is partly unknown (`orders-*`) cannot be `audit`.
        assert!(gate.allows(&access(
            "dynamodb:DeleteItem",
            "arn:aws:dynamodb:us-east-1:*:table/orders-*"
        )));
        assert!(!gate.allows(&access(
            "dynamodb:DeleteItem",
            "arn:aws:dynamodb:us-east-1:*:table/aud*"
        )));

        // An Allow on one table does not grant an unknown one; it must cover
        // every name the `*` could be.
        let scoped = policy_gate(
            r#"{"Statement":[{"Effect":"Allow","Action":"dynamodb:GetItem","Resource":[
                "arn:aws:dynamodb:*:*:table/orders",
                "arn:aws:dynamodb:*:*:table/carts-*"
            ]}]}"#,
        );
        assert!(!scoped.allows(&access("dynamodb:GetItem", "*")));
        assert!(!scoped.allows(&access(
            "dynamodb:GetItem",
            "arn:aws:dynamodb:*:*:table/orders*"
        )));
        assert!(scoped.allows(&access(
            "dynamodb:GetItem",
            "arn:aws:dynamodb:*:*:table/carts-*"
        )));
        assert!(!scoped.allows(&access(
            "dynamodb:GetItem",
            "arn:aws:dynamodb:*:*:table/cart*"
        )));

        // NotResource: an Allow must exclude every possibility, a Deny applies
        // unless every possibility is excluded.
        let not_resource = policy_gate(
            r#"{"Statement":[
                {"Effect":"Allow","Action":"s3:GetObject","NotResource":"arn:aws:s3:::vault/*"},
                {"Effect":"Deny","Action":"s3:GetObject","NotResource":"arn:aws:s3:::public/*"}
            ]}"#,
        );
        assert_eq!(
            not_resource.decide(&access("s3:GetObject", "*"), None),
            Decision::explicit_deny("Statement[1]".into())
        );
        assert!(not_resource.allows(&access("s3:GetObject", "arn:aws:s3:::public/a")));
        assert_eq!(
            not_resource
                .decide(&access("s3:PutObject", "arn:aws:s3:::public/a"), None)
                .reason,
            Reason::NoAllow
        );
    }

    #[test]
    fn wildcard_patterns_compare_as_sets() {
        assert!(arn_overlaps("table/a*", "table/*b"));
        assert!(!arn_overlaps("table/a?", "table/abc*"));
        assert!(arn_overlaps("table/a?", "table/a*"));
        assert!(!arn_covers("table/a?", "table/a*"));
        assert!(arn_covers("table/*", "table/a*"));
        assert!(arn_covers("*a", "*ba"));
        assert!(!arn_covers("*b", "*"));
        // Concrete values still match the way IAM matches them.
        assert!(arn_covers("arn:aws:s3:::b/?", "arn:aws:s3:::b/x"));
        assert!(glob_match("s3:Get*", "s3:GetObject"));
    }

    #[test]
    fn a_lock_grants_exactly_its_lines() {
        let gate = Gate::from_lock(
            "lenslet.lock",
            [
                access("dynamodb:PutItem", "arn:aws:dynamodb:*:*:table/orders-*"),
                access("sqs:SendMessage", "*"),
            ]
            .into(),
        );
        assert!(gate.allows(&access(
            "dynamodb:PutItem",
            "arn:aws:dynamodb:*:*:table/orders-*"
        )));
        // A generated-name `*` is not a pattern over other tables.
        assert!(!gate.allows(&access(
            "dynamodb:PutItem",
            "arn:aws:dynamodb:*:*:table/orders-archive"
        )));
        // An unreadable resource is accepted by its own `*` line only.
        assert!(gate.allows(&access("sqs:SendMessage", "*")));
        assert!(!gate.allows(&access("sqs:SendMessage", "arn:aws:sqs:*:*:jobs")));
        assert_eq!(
            gate.decide(&access("sqs:DeleteQueue", "*"), None),
            Decision::no_allow()
        );
    }

    fn temp_lock(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lenslet-lock-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join(FILE_NAME)
    }

    #[test]
    fn accept_and_test_marks_rewrite_atomically_in_the_canonical_format() {
        let path = temp_lock("accept");
        // A new lock gets the default header.
        let orders = access("dynamodb:PutItem", "arn:aws:dynamodb:*:*:table/orders");
        assert_eq!(accept(&path, &[(orders.clone(), false)]).unwrap(), 1);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(DEFAULT_HEADER), "{text}");
        assert!(text.ends_with("dynamodb:PutItem  arn:aws:dynamodb:*:*:table/orders\n"));

        // The user's own header comments, `test` and `implied` notes survive;
        // other comments are not kept.
        fs::write(
            &path,
            "# Owned by the payments team.\n\
             # Review with #infra.\n\
             \n\
             kms:Decrypt  arn:aws:kms:*:*:key/*  # implied\n\
             # stray note\n\
             dynamodb:PutItem  arn:aws:dynamodb:*:*:table/orders  # why: checkout\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        }
        let table = access("dynamodb:CreateTable", "arn:aws:dynamodb:*:*:table/orders");
        assert_eq!(
            accept(&path, &[(table.clone(), true), (orders.clone(), true)]).unwrap(),
            1,
            "a line already in the lock is left as it is"
        );
        let expected = "# Owned by the payments team.\n\
             # Review with #infra.\n\
             dynamodb:CreateTable  arn:aws:dynamodb:*:*:table/orders  # test\n\
             dynamodb:PutItem      arn:aws:dynamodb:*:*:table/orders\n\
             kms:Decrypt           arn:aws:kms:*:*:key/*  # implied\n";
        assert_eq!(fs::read_to_string(&path).unwrap(), expected);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o640, "the rewrite keeps the file's permissions");
        }
        assert_eq!(accept(&path, &[(orders.clone(), false)]).unwrap(), 0);

        assert!(set_test_mark(&path, &orders, true).unwrap());
        assert!(
            !set_test_mark(&path, &orders, true).unwrap(),
            "already marked"
        );
        assert!(set_test_mark(&path, &table, false).unwrap());
        assert!(!set_test_mark(&path, &access("s3:GetObject", "*"), true).unwrap());
        let lock = read(&path).unwrap().unwrap();
        assert_eq!(lock.test, [orders.clone()].into());
        assert_eq!(lock.header.len(), 2);

        // `--update` rewrites the entries but keeps the header.
        let mut used = BTreeMap::new();
        used.insert(orders.clone(), AccessUse::default());
        write(&path, &used, &lock.test).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# Owned by the payments team.\n\
             # Review with #infra.\n\
             dynamodb:PutItem  arn:aws:dynamodb:*:*:table/orders  # test\n"
        );
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name() != FILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "no temporary files are left behind");
        assert!(!set_test_mark(&temp_lock("missing"), &orders, true).unwrap());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn evaluation_marks_new_footprints_and_names_the_deciding_statement() {
        let trace = crate::trace::Trace {
            id: "1".into(),
            method: "POST".into(),
            path: "/".into(),
            service: "dynamodb".into(),
            operation: "DeleteItem".into(),
            region: Some("us-east-1".into()),
            request: crate::trace::Message {
                headers: Vec::new(),
                content_type: Some("application/x-amz-json-1.0".into()),
                body: Some(crate::trace::BodyTrace {
                    text: Some(r#"{"TableName":"audit-20261119","Key":{}}"#.into()),
                    bytes: 40,
                    ..Default::default()
                }),
            },
            ..Default::default()
        };
        let gate = policy_gate(
            r#"{"Statement":[
                {"Sid":"All","Effect":"Allow","Action":"dynamodb:*","Resource":"*"},
                {"Sid":"KeepAudit","Effect":"Deny","Action":"dynamodb:DeleteItem","Resource":"arn:aws:dynamodb:*:*:table/audit-*"}
            ]}"#,
        );
        let evaluation = evaluate(&trace, Some(&gate));
        assert_eq!(
            evaluation.denied,
            vec![Denial {
                action: "dynamodb:DeleteItem".into(),
                resource: "arn:aws:dynamodb:us-east-1:*:table/audit-20261119".into(),
                implied: false,
                unverified: false,
                decision: Decision::explicit_deny("KeepAudit".into()),
            }]
        );
        assert_eq!(
            evaluation.access,
            vec![CallAccess {
                action: "dynamodb:DeleteItem".into(),
                resource: "arn:aws:dynamodb:*:*:table/audit-*".into(),
                new: true,
                unverified: false,
            }]
        );
        // Without a gate nothing is denied; a lock compares footprints.
        assert!(evaluate(&trace, None).denied.is_empty());
        let lock = Gate::from_lock(
            "lenslet.lock",
            [access(
                "dynamodb:DeleteItem",
                "arn:aws:dynamodb:*:*:table/audit-*",
            )]
            .into(),
        );
        let evaluation = evaluate(&trace, Some(&lock));
        assert!(evaluation.denied.is_empty() && !evaluation.access[0].new);
    }

    #[test]
    fn wire_abac_tags_leading_keys_encryption_context_and_simulation_context() {
        let abac_policy = r#"{
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Sid": "TaggedS3",
                    "Effect": "Allow",
                    "Action": "s3:PutObject",
                    "Resource": "arn:aws:s3:::assets/*",
                    "Condition": {
                        "StringEquals": { "aws:RequestTag/env": "prod" },
                        "ForAllValues:StringEquals": { "aws:TagKeys": ["env", "team"] }
                    }
                },
                {
                    "Sid": "TenantRows",
                    "Effect": "Allow",
                    "Action": "dynamodb:GetItem",
                    "Resource": "arn:aws:dynamodb:*:*:table/orders",
                    "Condition": {
                        "ForAllValues:StringEquals": { "dynamodb:LeadingKeys": ["tenant#acme"] }
                    }
                },
                {
                    "Sid": "ScopedKms",
                    "Effect": "Allow",
                    "Action": "kms:Decrypt",
                    "Resource": "arn:aws:kms:*:*:key/*",
                    "Condition": {
                        "StringEquals": { "kms:EncryptionContext:tenant": "acme" }
                    }
                }
            ]
        }"#;
        let gate = Gate::from_policy_str(abac_policy, "abac.json").unwrap();

        let mut s3_put = crate::trace::Trace {
            method: "PUT".into(),
            path: "/assets/r.json?x-id=PutObject".into(),
            service: "s3".into(),
            operation: "PutObject".into(),
            request: crate::trace::Message {
                headers: vec![crate::trace::HeaderTrace {
                    name: "x-amz-tagging".into(),
                    value: "env=prod&team=payments".into(),
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        let put_acc = access("s3:PutObject", "arn:aws:s3:::assets/r.json");
        assert!(gate.allows_in_context(&put_acc, Some(&RequestContext::from_trace(&s3_put))));
        s3_put.request.headers[0].value = "env=prod&rogue=1".into();
        assert!(!gate.allows_in_context(&put_acc, Some(&RequestContext::from_trace(&s3_put))));

        let ddb_get = crate::trace::Trace {
            method: "POST".into(),
            path: "/".into(),
            service: "dynamodb".into(),
            operation: "GetItem".into(),
            request: crate::trace::Message {
                body: Some(crate::trace::BodyTrace {
                    text: Some(r#"{"TableName":"orders","Key":{"pk":{"S":"tenant#acme"}}}"#.into()),
                    bytes: 58,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(evaluate(&ddb_get, Some(&gate)).denied.is_empty());

        let kms_dec = crate::trace::Trace {
            method: "POST".into(),
            path: "/".into(),
            service: "kms".into(),
            operation: "Decrypt".into(),
            request: crate::trace::Message {
                body: Some(crate::trace::BodyTrace {
                    text: Some(r#"{"KeyId":"k1","EncryptionContext":{"tenant":"acme"}}"#.into()),
                    bytes: 52,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(evaluate(&kms_dec, Some(&gate)).denied.is_empty());

        // Simulation mode parses enterprise cloud keys and `${...}` variables,
        // returning NeedsContext until `--context` supplies them.
        let enterprise = r#"{
            "Statement": [{
                "Sid": "HomeFolderInOrg",
                "Effect": "Allow",
                "Action": "s3:GetObject",
                "Resource": "arn:aws:s3:::corp-home/${aws:username}/*",
                "Condition": {
                    "StringEquals": { "aws:PrincipalOrgID": "o-999" }
                }
            }]
        }"#;
        let sim_gate = Gate::from_policy_str_simulating(enterprise, "corp.json").unwrap();
        let alice_get = crate::trace::Trace {
            method: "GET".into(),
            path: "/corp-home/alice/notes.txt?x-id=GetObject".into(),
            service: "s3".into(),
            operation: "GetObject".into(),
            ..Default::default()
        };
        let eval_missing = evaluate(&alice_get, Some(&sim_gate));
        assert_eq!(eval_missing.denied.len(), 1);
        assert_eq!(eval_missing.denied[0].decision.reason, Reason::NeedsContext);
        assert_eq!(
            eval_missing.denied[0].decision.missing_context,
            vec!["aws:username".to_string(), "aws:PrincipalOrgID".to_string()]
        );

        let mut overrides = BTreeMap::new();
        overrides.insert("aws:username".into(), vec!["alice".into()]);
        overrides.insert("aws:PrincipalOrgID".into(), vec!["o-999".into()]);
        assert!(
            evaluate_with_context(&alice_get, Some(&sim_gate), &overrides)
                .denied
                .is_empty()
        );

        overrides.insert("aws:username".into(), vec!["bob".into()]);
        let eval_bob = evaluate_with_context(&alice_get, Some(&sim_gate), &overrides);
        assert_eq!(eval_bob.denied[0].decision.reason, Reason::NoAllow);
    }
}
