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
    /// Signing region from the SigV4 credential scope, when signed.
    pub region: Option<String>,
    /// Operation advertised by `x-amz-target` or `x-id`, if any.
    pub operation: Option<String>,
    /// SDK invocation identifier (`amz-sdk-invocation-id`), constant across retries.
    pub invocation_id: Option<String>,
    /// SDK attempt number (`amz-sdk-request: attempt=N; max=M`), 1-based.
    pub attempt: Option<u32>,
    /// Maximum attempts configured for this SDK invocation, when sent.
    pub max_attempts: Option<u32>,
}

/// Read the service, advertised operation and SDK retry metadata from request
/// headers and path.
///
/// The credential scope comes from the `Authorization` header, or from the
/// `X-Amz-Credential` query parameter of a presigned URL.
pub fn wire_info(headers: &HeaderMap, path: &str) -> Wire {
    let scope = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split("Credential=").nth(1))
        .map(|scope| scope.split([',', ' ']).next().unwrap_or(scope).to_string())
        .or_else(|| query_param(path, "X-Amz-Credential"));
    let mut parts = scope.as_deref().unwrap_or("").split('/');
    let region = parts.nth(2).filter(|region| !region.is_empty());
    let service = parts.next().filter(|service| !service.is_empty());
    // Legacy SigV2 (`AWS AKID:sig` or `?AWSAccessKeyId=..&Signature=..`) is S3-only.
    let sigv2 = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("AWS "))
        || (query_param(path, "AWSAccessKeyId").is_some()
            && query_param(path, "Signature").is_some());
    let service = service.or(sigv2.then_some("s3"));
    let invocation_id = headers
        .get("amz-sdk-invocation-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|id| !id.is_empty() && id.len() <= 128)
        .map(str::to_string);
    let (attempt, max_attempts) = headers
        .get("amz-sdk-request")
        .and_then(|value| value.to_str().ok())
        .map(parse_sdk_request)
        .unwrap_or((None, None));
    Wire {
        service: service.unwrap_or("aws").to_string(),
        region: region.map(str::to_string),
        operation: operation(headers, path),
        invocation_id,
        attempt,
        max_attempts,
    }
}

/// Parse `attempt=2; max=3` from `amz-sdk-request`.
pub fn parse_sdk_request(value: &str) -> (Option<u32>, Option<u32>) {
    let mut attempt = None;
    let mut max = None;
    for part in value.split(';') {
        let Some((key, raw)) = part.trim().split_once('=') else {
            continue;
        };
        let parsed = raw.trim().parse::<u32>().ok().filter(|n| *n > 0);
        match key.trim() {
            "attempt" => attempt = parsed,
            "max" => max = parsed,
            _ => {}
        }
    }
    (attempt, max)
}

fn operation(headers: &HeaderMap, path: &str) -> Option<String> {
    headers
        .get("x-amz-target")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit('.').next())
        .filter(|operation| !operation.is_empty())
        .map(str::to_string)
        // REST operations carry the operation name in the query string.
        .or_else(|| query_param(path, "x-id").filter(|operation| !operation.is_empty()))
}

/// One query parameter, percent-decoded. Matches the exact parameter name.
pub fn query_param(path: &str, name: &str) -> Option<String> {
    let (_, query) = path.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        (key == name).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    percent_decode_inner(value, true)
}

/// Percent-decode a URI path segment (`%XX` only; literal `+` stays `+`).
pub fn percent_decode_path(value: &str) -> String {
    percent_decode_inner(value, false)
}

fn percent_decode_inner(value: &str, plus_as_space: bool) -> String {
    fn hex(byte: u8) -> Option<u8> {
        (byte as char).to_digit(16).map(|digit| digit as u8)
    }
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            out.push(high << 4 | low);
            index += 3;
            continue;
        }
        out.push(if plus_as_space && byte == b'+' {
            b' '
        } else {
            byte
        });
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Last resort when the protocol advertises no operation at all.
///
/// S3, Lambda, Bedrock and API Gateway REST APIs name operations by HTTP method
/// and path shape, and some SDKs do not send `x-id`, so those services get
/// shape rules. Everything else keeps the HTTP method.
pub fn fallback_operation(service: &str, method: &str, path: &str, headers: &HeaderMap) -> String {
    let shaped = match service {
        "s3" => s3_operation(method, path, headers),
        "lambda" => lambda_operation(method, path),
        "bedrock" | "bedrock-runtime" => bedrock_operation(method, path),
        "execute-api" => Some("Invoke".to_string()),
        "appconfig" | "appconfigdata" => appconfig_operation(method, path),
        _ => None,
    };
    shaped.unwrap_or_else(|| method.to_string())
}

