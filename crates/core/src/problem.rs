//! What went wrong with a call, in one line.
//!
//! AWS protocols report errors in three shapes: a JSON body (`__type` /
//! `message`), an XML body (`<Code>` / `<Message>`, including the Query
//! protocol's `<Error>`), or the `x-amzn-ErrorType` header. This module reads
//! whichever is present and adds a short hint for the handful of failures that
//! have one obvious cause in local development. No per-service tables.

use serde::{Deserialize, Serialize};

use crate::trace::HeaderTrace;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Problem {
    /// AWS error code (`NoSuchKey`), or the HTTP reason when none was sent.
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// One-sentence likely cause, when the code has an obvious one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// The problem of a completed call, or `None` for a success.
pub fn from_response(status: u16, headers: &[HeaderTrace], body: Option<&str>) -> Option<Problem> {
    if status < 400 {
        return None;
    }
    let body = body.unwrap_or("");
    let (body_code, message) = from_json(body)
        .or_else(|| from_xml(body))
        .unwrap_or_default();
    let code = body_code
        .or_else(|| header_code(headers))
        .unwrap_or_else(|| reason_code(status));
    let hint = hint(&code, status).map(str::to_string);
    Some(Problem {
        code,
        message,
        hint,
    })
}

/// The problem of a call that never reached the emulator.
pub fn unreachable(upstream: &str, error: &str) -> Problem {
    Problem {
        code: "EmulatorUnreachable".to_string(),
        message: Some(error.to_string()),
        hint: Some(format!(
            "Lenslet could not reach the emulator at {upstream}. Is AWS Emulator running on that port?"
        )),
    }
}

/// Protocol-accurate AWS `AccessDenied` / `AccessDeniedException` wire reply
/// synthesized when `--enforce` blocks a call locally.
pub struct BlockedWire {
    pub status: u16,
    pub content_type: &'static str,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
    pub problem: Problem,
}

pub fn blocked(
    service: &str,
    request_headers: &[HeaderTrace],
    request_content_type: Option<&str>,
    denied: &crate::lock::Denial,
    gate_label: &str,
    request_id: &str,
) -> BlockedWire {
    let (message, hint) = if denied.unverified {
        (
            format!(
                "User is not authorized to perform: {} on resource: * (the resource could not be read from the request body; blocked by Lenslet --enforce against {gate_label})",
                denied.action
            ),
            format!(
                "Blocked locally by `lenslet run --enforce`: the request body was not readable (binary or truncated), so `{}` stands on `*`, which `{gate_label}` does not permit.",
                denied.action
            ),
        )
    } else {
        (
            format!(
                "User is not authorized to perform: {} on resource: {} (blocked by Lenslet --enforce against {gate_label})",
                denied.action, denied.resource
            ),
            match denied.decision.by.as_deref() {
                Some(statement) => format!(
                    "Blocked locally by `lenslet run --enforce`: `{}` on `{}` is denied by statement `{statement}` of `{gate_label}`.",
                    denied.action, denied.resource
                ),
                None => format!(
                    "Blocked locally by `lenslet run --enforce`: `{}` on `{}` is not permitted by `{gate_label}`. Run `lenslet run --update` if intended.",
                    denied.action, denied.resource
                ),
            },
        )
    };

    let has_target = request_headers
        .iter()
        .any(|h| h.name.eq_ignore_ascii_case("x-amz-target"));
    let is_form = request_content_type == Some("application/x-www-form-urlencoded");

    if matches!(service, "s3" | "s3-object-lambda") {
        let escaped = xml_escape(&message);
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>AccessDenied</Code><Message>{escaped}</Message><RequestId>{request_id}</RequestId></Error>"
        );
        BlockedWire {
            status: 403,
            content_type: "application/xml",
            headers: vec![("x-amz-request-id", request_id.to_string())],
            body,
            problem: Problem {
                code: "AccessDenied".to_string(),
                message: Some(message),
                hint: Some(hint),
            },
        }
    } else if !has_target
        && (is_form
            || matches!(
                service,
                "sts"
                    | "iam"
                    | "sns"
                    | "sqs"
                    | "email"
                    | "ses"
                    | "monitoring"
                    | "cloudwatch"
                    | "ec2"
            ))
    {
        let escaped = xml_escape(&message);
        let body = format!(
            "<ErrorResponse><Error><Type>Sender</Type><Code>AccessDenied</Code><Message>{escaped}</Message></Error><RequestId>{request_id}</RequestId></ErrorResponse>"
        );
        BlockedWire {
            status: 403,
            content_type: "text/xml; charset=utf-8",
            headers: vec![("x-amzn-requestid", request_id.to_string())],
            body,
            problem: Problem {
                code: "AccessDenied".to_string(),
                message: Some(message),
                hint: Some(hint),
            },
        }
    } else if has_target {
        let content_type = if request_content_type == Some("application/x-amz-json-1.1") {
            "application/x-amz-json-1.1"
        } else {
            "application/x-amz-json-1.0"
        };
        let body = serde_json::json!({
            "__type": "com.amazonaws.iam#AccessDeniedException",
            "message": message,
            "Message": message,
        })
        .to_string();
        BlockedWire {
            status: 400,
            content_type,
            headers: vec![
                ("x-amzn-errortype", "AccessDeniedException".to_string()),
                ("x-amzn-requestid", request_id.to_string()),
            ],
            body,
            problem: Problem {
                code: "AccessDeniedException".to_string(),
                message: Some(message),
                hint: Some(hint),
            },
        }
    } else {
        let body = serde_json::json!({
            "message": message,
            "Message": message,
        })
        .to_string();
        BlockedWire {
            status: 403,
            content_type: "application/json",
            headers: vec![
                ("x-amzn-errortype", "AccessDeniedException".to_string()),
                ("x-amzn-requestid", request_id.to_string()),
            ],
            body,
            problem: Problem {
                code: "AccessDeniedException".to_string(),
                message: Some(message),
                hint: Some(hint),
            },
        }
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `com.amazon.coral.service#ResourceNotFoundException:http://...` → `ResourceNotFoundException`.
fn normalize(code: &str) -> Option<String> {
    let code = code.rsplit('#').next().unwrap_or(code);
    let code = code.split(':').next().unwrap_or(code).trim();
    (!code.is_empty()).then(|| code.to_string())
}

fn from_json(body: &str) -> Option<(Option<String>, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    let object = value.as_object()?;
    // JSON-wrapped Query errors nest under "Error".
    let object = object
        .get("Error")
        .and_then(|error| error.as_object())
        .unwrap_or(object);
    let text = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| object.get(*key).and_then(|value| value.as_str()))
            .map(str::to_string)
    };
    let code = text(&["__type", "code", "Code", "errorType"]).and_then(|code| normalize(&code));
    let message = text(&["message", "Message", "errorMessage"]).filter(|m| !m.is_empty());
    (code.is_some() || message.is_some()).then_some((code, message))
}

