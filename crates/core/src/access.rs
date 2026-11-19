//! What a call needs from IAM: the action, the resource, and anything implied.
//!
//! The action is `{iam prefix}:{operation}`, which is right for almost every
//! AWS API; the exceptions (mostly S3) are listed. The resource ARN is read
//! from the request for the services local code uses most; anything else is
//! `*`. Account and region are always `*`, so the result is portable between
//! the emulator and real accounts. A call whose operation is only an HTTP
//! method has no action: Lenslet does not guess.
//!
//! This is *observed* access: what the run exercised, not a security review.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{trace::Trace, wire};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Access {
    pub action: String,
    pub resource: String,
}

impl Access {
    pub fn new(action: impl Into<String>, resource: impl Into<String>) -> Self {
        Access {
            action: action.into(),
            resource: resource.into(),
        }
    }
}

/// One permission recorded on a `call` trace line, carrying whether that
/// specific permission is outside the lock file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallAccess {
    pub action: String,
    pub resource: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub new: bool,
    /// The resource could not be read because the request body was not
    /// readable (binary or truncated): `resource` is `*`, standing for it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unverified: bool,
}

impl CallAccess {
    pub fn key(&self) -> Access {
        Access {
            action: self.action.clone(),
            resource: self.resource.clone(),
        }
    }
}

impl From<Access> for CallAccess {
    fn from(access: Access) -> Self {
        CallAccess {
            action: access.action,
            resource: access.resource,
            new: false,
            unverified: false,
        }
    }
}

/// Whether deriving IAM access for `service` requires inspecting the request
/// body (as opposed to REST-URI services whose operation and resource live in
/// the path and headers).
pub fn needs_request_body(
    service: &str,
    advertised_operation: Option<&str>,
    resolved_operation: &str,
    path: &str,
) -> bool {
    match service {
        // S3 DeleteObjects (`POST /bucket?delete`) names its `<Key>` targets in XML.
        "s3" => resolved_operation == "DeleteObjects",
        "s3-object-lambda" | "execute-api" | "apigateway" | "appconfig" | "appconfigdata" => false,
        "bedrock" | "bedrock-runtime" => bedrock_model(path).is_none(),
        // Lambda CreateFunction / UpdateFunctionConfiguration may pass a Role in JSON,
        // whereas Invoke / GetFunction / DeleteFunction only use the URL path.
        "lambda" => matches!(
            advertised_operation.or(Some(resolved_operation)),
            None | Some("CreateFunction" | "UpdateFunctionConfiguration")
        ),
        _ => true,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Normalized footprint ARNs for `lenslet.lock` and summaries (`*` for
    /// generated IDs, region, account, and S3 object keys).
    Footprint,
    /// Concrete request ARNs for IAM `--policy` evaluation (preserving exact
    /// S3 object keys, DeleteObjects `<Key>` elements, signing region, and
    /// un-normalized resource names).
    Concrete,
}

/// Whether a finished call had no recognizable IAM mapping (excluding calls
/// like `sts:GetCallerIdentity` that genuinely need no permission).
pub fn is_unmapped(trace: &Trace) -> bool {
    !trace.pending
        && trace.access.is_empty()
        && trace.implied.is_empty()
        && !(trace.service == "sts" && trace.operation == "GetCallerIdentity")
}

/// Everything one call needs from IAM, derived once from its request.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Derived {
    /// Normalized footprint: what `lenslet.lock` and reports record.
    pub own: Vec<Access>,
    pub implied: Vec<Access>,
    /// Exact request ARNs: what an IAM policy is evaluated against.
    pub concrete_own: Vec<Access>,
    pub concrete_implied: Vec<Access>,
    /// Entries (footprint and concrete alike) whose resource stands as `*`
    /// because the request body that names it could not be read.
    pub unverified: Vec<Access>,
}

/// The normalized footprint access one call needs (for `lenslet.lock` and reports).
pub fn derive(trace: &Trace) -> (Vec<Access>, Vec<Access>) {
    let derived = derive_call(trace);
    (derived.own, derived.implied)
}

/// Footprint and concrete access for one call, parsing its body once.
///
/// When the body that names the resource cannot be read (binary such as
/// CBOR, or truncated), the call still gets its action, on resource `*`, and
/// the entry is listed in [`Derived::unverified`]. A `*` is stable across
/// runs, so it can be accepted into the lock; a policy grants it only with
/// `Resource: "*"`, while any `Deny` on the action applies to it.
pub fn derive_call(trace: &Trace) -> Derived {
    if !mapped_operation(trace) {
        return Derived::default();
    }
    let body = Body::parse(trace);
    let (own, implied, unverified) = derive_inner(trace, Mode::Footprint, &body);
    let (concrete_own, concrete_implied, _) = derive_inner(trace, Mode::Concrete, &body);
    Derived {
        own,
        implied,
        concrete_own,
        concrete_implied,
        unverified,
    }
}

/// An operation Lenslet can name (not just an HTTP method).
fn mapped_operation(trace: &Trace) -> bool {
    let operation = trace.operation.as_str();
    !(operation.is_empty()
        || operation == trace.method
        || trace.service == "aws"
        || !operation.bytes().all(|byte| byte.is_ascii_alphanumeric()))
}

/// The request body was captured but cannot be read in full.
fn body_unreadable(trace: &Trace) -> bool {
    trace
        .request
        .body
        .as_ref()
        .is_some_and(|body| body.truncated || body.binary || body.text.is_none())
}

/// Whether this call's resource (or a role it passes) is named in its body.
fn resource_in_body(prefix: &str, operation: &str, trace: &Trace) -> bool {
    match prefix {
        "sqs" | "sns" | "secretsmanager" | "ssm" | "logs" | "kinesis" | "kms" | "sts" | "iam"
        | "events" | "states" | "cognito-idp" | "ecr" | "firehose" | "scheduler" | "rds-data"
        | "dynamodb" | "pipes" | "glue" => true,
        "lambda" => {
            lambda_function(&trace.path).is_none()
                || matches!(operation, "CreateFunction" | "UpdateFunctionConfiguration")
        }
        "bedrock" => bedrock_model(&trace.path).is_none(),
        _ => false,
    }
}

/// Operations that always pass a role: a role hidden in an unreadable body
/// still needs `iam:PassRole`.
fn always_passes_role(prefix: &str, operation: &str) -> bool {
    matches!(
        (prefix, operation),
        ("lambda", "CreateFunction")
            | ("states", "CreateStateMachine")
            | ("pipes", "CreatePipe")
            | ("scheduler", "CreateSchedule")
            | ("glue", "CreateJob")
    )
}

/// Own permissions, implied permissions, and the `*` stand-ins among them.
type Derivation = (Vec<Access>, Vec<Access>, Vec<Access>);

fn derive_inner(trace: &Trace, mode: Mode, body: &Body) -> Derivation {
    let operation = trace.operation.as_str();
    let prefix = iam_prefix(&trace.service);
    let unreadable = body_unreadable(trace) && resource_in_body(prefix, operation, trace);
    let mut implied = implied(mode, prefix, operation, trace, body);
    let mut unverified = Vec::new();
    if unreadable && always_passes_role(prefix, operation) {
        let fallback = Access::new("iam:PassRole", "*");
        if !implied.contains(&fallback) {
            implied.push(fallback.clone());
        }
        unverified.push(fallback);
    }
    let mut own = if prefix == "dynamodb" && operation.starts_with("Transact") {
        transaction(mode, trace, body)
    } else if prefix == "dynamodb" && is_partiql(operation) {
        partiql(mode, trace, body)
    } else {
        let actions = action_names(prefix, operation, trace);
        let resources = resources(mode, prefix, operation, trace, body);
        let mut own: Vec<Access> = actions
            .iter()
            .flat_map(|action| {
                resources
                    .iter()
                    .map(move |resource| Access::new(action.clone(), resource.clone()))
            })
            .collect();
        if prefix == "s3"
            && let Some(source) = copy_source(mode, trace)
        {
            own.push(Access::new(source_read_action(trace), source));
        }
        own
    };
    if unreadable {
        for action in unreadable_actions(prefix, operation, trace) {
            let fallback = Access::new(action, "*");
            if !own.contains(&fallback) {
                own.push(fallback.clone());
            }
            unverified.push(fallback);
        }
    }
    (own, implied, unverified)
}

/// The actions whose resources an unreadable body hides.
fn unreadable_actions(prefix: &str, operation: &str, trace: &Trace) -> Vec<String> {
    match (prefix, operation) {
        ("dynamodb", "TransactWriteItems") => {
            ["PutItem", "UpdateItem", "DeleteItem", "ConditionCheckItem"]
                .iter()
                .map(|action| format!("dynamodb:{action}"))
                .collect()
        }
        ("dynamodb", "TransactGetItems") => vec!["dynamodb:GetItem".to_string()],
        // The statement, and so the action, is in the unreadable body too.
        ("dynamodb", op) if is_partiql(op) => Vec::new(),
        _ => action_names(prefix, operation, trace),
    }
}

/// `TransactWriteItems` / `TransactGetItems` are not IAM actions: each item
/// needs its own (`PutItem`, `UpdateItem`, `DeleteItem`, `ConditionCheckItem`,
/// `GetItem`) on its table. A body that cannot be read yields nothing rather
/// than a guess.
fn transaction(mode: Mode, trace: &Trace, body: &Body) -> Vec<Access> {
    let mut out: Vec<Access> = match body {
        Body::Json(value) => {
            let Some(items) = value.get("TransactItems").and_then(Value::as_array) else {
                return Vec::new();
            };
            items
                .iter()
                .filter_map(Value::as_object)
                .flat_map(|item| item.iter())
                .filter_map(|(kind, request)| {
                    let action = transact_action(kind)?;
                    let table = request.get("TableName")?.as_str()?;
                    Some(Access::new(
                        format!("dynamodb:{action}"),
                        arn_for(
                            mode,
                            trace,
                            "dynamodb",
                            &format!("table/{}", norm(mode, table)),
                        ),
                    ))
                })
                .collect()
        }
        Body::TruncatedJson(raw) => scan_truncated_transact(mode, trace, raw),
        _ => Vec::new(),
    };
    out.sort();
    out.dedup();
    out
}

fn transact_action(kind: &str) -> Option<&'static str> {
    match kind {
        "Put" => Some("PutItem"),
        "Update" => Some("UpdateItem"),
        "Delete" => Some("DeleteItem"),
        "ConditionCheck" => Some("ConditionCheckItem"),
        "Get" => Some("GetItem"),
        _ => None,
    }
}

fn scan_truncated_transact(mode: Mode, trace: &Trace, raw: &str) -> Vec<Access> {
    let mut out = Vec::new();
    for (kind, action) in [
        ("Put", "PutItem"),
        ("Update", "UpdateItem"),
        ("Delete", "DeleteItem"),
        ("ConditionCheck", "ConditionCheckItem"),
        ("Get", "GetItem"),
    ] {
        let needle = format!("\"{kind}\"");
        let mut rest = raw;
        while let Some(pos) = rest.find(&needle) {
            rest = &rest[pos + needle.len()..];
            let after = rest.trim_start();
            let Some(after_colon) = after.strip_prefix(':') else {
                continue;
            };
            let Some(obj) = after_colon.trim_start().strip_prefix('{') else {
                continue;
            };
            // Scan within this item's leading fields up to the closing `}` at depth 0.
            let mut end = obj.len();
            let mut depth = 0i32;
            let mut in_str = false;
            let mut escape = false;
            for (idx, ch) in obj.char_indices() {
                if escape {
                    escape = false;
                    continue;
                }
                if in_str {
                    if ch == '\\' {
                        escape = true;
                    } else if ch == '"' {
                        in_str = false;
                    }
                    continue;
                }
                match ch {
                    '"' => in_str = true,
                    '{' | '[' => depth += 1,
                    '}' | ']' => {
                        if depth == 0 {
                            end = idx;
                            break;
                        }
                        depth -= 1;
                    }
                    _ => {}
                }
            }
            if let Some(table) = scan_json_string_values(&obj[..end], "TableName")
                .into_iter()
                .next()
            {
                out.push(Access::new(
                    format!("dynamodb:{action}"),
                    arn_for(
                        mode,
                        trace,
                        "dynamodb",
                        &format!("table/{}", norm(mode, &table)),
                    ),
                ));
            }
        }
    }
    out
}

