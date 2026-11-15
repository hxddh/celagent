//! Static, clearly-labelled demo data for screenshots and `LENSLET_DEMO=1`.
//! This module is never used unless demo mode is explicitly requested.

use serde_json::{Value, json};

use crate::trace::{Trace, parse_line};

const HOST: &str = "127.0.0.1:9297";

pub fn demo_rows() -> Vec<Trace> {
    let rows = vec![
        row(
            "3f8a1c7e",
            "2026-11-15T12:41:02.118Z",
            "PUT",
            "/demo-assets/uploads/hero.png?x-id=PutObject",
            "s3",
            "PutObject",
            200,
            8,
            true,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("authorization", S3_AUTH),
                    ("x-amz-date", "20261115T124102Z"),
                    ("x-amz-content-sha256", "UNSIGNED-PAYLOAD"),
                    ("content-type", "image/png"),
                    ("content-length", "184320"),
                    ("user-agent", "aws-sdk-nodejs/3.658.0 linux/v22.9.0"),
                ]),
                Some("image/png"),
                Some(binary_body(
                    184_320,
                    "9f2c41d7e0a8c3b5f6d1e2a4c8b7f0d3e5a6c9b1d2e4f7a8c0b3d5e6f9a1c2b4",
                )),
            )),
            Some(exchange(
                headers(&[
                    ("x-amz-request-id", "3f8a1c7e"),
                    ("x-amz-id-2", "lenslet-demo/put-object"),
                    ("etag", "\"5d41402abc4b2a76b9719d911017c592\""),
                    ("server", "AWS Emulator"),
                ]),
                None,
                None,
            )),
        ),
        row(
            "91b2d4a0",
            "2026-11-15T12:41:02.243Z",
            "POST",
            "/",
            "sqs",
            "SendMessage",
            200,
            4,
            true,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("authorization", SQS_AUTH),
                    ("x-amz-target", "AmazonSQS.SendMessage"),
                    ("content-type", "application/x-amz-json-1.0"),
                ]),
                Some("application/x-amz-json-1.0"),
                Some(text_body(
                    "{\n  \"QueueUrl\": \"http://127.0.0.1:9298/000000000000/thumb-jobs\",\n  \"MessageBody\": \"{\\\"task\\\":\\\"thumbnail\\\",\\\"bucket\\\":\\\"demo-assets\\\",\\\"key\\\":\\\"uploads/hero.png\\\"}\",\n  \"DelaySeconds\": 0\n}",
                )),
            )),
            Some(exchange(
                headers(&[
                    ("x-amzn-requestid", "91b2d4a0-6f13-4c1a-9b2e-7d5f0a3c8e11"),
                    ("content-type", "application/x-amz-json-1.0"),
                ]),
                Some("application/x-amz-json-1.0"),
                Some(text_body(
                    "{\n  \"MessageId\": \"0f9a7c31-4b62-4d8e-a1f0-3c5d7e9b2a48\",\n  \"MD5OfMessageBody\": \"4c2f1e7a9b3d5c8e0f2a6b4d7c9e1a3b\"\n}",
                )),
            )),
        ),
        row(
            "7c4e2b19",
            "2026-11-15T12:41:02.361Z",
            "POST",
            "/",
            "dynamodb",
            "PutItem",
            200,
            11,
            true,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("authorization", DDB_AUTH),
                    ("x-amz-target", "DynamoDB_20120810.PutItem"),
                    ("content-type", "application/x-amz-json-1.0"),
                ]),
                Some("application/x-amz-json-1.0"),
                Some(text_body(
                    "{\n  \"TableName\": \"JobsTable\",\n  \"Item\": {\n    \"jobId\": { \"S\": \"job#42\" },\n    \"status\": { \"S\": \"queued\" },\n    \"sourceKey\": { \"S\": \"uploads/hero.png\" },\n    \"createdAt\": { \"N\": \"1763200862\" }\n  }\n}",
                )),
            )),
            Some(exchange(
                headers(&[
                    ("x-amzn-requestid", "7c4e2b19-8a02-4f5e-bd61-2e9c4a7f1d30"),
                    ("content-type", "application/x-amz-json-1.0"),
                ]),
                Some("application/x-amz-json-1.0"),
                Some(text_body(
                    "{\n  \"ConsumedCapacity\": {\n    \"TableName\": \"JobsTable\",\n    \"CapacityUnits\": 1.0\n  }\n}",
                )),
            )),
        ),
        row(
            "a0d7f35c",
            "2026-11-15T12:41:02.508Z",
            "POST",
            "/",
            "sns",
            "Publish",
            200,
            6,
            true,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("authorization", SNS_AUTH),
                    ("x-amz-target", "AmazonSNS.Publish"),
                    ("content-type", "application/x-amz-json-1.0"),
                ]),
                Some("application/x-amz-json-1.0"),
                Some(text_body(
                    "{\n  \"TopicArn\": \"arn:aws:sns:us-east-1:000000000000:thumbnail-events\",\n  \"Message\": \"{\\\"event\\\":\\\"thumbnail.created\\\",\\\"key\\\":\\\"uploads/hero.png\\\"}\",\n  \"Subject\": \"thumbnail.created\"\n}",
                )),
            )),
            Some(exchange(
                headers(&[("x-amzn-requestid", "a0d7f35c-2b91-4e77-9c40-6a1d8f3b5e02")]),
                Some("application/x-amz-json-1.0"),
                Some(text_body(
                    "{\n  \"MessageId\": \"8e1c3a57-0d9f-4b26-8a71-5c3e9d0f2b64\"\n}",
                )),
            )),
        ),
        row(
            "c5e91d72",
            "2026-11-15T12:41:02.731Z",
            "POST",
            "/2015-03-31/functions/thumbnailer/invocations",
            "lambda",
            "Invoke",
            202,
            1_240,
            true,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("authorization", LAMBDA_AUTH),
                    ("content-type", "application/json"),
                    ("x-amz-invocation-type", "RequestResponse"),
                    ("x-amz-log-type", "None"),
                ]),
                Some("application/json"),
                Some(text_body(
                    "{\n  \"Records\": [\n    {\n      \"s3\": {\n        \"bucket\": { \"name\": \"demo-assets\" },\n        \"object\": { \"key\": \"uploads/hero.png\" }\n      }\n    }\n  ]\n}",
                )),
            )),
            Some(exchange(
                headers(&[
                    ("x-amzn-requestid", "c5e91d72-4a8b-49e0-8f13-7b2d6c0e9a51"),
                    ("content-type", "application/json"),
                    ("x-amz-executed-version", "$LATEST"),
                ]),
                Some("application/json"),
                Some(json!({
                    "text": "{\n  \"statusCode\": 200,\n  \"body\": \"{\\\"width\\\":1280,\\\"height\\\":720,\\\"bytes\\\":184320}\",\n  \"coldStart\": false\n}\n…",
                    "bytes": 4_096,
                    "truncated": true,
                    "binary": false,
                })),
            )),
        ),
        row(
            "e2b60f48",
            "2026-11-15T12:41:03.004Z",
            "GET",
            "/demo-assets/uploads/missing.png?x-id=GetObject",
            "s3",
            "GetObject",
            404,
            3,
            true,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("authorization", S3_AUTH),
                    ("x-amz-date", "20261115T124103Z"),
                    ("user-agent", "aws-sdk-nodejs/3.658.0 linux/v22.9.0"),
                ]),
                None,
                None,
            )),
            Some(exchange(
                headers(&[
                    ("x-amz-request-id", "e2b60f48"),
                    ("content-type", "application/xml"),
                    ("server", "AWS Emulator"),
                ]),
                Some("application/xml"),
                Some(text_body(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error>\n  <Code>NoSuchKey</Code>\n  <Message>The specified key does not exist.</Message>\n  <Key>uploads/missing.png</Key>\n  <RequestId>e2b60f48</RequestId>\n  <HostId>lenslet-demo/get-object</HostId>\n</Error>",
                )),
            )),
        ),
        row(
            "f7a3c806",
            "2026-11-15T12:41:03.190Z",
            "GET",
            "/demo-assets/uploads/hero.png?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIDEMO%2F20261115%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20261115T124103Z&X-Amz-Expires=900&X-Amz-SignedHeaders=host&X-Amz-Signature=6d1f0a9c3b7e5d2f8a4c1b6e9d0f3a7c5b2e8d4f1a6c9b0e3d7f2a5c8b1e4d6f",
            "s3",
            "GetObject",
            200,
            6,
            false,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("user-agent", "Mozilla/5.0 (presigned URL demo)"),
                ]),
                None,
                None,
            )),
            Some(exchange(
                headers(&[
                    ("x-amz-request-id", "f7a3c806"),
                    ("content-type", "image/png"),
                    ("content-length", "184320"),
                    ("etag", "\"5d41402abc4b2a76b9719d911017c592\""),
                ]),
                Some("image/png"),
                Some(binary_body(
                    184_320,
                    "9f2c41d7e0a8c3b5f6d1e2a4c8b7f0d3e5a6c9b1d2e4f7a8c0b3d5e6f9a1c2b4",
                )),
            )),
        ),
        row(
            "2d90b5e1",
            "2026-11-15T12:41:04.402Z",
            "POST",
            "/",
            "sqs",
            "CreateQueue",
            200,
            2,
            true,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("authorization", SQS_AUTH),
                    ("x-amz-target", "AmazonSQS.CreateQueue"),
                    ("content-type", "application/x-amz-json-1.0"),
                ]),
                Some("application/x-amz-json-1.0"),
                Some(text_body(
                    "{\n  \"QueueName\": \"thumb-jobs\",\n  \"Attributes\": { \"VisibilityTimeout\": \"30\" }\n}",
                )),
            )),
            Some(exchange(
                headers(&[("x-amzn-requestid", "2d90b5e1-0c7a-4b3d-9e81-4f6a2c8d0b35")]),
                Some("application/x-amz-json-1.0"),
                Some(text_body(
                    "{\n  \"QueueUrl\": \"http://127.0.0.1:9298/000000000000/thumb-jobs\"\n}",
                )),
            )),
        ),
        row(
            "5e7b0c93",
            "2026-11-15T12:41:04.650Z",
            "POST",
            "/",
            "kinesis",
            "PutRecord",
            200,
            7,
            true,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("authorization", KINESIS_AUTH),
                    ("x-amz-target", "Kinesis_20131202.PutRecord"),
                    ("content-type", "application/x-amz-json-1.1"),
                ]),
                Some("application/x-amz-json-1.1"),
                Some(text_body(
                    "{\n  \"StreamName\": \"thumbnail-events\",\n  \"PartitionKey\": \"job#42\",\n  \"Data\": \"eyJ0YXNrIjoidGh1bWJuYWlsIiwia2V5IjoidXBsb2Fkcy9oZXJvLnBuZyJ9\"\n}",
                )),
            )),
            Some(exchange(
                headers(&[
                    ("x-amzn-requestid", "5e7b0c93-71d4-4e28-b0f9-3a6c8d2e5b17"),
                    ("content-type", "application/x-amz-json-1.1"),
                ]),
                Some("application/x-amz-json-1.1"),
                Some(text_body(
                    "{\n  \"ShardId\": \"shardId-000000000001\",\n  \"SequenceNumber\": \"4964512345678901234567890123456789012345678901234567890\"\n}",
                )),
            )),
        ),
        row(
            "6b1e8a44",
            "2026-11-15T12:41:04.880Z",
            "PUT",
            "/demo-assets/",
            "s3",
            "CreateBucket",
            200,
            5,
            true,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("authorization", S3_AUTH),
                    ("x-amz-date", "20261115T124104Z"),
                    ("content-type", "application/xml"),
                ]),
                Some("application/xml"),
                Some(text_body(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CreateBucketConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\n  <LocationConstraint>us-east-1</LocationConstraint>\n</CreateBucketConfiguration>",
                )),
            )),
            Some(exchange(
                headers(&[
                    ("x-amz-request-id", "6b1e8a44"),
                    ("location", "/demo-assets"),
                    ("server", "AWS Emulator"),
                ]),
                None,
                None,
            )),
        ),
        row(
            "8c2f5d07",
            "2026-11-15T12:41:05.210Z",
            "POST",
            "/",
            "dynamodb",
            "DescribeTable",
            500,
            61,
            true,
            Some(exchange(
                headers(&[
                    ("host", HOST),
                    ("authorization", DDB_AUTH),
                    ("x-amz-target", "DynamoDB_20120810.DescribeTable"),
                    ("content-type", "application/x-amz-json-1.0"),
                ]),
                Some("application/x-amz-json-1.0"),
                Some(text_body("{\n  \"TableName\": \"JobsTable\"\n}")),
            )),
            Some(exchange(
                headers(&[("x-amzn-requestid", "8c2f5d07-3e6b-4a20-8c19-5d7f1b9e3a62")]),
                Some("application/x-amz-json-1.0"),
                Some(text_body(
                    "{\n  \"__type\": \"com.amazonaws.dynamodb.v20120810#InternalServerError\",\n  \"message\": \"table metadata is temporarily unavailable\"\n}",
                )),
            )),
        ),
    ];
    rows
}

