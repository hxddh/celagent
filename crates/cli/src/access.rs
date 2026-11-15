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
    fn new(action: impl Into<String>, resource: impl Into<String>) -> Self {
        Access {
            action: action.into(),
            resource: resource.into(),
        }
    }
}

/// The access one call needs, plus permissions it implies on other resources
/// (the KMS key behind an SSE-KMS object, the role passed to Lambda).
pub fn derive(trace: &Trace) -> (Option<Access>, Vec<Access>) {
    let operation = trace.operation.as_str();
    if operation.is_empty()
        || operation == trace.method
        || trace.service == "aws"
        || !operation.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return (None, Vec::new());
    }
    let prefix = iam_prefix(&trace.service);
    let Some(action) = action_name(prefix, operation) else {
        return (None, Vec::new());
    };
    let body = Body::parse(trace);
    let resource = resource(prefix, operation, trace, &body);
    let implied = implied(prefix, operation, trace, &body);
    (Some(Access::new(action, resource)), implied)
}

/// SigV4 signing names that differ from the IAM action prefix.
fn iam_prefix(service: &str) -> &str {
    match service {
        "monitoring" => "cloudwatch",
        "email" => "ses",
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
        _ => operation,
    };
    Some(format!("{prefix}:{mapped}"))
}

/// The request body, parsed once: JSON fields or form fields.
enum Body {
    Json(Value),
    Form(Vec<(String, String)>),
    None,
}

impl Body {
    fn parse(trace: &Trace) -> Body {
        let Some(text) = trace
            .request
            .body
            .as_ref()
            .and_then(|body| body.text.as_deref())
        else {
            return Body::None;
        };
        let trimmed = text.trim_start();
        if trimmed.starts_with('{') {
            return serde_json::from_str(text).map_or(Body::None, Body::Json);
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
            Body::Form(fields) => fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone()),
            Body::None => None,
        }
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
        let mut out = Vec::new();
        if let Body::Json(value) = self {
            walk(value, &mut out);
        }
        out.sort();
        out.dedup();
        out
    }
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
            || (part.len() >= 4
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

fn resource(prefix: &str, operation: &str, trace: &Trace, body: &Body) -> String {
    match prefix {
        "s3" => s3_resource(operation, trace),
        "dynamodb" => {
            let tables = body.tables();
            match tables.as_slice() {
                [] => "*".to_string(),
                [one] => match body.field("IndexName") {
                    Some(index) => arn(
                        "dynamodb",
                        &format!("table/{}/index/{}", normalize(one), normalize(&index)),
                    ),
                    None => arn("dynamodb", &format!("table/{}", normalize(one))),
                },
                // Several tables: one entry per call keeps the lock readable.
                many => arn(
                    "dynamodb",
                    &format!(
                        "table/{}",
                        many.iter()
                            .map(|table| normalize(table))
                            .collect::<Vec<_>>()
                            .join(",")
                    ),
                ),
            }
        }
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
                let name = secret
                    .strip_prefix("arn:")
                    .map(|_| last_segment(&secret).to_string())
                    .unwrap_or(secret);
                arn("secretsmanager", &format!("secret:{}-*", normalize(&name)))
            })
            .unwrap_or_else(|| "*".to_string()),
        "kms" => body
            .field("KeyId")
            .map(|key| kms_key(&key))
            .unwrap_or_else(|| "*".to_string()),
        "sts" => body
            .field("RoleArn")
            .map(|role| format!("arn:aws:iam::*:role/{}", normalize(last_segment(&role))))
            .unwrap_or_else(|| "*".to_string()),
        "iam" => body
            .field("RoleName")
            .map(|role| format!("arn:aws:iam::*:role/{}", normalize(&role)))
            .or_else(|| {
                body.field("UserName")
                    .map(|user| format!("arn:aws:iam::*:user/{}", normalize(&user)))
            })
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

fn kms_key(key: &str) -> String {
    if key.starts_with("alias/") {
        arn("kms", &normalize(key))
    } else {
        arn("kms", &format!("key/{}", normalize(last_segment(key))))
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
    let host = trace
        .host
        .rsplit_once(':')
        .map_or(trace.host.as_str(), |(name, _)| name);
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
    }
    // Passing a role to a service needs iam:PassRole on that role.
    let passed_role = match (prefix, operation) {
        ("lambda", "CreateFunction" | "UpdateFunctionConfiguration") => body.field("Role"),
        ("states", "CreateStateMachine" | "UpdateStateMachine") => body.field("roleArn"),
        ("ecs", "RegisterTaskDefinition") => body
            .field("taskRoleArn")
            .or_else(|| body.field("executionRoleArn")),
        _ => None,
    };
    if let Some(role) = passed_role {
        out.push(Access::new(
            "iam:PassRole",
            format!("arn:aws:iam::*:role/{}", normalize(last_segment(&role))),
        ));
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
            pending: false,
            error: None,
            problem: None,
            action: None,
            resource: None,
            implied: Vec::new(),
            new: false,
            request: Message {
                headers: Vec::new(),
                content_type: Some(if body.is_some_and(|b| b.starts_with('{')) {
                    "application/x-amz-json-1.0".into()
                } else {
                    "application/x-www-form-urlencoded".into()
                }),
                body: body.map(|text| BodyTrace {
                    text: Some(text.into()),
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
        let access = derive(trace).0.expect("has access");
        (access.action, access.resource)
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
        assert_eq!(
            access(&call(
                "dynamodb",
                "BatchWriteItem",
                "POST",
                "/",
                Some(r#"{"RequestItems":{"orders":[],"users":[]}}"#)
            ))
            .1,
            "arn:aws:dynamodb:*:*:table/orders,users"
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
    fn calls_without_a_named_operation_or_permission_have_no_access() {
        assert!(
            derive(&call("s3", "PATCH", "PATCH", "/b/k", None))
                .0
                .is_none()
        );
        assert!(derive(&call("aws", "GET", "GET", "/", None)).0.is_none());
        assert!(
            derive(&call(
                "sts",
                "GetCallerIdentity",
                "POST",
                "/",
                Some("Action=GetCallerIdentity")
            ))
            .0
            .is_none()
        );
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
        let (main, implied) = derive(&create);
        assert_eq!(main.unwrap().resource, "arn:aws:lambda:*:*:function:f");
        assert_eq!(
            implied,
            vec![Access::new(
                "iam:PassRole",
                "arn:aws:iam::*:role/lambda-exec"
            )]
        );
    }
}
