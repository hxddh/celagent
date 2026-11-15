//! Generic AWS wire-protocol naming.
//!
//! Nothing here is service-specific: the service is read from the SigV4
//! credential scope (so every emulator service works), and the operation is read
//! from whatever the protocol advertises:
//!
//! * `x-amz-target` — AWS JSON protocols (`DynamoDB_20120810.PutItem`)
//! * `x-id` query parameter — REST-JSON / REST-XML (`?x-id=PutObject`)
//! * `Action=` in an `application/x-www-form-urlencoded` body — AWS Query
//!   protocols (IAM, STS, EC2, CloudFormation, CloudWatch, SNS, ...)
//!
//! Operation fidelity therefore varies by protocol: services that advertise
//! nothing end up with the HTTP method (or a path heuristic) as the operation.

use hyper::HeaderMap;

/// What a request advertises about itself on the wire.
#[derive(Clone, Debug)]
pub struct Wire {
    /// Signing service from the SigV4 credential scope, `aws` when unsigned.
    pub service: String,
    /// Operation advertised by `x-amz-target` or `x-id`, if any.
    pub operation: Option<String>,
}

/// Read the service and the advertised operation from request headers and path.
pub fn wire_info(headers: &HeaderMap, path: &str) -> Wire {
    Wire {
        service: service(headers),
        operation: operation(headers, path),
    }
}

fn service(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split("Credential=").nth(1))
        .and_then(|scope| scope.split('/').nth(3))
        .filter(|service| !service.is_empty())
        .unwrap_or("aws")
        .to_string()
}

fn operation(headers: &HeaderMap, path: &str) -> Option<String> {
    headers
        .get("x-amz-target")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('.').next())
        .filter(|operation| !operation.is_empty())
        .map(str::to_string)
        .or_else(|| {
            // REST operations carry the operation name in the query string.
            path.split("x-id=")
                .nth(1)
                .map(|value| value.split('&').next().unwrap_or(value))
                .filter(|operation| !operation.is_empty())
                .map(str::to_string)
        })
}

/// Last resort when the protocol advertises no operation at all.
pub fn fallback_operation(method: &str, path: &str) -> String {
    if method == "PUT" && path.ends_with('/') {
        "CreateBucket".to_string()
    } else {
        method.to_string()
    }
}

/// The `Action` of an AWS Query protocol form body, if present.
///
/// Only the captured prefix is inspected; SDKs send `Action` as the first field,
/// so the bounded capture is enough even though the body is streamed.
pub fn form_action(body: &[u8]) -> Option<String> {
    let body = std::str::from_utf8(body).ok()?;
    body.split('&').find_map(|field| {
        let (key, value) = field.split_once('=')?;
        if key != "Action" {
            return None;
        }
        let value = value.trim();
        // AWS operation names are alphanumeric; anything else is not an Action.
        if value.is_empty()
            || value.len() > 128
            || !value.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            return None;
        }
        Some(value.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::header::HeaderValue;

    fn headers(auth: &str, target: Option<&str>) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert("authorization", HeaderValue::from_str(auth).unwrap());
        if let Some(target) = target {
            map.insert("x-amz-target", HeaderValue::from_str(target).unwrap());
        }
        map
    }

    fn scope(service: &str) -> String {
        format!("AWS4-HMAC-SHA256 Credential=AKID/20261115/us-east-1/{service}/aws4_request")
    }

    #[test]
    fn parses_s3_scope_and_x_id() {
        let map = headers(&scope("s3"), None);
        let wire = wire_info(&map, "/bucket/key?x-id=PutObject");
        assert_eq!(wire.service, "s3");
        assert_eq!(wire.operation.as_deref(), Some("PutObject"));
    }

    #[test]
    fn parses_dynamodb_target() {
        let map = headers(&scope("dynamodb"), Some("DynamoDB_20120810.PutItem"));
        let wire = wire_info(&map, "/");
        assert_eq!(wire.service, "dynamodb");
        assert_eq!(wire.operation.as_deref(), Some("PutItem"));
    }

    #[test]
    fn recognizes_create_bucket_without_x_id() {
        let map = headers(&scope("s3"), None);
        let wire = wire_info(&map, "/bucket/");
        assert_eq!(wire.operation, None);
        assert_eq!(fallback_operation("PUT", "/bucket/"), "CreateBucket");
    }

    #[test]
    fn extracts_action_from_query_protocol_bodies() {
        assert_eq!(
            form_action(b"Action=DescribeInstances&Version=2016-11-15").as_deref(),
            Some("DescribeInstances")
        );
        assert_eq!(
            form_action(b"Version=2016-11-15&Action=GetCallerIdentity").as_deref(),
            Some("GetCallerIdentity")
        );
        assert_eq!(
            form_action(b"Action=AssumeRole&RoleArn=arn%3Aaws%3Aiam%3A%3A123%3Arole%2Fx")
                .as_deref(),
            Some("AssumeRole")
        );
        assert_eq!(
            form_action(b"Action=CreateStack&TemplateBody=%7B%7D").as_deref(),
            Some("CreateStack")
        );
        assert_eq!(
            form_action(b"Action=PutMetricData&Namespace=AWS%2FEC2").as_deref(),
            Some("PutMetricData")
        );
    }

    #[test]
    fn ignores_bodies_without_an_action() {
        assert_eq!(form_action(b"Version=2016-11-15"), None);
        assert_eq!(form_action(b"Action="), None);
        assert_eq!(form_action(b"Action=Describe%20Instances"), None);
        assert_eq!(form_action(b"{\"Action\":\"DescribeInstances\"}"), None);
        assert_eq!(form_action(b""), None);
        assert_eq!(form_action(&[0xff, 0xfe]), None);
    }

    #[test]
    fn x_amz_target_wins_over_x_id_in_the_path() {
        let map = headers(&scope("dynamodb"), Some("DynamoDB_20120810.PutItem"));
        let wire = wire_info(&map, "/?x-id=SendMessage");
        assert_eq!(wire.service, "dynamodb");
        assert_eq!(wire.operation.as_deref(), Some("PutItem"));
    }

    #[test]
    fn falls_back_to_method_and_path() {
        assert_eq!(fallback_operation("POST", "/"), "POST");
        assert_eq!(fallback_operation("GET", "/bucket/key"), "GET");
        assert_eq!(fallback_operation("PUT", "/bucket/"), "CreateBucket");
        assert_eq!(fallback_operation("PUT", "/bucket/key"), "PUT");
    }

    #[test]
    fn unsigned_requests_still_get_a_service_and_no_operation() {
        let wire = wire_info(&HeaderMap::new(), "/");
        assert_eq!(wire.service, "aws");
        assert_eq!(wire.operation, None);
    }
}