const S3_AUTH: &str = "AWS4-HMAC-SHA256 Credential=AKIDDEMO/20261115/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature=7f3d0c2a9b1e4f6a8d5c3b0e7f2a9c4d6b8e1f0a3c5d7e9b2f4a6c8d0e1b3f5a";
const SQS_AUTH: &str = "AWS4-HMAC-SHA256 Credential=AKIDDEMO/20261115/us-east-1/sqs/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-target, Signature=1a9f4c7e2b6d8a0c3e5f7b9d1a4c6e8f0b2d5a7c9e1f3b6d8a0c2e4f6b8d1a3c";
const DDB_AUTH: &str = "AWS4-HMAC-SHA256 Credential=AKIDDEMO/20261115/us-east-1/dynamodb/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-target, Signature=4e8b1d5f9a2c6e0b3d7f1a4c8e2b6d0f5a9c3e7b1d4f8a2c6e0b3d5f7a9c1e4b";
const SNS_AUTH: &str = "AWS4-HMAC-SHA256 Credential=AKIDDEMO/20261115/us-east-1/sns/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-target, Signature=9c3e7a1d5f8b2e6c0a4d7f1b3e9c5a8d2f6b0e4c8a1d5f7b9e3c6a0d4f8b2e5c";
const LAMBDA_AUTH: &str = "AWS4-HMAC-SHA256 Credential=AKIDDEMO/20261115/us-east-1/lambda/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature=2f6a0c4e8b1d5f9a3c7e2b6d0a4f8c1e5b9d3a7f0c2e6b4d8a1f5c9e3b7d0a4f";
const KINESIS_AUTH: &str = "AWS4-HMAC-SHA256 Credential=AKIDDEMO/20261115/us-east-1/kinesis/aws4_request, SignedHeaders=content-type;host;x-amz-date;x-amz-target, Signature=8b3e6a0d4f7c1b5e9a2d6f0c3b7e1a5d8c2f6b0e4a9d3c7f1b5e8a2c6d0f4b7e";

