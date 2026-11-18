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
}

impl CallAccess {
    pub fn from_gate(access: Access, gate: Option<&crate::lock::Gate>) -> Self {
        let new = gate.is_some_and(|gate| !gate.allows(&access));
        CallAccess {
            action: access.action,
            resource: access.resource,
            new,
        }
    }

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
        }
    }
}

/// Whether deriving IAM access for `service` requires inspecting the request
/// body (as opposed to REST-URI services like S3, Lambda, and Bedrock whose
/// operation and resource live in the path and headers).
pub fn needs_request_body(service: &str, advertised_operation: Option<&str>) -> bool {
    match service {
        "s3" | "s3-object-lambda" | "bedrock" | "bedrock-runtime" | "execute-api"
        | "apigateway" | "appconfig" | "appconfigdata" => false,
        // Lambda CreateFunction / UpdateFunctionConfiguration may pass a Role in JSON,
        // whereas Invoke / GetFunction / DeleteFunction only use the URL path.
        "lambda" => matches!(
            advertised_operation,
            None | Some("CreateFunction" | "UpdateFunctionConfiguration")
        ),
        _ => true,
    }
}

/// Whether a finished call had no recognizable IAM mapping (excluding calls
/// like `sts:GetCallerIdentity` that genuinely need no permission).
pub fn is_unmapped(trace: &Trace) -> bool {
    !trace.pending
        && trace.access.is_empty()
        && trace.implied.is_empty()
        && !(trace.service == "sts" && trace.operation == "GetCallerIdentity")
}

/// The access one call needs, and the permissions it implies on other
/// resources (the KMS key behind an SSE-KMS object, the role passed to Lambda).
///
/// One call can need several permissions: a batch names several tables, a
/// DynamoDB transaction needs each item's own action, and a copy reads its
/// source as well as writing its destination.
pub fn derive(trace: &Trace) -> (Vec<Access>, Vec<Access>) {
    let operation = trace.operation.as_str();
    if operation.is_empty()
        || operation == trace.method
        || trace.service == "aws"
        || !operation.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return (Vec::new(), Vec::new());
    }
    let prefix = iam_prefix(&trace.service);
    let body = Body::parse(trace);
    let implied = implied(prefix, operation, trace, &body);
    if prefix == "dynamodb" && operation.starts_with("Transact") {
        return (transaction(&body), implied);
    }
    let Some(action) = action_name(prefix, operation) else {
        return (Vec::new(), Vec::new());
    };
    let mut own: Vec<Access> = resources(prefix, operation, trace, &body)
        .into_iter()
        .map(|resource| Access::new(action.clone(), resource))
        .collect();
    if prefix == "s3"
        && let Some(source) = copy_source(trace)
    {
        own.push(Access::new("s3:GetObject", source));
    }
    (own, implied)
}

/// `TransactWriteItems` / `TransactGetItems` are not IAM actions: each item
/// needs its own (`PutItem`, `UpdateItem`, `DeleteItem`, `ConditionCheckItem`,
/// `GetItem`) on its table. A body that cannot be read yields nothing rather
/// than a guess.
fn transaction(body: &Body) -> Vec<Access> {
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
                        arn("dynamodb", &format!("table/{}", normalize(table))),
                    ))
                })
                .collect()
        }
        Body::TruncatedJson(raw) => scan_truncated_transact(raw),
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

fn scan_truncated_transact(raw: &str) -> Vec<Access> {
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
                    arn("dynamodb", &format!("table/{}", normalize(&table))),
                ));
            }
        }
    }
    out
}

/// The bucket a `CopyObject` / `UploadPartCopy` reads from, as an object ARN.
fn copy_source(trace: &Trace) -> Option<String> {
    let value = trace
        .request
        .headers
        .iter()
        .find(|header| header.name.eq_ignore_ascii_case("x-amz-copy-source"))?
        .value
        .as_str();
    let decoded = wire::query_param(&format!("?v={value}"), "v")?;
    let path = decoded.split('?').next().unwrap_or(&decoded);
    if path.starts_with("arn:") {
        // An access point or Outposts ARN, not a bucket name.
        return Some("*".to_string());
    }
    let bucket = path.trim_start_matches('/').split('/').next()?;
    (!bucket.is_empty()).then(|| format!("arn:aws:s3:::{}/*", normalize(bucket)))
}

/// SigV4 signing names that differ from the IAM action prefix.
fn iam_prefix(service: &str) -> &str {
    match service {
        "monitoring" => "cloudwatch",
        "email" | "sesv2" => "ses",
        "bedrock-runtime" => "bedrock",
        "streams.dynamodb" | "dynamodbstreams" => "dynamodb",
        "appconfigdata" => "appconfig",
        "logs" => "logs",
        "execute-api" => "execute-api",
        "s3-object-lambda" => "s3-object-lambda",
        other => other,
    }
}