fn from_xml(body: &str) -> Option<(Option<String>, Option<String>)> {
    let code = xml_element(body, "Code").and_then(|code| normalize(&code));
    let message = xml_element(body, "Message").filter(|m| !m.is_empty());
    (code.is_some() || message.is_some()).then_some((code, message))
}

fn xml_element(body: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = start + body[start..].find(&close)?;
    Some(
        body[start..end]
            .trim()
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

fn header_code(headers: &[HeaderTrace]) -> Option<String> {
    headers.iter().find_map(|header| {
        let name = header.name.to_ascii_lowercase();
        match name.as_str() {
            "x-amzn-errortype" => normalize(&header.value),
            // `AWS.SimpleQueueService.NonExistentQueue;Sender`
            "x-amzn-query-error" => normalize(header.value.split(';').next().unwrap_or("")),
            _ => None,
        }
    })
}

fn reason_code(status: u16) -> String {
    http::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .map(|reason| reason.replace([' ', '-'], ""))
        .unwrap_or_else(|| format!("Http{status}"))
}

/// The one obvious cause for common local-development failures.
pub fn hint(code: &str, status: u16) -> Option<&'static str> {
    let is = |names: &[&str]| names.iter().any(|name| code.eq_ignore_ascii_case(name));
    if is(&[
        "SignatureDoesNotMatch",
        "InvalidSignatureException",
        "IncompleteSignature",
        "IncompleteSignatureException",
    ]) {
        return Some(
            "The SigV4 signature did not verify: the request changed after signing, or the secret key is not the emulator's.",
        );
    }
    if is(&[
        "UnrecognizedClientException",
        "InvalidClientTokenId",
        "InvalidAccessKeyId",
        "AuthFailure",
        "ExpiredToken",
        "ExpiredTokenException",
        "MissingAuthenticationToken",
        "MissingAuthenticationTokenException",
    ]) {
        return Some(
            "The emulator does not know these credentials. Use the emulator's credentials file (`lenslet run` sets it), not real AWS keys.",
        );
    }
    if is(&[
        "AuthorizationHeaderMalformed",
        "IllegalLocationConstraintException",
        "InvalidLocationConstraint",
        "PermanentRedirect",
    ]) {
        return Some(
            "Region mismatch: the request is signed for a different region than the resource.",
        );
    }
    if is(&[
        "AccessDenied",
        "AccessDeniedException",
        "UnauthorizedOperation",
        "Forbidden",
    ]) {
        return Some("Denied by the emulator's IAM evaluation for this principal.");
    }
    if is(&[
        "NotImplemented",
        "UnknownOperationException",
        "InvalidAction",
        "UnsupportedOperation",
        "UnsupportedOperationException",
    ]) || status == 501
    {
        return Some(
            "The emulator does not implement this operation, or the SDK sent an unknown action.",
        );
    }
    let lower = code.to_ascii_lowercase();
    if lower.starts_with("nosuch")
        || lower.ends_with("notfound")
        || lower.ends_with("notfoundexception")
        || lower.contains("nonexistent")
        || lower.contains("doesnotexist")
    {
        return Some(
            "The resource does not exist in the emulator. Was it created in this run, in the same region?",
        );
    }
    if lower.contains("alreadyexists")
        || lower.contains("alreadyowned")
        || is(&[
            "ResourceInUseException",
            "ConflictException",
            "EntityAlreadyExists",
        ])
    {
        return Some("The resource already exists, probably left over from an earlier run.");
    }
    if is(&[
        "ThrottlingException",
        "Throttling",
        "TooManyRequestsException",
        "SlowDown",
        "RequestLimitExceeded",
        "ProvisionedThroughputExceededException",
    ]) {
        return Some("Throttled. Retry with backoff.");
    }
    if is(&[
        "ValidationException",
        "ValidationError",
        "InvalidParameterValue",
        "InvalidParameterValueException",
        "InvalidParameterException",
        "InvalidParameterCombination",
        "MissingParameter",
        "InvalidRequest",
        "InvalidArgument",
        "MalformedXML",
        "SerializationException",
    ]) {
        return Some("The emulator rejected the request parameters; the message names the field.");
    }
    if status >= 500 {
        return Some(
            "The emulator failed internally: an emulator bug or unsupported input, not a client error.",
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str, value: &str) -> HeaderTrace {
        HeaderTrace {
            name: name.to_string(),
            value: value.to_string(),
        }
    }

    #[test]
    fn successes_have_no_problem() {
        assert_eq!(from_response(200, &[], Some("{}")), None);
        assert_eq!(from_response(304, &[], None), None);
    }

    #[test]
    fn reads_json_protocol_errors() {
        let problem = from_response(
            400,
            &[],
            Some(r#"{"__type":"com.amazonaws.dynamodb.v20120810#ResourceNotFoundException","message":"Requested resource not found"}"#),
        )
        .unwrap();
        assert_eq!(problem.code, "ResourceNotFoundException");
        assert_eq!(
            problem.message.as_deref(),
            Some("Requested resource not found")
        );
        assert!(problem.hint.unwrap().contains("does not exist"));
    }

    #[test]
    fn reads_xml_and_query_errors() {
        let s3 = r#"<?xml version="1.0"?><Error><Code>NoSuchKey</Code><Message>The specified key does not exist.</Message></Error>"#;
        let problem = from_response(404, &[], Some(s3)).unwrap();
        assert_eq!(problem.code, "NoSuchKey");
        assert_eq!(
            problem.message.as_deref(),
            Some("The specified key does not exist.")
        );

        let query = r#"<ErrorResponse><Error><Type>Sender</Type><Code>InvalidParameterValue</Code><Message>Value &lt;x&gt; is invalid</Message></Error></ErrorResponse>"#;
        let problem = from_response(400, &[], Some(query)).unwrap();
        assert_eq!(problem.code, "InvalidParameterValue");
        assert_eq!(problem.message.as_deref(), Some("Value <x> is invalid"));
        assert!(problem.hint.is_some());
    }

    #[test]
    fn falls_back_to_headers_then_status() {
        let problem = from_response(
            400,
            &[header(
                "X-Amzn-ErrorType",
                "ValidationException:http://internal.amazon.com/",
            )],
            Some(""),
        )
        .unwrap();
        assert_eq!(problem.code, "ValidationException");

        let problem = from_response(
            400,
            &[header(
                "x-amzn-query-error",
                "AWS.SimpleQueueService.NonExistentQueue;Sender",
            )],
            None,
        )
        .unwrap();
        assert_eq!(problem.code, "AWS.SimpleQueueService.NonExistentQueue");
        assert!(problem.hint.unwrap().contains("does not exist"));

        // S3 HEAD 404 has no body at all.
        let problem = from_response(404, &[], None).unwrap();
        assert_eq!(problem.code, "NotFound");
        assert!(problem.hint.is_some());
        assert_eq!(from_response(418, &[], None).unwrap().hint, None);
    }

    #[test]
    fn hints_cover_the_common_local_failures() {
        for (code, status, needle) in [
            ("SignatureDoesNotMatch", 403, "signature"),
            ("UnrecognizedClientException", 400, "credentials"),
            ("AuthorizationHeaderMalformed", 400, "Region"),
            ("BucketAlreadyOwnedByYou", 409, "already exists"),
            ("ThrottlingException", 400, "Throttled"),
            ("InternalError", 500, "internally"),
            ("Anything", 501, "implement"),
        ] {
            let hint = hint(code, status).unwrap_or_default();
            assert!(hint.contains(needle), "{code}: {hint}");
        }
    }

    #[test]
    fn truncated_bodies_do_not_panic() {
        assert!(from_response(400, &[], Some(r#"{"__type":"Valid"#)).is_some());
        assert!(from_response(400, &[], Some("<Error><Code>NoSu")).is_some());
    }
}