/// The bucket (and in Concrete mode, key) a `CopyObject` / `UploadPartCopy` reads from.
fn copy_source(mode: Mode, trace: &Trace) -> Option<String> {
    let value = trace
        .request
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("x-amz-copy-source"))?
        .value
        .as_str();
    let raw_path = value.split('?').next().unwrap_or(value);
    if raw_path.starts_with("arn:") {
        let decoded_arn = wire::percent_decode_path(raw_path);
        return Some(match mode {
            Mode::Footprint => "*".to_string(),
            Mode::Concrete => decoded_arn,
        });
    }
    let trimmed = raw_path.strip_prefix('/').unwrap_or(raw_path);
    let (raw_bucket, raw_key) = trimmed.split_once('/').unwrap_or((trimmed, ""));
    let bucket = wire::percent_decode_path(raw_bucket);
    let key = wire::percent_decode_path(raw_key);
    if bucket.is_empty() {
        return None;
    }
    Some(match mode {
        Mode::Footprint => format!("arn:aws:s3:::{}/*", normalize(&bucket)),
        Mode::Concrete => {
            if key.is_empty() {
                format!("arn:aws:s3:::{bucket}/*")
            } else {
                format!("arn:aws:s3:::{bucket}/{key}")
            }
        }
    })
}

/// SigV4 signing names that differ from the IAM action prefix.
fn iam_prefix(service: &str) -> &str {
    match service {
        "monitoring" => "cloudwatch",
        "email" | "sesv2" => "ses",
        "bedrock-runtime" => "bedrock",
        "streams.dynamodb" | "dynamodbstreams" => "dynamodb",
        "appconfigdata" => "appconfig",
        "cognito-idp" => "cognito-idp",
        "rds-data" => "rds-data",
        "logs" => "logs",
        "execute-api" => "execute-api",
        "s3-object-lambda" => "s3-object-lambda",
        other => other,
    }
}

/// The IAM actions for an operation: usually one; none when the call needs no
/// permission.
fn action_names(prefix: &str, operation: &str, trace: &Trace) -> Vec<String> {
    if prefix == "s3" && wire::query_param(&trace.path, "versionId").is_some() {
        // A request for one object version needs the `*Version*` action.
        let versioned: &[&str] = match operation {
            "GetObject" | "HeadObject" => &["GetObjectVersion"],
            "DeleteObject" => &["DeleteObjectVersion"],
            "GetObjectTagging" => &["GetObjectVersionTagging"],
            "PutObjectTagging" => &["PutObjectVersionTagging"],
            "DeleteObjectTagging" => &["DeleteObjectVersionTagging"],
            "GetObjectAcl" => &["GetObjectVersionAcl"],
            "PutObjectAcl" => &["PutObjectVersionAcl"],
            "GetObjectAttributes" => &["GetObjectVersion", "GetObjectVersionAttributes"],
            _ => &[],
        };
        if !versioned.is_empty() {
            return versioned.iter().map(|name| format!("s3:{name}")).collect();
        }
    }
    let mapped = match (prefix, operation) {
        ("sts", "GetCallerIdentity") => return Vec::new(),
        ("s3", "GetObjectAttributes") => {
            return vec!["s3:GetObject".into(), "s3:GetObjectAttributes".into()];
        }
        ("s3", "ListObjects" | "ListObjectsV2" | "HeadBucket") => "ListBucket",
        ("s3", "ListBuckets") => "ListAllMyBuckets",
        ("s3", "HeadObject" | "SelectObjectContent") => "GetObject",
        (
            "s3",
            "PostObject"
            | "CopyObject"
            | "UploadPart"
            | "UploadPartCopy"
            | "CreateMultipartUpload"
            | "CompleteMultipartUpload",
        ) => "PutObject",
        ("s3", "DeleteObjects") => "DeleteObject",
        ("s3", "ListObjectVersions") => "ListBucketVersions",
        ("s3", "ListMultipartUploads") => "ListBucketMultipartUploads",
        ("s3", "ListParts") => "ListMultipartUploadParts",
        ("s3", "GetBucketCors") => "GetBucketCORS",
        ("s3", "PutBucketCors" | "DeleteBucketCors") => "PutBucketCORS",
        ("s3", "GetBucketLifecycleConfiguration" | "GetBucketLifecycle") => {
            "GetLifecycleConfiguration"
        }
        (
            "s3",
            "PutBucketLifecycleConfiguration"
            | "PutBucketLifecycle"
            | "DeleteBucketLifecycleConfiguration"
            | "DeleteBucketLifecycle",
        ) => "PutLifecycleConfiguration",
        ("s3", "GetBucketEncryption") => "GetEncryptionConfiguration",
        ("s3", "PutBucketEncryption" | "DeleteBucketEncryption") => "PutEncryptionConfiguration",
        ("s3", "GetBucketNotificationConfiguration") => "GetBucketNotification",
        ("s3", "PutBucketNotificationConfiguration") => "PutBucketNotification",
        ("s3", "GetBucketReplication") => "GetReplicationConfiguration",
        ("s3", "PutBucketReplication" | "DeleteBucketReplication") => "PutReplicationConfiguration",
        ("s3", "GetPublicAccessBlock") => "GetBucketPublicAccessBlock",
        ("s3", "PutPublicAccessBlock" | "DeletePublicAccessBlock") => "PutBucketPublicAccessBlock",
        ("s3", "GetObjectLockConfiguration") => "GetBucketObjectLockConfiguration",
        ("s3", "PutObjectLockConfiguration") => "PutBucketObjectLockConfiguration",
        ("s3", "GetBucketAccelerateConfiguration") => "GetAccelerateConfiguration",
        ("s3", "PutBucketAccelerateConfiguration") => "PutAccelerateConfiguration",
        ("s3", "GetBucketAnalyticsConfiguration" | "ListBucketAnalyticsConfigurations") => {
            "GetAnalyticsConfiguration"
        }
        ("s3", "PutBucketAnalyticsConfiguration" | "DeleteBucketAnalyticsConfiguration") => {
            "PutAnalyticsConfiguration"
        }
        ("s3", "GetBucketInventoryConfiguration" | "ListBucketInventoryConfigurations") => {
            "GetInventoryConfiguration"
        }
        ("s3", "PutBucketInventoryConfiguration" | "DeleteBucketInventoryConfiguration") => {
            "PutInventoryConfiguration"
        }
        ("s3", "GetBucketMetricsConfiguration" | "ListBucketMetricsConfigurations") => {
            "GetMetricsConfiguration"
        }
        ("s3", "PutBucketMetricsConfiguration" | "DeleteBucketMetricsConfiguration") => {
            "PutMetricsConfiguration"
        }
        (
            "s3",
            "GetBucketIntelligentTieringConfiguration"
            | "ListBucketIntelligentTieringConfigurations",
        ) => "GetIntelligentTieringConfiguration",
        (
            "s3",
            "PutBucketIntelligentTieringConfiguration"
            | "DeleteBucketIntelligentTieringConfiguration",
        ) => "PutIntelligentTieringConfiguration",
        ("s3", "DeleteBucketTagging") => "PutBucketTagging",
        ("s3", "DeleteBucketOwnershipControls") => "PutBucketOwnershipControls",
        // Batch APIs are authorized as the single-entry action.
        ("sqs", "SendMessageBatch") => "SendMessage",
        ("sqs", "DeleteMessageBatch") => "DeleteMessage",
        ("sqs", "ChangeMessageVisibilityBatch") => "ChangeMessageVisibility",
        ("sns", "PublishBatch") => "Publish",
        ("lambda", "Invoke" | "InvokeAsync" | "InvokeWithResponseStream") => "InvokeFunction",
        ("bedrock", "Converse") => "InvokeModel",
        ("bedrock", "ConverseStream") => "InvokeModelWithResponseStream",
        _ => operation,
    };
    vec![format!("{prefix}:{mapped}")]
}

/// The read a copy needs on its source: a specific version needs
/// `s3:GetObjectVersion`.
fn source_read_action(trace: &Trace) -> &'static str {
    let versioned = trace
        .request
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("x-amz-copy-source"))
        .and_then(|header| header.value.split_once('?'))
        .is_some_and(|(_, query)| {
            query
                .split('&')
                .any(|pair| pair.split('=').next() == Some("versionId"))
        });
    if versioned {
        "s3:GetObjectVersion"
    } else {
        "s3:GetObject"
    }
}

/// DynamoDB PartiQL operations: not IAM actions themselves.
fn is_partiql(operation: &str) -> bool {
    matches!(
        operation,
        "ExecuteStatement" | "BatchExecuteStatement" | "ExecuteTransaction"
    )
}

/// PartiQL is authorized per statement verb (`dynamodb:PartiQLSelect`,
/// `PartiQLInsert`, `PartiQLUpdate`, `PartiQLDelete`) on the table it names.
fn partiql(mode: Mode, trace: &Trace, body: &Body) -> Vec<Access> {
    let Body::Json(value) = body else {
        return Vec::new();
    };
    let statements: Vec<&str> = match trace.operation.as_str() {
        "ExecuteStatement" => value
            .get("Statement")
            .and_then(Value::as_str)
            .into_iter()
            .collect(),
        list => {
            let key = if list == "BatchExecuteStatement" {
                "Statements"
            } else {
                "TransactStatements"
            };
            value
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|item| item.get("Statement").and_then(Value::as_str))
                .collect()
        }
    };
    let mut out: Vec<Access> = statements
        .into_iter()
        .filter_map(|statement| {
            let (action, target) = partiql_statement(statement)?;
            let resource = match target {
                Some((table, index)) => {
                    let table = norm(mode, &table);
                    let rest = match index {
                        Some(index) => format!("table/{table}/index/{}", norm(mode, &index)),
                        None => format!("table/{table}"),
                    };
                    arn_for(mode, trace, "dynamodb", &rest)
                }
                None => "*".to_string(),
            };
            Some(Access::new(format!("dynamodb:{action}"), resource))
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The IAM action and target (table, index) of one PartiQL statement.
#[allow(clippy::type_complexity)]
fn partiql_statement(statement: &str) -> Option<(&'static str, Option<(String, Option<String>)>)> {
    let mut words = statement.split_whitespace();
    let verb = words.next()?.to_ascii_uppercase();
    let (action, keyword) = match verb.as_str() {
        "SELECT" => ("PartiQLSelect", Some("FROM")),
        "INSERT" => ("PartiQLInsert", Some("INTO")),
        "UPDATE" => ("PartiQLUpdate", None),
        "DELETE" => ("PartiQLDelete", Some("FROM")),
        _ => return None,
    };
    let target_start = match keyword {
        None => statement.trim_start()[verb.len()..].trim_start(),
        Some(keyword) => after_keyword(statement, keyword)?,
    };
    Some((action, partiql_target(target_start)))
}

/// The text after the first whitespace-delimited `keyword` (ASCII, any case).
fn after_keyword<'a>(statement: &'a str, keyword: &str) -> Option<&'a str> {
    let upper = statement.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    let mut from = 0;
    while let Some(found) = upper[from..].find(keyword) {
        let at = from + found;
        let end = at + keyword.len();
        if at > 0
            && bytes[at - 1].is_ascii_whitespace()
            && bytes.get(end).is_some_and(u8::is_ascii_whitespace)
        {
            return Some(statement[end..].trim_start());
        }
        from = end;
    }
    None
}

/// `"orders"."byUser"` or `orders` at the start of `text`.
fn partiql_target(text: &str) -> Option<(String, Option<String>)> {
    fn ident(text: &str) -> Option<(String, &str)> {
        if let Some(rest) = text.strip_prefix('"') {
            let end = rest.find('"')?;
            Some((rest[..end].to_string(), &rest[end + 1..]))
        } else {
            let end = text
                .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
                .unwrap_or(text.len());
            (end > 0).then(|| (text[..end].to_string(), &text[end..]))
        }
    }
    let (table, rest) = ident(text)?;
    let index = rest
        .strip_prefix('.')
        .and_then(ident)
        .map(|(index, _)| index);
    Some((table, index))
}

/// The request body, parsed once: JSON fields, truncated JSON, or form fields.
enum Body {
    Json(Value),
    TruncatedJson(String),
    Form(Vec<(String, String)>),
    None,
}

impl Body {
    fn parse(trace: &Trace) -> Body {
        let Some(body_trace) = trace.request.body.as_ref() else {
            return Body::None;
        };
        let Some(text) = body_trace.text.as_deref() else {
            return Body::None;
        };
        let trimmed = text.trim_start();
        if trimmed.starts_with('{') {
            return match serde_json::from_str(text) {
                Ok(json) => Body::Json(json),
                Err(_) if body_trace.truncated => Body::TruncatedJson(text.to_string()),
                Err(_) => Body::None,
            };
        }
        if trace.request.content_type.as_deref() == Some("application/x-www-form-urlencoded")
            || trimmed.starts_with("Action=")
        {
            let fields = text
                .split('&')
                .filter_map(|pair| pair.split_once('='))
                .map(|(key, value)| {
                    (
                        key.to_string(),
                        wire::query_param(&format!("?v={value}"), "v").unwrap_or_default(),
                    )
                })
                .collect();
            return Body::Form(fields);
        }
        Body::None
    }