/// The IAM action for an operation; `None` when the call needs no permission.
fn action_name(prefix: &str, operation: &str) -> Option<String> {
    let mapped = match (prefix, operation) {
        ("sts", "GetCallerIdentity") => return None,
        ("s3", "ListObjects" | "ListObjectsV2" | "HeadBucket") => "ListBucket",
        ("s3", "ListBuckets") => "ListAllMyBuckets",
        ("s3", "HeadObject" | "GetObjectAttributes") => "GetObject",
        ("s3", "CopyObject" | "UploadPart" | "UploadPartCopy" | "CreateMultipartUpload") => {
            "PutObject"
        }
        ("s3", "CompleteMultipartUpload") => "PutObject",
        ("s3", "DeleteObjects") => "DeleteObject",
        ("s3", "ListObjectVersions") => "ListBucketVersions",
        ("s3", "ListMultipartUploads") => "ListBucketMultipartUploads",
        ("s3", "ListParts") => "ListMultipartUploadParts",
        ("s3", "GetBucketCors") => "GetBucketCORS",
        ("s3", "PutBucketCors") => "PutBucketCORS",
        ("s3", "DeleteBucketCors") => "PutBucketCORS",
        ("s3", "GetBucketLifecycleConfiguration") => "GetLifecycleConfiguration",
        ("s3", "PutBucketLifecycleConfiguration") => "PutLifecycleConfiguration",
        ("s3", "GetBucketEncryption") => "GetEncryptionConfiguration",
        ("s3", "PutBucketEncryption") => "PutEncryptionConfiguration",
        ("s3", "DeleteObjectTagging") => "DeleteObjectTagging",
        ("lambda", "Invoke" | "InvokeAsync" | "InvokeWithResponseStream") => "InvokeFunction",
        ("lambda", "GetFunction") => "GetFunction",
        ("bedrock", "Converse") => "InvokeModel",
        ("bedrock", "ConverseStream") => "InvokeModelWithResponseStream",
        _ => operation,
    };
    Some(format!("{prefix}:{mapped}"))
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

fn arn(service: &str, rest: &str) -> String {
    format!("arn:aws:{service}:*:*:{rest}")
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
/// DynamoDB batch, and one per parameter for SSM `GetParameters`/`DeleteParameters`.
fn resources(prefix: &str, operation: &str, trace: &Trace, body: &Body) -> Vec<String> {
    if prefix == "ssm" {
        let names = body.string_list("Names");
        if !names.is_empty() {
            return names.iter().map(|name| ssm_parameter(name)).collect();
        }
    }
    if prefix != "dynamodb" {
        return vec![resource(prefix, operation, trace, body)];
    }
    let tables = body.tables();
    match tables.as_slice() {
        [] => vec!["*".to_string()],
        [one] => vec![match body.field("IndexName") {
            Some(index) => arn(
                "dynamodb",
                &format!("table/{}/index/{}", normalize(one), normalize(&index)),
            ),
            None => arn("dynamodb", &format!("table/{}", normalize(one))),
        }],
        many => many
            .iter()
            .map(|table| arn("dynamodb", &format!("table/{}", normalize(table))))
            .collect(),
    }
}

fn resource(prefix: &str, operation: &str, trace: &Trace, body: &Body) -> String {
    match prefix {
        "s3" => s3_resource(operation, trace),
        "sqs" => body
            .field("QueueUrl")
            .or_else(|| body.field("QueueName"))
            .map(|queue| arn("sqs", &normalize(last_segment(&queue))))
            .unwrap_or_else(|| "*".to_string()),
        "sns" => body
            .field("TopicArn")
            .or_else(|| body.field("TargetArn"))
            .map(|topic| last_segment(&topic).to_string())
            .or_else(|| {
                (operation == "CreateTopic")
                    .then(|| body.field("Name"))
                    .flatten()
            })
            .map(|topic| arn("sns", &normalize(&topic)))
            .unwrap_or_else(|| "*".to_string()),
        "lambda" => lambda_function(&trace.path)
            .or_else(|| body.field("FunctionName"))
            .map(|function| {
                arn(
                    "lambda",
                    &format!("function:{}", normalize(last_segment(&function))),
                )
            })
            .unwrap_or_else(|| "*".to_string()),
        "secretsmanager" => body
            .field("SecretId")
            .or_else(|| body.field("Name"))
            .map(|secret| {
                arn(
                    "secretsmanager",
                    &format!("secret:{}-*", secret_name(&secret)),
                )
            })
            .unwrap_or_else(|| "*".to_string()),
        "ssm" => body
            .field("Name")
            .map(|name| ssm_parameter(&name))
            .or_else(|| {
                body.field("Path").map(|path| {
                    let clean = path.trim_matches('/');
                    if clean.is_empty() {
                        arn("ssm", "parameter/*")
                    } else {
                        arn("ssm", &format!("parameter/{}/*", normalize_path(clean)))
                    }
                })
            })
            .unwrap_or_else(|| "*".to_string()),
        "logs" => body
            .field("logGroupName")
            .map(|group| {
                let raw = group
                    .split_once(":log-group:")
                    .map_or(group.as_str(), |(_, rest)| rest)
                    .trim_end_matches(":*");
                arn("logs", &format!("log-group:{}:*", normalize_path(raw)))
            })
            .unwrap_or_else(|| "*".to_string()),
        "kinesis" => body
            .field("StreamARN")
            .and_then(|stream| stream.split(":stream/").nth(1).map(str::to_string))
            .or_else(|| body.field("StreamName"))
            .map(|stream| arn("kinesis", &format!("stream/{}", normalize_path(&stream))))
            .unwrap_or_else(|| "*".to_string()),
        "bedrock" => bedrock_model(&trace.path)
            .or_else(|| body.field("modelId"))
            .map(|model| bedrock_resource(&model))
            .unwrap_or_else(|| "*".to_string()),
        "kms" => body
            .field("KeyId")
            .map(|key| kms_key(&key))
            .unwrap_or_else(|| "*".to_string()),
        "sts" => body
            .field("RoleArn")
            .map(|role| iam_entity("role", &role))
            .unwrap_or_else(|| "*".to_string()),
        "iam" => body
            .field("RoleName")
            .map(|role| iam_entity("role", &role))
            .or_else(|| body.field("UserName").map(|user| iam_entity("user", &user)))
            .unwrap_or_else(|| "*".to_string()),
        "events" => body
            .field("EventBusName")
            .map(|bus| {
                arn(
                    "events",
                    &format!("event-bus/{}", normalize(last_segment(&bus))),
                )
            })
            .unwrap_or_else(|| "*".to_string()),
        "states" => body
            .field("stateMachineArn")
            .map(|machine| {
                arn(
                    "states",
                    &format!("stateMachine:{}", normalize(last_segment(&machine))),
                )
            })
            .unwrap_or_else(|| "*".to_string()),
        _ => "*".to_string(),
    }
}

/// Preserve hierarchical secret names (`db/password`) whether given by name or
/// by full ARN (`arn:aws:secretsmanager:...:secret:db/password-AbCdEf`).
fn secret_name(secret: &str) -> String {
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
        normalize_path(base)
    } else {
        normalize_path(secret.trim_start_matches('/'))
    }
}

fn ssm_parameter(name: &str) -> String {
    let raw = name
        .split_once(":parameter/")
        .map_or(name, |(_, rest)| rest)
        .trim_start_matches('/');
    arn("ssm", &format!("parameter/{}", normalize_path(raw)))
}

/// Preserve IAM role/user paths (`service-role/my-role`) from names or ARNs.
fn iam_entity(kind: &str, value: &str) -> String {
    let needle = format!(":{kind}/");
    let raw = value
        .split_once(&needle)
        .map_or(value, |(_, rest)| rest)
        .trim_start_matches('/');
    format!("arn:aws:iam::*:{kind}/{}", normalize_path(raw))
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
    let decoded = wire::query_param(&format!("?v={raw}"), "v").unwrap_or_default();
    (!decoded.is_empty()).then_some(decoded)
}

fn bedrock_resource(model: &str) -> String {
    if model.starts_with("arn:") {
        let Some(rest) = model.splitn(6, ':').nth(5).filter(|s| !s.is_empty()) else {
            return "*".to_string();
        };
        if let Some((kind, id)) = rest.split_once('/') {
            return match kind {
                "foundation-model" => arn("bedrock", rest),
                "inference-profile" if is_system_inference_profile(id) => arn("bedrock", rest),
                _ => arn("bedrock", &format!("{kind}/{}", normalize_path(id))),
            };
        }
        return arn("bedrock", rest);
    }
    if is_system_inference_profile(model) {
        arn("bedrock", &format!("inference-profile/{model}"))
    } else if is_foundation_model_id(model) {
        arn("bedrock", &format!("foundation-model/{model}"))
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

fn kms_key(key: &str) -> String {
    let raw = key
        .split_once(":alias/")
        .map(|(_, alias)| format!("alias/{alias}"))
        .unwrap_or_else(|| key.to_string());
    if raw.starts_with("alias/") {
        arn("kms", &normalize_path(&raw))
    } else {
        arn("kms", &format!("key/{}", normalize(last_segment(&raw))))
    }
}

/// `/2015-03-31/functions/NAME/...` → `NAME`.
fn lambda_function(path: &str) -> Option<String> {
    let bare = path.split('?').next().unwrap_or(path);
    let mut segments = bare.split('/');
    segments.find(|segment| *segment == "functions")?;
    let name = segments.next()?;
    let name = wire::query_param(&format!("?v={name}"), "v").unwrap_or_default();
    (!name.is_empty()).then_some(name)
}

fn s3_resource(operation: &str, trace: &Trace) -> String {
    let bare = trace.path.split('?').next().unwrap_or(&trace.path);
    let host = wire::strip_port(&trace.host);
    let host_bucket = (host.ends_with(".localhost") || host.contains(".s3"))
        .then(|| host.split('.').next().unwrap_or(""))
        .filter(|bucket| !bucket.is_empty());
    let mut segments = bare.split('/').filter(|segment| !segment.is_empty());
    let bucket = match host_bucket {
        Some(bucket) => Some(bucket.to_string()),
        None => segments.next().map(str::to_string),
    };
    let has_key = segments.next().is_some();
    let Some(bucket) = bucket else {
        return "*".to_string();
    };
    let bucket = normalize(&bucket);
    let bucket_level = matches!(
        operation,
        "ListObjects"
            | "ListObjectsV2"
            | "HeadBucket"
            | "ListObjectVersions"
            | "ListMultipartUploads"
    );
    if has_key || (operation == "DeleteObjects" && !bucket_level) {
        format!("arn:aws:s3:::{bucket}/*")
    } else {
        format!("arn:aws:s3:::{bucket}")
    }
}

/// Permissions a call needs on resources it does not name in its target.
fn implied(prefix: &str, operation: &str, trace: &Trace, body: &Body) -> Vec<Access> {
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
                .map(|key| kms_key(&key))
                .unwrap_or_else(|| "*".to_string());
            let action = match operation {
                "GetObject" | "HeadObject" | "CopyObject" => Some("kms:Decrypt"),
                "PutObject"
                | "CreateMultipartUpload"
                | "UploadPart"
                | "CompleteMultipartUpload" => Some("kms:GenerateDataKey"),
                _ => None,
            };
            if let Some(action) = action {
                out.push(Access::new(action, key));
            }
        }
    } else if let Some(key) = body
        .field("KmsKeyId")
        .or_else(|| body.field("KMSMasterKeyId"))
        .or_else(|| body.field("KmsMasterKeyId"))
        .or_else(|| body.field("KeyId").filter(|_| prefix == "ssm"))
    {
        let kms_action = match (prefix, operation) {
            ("secretsmanager", "GetSecretValue") => Some("kms:Decrypt"),
            ("secretsmanager", "CreateSecret" | "PutSecretValue" | "UpdateSecret") => {
                Some("kms:GenerateDataKey")
            }
            ("ssm", "PutParameter") => Some("kms:GenerateDataKey"),
            ("sns", "Publish" | "PublishBatch") | ("sqs", "SendMessage" | "SendMessageBatch") => {
                Some("kms:GenerateDataKey")
            }
            _ => None,
        };
        if let Some(action) = kms_action {
            out.push(Access::new(action, kms_key(&key)));
        }
    }
    // Passing a role to a service needs iam:PassRole on that role.
    let passed_role = match (prefix, operation) {
        ("lambda", "CreateFunction" | "UpdateFunctionConfiguration") => body.field("Role"),
        ("states", "CreateStateMachine" | "UpdateStateMachine") => body.field("roleArn"),
        ("ecs", "RegisterTaskDefinition") => body
            .field("taskRoleArn")
            .or_else(|| body.field("executionRoleArn")),
        ("events", "PutRule" | "PutTargets") => body.field("RoleArn"),
        _ => None,
    };
    if let Some(role) = passed_role {
        out.push(Access::new("iam:PassRole", iam_entity("role", &role)));
    }
    out
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
                    "s3:GetObject".to_string(),
                    "arn:aws:s3:::uploads/*".to_string()
                ),
            ]
        );
        copy.request.headers[0].value = "/uploads/a.png".into();
        assert_eq!(all(&copy)[1].1, "arn:aws:s3:::uploads/*");
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
        assert_eq!(
            derive(&batch).0,
            vec![
                Access::new(
                    "dynamodb:BatchWriteItem",
                    "arn:aws:dynamodb:*:*:table/audit"
                ),
                Access::new(
                    "dynamodb:BatchWriteItem",
                    "arn:aws:dynamodb:*:*:table/orders"
                ),
            ]
        );

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
            vec![Access::new(
                "dynamodb:PutItem",
                "arn:aws:dynamodb:*:*:table/orders"
            )]
        );
    }
}