fn row(
    id: &str,
    at: &str,
    method: &str,
    path: &str,
    service: &str,
    operation: &str,
    status: u16,
    ms: u64,
    authorization: bool,
    request: Option<Value>,
    response: Option<Value>,
) -> Trace {
    let mut value = json!({
        "id": id,
        "at": at,
        "method": method,
        "path": path,
        "host": HOST,
        "service": service,
        "operation": operation,
        "status": status,
        "ms": ms,
        "authorization": authorization,
    });
    if let Some(request) = request {
        value["request"] = request;
    }
    if let Some(response) = response {
        value["response"] = response;
    }
    parse_line(&value.to_string()).expect("demo trace is valid")
}

fn headers(items: &[(&str, &str)]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect(),
    )
}

fn exchange(headers: Value, content_type: Option<&str>, body: Option<Value>) -> Value {
    let mut value = json!({ "headers": headers });
    if let Some(content_type) = content_type {
        value["content_type"] = json!(content_type);
    }
    if let Some(body) = body {
        value["body"] = body;
    }
    value
}

fn text_body(text: &str) -> Value {
    json!({
        "text": text,
        "bytes": text.len(),
        "truncated": false,
        "binary": false,
    })
}

fn binary_body(bytes: usize, sha256: &str) -> Value {
    json!({
        "bytes": bytes,
        "binary": true,
        "sha256": sha256,
    })
}