/// Bedrock and Bedrock Runtime REST APIs: `/model/{id}/invoke`, `/foundation-models`, etc.
fn bedrock_operation(method: &str, path: &str) -> Option<String> {
    let bare = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = bare.split('/').filter(|s| !s.is_empty()).collect();
    let operation = match (method, segments.as_slice()) {
        ("POST", ["model", mid @ .., "invoke"]) if !mid.is_empty() => "InvokeModel",
        ("POST", ["model", mid @ .., "invoke-with-response-stream"]) if !mid.is_empty() => {
            "InvokeModelWithResponseStream"
        }
        ("POST", ["model", mid @ .., "converse"]) if !mid.is_empty() => "Converse",
        ("POST", ["model", mid @ .., "converse-stream"]) if !mid.is_empty() => "ConverseStream",
        ("GET", ["foundation-models"]) => "ListFoundationModels",
        ("GET", ["foundation-models", _]) => "GetFoundationModel",
        ("GET", ["inference-profiles"]) => "ListInferenceProfiles",
        ("GET", ["inference-profiles", _]) => "GetInferenceProfile",
        _ => return None,
    };
    Some(operation.to_string())
}

fn appconfig_operation(method: &str, path: &str) -> Option<String> {
    let bare = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = bare.split('/').filter(|s| !s.is_empty()).collect();
    let operation = match (method, segments.as_slice()) {
        ("POST", ["configurationsessions"]) => "StartConfigurationSession",
        ("GET", ["configuration"]) => "GetLatestConfiguration",
        _ => return None,
    };
    Some(operation.to_string())
}

/// Lambda's REST API: `/2015-03-31/functions/{name}/...`.
fn lambda_operation(method: &str, path: &str) -> Option<String> {
    let bare = path.split('?').next().unwrap_or(path);
    let segments: Vec<&str> = bare.split('/').filter(|s| !s.is_empty()).collect();
    let operation = match (method, segments.get(1..).unwrap_or(&[])) {
        ("POST", ["functions"]) => "CreateFunction",
        ("GET", ["functions"]) => "ListFunctions",
        ("GET", ["functions", _]) => "GetFunction",
        ("DELETE", ["functions", _]) => "DeleteFunction",
        ("POST", ["functions", _, "invocations"]) => "Invoke",
        ("POST", ["functions", _, "invoke-async"]) => "InvokeAsync",
        ("POST", ["functions", _, "response-streaming-invocations"]) => "InvokeWithResponseStream",
        ("PUT", ["functions", _, "code"]) => "UpdateFunctionCode",
        ("GET", ["functions", _, "configuration"]) => "GetFunctionConfiguration",
        ("PUT", ["functions", _, "configuration"]) => "UpdateFunctionConfiguration",
        ("POST", ["functions", _, "versions"]) => "PublishVersion",
        ("GET", ["functions", _, "versions"]) => "ListVersionsByFunction",
        ("POST", ["functions", _, "aliases"]) => "CreateAlias",
        ("GET", ["functions", _, "aliases"]) => "ListAliases",
        ("GET", ["functions", _, "aliases", _]) => "GetAlias",
        ("POST", ["functions", _, "policy"]) => "AddPermission",
        ("GET", ["functions", _, "policy"]) => "GetPolicy",
        ("DELETE", ["functions", _, "policy", _]) => "RemovePermission",
        ("POST", ["functions", _, "url"]) => "CreateFunctionUrlConfig",
        ("GET", ["functions", _, "url"]) => "GetFunctionUrlConfig",
        ("POST", ["event-source-mappings"]) => "CreateEventSourceMapping",
        ("GET", ["event-source-mappings"]) => "ListEventSourceMappings",
        ("GET", ["event-source-mappings", _]) => "GetEventSourceMapping",
        ("PUT", ["event-source-mappings", _]) => "UpdateEventSourceMapping",
        ("DELETE", ["event-source-mappings", _]) => "DeleteEventSourceMapping",
        ("GET", ["tags", ..]) => "ListTags",
        ("POST", ["tags", ..]) => "TagResource",
        ("DELETE", ["tags", ..]) => "UntagResource",
        _ => return None,
    };
    Some(operation.to_string())
}