    /// A top-level string field (JSON) or form field.
    fn field(&self, name: &str) -> Option<String> {
        match self {
            Body::Json(value) => value.get(name)?.as_str().map(str::to_string),
            Body::TruncatedJson(raw) => scan_json_string_values(raw, name).into_iter().next(),
            Body::Form(fields) => fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone()),
            Body::None => None,
        }
    }

    /// String values at a JSON path; a `[]` segment steps into every element
    /// of an array, e.g. `["Targets", "[]", "RoleArn"]`.
    fn at(&self, path: &[&str]) -> Vec<String> {
        fn walk(value: &Value, path: &[&str], out: &mut Vec<String>) {
            match path.split_first() {
                None => out.extend(value.as_str().map(str::to_string)),
                Some((&"[]", rest)) => value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .for_each(|item| walk(item, rest, out)),
                Some((key, rest)) => {
                    if let Some(item) = value.get(*key) {
                        walk(item, rest, out);
                    }
                }
            }
        }
        let mut out = Vec::new();
        if let Body::Json(value) = self {
            walk(value, path, &mut out);
        }
        out
    }

    /// Every string value of `key` at any depth of a JSON body.
    fn anywhere(&self, key: &str) -> Vec<String> {
        fn walk(value: &Value, key: &str, out: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    for (name, item) in map {
                        if name == key
                            && let Some(text) = item.as_str()
                        {
                            out.push(text.to_string());
                        }
                        walk(item, key, out);
                    }
                }
                Value::Array(items) => items.iter().for_each(|item| walk(item, key, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        if let Body::Json(value) = self {
            walk(value, key, &mut out);
        }
        out
    }

    /// A queue or topic attribute: JSON `Attributes.{name}`, or the Query
    /// forms `Attribute.N.Name/Value` (SQS), `Attributes.entry.N.key/value`
    /// (SNS `CreateTopic`) and `AttributeName`/`AttributeValue` (SNS
    /// `SetTopicAttributes`).
    fn attribute(&self, name: &str) -> Option<String> {
        match self {
            Body::Json(value) => value
                .get("Attributes")?
                .get(name)?
                .as_str()
                .map(str::to_string),
            Body::Form(fields) => {
                let get = |key: &str| {
                    fields
                        .iter()
                        .find(|(field, _)| field == key)
                        .map(|(_, value)| value.as_str())
                };
                if get("AttributeName") == Some(name) {
                    return get("AttributeValue").map(str::to_string);
                }
                fields.iter().find_map(|(field, value)| {
                    if value != name {
                        return None;
                    }
                    let value_key = if let Some(stem) = field.strip_suffix(".Name") {
                        format!("{stem}.Value")
                    } else {
                        format!("{}.value", field.strip_suffix(".key")?)
                    };
                    get(&value_key).map(str::to_string)
                })
            }
            _ => None,
        }
    }

    /// A top-level array of strings (JSON), e.g. SSM `Names`.
    fn string_list(&self, name: &str) -> Vec<String> {
        let Body::Json(value) = self else {
            return Vec::new();
        };
        let Some(items) = value.get(name).and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut out: Vec<String> = items
            .iter()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every EventBridge target bus named in `Entries[].EventBusName` (defaulting
    /// omitted entry buses to `"default"`, matching EventBridge `PutEvents` semantics).
    fn event_buses(&self) -> Vec<String> {
        let Body::Json(value) = self else {
            return Vec::new();
        };
        let Some(entries) = value.get("Entries").and_then(Value::as_array) else {
            return Vec::new();
        };
        let mut out: Vec<String> = entries
            .iter()
            .filter_map(Value::as_object)
            .map(|entry| {
                entry
                    .get("EventBusName")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or("default")
                    .to_string()
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Every DynamoDB table named anywhere in the body (batch and transact
    /// calls name several).
    fn tables(&self) -> Vec<String> {
        fn walk(value: &Value, out: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    for (key, item) in map {
                        if key == "TableName"
                            && let Some(name) = item.as_str()
                        {
                            out.push(name.to_string());
                        } else if key == "RequestItems"
                            && let Some(tables) = item.as_object()
                        {
                            out.extend(tables.keys().cloned());
                        }
                        walk(item, out);
                    }
                }
                Value::Array(items) => items.iter().for_each(|item| walk(item, out)),
                _ => {}
            }
        }
        let mut out = match self {
            Body::Json(value) => {
                let mut found = Vec::new();
                walk(value, &mut found);
                found
            }
            Body::TruncatedJson(raw) => {
                let mut found = scan_json_string_values(raw, "TableName");
                found.extend(scan_request_items_tables(raw));
                found
            }
            _ => Vec::new(),
        };
        out.sort();
        out.dedup();
        out
    }
}

/// Extract complete `"key": "value"` string fields from a truncated JSON prefix.
fn scan_json_string_values(raw: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\"");
    let mut out = Vec::new();
    let mut rest = raw;
    while let Some(pos) = rest.find(&needle) {
        rest = &rest[pos + needle.len()..];
        let after = rest.trim_start();
        let Some(after_colon) = after.strip_prefix(':') else {
            continue;
        };
        if let Some((val, tail)) = parse_leading_json_string(after_colon.trim_start()) {
            if !val.is_empty() {
                out.push(val);
            }
            rest = tail;
        }
    }
    out
}

/// Extract complete table keys under `"RequestItems": { "table_a": ... }` from truncated JSON.
fn scan_request_items_tables(raw: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Some(pos) = raw.find("\"RequestItems\"") else {
        return out;
    };
    let after = raw[pos + "\"RequestItems\"".len()..].trim_start();
    let Some(after_colon) = after.strip_prefix(':') else {
        return out;
    };
    let Some(mut cur) = after_colon.trim_start().strip_prefix('{') else {
        return out;
    };
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    while !cur.is_empty() {
        if depth == 0 {
            let trimmed = cur.trim_start_matches(|c: char| c.is_ascii_whitespace() || c == ',');
            if trimmed.starts_with('}') {
                break;
            }
            if let Some((table, after_key)) = parse_leading_json_string(trimmed)
                && let Some(after_sep) = after_key.trim_start().strip_prefix(':')
            {
                if !table.is_empty() {
                    out.push(table);
                }
                cur = after_sep;
                // Advance past the value at depth 0 until `,` or `}`.
                let mut val_bytes = 0usize;
                for (idx, ch) in cur.char_indices() {
                    val_bytes = idx + ch.len_utf8();
                    if escape {
                        escape = false;
                        continue;
                    }
                    if in_str {
                        if ch == '\\' {
                            escape = true;
                        } else if ch == '"' {
                            in_str = false;
                        }
                        continue;
                    }
                    match ch {
                        '"' => in_str = true,
                        '{' | '[' => depth += 1,
                        '}' | ']' => {
                            if depth == 0 {
                                val_bytes = idx;
                                break;
                            }
                            depth -= 1;
                        }
                        ',' if depth == 0 => {
                            val_bytes = idx + 1;
                            break;
                        }
                        _ => {}
                    }
                }
                cur = &cur[val_bytes..];
                if depth != 0 || in_str {
                    break;
                }
                continue;
            }
        }
        break;
    }
    out
}

fn parse_leading_json_string(s: &str) -> Option<(String, &str)> {
    let rest = s.strip_prefix('"')?;
    let mut escape = false;
    for (idx, ch) in rest.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if ch == '"' {
            let literal = &s[..=idx + 1];
            let parsed: String = serde_json::from_str(literal).ok()?;
            return Some((parsed, &rest[idx + 1..]));
        }
    }
    None
}

fn arn_for(mode: Mode, trace: &Trace, service: &str, rest: &str) -> String {
    match mode {
        Mode::Footprint => format!("arn:aws:{service}:*:*:{rest}"),
        Mode::Concrete => {
            let region = trace
                .region
                .as_deref()
                .filter(|r| !r.is_empty())
                .unwrap_or("*");
            format!("arn:aws:{service}:{region}:*:{rest}")
        }
    }
}

fn norm(mode: Mode, name: &str) -> String {
    match mode {
        Mode::Footprint => normalize(name),
        Mode::Concrete => name.to_string(),
    }
}

fn norm_path(mode: Mode, path: &str) -> String {
    match mode {
        Mode::Footprint => normalize_path(path),
        Mode::Concrete => path.to_string(),
    }
}

/// Generated names (UUIDs, timestamps, long hex) become `*`, so a test that
/// creates `orders-3f9c…` each run does not change the lock every time.
pub fn normalize(name: &str) -> String {
    fn generated(part: &str) -> bool {
        let hex = part.bytes().all(|b| b.is_ascii_hexdigit());
        let digits = part.bytes().all(|b| b.is_ascii_digit());
        (part.len() >= 8 && hex)
            || (part.len() >= 6 && digits)
            || (part.len() >= 6
                && hex
                && part.bytes().any(|b| b.is_ascii_digit())
                && part.bytes().any(|b| b.is_ascii_alphabetic()))
    }
    let name = &strip_uuids(name);
    let mut out = String::with_capacity(name.len());
    let mut part = String::new();
    let flush = |part: &mut String, out: &mut String| {
        out.push_str(if generated(part) { "*" } else { part });
        part.clear();
    };
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            flush(&mut part, &mut out);
            out.push(ch);
        } else {
            part.push(ch);
        }
    }
    flush(&mut part, &mut out);
    // `*-*-*` (what a UUID leaves behind) is just `*`.
    loop {
        let collapsed = out
            .replace("*-*", "*")
            .replace("*_*", "*")
            .replace("*.*", "*");
        if collapsed == out {
            return out;
        }
        out = collapsed;
    }
}

/// Normalize each `/`-delimited segment while preserving path hierarchy
/// (`db/password`, `service-role/lambda-exec`, `app/prod/db-url`).
fn normalize_path(path: &str) -> String {
    path.split('/').map(normalize).collect::<Vec<_>>().join("/")
}

/// Replace every `8-4-4-4-12` hex UUID with `*`.
fn strip_uuids(name: &str) -> String {
    const SHAPE: [usize; 5] = [8, 4, 4, 4, 12];
    let is_uuid = |candidate: &[u8]| {
        let mut index = 0;
        for (group, len) in SHAPE.iter().enumerate() {
            if group > 0 {
                if candidate.get(index) != Some(&b'-') {
                    return false;
                }
                index += 1;
            }
            let Some(part) = candidate.get(index..index + len) else {
                return false;
            };
            if !part.iter().all(u8::is_ascii_hexdigit) {
                return false;
            }
            index += len;
        }
        true
    };
    let bytes = name.as_bytes();
    let mut out = String::with_capacity(name.len());
    let mut index = 0;
    while index < bytes.len() {
        if index + 36 <= bytes.len() && is_uuid(&bytes[index..index + 36]) {
            out.push('*');
            index += 36;
        } else {
            let ch = name[index..].chars().next().unwrap_or('?');
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    out
}

/// Last path segment of a URL or ARN-ish name: queue URLs, ARNs, plain names.
fn last_segment(value: &str) -> &str {
    value
        .rsplit(['/', ':'])
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or(value)
}

/// The resources an action applies to: usually one; one per table for a
/// DynamoDB batch, one per `<Key>` for concrete S3 `DeleteObjects`, and one per
/// parameter for SSM `GetParameters`/`DeleteParameters`.
fn resources(mode: Mode, prefix: &str, operation: &str, trace: &Trace, body: &Body) -> Vec<String> {
    if prefix == "s3" {
        return s3_resources(mode, operation, trace);
    }
    if prefix == "ssm" {
        let names = body.string_list("Names");
        if !names.is_empty() {
            return names
                .iter()
                .map(|name| ssm_parameter(mode, trace, name))
                .collect();
        }
    }
    if prefix == "events" && operation == "PutEvents" {
        let buses = body.event_buses();
        if !buses.is_empty() {
            return buses
                .into_iter()
                .map(|bus| event_bus_arn(mode, trace, &bus))
                .collect();
        }
    }
    if prefix != "dynamodb" {
        return vec![resource(mode, prefix, operation, trace, body)];
    }
    let tables = body.tables();
    match tables.as_slice() {
        [] => match body.field("StreamArn") {
            Some(stream) => vec![dynamodb_stream_arn(mode, trace, &stream)],
            None => vec!["*".to_string()],
        },
        [one] => vec![match body.field("IndexName") {
            Some(index) => arn_for(
                mode,
                trace,
                "dynamodb",
                &format!("table/{}/index/{}", norm(mode, one), norm(mode, &index)),
            ),
            None => arn_for(
                mode,
                trace,
                "dynamodb",
                &format!("table/{}", norm(mode, one)),
            ),
        }],
        many => many
            .iter()
            .map(|table| {
                arn_for(
                    mode,
                    trace,
                    "dynamodb",
                    &format!("table/{}", norm(mode, table)),
                )
            })
            .collect(),
    }
}

fn resource(mode: Mode, prefix: &str, operation: &str, trace: &Trace, body: &Body) -> String {
    match prefix {
        "sqs" => body
            .field("QueueUrl")
            .or_else(|| body.field("QueueName"))
            .map(|queue| {
                if mode == Mode::Concrete && queue.starts_with("arn:") {
                    queue
                } else {
                    arn_for(mode, trace, "sqs", &norm(mode, last_segment(&queue)))
                }
            })
            .unwrap_or_else(|| "*".to_string()),
        "sns" => body
            .field("TopicArn")
            .or_else(|| body.field("TargetArn"))
            .map(|topic| {
                if mode == Mode::Concrete && topic.starts_with("arn:") {
                    topic
                } else {
                    arn_for(mode, trace, "sns", &norm(mode, last_segment(&topic)))
                }
            })
            .or_else(|| {
                (operation == "CreateTopic")
                    .then(|| body.field("Name"))
                    .flatten()
                    .map(|topic| arn_for(mode, trace, "sns", &norm(mode, &topic)))
            })
            .unwrap_or_else(|| "*".to_string()),
        "lambda" => lambda_function(&trace.path)
            .or_else(|| body.field("FunctionName"))
            .map(|function| {
                if mode == Mode::Concrete && function.starts_with("arn:") {
                    function
                } else {
                    arn_for(
                        mode,
                        trace,
                        "lambda",
                        &format!("function:{}", norm(mode, last_segment(&function))),
                    )
                }
            })
            .unwrap_or_else(|| "*".to_string()),
        "secretsmanager" => body
            .field("SecretId")
            .or_else(|| body.field("Name"))
            .map(|secret| {
                if mode == Mode::Concrete && secret.starts_with("arn:") {
                    secret
                } else {
                    arn_for(
                        mode,
                        trace,
                        "secretsmanager",
                        &format!("secret:{}-*", secret_name(mode, &secret)),
                    )
                }
            })
            .unwrap_or_else(|| "*".to_string()),
        "ssm" => body
            .field("Name")
            .map(|name| ssm_parameter(mode, trace, &name))
            .or_else(|| {
                body.field("Path").map(|path| {
                    let clean = path.trim_matches('/');
                    if clean.is_empty() {
                        arn_for(mode, trace, "ssm", "parameter/*")
                    } else {
                        arn_for(
                            mode,
                            trace,
                            "ssm",
                            &format!("parameter/{}/*", norm_path(mode, clean)),
                        )
                    }
                })
            })
            .unwrap_or_else(|| "*".to_string()),
        "logs" => body
            .field("logGroupName")
            .map(|group| {
                if mode == Mode::Concrete && group.starts_with("arn:") {
                    group
                } else {
                    let raw = group
                        .split_once(":log-group:")
                        .map_or(group.as_str(), |(_, rest)| rest)
                        .trim_end_matches(":*");
                    arn_for(
                        mode,
                        trace,
                        "logs",
                        &format!("log-group:{}:*", norm_path(mode, raw)),
                    )
                }
            })
            .unwrap_or_else(|| "*".to_string()),
        "kinesis" => body
            .field("StreamARN")
            .and_then(|stream| {
                if mode == Mode::Concrete && stream.starts_with("arn:") {
                    Some(stream)
                } else {
                    stream.split(":stream/").nth(1).map(|s| {
                        arn_for(
                            mode,
                            trace,
                            "kinesis",
                            &format!("stream/{}", norm_path(mode, s)),
                        )
                    })
                }
            })
            .or_else(|| {
                body.field("StreamName").map(|stream| {
                    arn_for(
                        mode,
                        trace,
                        "kinesis",
                        &format!("stream/{}", norm_path(mode, &stream)),
                    )
                })
            })
            .unwrap_or_else(|| "*".to_string()),
        "bedrock" => bedrock_model(&trace.path)
            .or_else(|| body.field("modelId"))
            .map(|model| bedrock_resource(mode, trace, &model))
            .unwrap_or_else(|| "*".to_string()),
        "kms" => body
            .field("KeyId")
            .map(|key| kms_key(mode, trace, &key))
            .unwrap_or_else(|| "*".to_string()),
        "sts" => body
            .field("RoleArn")
            .map(|role| iam_entity(mode, "role", &role))
            .unwrap_or_else(|| "*".to_string()),
        "iam" => body
            .field("RoleName")
            .map(|role| iam_entity(mode, "role", &role))
            .or_else(|| {
                body.field("UserName")
                    .map(|user| iam_entity(mode, "user", &user))
            })
            .unwrap_or_else(|| "*".to_string()),
        "events" => body
            .field("EventBusName")
            .or_else(|| {
                (operation == "CreateEventBus")
                    .then(|| body.field("Name"))
                    .flatten()
            })
            .map(|bus| event_bus_arn(mode, trace, &bus))
            .unwrap_or_else(|| "*".to_string()),
        "states" => body
            .field("executionArn")
            .map(|exec| states_execution_arn(mode, trace, &exec))
            .or_else(|| {
                body.field("stateMachineArn").map(|machine| {
                    if mode == Mode::Concrete && machine.starts_with("arn:") {
                        machine
                    } else {
                        arn_for(
                            mode,
                            trace,
                            "states",
                            &format!("stateMachine:{}", norm(mode, last_segment(&machine))),
                        )
                    }
                })
            })
            .or_else(|| {
                (operation == "CreateStateMachine")
                    .then(|| body.field("name"))
                    .flatten()
                    .map(|name| {
                        arn_for(
                            mode,
                            trace,
                            "states",
                            &format!("stateMachine:{}", norm(mode, &name)),
                        )
                    })
            })
            .unwrap_or_else(|| "*".to_string()),
        "cognito-idp" => body
            .field("UserPoolId")
            .map(|pool| {
                arn_for(
                    mode,
                    trace,
                    "cognito-idp",
                    &format!("userpool/{}", norm(mode, last_segment(&pool))),
                )
            })
            .unwrap_or_else(|| "*".to_string()),
        "ecr" => body
            .field("repositoryName")
            .map(|repo| {
                arn_for(
                    mode,
                    trace,
                    "ecr",
                    &format!("repository/{}", norm_path(mode, &repo)),
                )
            })
            .unwrap_or_else(|| "*".to_string()),
        "firehose" => body
            .field("DeliveryStreamName")
            .map(|stream| {
                arn_for(
                    mode,
                    trace,
                    "firehose",
                    &format!("deliverystream/{}", norm(mode, &stream)),
                )
            })
            .unwrap_or_else(|| "*".to_string()),
        "scheduler" => body
            .field("Name")
            .map(|name| {
                let group = body
                    .field("GroupName")
                    .unwrap_or_else(|| "default".to_string());
                arn_for(
                    mode,
                    trace,
                    "scheduler",
                    &format!("schedule/{}/{}", norm(mode, &group), norm(mode, &name)),
                )
            })
            .unwrap_or_else(|| "*".to_string()),
        "rds-data" => body
            .field("resourceArn")
            .map(|arn| {
                if mode == Mode::Concrete && arn.starts_with("arn:") {
                    arn
                } else {
                    arn_for(
                        mode,
                        trace,
                        "rds",
                        &format!("cluster:{}", norm(mode, last_segment(&arn))),
                    )
                }
            })
            .unwrap_or_else(|| "*".to_string()),
        _ => "*".to_string(),
    }
}

fn event_bus_arn(mode: Mode, trace: &Trace, bus: &str) -> String {
    if mode == Mode::Concrete && bus.starts_with("arn:") {
        bus.to_string()
    } else {
        arn_for(
            mode,
            trace,
            "events",
            &format!("event-bus/{}", norm(mode, last_segment(bus))),
        )
    }
}

fn states_execution_arn(mode: Mode, trace: &Trace, exec: &str) -> String {
    if mode == Mode::Concrete && exec.starts_with("arn:") {
        return exec.to_string();
    }
    if let Some((_, rest)) = exec.split_once(":execution:") {
        let machine = rest.split(':').next().unwrap_or(rest);
        return arn_for(
            mode,
            trace,
            "states",
            &format!("execution:{}:*", norm(mode, machine)),
        );
    }
    arn_for(
        mode,
        trace,
        "states",
        &format!("execution:{}:*", norm(mode, last_segment(exec))),
    )
}

fn dynamodb_stream_arn(mode: Mode, trace: &Trace, stream: &str) -> String {
    if mode == Mode::Concrete && stream.starts_with("arn:") {
        return stream.to_string();
    }
    if let Some((_, rest)) = stream.split_once(":table/") {
        let table = rest.split('/').next().unwrap_or(rest);
        return arn_for(
            mode,
            trace,
            "dynamodb",
            &format!("table/{}/stream/*", norm(mode, table)),
        );
    }
    "*".to_string()
}

/// Preserve hierarchical secret names (`db/password`) whether given by name or
/// by full ARN (`arn:aws:secretsmanager:...:secret:db/password-AbCdEf`).
fn secret_name(mode: Mode, secret: &str) -> String {
    if let Some((_, rest)) = secret.split_once(":secret:") {
        let base = match rest.rsplit_once('-') {
            Some((prefix, suffix))
                if !prefix.is_empty()
                    && suffix.len() == 6
                    && suffix.bytes().all(|b| b.is_ascii_alphanumeric()) =>
            {
                prefix
            }
            _ => rest,
        };
        norm_path(mode, base)
    } else {
        norm_path(mode, secret.trim_start_matches('/'))
    }
}

fn ssm_parameter(mode: Mode, trace: &Trace, name: &str) -> String {
    if mode == Mode::Concrete && name.starts_with("arn:") {
        return name.to_string();
    }
    let raw = name
        .split_once(":parameter/")
        .map_or(name, |(_, rest)| rest)
        .trim_start_matches('/');
    arn_for(
        mode,
        trace,
        "ssm",
        &format!("parameter/{}", norm_path(mode, raw)),
    )
}

/// Preserve IAM role/user paths (`service-role/my-role`) from names or ARNs.
fn iam_entity(mode: Mode, kind: &str, value: &str) -> String {
    if mode == Mode::Concrete && value.starts_with("arn:") {
        return value.to_string();
    }
    let needle = format!(":{kind}/");
    let raw = value
        .split_once(&needle)
        .map_or(value, |(_, rest)| rest)
        .trim_start_matches('/');
    format!("arn:aws:iam::*:{kind}/{}", norm_path(mode, raw))
}

fn bedrock_model(path: &str) -> Option<String> {
    let bare = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = bare.split('/').filter(|s| !s.is_empty()).collect();
    let raw = match segments.as_slice() {
        [
            "model",
            mid @ ..,
            "invoke" | "invoke-with-response-stream" | "converse" | "converse-stream",
        ] if !mid.is_empty() => mid.join("/"),
        ["foundation-models" | "inference-profiles", id] => (*id).to_string(),
        _ => return None,
    };
    let decoded = wire::percent_decode_path(&raw);
    (!decoded.is_empty()).then_some(decoded)
}

fn bedrock_resource(mode: Mode, trace: &Trace, model: &str) -> String {
    if model.starts_with("arn:") {
        if mode == Mode::Concrete {
            return model.to_string();
        }
        let Some(rest) = model.splitn(6, ':').nth(5).filter(|s| !s.is_empty()) else {
            return "*".to_string();
        };
        if let Some((kind, id)) = rest.split_once('/') {
            return match kind {
                "foundation-model" => arn_for(mode, trace, "bedrock", rest),
                "inference-profile" if is_system_inference_profile(id) => {
                    arn_for(mode, trace, "bedrock", rest)
                }
                _ => arn_for(
                    mode,
                    trace,
                    "bedrock",
                    &format!("{kind}/{}", norm_path(mode, id)),
                ),
            };
        }
        return arn_for(mode, trace, "bedrock", rest);
    }
    if is_system_inference_profile(model) {
        arn_for(
            mode,
            trace,
            "bedrock",
            &format!("inference-profile/{model}"),
        )
    } else if is_foundation_model_id(model) {
        arn_for(mode, trace, "bedrock", &format!("foundation-model/{model}"))
    } else {
        "*".to_string()
    }
}

fn is_system_inference_profile(id: &str) -> bool {
    let Some((geo, rest)) = id.split_once('.') else {
        return false;
    };
    matches!(
        geo,
        "us" | "eu" | "apac" | "global" | "us-gov" | "ca" | "jp" | "au"
    ) && is_foundation_model_id(rest)
}

fn is_foundation_model_id(id: &str) -> bool {
    let Some((provider, model)) = id.split_once('.') else {
        return false;
    };
    !provider.is_empty()
        && !model.is_empty()
        && !id.contains('/')
        && provider
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && provider.bytes().any(|b| b.is_ascii_alphabetic())
}

fn kms_key(mode: Mode, trace: &Trace, key: &str) -> String {
    if mode == Mode::Concrete && key.starts_with("arn:") {
        return key.to_string();
    }
    let raw = key
        .split_once(":alias/")
        .map(|(_, alias)| format!("alias/{alias}"))
        .unwrap_or_else(|| key.to_string());
    if raw.starts_with("alias/") {
        arn_for(mode, trace, "kms", &norm_path(mode, &raw))
    } else {
        arn_for(
            mode,
            trace,
            "kms",
            &format!("key/{}", norm(mode, last_segment(&raw))),
        )
    }
}

/// `/2015-03-31/functions/NAME/...` → `NAME`.
fn lambda_function(path: &str) -> Option<String> {
    let bare = path.split('?').next().unwrap_or(path);
    let mut segments = bare.split('/');
    segments.find(|segment| *segment == "functions")?;
    let name = segments.next()?;
    let name = wire::percent_decode_path(name);
    (!name.is_empty()).then_some(name)
}

fn s3_resources(mode: Mode, operation: &str, trace: &Trace) -> Vec<String> {
    let bare = trace.path.split('?').next().unwrap_or(&trace.path);
    let host = wire::strip_port(&trace.host);
    let host_bucket = (host.ends_with(".localhost")
        || host.contains(".s3.")
        || host.contains(".s3-")
        || host.ends_with(".s3"))
    .then(|| host.split('.').next().unwrap_or(""))
    .filter(|bucket| !bucket.is_empty());
    let (raw_bucket, raw_key): (Option<String>, &str) = match host_bucket {
        Some(bucket) => (
            Some(bucket.to_string()),
            bare.strip_prefix('/').unwrap_or(bare),
        ),
        None => {
            let after_root = bare.strip_prefix('/').unwrap_or(bare);
            match after_root.split_once('/') {
                Some((bucket, key)) if !bucket.is_empty() => {
                    (Some(wire::percent_decode_path(bucket)), key)
                }
                None if !after_root.is_empty() => (Some(wire::percent_decode_path(after_root)), ""),
                _ => (None, ""),
            }
        }
    };
    let decoded_key = wire::percent_decode_path(raw_key);
    let has_key = !decoded_key.is_empty();
    let Some(raw_bucket) = raw_bucket else {
        return vec!["*".to_string()];
    };
    let bucket = norm(mode, &raw_bucket);
    let bucket_level = matches!(
        operation,
        "CreateBucket"
            | "DeleteBucket"
            | "ListObjects"
            | "ListObjectsV2"
            | "HeadBucket"
            | "ListObjectVersions"
            | "ListMultipartUploads"
    );
    if operation == "DeleteObjects" && !bucket_level {
        return match mode {
            Mode::Footprint => vec![format!("arn:aws:s3:::{bucket}/*")],
            Mode::Concrete => {
                let xml = trace
                    .request
                    .body
                    .as_ref()
                    .filter(|b| !b.truncated)
                    .and_then(|b| b.text.as_deref())
                    .unwrap_or("");
                match parse_s3_delete_keys(xml) {
                    Ok(keys) => keys
                        .into_iter()
                        .map(|key| format!("arn:aws:s3:::{bucket}/{key}"))
                        .collect(),
                    // Keys that cannot be read (truncated or malformed XML,
                    // DTD/entity declarations, unknown references) stand as
                    // any key in the bucket: a policy must allow all of them,
                    // and a Deny on any key in the bucket applies.
                    Err(()) => vec![format!("arn:aws:s3:::{bucket}/*")],
                }
            }
        };
    }
    if has_key && !bucket_level {
        match mode {
            Mode::Footprint => vec![format!("arn:aws:s3:::{bucket}/*")],
            Mode::Concrete => vec![format!("arn:aws:s3:::{bucket}/{decoded_key}")],
        }
    } else {
        vec![format!("arn:aws:s3:::{bucket}")]
    }
}

/// Strictly parse `<Delete><Object><Key>...</Key></Object>...</Delete>` payloads,
/// resolving all XML 1.0 named entities, decimal (`&#47;`) and hex (`&#x2f;`)
/// character references, and `<![CDATA[...]]>` sections while failing closed
/// (`Err(())`) on DTD/`<!ENTITY` declarations, unknown entities, or malformed markup.
fn parse_s3_delete_keys(raw_xml: &str) -> Result<Vec<String>, ()> {
    let xml = raw_xml.strip_prefix('\u{FEFF}').unwrap_or(raw_xml);
    let mut pos = 0usize;
    let mut stack: Vec<&str> = Vec::new();
    let mut saw_root = false;
    let mut in_key: Option<String> = None;
    let mut keys: Vec<String> = Vec::new();

    fn local_name(tag: &str) -> &str {
        tag.rsplit_once(':').map_or(tag, |(_, local)| local)
    }

    fn valid_xml_name(name: &str) -> bool {
        !name.is_empty()
            && !name.starts_with(['-', '.', ':'])
            && !name.ends_with(':')
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    }

    while pos < xml.len() {
        let rest = &xml[pos..];
        if let Some(after) = rest.strip_prefix("<!--") {
            let end = after.find("-->").ok_or(())?;
            let body = &after[..end];
            if body.contains("--") || body.ends_with('-') {
                return Err(());
            }
            pos += 4 + end + 3;
        } else if let Some(after) = rest.strip_prefix("<![CDATA[") {
            if stack.is_empty() {
                return Err(());
            }
            let end = after.find("]]>").ok_or(())?;
            if let Some(buf) = in_key.as_mut() {
                buf.push_str(&after[..end]);
            }
            pos += 9 + end + 3;
        } else if let Some(after) = rest.strip_prefix("<?") {
            let end = after.find("?>").ok_or(())?;
            pos += 2 + end + 2;
        } else if rest.starts_with("<!") {
            // Reject DOCTYPE / ENTITY / ATTLIST declarations fail-closed.
            return Err(());
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after.find('>').ok_or(())?;
            let inner = &after[..end];
            let raw_name = inner.trim_end_matches([' ', '\t', '\r', '\n']);
            if !valid_xml_name(raw_name) {
                return Err(());
            }
            let expected = stack.pop().ok_or(())?;
            if expected != raw_name {
                return Err(());
            }
            if local_name(raw_name) == "Key" {
                let val = in_key.take().ok_or(())?;
                if val.is_empty() {
                    return Err(());
                }
                keys.push(val);
            }
            pos += 2 + end + 1;
        } else if let Some(after) = rest.strip_prefix('<') {
            if in_key.is_some() {
                return Err(());
            }
            let end = find_tag_end(after)?;
            let inner = &after[..end];
            let (tag_body, self_closing) = match inner.strip_suffix('/') {
                Some(stripped) => (stripped, true),
                None => (inner, false),
            };
            let name_end = tag_body
                .find([' ', '\t', '\r', '\n'])
                .unwrap_or(tag_body.len());
            let raw_name = &tag_body[..name_end];
            if !valid_xml_name(raw_name) {
                return Err(());
            }
            validate_attributes(&tag_body[name_end..])?;
            if stack.is_empty() {
                if saw_root || local_name(raw_name) != "Delete" {
                    return Err(());
                }
                saw_root = true;
            }
            if local_name(raw_name) == "Key" {
                let under_object = stack.last().is_some_and(|p| local_name(p) == "Object");
                if !under_object || self_closing {
                    return Err(());
                }
                in_key = Some(String::new());
            }
            if !self_closing {
                stack.push(raw_name);
            }
            pos += 1 + end + 1;
        } else {
            let next_lt = rest.find('<').unwrap_or(rest.len());
            let chunk = &rest[..next_lt];
            if chunk.contains("]]>") {
                return Err(());
            }
            if stack.is_empty() {
                if !chunk.trim_matches([' ', '\t', '\r', '\n']).is_empty() {
                    return Err(());
                }
            } else {
                let decoded = decode_xml_text(chunk)?;
                if let Some(buf) = in_key.as_mut() {
                    buf.push_str(&decoded);
                }
            }
            pos += next_lt;
        }
    }

    if !saw_root || !stack.is_empty() || in_key.is_some() || keys.is_empty() {
        return Err(());
    }
    Ok(keys)
}

fn find_tag_end(s: &str) -> Result<usize, ()> {
    let mut quote: Option<char> = None;
    for (idx, ch) in s.char_indices() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) if ch == '<' => return Err(()),
            Some(_) => {}
            None => match ch {
                '"' | '\'' => quote = Some(ch),
                '<' => return Err(()),
                '>' => return Ok(idx),
                _ => {}
            },
        }
    }
    Err(())
}