fn s3_operation(method: &str, path: &str, headers: &HeaderMap) -> Option<String> {
    let (bare, query) = path.split_once('?').unwrap_or((path, ""));
    let keys: Vec<&str> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').map_or(pair, |(key, _)| key))
        .collect();
    let has = |key: &str| keys.contains(&key);
    // Virtual-hosted style keeps the bucket in the Host header.
    let host = headers
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let host = strip_port(host);
    let bucket_in_host =
        host.ends_with(".localhost") || host.contains(".s3.") || host.contains(".s3-");
    let segments = bare
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count()
        + usize::from(bucket_in_host);
    let object = segments >= 2;
    let copy = headers.contains_key("x-amz-copy-source");
    let name = |verb: &str, noun: &str| Some(format!("{verb}{noun}"));
    let verb = match method {
        "GET" => "Get",
        "PUT" => "Put",
        "DELETE" => "Delete",
        _ => "",
    };

    if segments == 0 {
        return (method == "GET").then(|| "ListBuckets".to_string());
    }
    // Multipart uploads.
    if has("uploadId") {
        return match method {
            "PUT" if copy => name("UploadPart", "Copy"),
            "PUT" => name("UploadPart", ""),
            "POST" => name("CompleteMultipartUpload", ""),
            "DELETE" => name("AbortMultipartUpload", ""),
            "GET" => name("ListParts", ""),
            _ => None,
        };
    }
    if has("uploads") {
        return match (method, object) {
            ("POST", true) => name("CreateMultipartUpload", ""),
            ("GET", false) => name("ListMultipartUploads", ""),
            _ => None,
        };
    }
    if method == "POST" && has("delete") {
        return name("DeleteObjects", "");
    }
    if !object && method == "GET" && has("versions") {
        return name("ListObjectVersions", "");
    }
    // Subresources: GET /b?policy → GetBucketPolicy, PUT /b/k?tagging → PutObjectTagging.
    const BUCKET: &[(&str, &str)] = &[
        ("acl", "BucketAcl"),
        ("policy", "BucketPolicy"),
        ("tagging", "BucketTagging"),
        ("versioning", "BucketVersioning"),
        ("cors", "BucketCors"),
        ("lifecycle", "BucketLifecycleConfiguration"),
        ("website", "BucketWebsite"),
        ("location", "BucketLocation"),
        ("notification", "BucketNotificationConfiguration"),
        ("encryption", "BucketEncryption"),
        ("logging", "BucketLogging"),
        ("replication", "BucketReplication"),
        ("ownershipControls", "BucketOwnershipControls"),
        ("publicAccessBlock", "PublicAccessBlock"),
        ("object-lock", "ObjectLockConfiguration"),
    ];
    const OBJECT: &[(&str, &str)] = &[
        ("acl", "ObjectAcl"),
        ("tagging", "ObjectTagging"),
        ("retention", "ObjectRetention"),
        ("legal-hold", "ObjectLegalHold"),
        ("attributes", "ObjectAttributes"),
    ];
    let table = if object { OBJECT } else { BUCKET };
    if let Some((_, noun)) = table.iter().find(|(key, _)| has(key)) {
        return (!verb.is_empty()).then(|| format!("{verb}{noun}"));
    }
    match (method, object) {
        ("PUT", false) => name("CreateBucket", ""),
        ("HEAD", false) => name("HeadBucket", ""),
        ("DELETE", false) => name("DeleteBucket", ""),
        ("GET", false) if has("list-type") => name("ListObjectsV2", ""),
        ("GET", false) => name("ListObjects", ""),
        ("POST", false) => name("PostObject", ""),
        ("PUT", true) if copy => name("CopyObject", ""),
        ("PUT", true) => name("PutObject", ""),
        ("GET", true) => name("GetObject", ""),
        ("HEAD", true) => name("HeadObject", ""),
        ("DELETE", true) => name("DeleteObject", ""),
        _ => None,
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

/// Strip an optional `:port` from a `Host` header value without mangling
/// bracketed IPv6 literals such as `[::1]` or `[::1]:9297`.
pub fn strip_port(host: &str) -> &str {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[')
        && let Some(end) = rest.find(']')
    {
        return &host[..=end + 1];
    }
    match host.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => {
            name
        }
        _ => host,
    }
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
        let none = HeaderMap::new();
        assert_eq!(
            fallback_operation("s3", "PUT", "/bucket/", &none),
            "CreateBucket"
        );
        assert_eq!(
            fallback_operation("s3", "PUT", "/bucket", &none),
            "CreateBucket"
        );
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
        let none = HeaderMap::new();
        assert_eq!(fallback_operation("sqs", "POST", "/", &none), "POST");
        assert_eq!(
            fallback_operation("lambda", "PUT", "/2015-03-31/", &none),
            "PUT"
        );
    }

    #[test]
    fn names_s3_rest_operations_by_shape() {
        let none = HeaderMap::new();
        let mut copy = HeaderMap::new();
        copy.insert("x-amz-copy-source", HeaderValue::from_static("/a/b"));
        let mut virtual_host = HeaderMap::new();
        virtual_host.insert("host", HeaderValue::from_static("demo.localhost:9297"));
        for (method, path, headers, expected) in [
            ("GET", "/", &none, "ListBuckets"),
            ("PUT", "/b", &none, "CreateBucket"),
            ("HEAD", "/b", &none, "HeadBucket"),
            ("DELETE", "/b", &none, "DeleteBucket"),
            ("GET", "/b?list-type=2&prefix=x", &none, "ListObjectsV2"),
            ("GET", "/b", &none, "ListObjects"),
            ("PUT", "/b/k", &none, "PutObject"),
            ("PUT", "/b/k", &copy, "CopyObject"),
            ("GET", "/b/dir/k.txt", &none, "GetObject"),
            ("HEAD", "/b/k", &none, "HeadObject"),
            ("DELETE", "/b/k", &none, "DeleteObject"),
            ("POST", "/b?delete", &none, "DeleteObjects"),
            ("GET", "/b?policy", &none, "GetBucketPolicy"),
            ("PUT", "/b?versioning", &none, "PutBucketVersioning"),
            ("PUT", "/b/k?tagging", &none, "PutObjectTagging"),
            ("POST", "/b/k?uploads", &none, "CreateMultipartUpload"),
            ("PUT", "/b/k?partNumber=1&uploadId=u", &none, "UploadPart"),
            ("POST", "/b/k?uploadId=u", &none, "CompleteMultipartUpload"),
            ("GET", "/b?uploads", &none, "ListMultipartUploads"),
            ("GET", "/k", &virtual_host, "GetObject"),
            ("GET", "/", &virtual_host, "ListObjects"),
        ] {
            assert_eq!(
                fallback_operation("s3", method, path, headers),
                expected,
                "{method} {path}"
            );
        }
        assert_eq!(fallback_operation("s3", "PATCH", "/b/k", &none), "PATCH");
    }

    #[test]
    fn names_lambda_rest_operations_by_shape() {
        let none = HeaderMap::new();
        for (method, path, expected) in [
            ("POST", "/2015-03-31/functions", "CreateFunction"),
            ("POST", "/2015-03-31/functions/f/invocations", "Invoke"),
            (
                "PUT",
                "/2015-03-31/functions/f/configuration",
                "UpdateFunctionConfiguration",
            ),
            ("GET", "/2015-03-31/functions/f", "GetFunction"),
            (
                "POST",
                "/2015-03-31/event-source-mappings/",
                "CreateEventSourceMapping",
            ),
            ("PATCH", "/2015-03-31/functions/f", "PATCH"),
        ] {
            assert_eq!(
                fallback_operation("lambda", method, path, &none),
                expected,
                "{method} {path}"
            );
        }
    }

    #[test]
    fn sigv2_requests_are_s3() {
        let wire = wire_info(
            &HeaderMap::new(),
            "/b/k?AWSAccessKeyId=AKID&Signature=abc%3D&Expires=1",
        );
        assert_eq!(wire.service, "s3");
        let mut map = HeaderMap::new();
        map.insert("authorization", HeaderValue::from_static("AWS AKID:sig"));
        assert_eq!(wire_info(&map, "/b").service, "s3");
    }

    #[test]
    fn x_id_is_a_whole_query_parameter() {
        let map = headers(&scope("s3"), None);
        assert_eq!(wire_info(&map, "/b/k?max-x-id=Nope").operation, None);
        assert_eq!(wire_info(&map, "/b/x-id=Nope").operation, None);
        assert_eq!(
            wire_info(&map, "/b/k?list-type=2&x-id=ListObjectsV2")
                .operation
                .as_deref(),
            Some("ListObjectsV2")
        );
    }

    #[test]
    fn reads_region_and_presigned_scopes() {
        let map = headers(
            "AWS4-HMAC-SHA256 Credential=AKID/20261115/eu-west-1/sqs/aws4_request, SignedHeaders=host, Signature=x",
            None,
        );
        let wire = wire_info(&map, "/");
        assert_eq!(wire.service, "sqs");
        assert_eq!(wire.region.as_deref(), Some("eu-west-1"));

        let path = "/bucket/key?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKID%2F20261115%2Fus-west-2%2Fs3%2Faws4_request&X-Amz-Signature=abc";
        let wire = wire_info(&HeaderMap::new(), path);
        assert_eq!(wire.service, "s3");
        assert_eq!(wire.region.as_deref(), Some("us-west-2"));
    }

    #[test]
    fn unsigned_requests_still_get_a_service_and_no_operation() {
        let wire = wire_info(&HeaderMap::new(), "/");
        assert_eq!(wire.service, "aws");
        assert_eq!(wire.operation, None);
        assert_eq!(wire.invocation_id, None);
        assert_eq!(wire.attempt, None);
    }

    #[test]
    fn parses_sdk_invocation_and_retry_headers() {
        let mut map = headers(&scope("sqs"), None);
        map.insert(
            "amz-sdk-invocation-id",
            HeaderValue::from_static("9ab7d414-3ade-40c4-a305-c1dc7137bbfa"),
        );
        map.insert(
            "amz-sdk-request",
            HeaderValue::from_static("attempt=2; max=4; ttl=20261118T000000Z"),
        );
        let wire = wire_info(&map, "/");
        assert_eq!(
            wire.invocation_id.as_deref(),
            Some("9ab7d414-3ade-40c4-a305-c1dc7137bbfa")
        );
        assert_eq!(wire.attempt, Some(2));
        assert_eq!(wire.max_attempts, Some(4));
        assert_eq!(parse_sdk_request("attempt=0; max=nope"), (None, None));
    }

    #[test]
    fn names_bedrock_and_api_gateway_rest_operations() {
        let none = HeaderMap::new();
        assert_eq!(
            fallback_operation(
                "bedrock-runtime",
                "POST",
                "/model/anthropic.claude-3-haiku/invoke",
                &none
            ),
            "InvokeModel"
        );
        assert_eq!(
            fallback_operation(
                "bedrock-runtime",
                "POST",
                "/model/anthropic.claude-3-haiku/converse-stream",
                &none
            ),
            "ConverseStream"
        );
        assert_eq!(
            fallback_operation("execute-api", "GET", "/prod/orders", &none),
            "Invoke"
        );
        assert_eq!(
            fallback_operation("appconfigdata", "POST", "/configurationsessions", &none),
            "StartConfigurationSession"
        );
        assert_eq!(
            fallback_operation(
                "appconfigdata",
                "GET",
                "/configuration?configuration_token=tok",
                &none
            ),
            "GetLatestConfiguration"
        );
    }

    #[test]
    fn strips_ports_without_breaking_ipv6_hosts() {
        assert_eq!(
            strip_port("my-bucket.localhost:9297"),
            "my-bucket.localhost"
        );
        assert_eq!(strip_port("[::1]:9297"), "[::1]");
        assert_eq!(strip_port("[::1]"), "[::1]");
        assert_eq!(strip_port("127.0.0.1:9297"), "127.0.0.1");
        assert_eq!(
            strip_port("s3.us-east-1.amazonaws.com"),
            "s3.us-east-1.amazonaws.com"
        );
    }
}