fn validate_attributes(mut attrs: &str) -> Result<(), ()> {
    loop {
        attrs = attrs.trim_start_matches([' ', '\t', '\r', '\n']);
        if attrs.is_empty() {
            return Ok(());
        }
        let eq = attrs.find('=').ok_or(())?;
        let name = attrs[..eq].trim_end_matches([' ', '\t', '\r', '\n']);
        if name.is_empty() {
            return Err(());
        }
        let after_eq = attrs[eq + 1..].trim_start_matches([' ', '\t', '\r', '\n']);
        let q = after_eq
            .chars()
            .next()
            .filter(|c| matches!(c, '"' | '\''))
            .ok_or(())?;
        let rest = &after_eq[q.len_utf8()..];
        let close = rest.find(q).ok_or(())?;
        let val = &rest[..close];
        if val.contains('<') {
            return Err(());
        }
        let _ = decode_xml_text(val)?;
        attrs = &rest[close + q.len_utf8()..];
    }
}

fn decode_xml_text(s: &str) -> Result<String, ()> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let semi = after.find(';').ok_or(())?;
        let entity = &after[..semi];
        let ch = match entity {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                if let Some(hex) = entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                {
                    if hex.is_empty()
                        || hex.len() > 6
                        || !hex.bytes().all(|b| b.is_ascii_hexdigit())
                    {
                        return Err(());
                    }
                    let code = u32::from_str_radix(hex, 16).map_err(|_| ())?;
                    valid_xml_char(code)?
                } else if let Some(dec) = entity.strip_prefix('#') {
                    if dec.is_empty() || dec.len() > 7 || !dec.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(());
                    }
                    let code = dec.parse::<u32>().map_err(|_| ())?;
                    valid_xml_char(code)?
                } else {
                    return Err(());
                }
            }
        };
        out.push(ch);
        rest = &after[semi + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn valid_xml_char(code: u32) -> Result<char, ()> {
    let allowed = matches!(
        code,
        0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF
    );
    if !allowed {
        return Err(());
    }
    char::from_u32(code).ok_or(())
}

/// Permissions a call needs on resources it does not name in its target.
///
/// Only keys and roles visible in this request (or, for S3, its response
/// headers) are derived. A key that lives on the resource itself, such as an
/// encrypted queue's key on `SendMessage` or a secret's key on
/// `GetSecretValue`, is not on the wire and is not guessed.
fn implied(mode: Mode, prefix: &str, operation: &str, trace: &Trace, body: &Body) -> Vec<Access> {
    let mut out = Vec::new();
    if prefix == "s3" {
        // SSE-KMS: reading needs kms:Decrypt, writing kms:GenerateDataKey on the key.
        let kms_key_id = [&trace.response.headers, &trace.request.headers]
            .into_iter()
            .flatten()
            .find(|header| {
                header
                    .name
                    .eq_ignore_ascii_case("x-amz-server-side-encryption-aws-kms-key-id")
            })
            .map(|header| header.value.clone());
        let kms = [&trace.response.headers, &trace.request.headers]
            .into_iter()
            .flatten()
            .any(|header| {
                header
                    .name
                    .eq_ignore_ascii_case("x-amz-server-side-encryption")
                    && header.value.starts_with("aws:kms")
            });
        if kms || kms_key_id.is_some() {
            let key = kms_key_id
                .map(|key| kms_key(mode, trace, &key))
                .unwrap_or_else(|| "*".to_string());
            let action = match operation {
                "GetObject" | "HeadObject" => Some("kms:Decrypt"),
                // A copy's headers describe its destination, which it writes.
                "PutObject"
                | "CopyObject"
                | "CreateMultipartUpload"
                | "UploadPart"
                | "CompleteMultipartUpload" => Some("kms:GenerateDataKey"),
                _ => None,
            };
            if let Some(action) = action {
                out.push(Access::new(action, key));
            }
        }
    } else if let Some((action, key)) = kms_in_body(prefix, operation, body) {
        out.push(Access::new(action, kms_key(mode, trace, &key)));
    }
    // Passing one or more IAM roles to an AWS service requires `iam:PassRole` on each role.
    let passed_roles: Vec<String> = match (prefix, operation) {
        ("lambda", "CreateFunction" | "UpdateFunctionConfiguration") => body.at(&["Role"]),
        ("states", "CreateStateMachine" | "UpdateStateMachine") => body.at(&["roleArn"]),
        ("ecs", "RegisterTaskDefinition") => {
            let mut roles = body.at(&["taskRoleArn"]);
            roles.extend(body.at(&["executionRoleArn"]));
            roles
        }
        ("events", "PutRule") | ("pipes", "CreatePipe" | "UpdatePipe") => body.at(&["RoleArn"]),
        ("events", "PutTargets") => body.at(&["Targets", "[]", "RoleArn"]),
        ("scheduler", "CreateSchedule" | "UpdateSchedule") => body.at(&["Target", "RoleArn"]),
        // Every destination, backup and source configuration names its role.
        ("firehose", "CreateDeliveryStream" | "UpdateDestination") => body.anywhere("RoleARN"),
        ("glue", "CreateJob") => body.at(&["Role"]),
        ("glue", "UpdateJob") => body.at(&["JobUpdate", "Role"]),
        _ => Vec::new(),
    };
    for role in passed_roles.iter().filter(|role| !role.is_empty()) {
        let access = Access::new("iam:PassRole", iam_entity(mode, "role", role));
        if !out.contains(&access) {
            out.push(access);
        }
    }
    out
}

/// The KMS action and key a request names in its body.
///
/// SQS and SNS record the key their producers will need
/// (`kms:GenerateDataKey`) when a queue or topic is configured with one.
fn kms_in_body(prefix: &str, operation: &str, body: &Body) -> Option<(&'static str, String)> {
    match (prefix, operation) {
        ("secretsmanager", "CreateSecret" | "UpdateSecret") => {
            Some(("kms:GenerateDataKey", body.field("KmsKeyId")?))
        }
        // Standard parameters are encrypted directly; advanced ones with a data key.
        ("ssm", "PutParameter") => {
            let key = body.field("KeyId")?;
            let action = if body.field("Tier").as_deref() == Some("Advanced") {
                "kms:GenerateDataKey"
            } else {
                "kms:Encrypt"
            };
            Some((action, key))
        }
        ("sqs", "CreateQueue" | "SetQueueAttributes")
        | ("sns", "CreateTopic" | "SetTopicAttributes") => {
            Some(("kms:GenerateDataKey", body.attribute("KmsMasterKeyId")?))
        }
        ("dynamodb", "CreateTable" | "UpdateTable") => Some((
            "kms:DescribeKey",
            body.at(&["SSESpecification", "KMSMasterKeyId"])
                .into_iter()
                .next()?,
        )),
        ("kinesis", "StartStreamEncryption") => Some(("kms:GenerateDataKey", body.field("KeyId")?)),
        _ => None,
    }
    .filter(|(_, key)| !key.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{BodyTrace, HeaderTrace, Message};

    fn call(service: &str, operation: &str, method: &str, path: &str, body: Option<&str>) -> Trace {
        Trace {
            id: "t".into(),
            at: "2026-11-15T00:00:00Z".into(),
            method: method.into(),
            path: path.into(),
            host: "127.0.0.1:9297".into(),
            service: service.into(),
            operation: operation.into(),
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
            replay_of: None,
            blocked_phase: None,
            request: Message {
                headers: Vec::new(),
                content_type: Some(if body.is_some_and(|b| b.starts_with('{')) {
                    "application/x-amz-json-1.0".into()
                } else {
                    "application/x-www-form-urlencoded".into()
                }),
                body: body.map(|text| BodyTrace {
                    text: Some(text.into()),
                    hex: None,
                    encoding: None,
                    bytes: text.len() as u64,
                    truncated: false,
                    binary: false,
                    sha256: String::new(),
                }),
            },
            response: Message::default(),
        }
    }

    fn access(trace: &Trace) -> (String, String) {
        let own = derive(trace).0;
        assert_eq!(own.len(), 1, "one permission: {own:?}");
        (own[0].action.clone(), own[0].resource.clone())
    }

    fn all(trace: &Trace) -> Vec<(String, String)> {
        derive(trace)
            .0
            .into_iter()
            .map(|access| (access.action, access.resource))
            .collect()
    }

    #[test]
    fn maps_s3_rest_calls_to_bucket_and_object_resources() {
        assert_eq!(
            access(&call("s3", "GetObject", "GET", "/assets/img/a.png", None)),
            ("s3:GetObject".into(), "arn:aws:s3:::assets/*".into())
        );
        assert_eq!(
            access(&call(
                "s3",
                "ListObjectsV2",
                "GET",
                "/assets?list-type=2",
                None
            )),
            ("s3:ListBucket".into(), "arn:aws:s3:::assets".into())
        );
        assert_eq!(
            access(&call("s3", "HeadObject", "HEAD", "/assets/k", None)),
            ("s3:GetObject".into(), "arn:aws:s3:::assets/*".into())
        );
        assert_eq!(
            access(&call("s3", "ListBuckets", "GET", "/", None)),
            ("s3:ListAllMyBuckets".into(), "*".into())
        );
        let mut virtual_host = call("s3", "PutObject", "PUT", "/k", None);
        virtual_host.host = "assets.localhost:9297".into();
        assert_eq!(access(&virtual_host).1, "arn:aws:s3:::assets/*");
    }

    #[test]
    fn reads_resources_from_json_and_query_bodies() {
        assert_eq!(
            access(&call(
                "dynamodb",
                "Query",
                "POST",
                "/",
                Some(r#"{"TableName":"orders","IndexName":"byUser"}"#)
            )),
            (
                "dynamodb:Query".into(),
                "arn:aws:dynamodb:*:*:table/orders/index/byUser".into()
            )
        );
        // An IAM resource names one table: a batch gets one entry per table.
        assert_eq!(
            all(&call(
                "dynamodb",
                "BatchWriteItem",
                "POST",
                "/",
                Some(r#"{"RequestItems":{"orders":[],"users":[]}}"#)
            )),
            vec![
                (
                    "dynamodb:BatchWriteItem".to_string(),
                    "arn:aws:dynamodb:*:*:table/orders".to_string()
                ),
                (
                    "dynamodb:BatchWriteItem".to_string(),
                    "arn:aws:dynamodb:*:*:table/users".to_string()
                ),
            ]
        );
        assert_eq!(
            access(&call(
                "sqs",
                "SendMessage",
                "POST",
                "/",
                Some(r#"{"QueueUrl":"http://127.0.0.1:9297/000000000000/jobs","MessageBody":"x"}"#)
            )),
            ("sqs:SendMessage".into(), "arn:aws:sqs:*:*:jobs".into())
        );
        assert_eq!(
            access(&call(
                "sqs",
                "SendMessage",
                "POST",
                "/",
                Some("Action=SendMessage&QueueUrl=http%3A%2F%2Fh%2F000000000000%2Fjobs")
            ))
            .1,
            "arn:aws:sqs:*:*:jobs"
        );
        assert_eq!(
            access(&call(
                "sns",
                "Publish",
                "POST",
                "/",
                Some("Action=Publish&TopicArn=arn%3Aaws%3Asns%3Aus-east-1%3A000000000000%3Aalerts")
            ))
            .1,
            "arn:aws:sns:*:*:alerts"
        );
        assert_eq!(
            access(&call(
                "lambda",
                "Invoke",
                "POST",
                "/2015-03-31/functions/thumbnailer/invocations",
                None
            )),
            (
                "lambda:InvokeFunction".into(),
                "arn:aws:lambda:*:*:function:thumbnailer".into()
            )
        );
        assert_eq!(
            access(&call(
                "secretsmanager",
                "GetSecretValue",
                "POST",
                "/",
                Some(r#"{"SecretId":"db/password"}"#)
            ))
            .1,
            "arn:aws:secretsmanager:*:*:secret:db/password-*"
        );
        assert_eq!(
            access(&call(
                "monitoring",
                "PutMetricData",
                "POST",
                "/",
                Some(r#"{"Namespace":"x"}"#)
            )),
            ("cloudwatch:PutMetricData".into(), "*".into())
        );
    }

    #[test]
    fn transactions_need_each_item_action() {
        let write = call(
            "dynamodb",
            "TransactWriteItems",
            "POST",
            "/",
            Some(
                r#"{"TransactItems":[{"Put":{"TableName":"orders","Item":{}}},{"Delete":{"TableName":"carts","Key":{}}},{"ConditionCheck":{"TableName":"users","Key":{}}}]}"#,
            ),
        );
        assert_eq!(
            all(&write),
            vec![
                (
                    "dynamodb:ConditionCheckItem".to_string(),
                    "arn:aws:dynamodb:*:*:table/users".to_string()
                ),
                (
                    "dynamodb:DeleteItem".to_string(),
                    "arn:aws:dynamodb:*:*:table/carts".to_string()
                ),
                (
                    "dynamodb:PutItem".to_string(),
                    "arn:aws:dynamodb:*:*:table/orders".to_string()
                ),
            ]
        );
        let read = call(
            "dynamodb",
            "TransactGetItems",
            "POST",
            "/",
            Some(r#"{"TransactItems":[{"Get":{"TableName":"orders","Key":{}}}]}"#),
        );
        assert_eq!(
            access(&read),
            (
                "dynamodb:GetItem".into(),
                "arn:aws:dynamodb:*:*:table/orders".into()
            )
        );
        // A truncated body cannot be read: no guess.
        let truncated = call(
            "dynamodb",
            "TransactWriteItems",
            "POST",
            "/",
            Some(r#"{"TransactItems":[{"Put":"#),
        );
        assert!(derive(&truncated).0.is_empty());
    }

    #[test]
    fn copies_read_their_source_and_write_their_destination() {
        let mut copy = call("s3", "CopyObject", "PUT", "/archive/2026/a.png", None);
        copy.request.headers = vec![HeaderTrace {
            name: "x-amz-copy-source".into(),
            value: "uploads/a%20b.png?versionId=3".into(),
        }];
        assert_eq!(
            all(&copy),
            vec![
                (
                    "s3:PutObject".to_string(),
                    "arn:aws:s3:::archive/*".to_string()
                ),
                (
                    "s3:GetObjectVersion".to_string(),
                    "arn:aws:s3:::uploads/*".to_string()
                ),
            ]
        );
        copy.request.headers[0].value = "/uploads/a.png".into();
        assert_eq!(
            all(&copy)[1],
            pair("s3:GetObject", "arn:aws:s3:::uploads/*")
        );
    }

    #[test]
    fn calls_without_a_named_operation_or_permission_have_no_access() {
        assert!(
            derive(&call("s3", "PATCH", "PATCH", "/b/k", None))
                .0
                .is_empty()
        );
        assert!(derive(&call("aws", "GET", "GET", "/", None)).0.is_empty());
        let identity = call(
            "sts",
            "GetCallerIdentity",
            "POST",
            "/",
            Some("Action=GetCallerIdentity"),
        );
        assert!(derive(&identity).0.is_empty());
    }

    #[test]
    fn generated_names_become_wildcards() {
        assert_eq!(normalize("orders"), "orders");
        assert_eq!(normalize("orders-3f9c2a1b"), "orders-*");
        assert_eq!(normalize("test-1731650000123-jobs"), "test-*-jobs");
        assert_eq!(
            normalize("tmp-550e8400-e29b-41d4-a716-446655440000"),
            "tmp-*"
        );
        assert_eq!(normalize("my_bucket.v2"), "my_bucket.v2");
        assert_eq!(normalize("user-42"), "user-42");
        assert_eq!(normalize("6f0e1838-4281-4c5b-9a3e-2d1f0b7c8a91"), "*");
        assert_eq!(
            normalize("key-2d1f0b7c-1838-4281-9a3e-000000000001-x"),
            "key-*-x"
        );
        // Short 4-5 char identifiers stay intact rather than collapsing to `*`.
        assert_eq!(normalize("app-f100"), "app-f100");
        assert_eq!(normalize("node-ab12"), "node-ab12");
    }

    #[test]
    fn preserves_paths_in_secrets_roles_ssm_logs_kinesis_and_bedrock() {
        assert_eq!(
            access(&call(
                "secretsmanager",
                "GetSecretValue",
                "POST",
                "/",
                Some(
                    r#"{"SecretId":"arn:aws:secretsmanager:us-east-1:123456789012:secret:db/password-AbCd12"}"#
                )
            ))
            .1,
            "arn:aws:secretsmanager:*:*:secret:db/password-*"
        );
        assert_eq!(
            access(&call(
                "sts",
                "AssumeRole",
                "POST",
                "/",
                Some("Action=AssumeRole&RoleArn=arn%3Aaws%3Aiam%3A%3A123456789012%3Arole%2Fservice-role%2Fworker")
            ))
            .1,
            "arn:aws:iam::*:role/service-role/worker"
        );
        assert_eq!(
            all(&call(
                "ssm",
                "GetParameters",
                "POST",
                "/",
                Some(r#"{"Names":["/shop/prod/db-url","/shop/prod/api-key"]}"#)
            )),
            vec![
                (
                    "ssm:GetParameters".into(),
                    "arn:aws:ssm:*:*:parameter/shop/prod/api-key".into()
                ),
                (
                    "ssm:GetParameters".into(),
                    "arn:aws:ssm:*:*:parameter/shop/prod/db-url".into()
                ),
            ]
        );
        assert_eq!(
            access(&call(
                "logs",
                "PutLogEvents",
                "POST",
                "/",
                Some(r#"{"logGroupName":"/aws/lambda/checkout"}"#)
            )),
            (
                "logs:PutLogEvents".into(),
                "arn:aws:logs:*:*:log-group:/aws/lambda/checkout:*".into()
            )
        );
        assert_eq!(
            access(&call(
                "logs",
                "PutLogEvents",
                "POST",
                "/",
                Some(r#"{"logGroupName":"custom/checkout"}"#)
            ))
            .1,
            "arn:aws:logs:*:*:log-group:custom/checkout:*"
        );
        assert_eq!(
            access(&call(
                "kinesis",
                "PutRecord",
                "POST",
                "/",
                Some(r#"{"StreamName":"orders-events"}"#)
            )),
            (
                "kinesis:PutRecord".into(),
                "arn:aws:kinesis:*:*:stream/orders-events".into()
            )
        );
        assert_eq!(
            access(&call(
                "bedrock-runtime",
                "Converse",
                "POST",
                "/model/anthropic.claude-3-haiku-20240307-v1%3A0/converse",
                Some("{}")
            )),
            (
                "bedrock:InvokeModel".into(),
                "arn:aws:bedrock:*:*:foundation-model/anthropic.claude-3-haiku-20240307-v1:0"
                    .into()
            )
        );
        assert_eq!(
            access(&call(
                "bedrock-runtime",
                "InvokeModel",
                "POST",
                "/model/us.anthropic.claude-3-5-sonnet-20241022-v2%3A0/invoke",
                Some("{}")
            ))
            .1,
            "arn:aws:bedrock:*:*:inference-profile/us.anthropic.claude-3-5-sonnet-20241022-v2:0"
        );
        assert_eq!(
            access(&call(
                "bedrock-runtime",
                "InvokeModel",
                "POST",
                "/model/arn%3Aaws%3Abedrock%3Aus-east-1%3A123456789012%3Aprovisioned-model%2Fmy-throughput/invoke",
                Some("{}")
            ))
            .1,
            "arn:aws:bedrock:*:*:provisioned-model/my-throughput"
        );
        assert_eq!(
            access(&call(
                "bedrock-runtime",
                "InvokeModel",
                "POST",
                "/model/opaque-throughput-id/invoke",
                Some("{}")
            ))
            .1,
            "*"
        );
    }

    #[test]
    fn finds_implied_kms_and_pass_role() {
        let mut get = call("s3", "GetObject", "GET", "/vault/secret.txt", None);
        get.response.headers = vec![
            HeaderTrace {
                name: "x-amz-server-side-encryption".into(),
                value: "aws:kms".into(),
            },
            HeaderTrace {
                name: "x-amz-server-side-encryption-aws-kms-key-id".into(),
                value:
                    "arn:aws:kms:us-east-1:000000000000:key/1234abcd-12ab-34cd-56ef-1234567890ab"
                        .into(),
            },
        ];
        let (_, implied) = derive(&get);
        assert_eq!(
            implied,
            vec![Access::new("kms:Decrypt", "arn:aws:kms:*:*:key/*")]
        );

        let create = call(
            "lambda",
            "CreateFunction",
            "POST",
            "/2015-03-31/functions",
            Some(r#"{"FunctionName":"f","Role":"arn:aws:iam::000000000000:role/lambda-exec"}"#),
        );
        let (own, implied) = derive(&create);
        assert_eq!(own[0].resource, "arn:aws:lambda:*:*:function:f");
        assert_eq!(
            implied,
            vec![Access::new(
                "iam:PassRole",
                "arn:aws:iam::*:role/lambda-exec"
            )]
        );
    }

    #[test]
    fn salvages_table_permissions_from_truncated_dynamodb_batches() {
        let mut batch = call(
            "dynamodb",
            "BatchWriteItem",
            "POST",
            "/",
            Some(
                r#"{"RequestItems":{"orders":[{"PutRequest":{"Item":{"k":{"S":"v"}}}}],"audit":[{"PutRequest":{"Item":{"x":"#,
            ),
        );
        batch.request.body.as_mut().unwrap().truncated = true;
        // The tables read so far, and `*` for whatever the cut-off tail names.
        let derived = derive_call(&batch);
        assert_eq!(
            derived.own,
            vec![
                Access::new(
                    "dynamodb:BatchWriteItem",
                    "arn:aws:dynamodb:*:*:table/audit"
                ),
                Access::new(
                    "dynamodb:BatchWriteItem",
                    "arn:aws:dynamodb:*:*:table/orders"
                ),
                Access::new("dynamodb:BatchWriteItem", "*"),
            ]
        );
        assert_eq!(
            derived.unverified,
            vec![Access::new("dynamodb:BatchWriteItem", "*")]
        );
        assert_eq!(derived.concrete_own.last(), derived.own.last());

        let mut tx = call(
            "dynamodb",
            "TransactWriteItems",
            "POST",
            "/",
            Some(r#"{"TransactItems":[{"Put":{"TableName":"orders","Item":{"blob":{"S":"xxxx"#),
        );
        tx.request.body.as_mut().unwrap().truncated = true;
        assert_eq!(
            derive(&tx).0,
            vec![
                Access::new("dynamodb:PutItem", "arn:aws:dynamodb:*:*:table/orders"),
                Access::new("dynamodb:PutItem", "*"),
                Access::new("dynamodb:UpdateItem", "*"),
                Access::new("dynamodb:DeleteItem", "*"),
                Access::new("dynamodb:ConditionCheckItem", "*"),
            ]
        );
    }

    #[test]
    fn concrete_gate_derivation_preserves_object_keys_delete_objects_and_fails_closed() {
        let get = call("s3", "GetObject", "GET", "/bucket/public/a+b.txt", None);
        assert_eq!(
            derive(&get).0,
            vec![Access::new("s3:GetObject", "arn:aws:s3:::bucket/*")]
        );
        assert_eq!(
            derive_call(&get).concrete_own,
            vec![Access::new(
                "s3:GetObject",
                "arn:aws:s3:::bucket/public/a+b.txt"
            )]
        );

        // Consecutive, leading, and trailing slashes in S3 object keys are preserved verbatim.
        for (path, expected_arn) in [
            ("/bucket/private//file", "arn:aws:s3:::bucket/private//file"),
            ("/bucket//leading", "arn:aws:s3:::bucket//leading"),
            ("/bucket/dir/", "arn:aws:s3:::bucket/dir/"),
        ] {
            let weird = call("s3", "GetObject", "GET", path, None);
            assert_eq!(
                derive_call(&weird).concrete_own,
                vec![Access::new("s3:GetObject", expected_arn)]
            );
        }

        // Percent-encoded `?` (`%3F`) in `x-amz-copy-source` stays part of the source key
        // while literal `?versionId=...` query parameters are stripped first.
        let mut copy = call("s3", "CopyObject", "PUT", "/dest/out.txt", None);
        copy.request.headers = vec![HeaderTrace {
            name: "x-amz-copy-source".into(),
            value: "assets/public%3Fsecret?versionId=v1".into(),
        }];
        assert_eq!(
            derive_call(&copy).concrete_own,
            vec![
                Access::new("s3:PutObject", "arn:aws:s3:::dest/out.txt"),
                Access::new("s3:GetObjectVersion", "arn:aws:s3:::assets/public?secret"),
            ]
        );

        let del = call(
            "s3",
            "DeleteObjects",
            "POST",
            "/bucket?delete",
            Some(
                r#"<?xml version="1.0" encoding="UTF-8"?><!-- <Key>ignored</Key> --><Delete xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Object><Key>public/a.txt</Key></Object><Object><Key>private&#47;dec&#x2f;hex</Key></Object><Object><Key><![CDATA[private/cdata<&>.txt]]></Key></Object><Object><Key>/private//secret.txt</Key></Object></Delete>"#,
            ),
        );
        assert_eq!(
            derive_call(&del).concrete_own,
            vec![
                Access::new("s3:DeleteObject", "arn:aws:s3:::bucket/public/a.txt"),
                Access::new("s3:DeleteObject", "arn:aws:s3:::bucket/private/dec/hex"),
                Access::new(
                    "s3:DeleteObject",
                    "arn:aws:s3:::bucket/private/cdata<&>.txt"
                ),
                Access::new(
                    "s3:DeleteObject",
                    "arn:aws:s3:::bucket//private//secret.txt"
                ),
            ]
        );

        // DTD / ENTITY declarations, unknown entities, and malformed XML stand
        // for any key in the bucket.
        for bad_xml in [
            r#"<!DOCTYPE Delete [<!ENTITY p "private">]><Delete><Object><Key>&p;/file</Key></Object></Delete>"#,
            r#"<Delete><Object><Key>&unknown;/file</Key></Object></Delete>"#,
            r#"<Delete><Object><Key>unclosed</Object></Delete>"#,
        ] {
            let bad = call(
                "s3",
                "DeleteObjects",
                "POST",
                "/bucket?delete",
                Some(bad_xml),
            );
            assert_eq!(
                derive_call(&bad).concrete_own,
                vec![Access::new("s3:DeleteObject", "arn:aws:s3:::bucket/*")],
                "bad XML stands for every key: {bad_xml}"
            );
        }

        let mut truncated = call(
            "dynamodb",
            "BatchWriteItem",
            "POST",
            "/",
            Some(r#"{"RequestItems":{"orders":[{"PutRequest":{"Item":{"k":{"S":"v"#),
        );
        truncated.request.body.as_mut().unwrap().truncated = true;
        assert!(
            derive(&truncated)
                .0
                .contains(&Access::new("dynamodb:BatchWriteItem", "*")),
            "a truncated body-dependent call stands as `*`"
        );

        // EventBridge PutEvents extracts every target bus under Entries[], defaulting omitted entries to `default`.
        let put_events = call(
            "events",
            "PutEvents",
            "POST",
            "/",
            Some(
                r#"{"Entries":[{"Source":"app","DetailType":"OrderCreated"},{"EventBusName":"audit-bus","Source":"app"}]}"#,
            ),
        );
        assert_eq!(
            derive(&put_events).0,
            vec![
                Access::new("events:PutEvents", "arn:aws:events:*:*:event-bus/audit-bus"),
                Access::new("events:PutEvents", "arn:aws:events:*:*:event-bus/default"),
            ]
        );

        // Step Functions execution-scoped calls and DynamoDB Streams ARNs map cleanly.
        let desc_exec = call(
            "states",
            "DescribeExecution",
            "POST",
            "/",
            Some(
                r#"{"executionArn":"arn:aws:states:us-east-1:123456789012:execution:checkout-flow:run-12345678"}"#,
            ),
        );
        assert_eq!(
            derive(&desc_exec).0,
            vec![Access::new(
                "states:DescribeExecution",
                "arn:aws:states:*:*:execution:checkout-flow:*"
            )]
        );
        let stream_call = call(
            "streams.dynamodb",
            "DescribeStream",
            "POST",
            "/",
            Some(
                r#"{"StreamArn":"arn:aws:dynamodb:us-east-1:123456789012:table/orders/stream/2026-11-18T00:00:00.000"}"#,
            ),
        );
        assert_eq!(
            derive(&stream_call).0,
            vec![Access::new(
                "dynamodb:DescribeStream",
                "arn:aws:dynamodb:*:*:table/orders/stream/*"
            )]
        );

        // S3 subresources map to canonical AWS IAM actions, and ECS RegisterTaskDefinition
        // emits implied `iam:PassRole` for both `taskRoleArn` and `executionRoleArn`.
        for (op, path, expected_action, expected_res) in [
            (
                "PostObject",
                "/uploads",
                "s3:PutObject",
                "arn:aws:s3:::uploads",
            ),
            (
                "SelectObjectContent",
                "/uploads/data.csv?select&select-type=2",
                "s3:GetObject",
                "arn:aws:s3:::uploads/*",
            ),
            (
                "GetPublicAccessBlock",
                "/uploads?publicAccessBlock",
                "s3:GetBucketPublicAccessBlock",
                "arn:aws:s3:::uploads",
            ),
            (
                "DeletePublicAccessBlock",
                "/uploads?publicAccessBlock",
                "s3:PutBucketPublicAccessBlock",
                "arn:aws:s3:::uploads",
            ),
            (
                "GetBucketNotificationConfiguration",
                "/uploads?notification",
                "s3:GetBucketNotification",
                "arn:aws:s3:::uploads",
            ),
            (
                "GetBucketReplication",
                "/uploads?replication",
                "s3:GetReplicationConfiguration",
                "arn:aws:s3:::uploads",
            ),
        ] {
            let s3_sub = call("s3", op, "GET", path, None);
            assert_eq!(
                derive(&s3_sub).0,
                vec![Access::new(expected_action, expected_res)],
                "failed for {op}"
            );
        }

        let ecs_call = call(
            "ecs",
            "RegisterTaskDefinition",
            "POST",
            "/",
            Some(
                r#"{"family":"worker","taskRoleArn":"arn:aws:iam::123456789012:role/app-task","executionRoleArn":"arn:aws:iam::123456789012:role/ecs-exec"}"#,
            ),
        );
        assert_eq!(
            derive(&ecs_call).1,
            vec![
                Access::new("iam:PassRole", "arn:aws:iam::*:role/app-task"),
                Access::new("iam:PassRole", "arn:aws:iam::*:role/ecs-exec"),
            ]
        );
    }

    fn implied_of(trace: &Trace) -> Vec<(String, String)> {
        derive(trace)
            .1
            .into_iter()
            .map(|access| (access.action, access.resource))
            .collect()
    }

    fn pair(action: &str, resource: &str) -> (String, String) {
        (action.to_string(), resource.to_string())
    }

    #[test]
    fn implied_kms_reads_nested_queue_topic_and_table_keys() {
        let key = "arn:aws:kms:us-east-1:000000000000:key/app-key";
        let queue = call(
            "sqs",
            "CreateQueue",
            "POST",
            "/",
            Some(&format!(
                r#"{{"QueueName":"jobs","Attributes":{{"KmsMasterKeyId":"{key}"}}}}"#
            )),
        );
        assert_eq!(
            implied_of(&queue),
            vec![pair("kms:GenerateDataKey", "arn:aws:kms:*:*:key/app-key")]
        );
        let query_queue = call(
            "sqs",
            "SetQueueAttributes",
            "POST",
            "/",
            Some(
                "Action=SetQueueAttributes&QueueUrl=http%3A%2F%2Fh%2F0%2Fjobs&Attribute.1.Name=KmsMasterKeyId&Attribute.1.Value=alias%2Fjobs",
            ),
        );
        assert_eq!(
            implied_of(&query_queue),
            vec![pair("kms:GenerateDataKey", "arn:aws:kms:*:*:alias/jobs")]
        );
        let topic = call(
            "sns",
            "CreateTopic",
            "POST",
            "/",
            Some(
                "Action=CreateTopic&Name=alerts&Attributes.entry.1.key=KmsMasterKeyId&Attributes.entry.1.value=alias%2Falerts",
            ),
        );
        assert_eq!(
            implied_of(&topic),
            vec![pair("kms:GenerateDataKey", "arn:aws:kms:*:*:alias/alerts")]
        );
        let set_topic = call(
            "sns",
            "SetTopicAttributes",
            "POST",
            "/",
            Some(
                "Action=SetTopicAttributes&TopicArn=arn%3Aaws%3Asns%3Aus-east-1%3A0%3Aalerts&AttributeName=KmsMasterKeyId&AttributeValue=alias%2Falerts",
            ),
        );
        assert_eq!(
            implied_of(&set_topic),
            vec![pair("kms:GenerateDataKey", "arn:aws:kms:*:*:alias/alerts")]
        );
        let table = call(
            "dynamodb",
            "CreateTable",
            "POST",
            "/",
            Some(&format!(
                r#"{{"TableName":"orders","SSESpecification":{{"Enabled":true,"SSEType":"KMS","KMSMasterKeyId":"{key}"}}}}"#
            )),
        );
        assert_eq!(
            implied_of(&table),
            vec![pair("kms:DescribeKey", "arn:aws:kms:*:*:key/app-key")]
        );
        // The key of an encrypted queue or secret is not on the wire when it is used.
        for (service, operation, body) in [
            (
                "sqs",
                "SendMessage",
                r#"{"QueueUrl":"http://h/0/jobs","MessageBody":"x"}"#,
            ),
            ("secretsmanager", "GetSecretValue", r#"{"SecretId":"db"}"#),
        ] {
            assert!(implied_of(&call(service, operation, "POST", "/", Some(body))).is_empty());
        }
        // Standard SecureString parameters are encrypted directly.
        let parameter = call(
            "ssm",
            "PutParameter",
            "POST",
            "/",
            Some(r#"{"Name":"/app/db","Type":"SecureString","KeyId":"alias/app"}"#),
        );
        assert_eq!(
            implied_of(&parameter),
            vec![pair("kms:Encrypt", "arn:aws:kms:*:*:alias/app")]
        );
    }

    #[test]
    fn pass_role_reads_nested_target_destination_and_job_roles() {
        let role = |name: &str| pair("iam:PassRole", &format!("arn:aws:iam::*:role/{name}"));
        let targets = call(
            "events",
            "PutTargets",
            "POST",
            "/",
            Some(
                r#"{"Rule":"r","Targets":[{"Id":"1","Arn":"arn:aws:sqs:us-east-1:0:q"},{"Id":"2","Arn":"arn:aws:states:us-east-1:0:stateMachine:m","RoleArn":"arn:aws:iam::0:role/events-to-sfn"}]}"#,
            ),
        );
        assert_eq!(implied_of(&targets), vec![role("events-to-sfn")]);
        let schedule = call(
            "scheduler",
            "CreateSchedule",
            "POST",
            "/schedules/nightly",
            Some(
                r#"{"Name":"nightly","Target":{"Arn":"arn:aws:lambda:us-east-1:0:function:f","RoleArn":"arn:aws:iam::0:role/scheduler-invoke"}}"#,
            ),
        );
        assert_eq!(implied_of(&schedule), vec![role("scheduler-invoke")]);
        let firehose = call(
            "firehose",
            "CreateDeliveryStream",
            "POST",
            "/",
            Some(
                r#"{"DeliveryStreamName":"logs","KinesisStreamSourceConfiguration":{"KinesisStreamARN":"arn:aws:kinesis:us-east-1:0:stream/s","RoleARN":"arn:aws:iam::0:role/firehose-source"},"ExtendedS3DestinationConfiguration":{"BucketARN":"arn:aws:s3:::logs","RoleARN":"arn:aws:iam::0:role/firehose-s3","S3BackupConfiguration":{"RoleARN":"arn:aws:iam::0:role/firehose-s3"}}}"#,
            ),
        );
        assert_eq!(
            implied_of(&firehose),
            vec![role("firehose-source"), role("firehose-s3")]
        );
        let job = call(
            "glue",
            "UpdateJob",
            "POST",
            "/",
            Some(
                r#"{"JobName":"etl","JobUpdate":{"Role":"arn:aws:iam::0:role/glue-etl","Command":{}}}"#,
            ),
        );
        assert_eq!(implied_of(&job), vec![role("glue-etl")]);
    }

    #[test]
    fn batch_and_version_requests_use_the_iam_action_names() {
        for (service, operation, expected) in [
            ("sqs", "SendMessageBatch", "sqs:SendMessage"),
            ("sqs", "DeleteMessageBatch", "sqs:DeleteMessage"),
            (
                "sqs",
                "ChangeMessageVisibilityBatch",
                "sqs:ChangeMessageVisibility",
            ),
            ("sns", "PublishBatch", "sns:Publish"),
        ] {
            let batch = call(service, operation, "POST", "/", Some(r#"{"QueueUrl":"q"}"#));
            assert_eq!(access(&batch).0, expected, "{operation}");
        }
        for (operation, method, expected) in [
            ("GetObject", "GET", vec!["s3:GetObjectVersion"]),
            ("HeadObject", "HEAD", vec!["s3:GetObjectVersion"]),
            ("DeleteObject", "DELETE", vec!["s3:DeleteObjectVersion"]),
            (
                "GetObjectTagging",
                "GET",
                vec!["s3:GetObjectVersionTagging"],
            ),
            (
                "PutObjectTagging",
                "PUT",
                vec!["s3:PutObjectVersionTagging"],
            ),
            ("GetObjectAcl", "GET", vec!["s3:GetObjectVersionAcl"]),
            ("PutObjectAcl", "PUT", vec!["s3:PutObjectVersionAcl"]),
            (
                "GetObjectAttributes",
                "GET",
                vec!["s3:GetObjectVersion", "s3:GetObjectVersionAttributes"],
            ),
        ] {
            let versioned = call("s3", operation, method, "/b/k?versionId=3", None);
            let actions: Vec<String> = all(&versioned).into_iter().map(|(a, _)| a).collect();
            assert_eq!(actions, expected, "{operation}");
        }
        let attributes = call("s3", "GetObjectAttributes", "GET", "/b/k?attributes", None);
        assert_eq!(
            all(&attributes),
            vec![
                pair("s3:GetObject", "arn:aws:s3:::b/*"),
                pair("s3:GetObjectAttributes", "arn:aws:s3:::b/*"),
            ]
        );
        let mut copy = call("s3", "CopyObject", "PUT", "/dest/k", None);
        copy.request.headers = vec![HeaderTrace {
            name: "x-amz-copy-source".into(),
            value: "src/k?versionId=7".into(),
        }];
        assert_eq!(
            all(&copy)[1],
            pair("s3:GetObjectVersion", "arn:aws:s3:::src/*")
        );
        let lifecycle = call(
            "s3",
            "DeleteBucketLifecycle",
            "DELETE",
            "/b?lifecycle",
            None,
        );
        assert_eq!(access(&lifecycle).0, "s3:PutLifecycleConfiguration");
    }

    #[test]
    fn partiql_statements_need_their_verb_action_on_their_table() {
        let select = call(
            "dynamodb",
            "ExecuteStatement",
            "POST",
            "/",
            Some(r#"{"Statement":"SELECT * FROM \"orders\".\"byUser\" WHERE pk = ?"}"#),
        );
        assert_eq!(
            access(&select),
            pair(
                "dynamodb:PartiQLSelect",
                "arn:aws:dynamodb:*:*:table/orders/index/byUser"
            )
        );
        let batch = call(
            "dynamodb",
            "BatchExecuteStatement",
            "POST",
            "/",
            Some(
                r#"{"Statements":[{"Statement":"INSERT INTO orders VALUE {'pk': ?}"},{"Statement":"update \"carts\" SET n = 1 WHERE pk = ?"},{"Statement":"DELETE\nFROM audit WHERE pk = ?"}]}"#,
            ),
        );
        assert_eq!(
            all(&batch),
            vec![
                pair("dynamodb:PartiQLDelete", "arn:aws:dynamodb:*:*:table/audit"),
                pair(
                    "dynamodb:PartiQLInsert",
                    "arn:aws:dynamodb:*:*:table/orders"
                ),
                pair("dynamodb:PartiQLUpdate", "arn:aws:dynamodb:*:*:table/carts"),
            ]
        );
    }

    #[test]
    fn unreadable_bodies_stand_as_a_flagged_wildcard() {
        let mut cbor = call("sqs", "SendMessage", "POST", "/", None);
        cbor.request.body = Some(BodyTrace {
            text: None,
            hex: Some("a1".into()),
            encoding: None,
            bytes: 40,
            truncated: false,
            binary: true,
            sha256: String::new(),
        });
        let derived = derive_call(&cbor);
        assert_eq!(derived.own, vec![Access::new("sqs:SendMessage", "*")]);
        assert_eq!(derived.concrete_own, derived.own);
        assert_eq!(derived.unverified, derived.own);
        // The same call derives the same `*` every run: it can be locked.
        assert_eq!(derive_call(&cbor), derived);

        // A role a create call must pass is still needed when it cannot be read.
        let mut create = call(
            "lambda",
            "CreateFunction",
            "POST",
            "/2015-03-31/functions",
            None,
        );
        create.request.body = cbor.request.body.clone();
        let derived = derive_call(&create);
        assert_eq!(derived.implied, vec![Access::new("iam:PassRole", "*")]);
        assert!(
            derived
                .unverified
                .contains(&Access::new("iam:PassRole", "*"))
        );

        // A binary S3 upload names its resource in the path: nothing is unknown.
        let mut put = call("s3", "PutObject", "PUT", "/b/k", None);
        put.request.body = cbor.request.body.clone();
        assert!(derive_call(&put).unverified.is_empty());
    }
}
