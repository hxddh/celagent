//! Built-in zero-dependency stateful local AWS sandbox (`sandbox://local`).
//!
//! When no external emulator (`AWS Emulator`, `LocalStack`, `Moto`, `MinIO`) is
//! listening on localhost—or when `--sandbox` is explicitly requested—Lenslet
//! serves stateful, protocol-accurate AWS responses directly in memory while
//! recording 100% of wire headers, payloads, causal diagnostics, and derived
//! IAM permissions through the exact same `TraceSink` and `--enforce` gate.
//!
//! Strictly local: zero network egress, zero cloud credentials, zero persistence
//! beyond the life of the proxy process.

use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use bytes::Bytes;
use chrono::Utc;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Method, Response, StatusCode, header};
use serde_json::{Map, Value, json};

use crate::{
    capture::{self, Content},
    wire,
};

type BoxBody = http_body_util::combinators::BoxBody<Bytes, std::io::Error>;

const ACCOUNT_ID: &str = "000000000000";

pub struct Sandbox {
    endpoint: String,
    seq: AtomicU64,
    store: Mutex<SandboxStore>,
}

#[derive(Default)]
struct SandboxStore {
    s3_buckets: BTreeMap<String, S3Bucket>,
    s3_uploads: BTreeMap<String, S3MultipartUpload>,
    ddb_tables: BTreeMap<(String, String), DdbTable>,
    sqs_queues: BTreeMap<(String, String), SqsQueue>,
    sns_topics: BTreeMap<(String, String), SnsTopic>,
    secrets: BTreeMap<(String, String), SecretRecord>,
    ssm_params: BTreeMap<(String, String), SsmParam>,
    kms_keys: BTreeMap<String, KmsKeyRecord>,
    kms_aliases: BTreeMap<String, String>,
    lambda_fns: BTreeMap<(String, String), LambdaRecord>,
    log_groups: BTreeMap<(String, String), BTreeMap<String, Vec<Value>>>,
    eb_rules: BTreeMap<(String, String), Value>,
    kinesis_streams: BTreeMap<(String, String), Vec<Value>>,
    sfn_machines: BTreeMap<(String, String), Value>,
    sfn_executions: BTreeMap<String, Value>,
    iam_roles: BTreeMap<String, String>,
}

#[derive(Clone)]
struct S3Bucket {
    created_at: String,
    objects: BTreeMap<String, S3Object>,
}

#[derive(Clone)]
struct S3Object {
    bytes: Bytes,
    content_type: String,
    etag: String,
    last_modified: String,
    sse: Option<String>,
    kms_key_id: Option<String>,
    meta: Vec<(String, String)>,
}

#[derive(Clone)]
struct S3MultipartUpload {
    bucket: String,
    key: String,
    content_type: String,
    sse: Option<String>,
    kms_key_id: Option<String>,
    meta: Vec<(String, String)>,
    initiated_at: String,
    parts: BTreeMap<u32, S3UploadPart>,
}

#[derive(Clone)]
struct S3UploadPart {
    bytes: Bytes,
    etag: String,
    md5: [u8; 16],
    last_modified: String,
}

#[derive(Clone)]
struct DdbTable {
    name: String,
    arn: String,
    key_attrs: Vec<String>,
    indexes: BTreeMap<String, Vec<String>>,
    attr_defs: Value,
    created_epoch: f64,
    items: BTreeMap<String, Value>,
}

#[derive(Clone, Default)]
struct SqsQueue {
    arn: String,
    attrs: BTreeMap<String, String>,
    messages: VecDeque<SqsMsg>,
}

#[derive(Clone)]
struct SqsMsg {
    id: String,
    receipt: String,
    body: String,
    md5: String,
    attrs: Value,
}

#[derive(Clone)]
struct SnsTopic {
    arn: String,
    subscriptions: Vec<String>,
}

#[derive(Clone)]
struct SecretRecord {
    arn: String,
    name: String,
    version_id: String,
    secret_string: Option<String>,
    secret_binary: Option<String>,
    created_epoch: f64,
}

#[derive(Clone)]
struct SsmParam {
    name: String,
    r#type: String,
    value: String,
    version: u64,
    arn: String,
    last_modified: f64,
}

#[derive(Clone)]
struct KmsKeyRecord {
    key_id: String,
    arn: String,
    description: String,
    created_epoch: f64,
}

#[derive(Clone)]
struct LambdaRecord {
    name: String,
    arn: String,
    role: String,
    runtime: String,
    handler: String,
    last_modified: String,
}

struct ReqCtx<'a> {
    method: &'a Method,
    path: &'a str,
    headers: &'a HeaderMap,
    region: &'a str,
    operation: &'a str,
    content: &'a Content,
    raw_body: &'a [u8],
    req_id: String,
}

impl Sandbox {
    pub fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            seq: AtomicU64::new(1),
            store: Mutex::new(SandboxStore::default()),
        }
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn next_uuid(&self) -> String {
        let n = self.next_seq();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        format!(
            "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
            (nanos >> 32) as u32,
            (nanos >> 16) as u16,
            nanos as u16 & 0x0fff,
            (n >> 48) as u16 & 0x0fff,
            n & 0xffff_ffff_ffff
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn handle(
        &self,
        method: &Method,
        path: &str,
        headers: &HeaderMap,
        service: &str,
        region: Option<&str>,
        advertised_op: Option<&str>,
        fallback_op: &str,
        content: &Content,
        wire_body: &[u8],
    ) -> Response<BoxBody> {
        let decoded = capture::decode_wire_bytes(wire_body, content, 64 * 1024 * 1024);
        let query_op = if advertised_op.is_none() {
            let is_form = content.content_type.as_deref()
                == Some("application/x-www-form-urlencoded")
                || decoded.trim_ascii_start().starts_with(b"Action=");
            if is_form {
                wire::form_action(&decoded)
            } else {
                wire::query_param(path, "Action")
            }
        } else {
            None
        };
        let op = advertised_op.or(query_op.as_deref()).unwrap_or(fallback_op);
        let reg = region.filter(|r| !r.is_empty()).unwrap_or("us-east-1");
        let req_id = self.next_uuid();

        let s3_raw = if service == "s3" {
            capture::unframe_aws_chunked_bytes(wire_body, content.aws_chunked, 64 * 1024 * 1024)
        } else {
            decoded
        };

        let ctx = ReqCtx {
            method,
            path,
            headers,
            region: reg,
            operation: op,
            content,
            raw_body: &s3_raw,
            req_id,
        };

        match service {
            "s3" => self.handle_s3(&ctx),
            "dynamodb" => self.handle_dynamodb(&ctx),
            "sqs" => self.handle_sqs(&ctx),
            "sns" => self.handle_sns(&ctx),
            "secretsmanager" => self.handle_secrets(&ctx),
            "ssm" => self.handle_ssm(&ctx),
            "kms" => self.handle_kms(&ctx),
            "sts" => self.handle_sts(&ctx),
            "iam" => self.handle_iam(&ctx),
            "lambda" => self.handle_lambda(&ctx),
            "logs" => self.handle_logs(&ctx),
            "events" | "eventbridge" => self.handle_events(&ctx),
            "monitoring" | "cloudwatch" => self.handle_cloudwatch(&ctx),
            "kinesis" => self.handle_kinesis(&ctx),
            "states" | "stepfunctions" => self.handle_sfn(&ctx),
            _ => self.handle_generic(&ctx),
        }
    }

    fn lock_store(&self) -> std::sync::MutexGuard<'_, SandboxStore> {
        self.store.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reject a request whose wire body exceeded the proxy's bounded inspection
    /// limit before any truncated payload can mutate sandbox state.
    pub fn payload_too_large(
        &self,
        service: &str,
        headers: &HeaderMap,
        content: &Content,
        limit: usize,
    ) -> Response<BoxBody> {
        let req_id = self.next_uuid();
        let message = format!(
            "Request body exceeds the built-in local sandbox limit ({limit} bytes); start a local emulator (e.g. MinIO or LocalStack) for larger uploads."
        );
        if matches!(service, "s3" | "s3-object-lambda") {
            return s3_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "EntityTooLarge",
                &message,
                &req_id,
            );
        }
        let is_query = !headers.contains_key("x-amz-target")
            && content.content_type.as_deref() == Some("application/x-www-form-urlencoded");
        if is_query {
            let xml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                 <ErrorResponse><Error><Type>Sender</Type><Code>RequestEntityTooLarge</Code>\
                 <Message>{}</Message></Error><RequestId>{}</RequestId></ErrorResponse>",
                xml_escape(&message),
                xml_escape(&req_id),
            );
            return s3_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "text/xml; charset=utf-8",
                Bytes::from(xml),
                &req_id,
                &[],
            );
        }
        let dummy_ctx = ReqCtx {
            method: &Method::POST,
            path: "/",
            headers,
            region: "us-east-1",
            operation: "",
            content,
            raw_body: &[],
            req_id,
        };
        rpc_error(
            &dummy_ctx,
            StatusCode::PAYLOAD_TOO_LARGE,
            "RequestEntityTooLargeException",
            &message,
        )
    }

    // =========================================================================
    // S3 (REST XML / Object Storage)
    // =========================================================================

    fn handle_s3(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let (bucket, key) = parse_s3_target(ctx.headers, ctx.path);
        let mut store = self.lock_store();

        match ctx.operation {
            "ListBuckets" => {
                let mut items = String::new();
                for (name, b) in &store.s3_buckets {
                    items.push_str(&format!(
                        "<Bucket><Name>{}</Name><CreationDate>{}</CreationDate></Bucket>",
                        xml_escape(name),
                        xml_escape(&b.created_at)
                    ));
                }
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <ListAllMyBucketsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                     <Owner><ID>lenslet</ID><DisplayName>lenslet</DisplayName></Owner>\
                     <Buckets>{items}</Buckets></ListAllMyBucketsResult>"
                );
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::from(xml),
                    &ctx.req_id,
                    &[],
                )
            }
            "CreateBucket" => {
                let Some(bucket) = bucket else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidBucketName",
                        "Missing bucket name.",
                        &ctx.req_id,
                    );
                };
                store
                    .s3_buckets
                    .entry(bucket.clone())
                    .or_insert_with(|| S3Bucket {
                        created_at: Utc::now().to_rfc3339(),
                        objects: BTreeMap::new(),
                    });
                let loc = format!("/{bucket}");
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::new(),
                    &ctx.req_id,
                    &[("location", loc.as_str())],
                )
            }
            "HeadBucket" => {
                let Some(bucket) = bucket else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidBucketName",
                        "Missing bucket name.",
                        &ctx.req_id,
                    );
                };
                if store.s3_buckets.contains_key(&bucket) {
                    s3_response(
                        StatusCode::OK,
                        "application/xml",
                        Bytes::new(),
                        &ctx.req_id,
                        &[("x-amz-bucket-region", ctx.region)],
                    )
                } else {
                    s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchBucket",
                        "The specified bucket does not exist.",
                        &ctx.req_id,
                    )
                }
            }
            "DeleteBucket" => {
                let Some(bucket) = bucket else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidBucketName",
                        "Missing bucket name.",
                        &ctx.req_id,
                    );
                };
                match store.s3_buckets.get(&bucket) {
                    None => s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchBucket",
                        "The specified bucket does not exist.",
                        &ctx.req_id,
                    ),
                    Some(b) if !b.objects.is_empty() => s3_error(
                        StatusCode::CONFLICT,
                        "BucketNotEmpty",
                        "The bucket you tried to delete is not empty.",
                        &ctx.req_id,
                    ),
                    Some(_) => {
                        store.s3_buckets.remove(&bucket);
                        s3_response(
                            StatusCode::NO_CONTENT,
                            "application/xml",
                            Bytes::new(),
                            &ctx.req_id,
                            &[],
                        )
                    }
                }
            }
            "PutObject" => {
                let (Some(bucket), Some(key)) = (bucket, key) else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequest",
                        "Bucket and key required.",
                        &ctx.req_id,
                    );
                };
                // Auto-provision bucket on PutObject so 1-line scripts work effortlessly.
                let b = store.s3_buckets.entry(bucket).or_insert_with(|| S3Bucket {
                    created_at: Utc::now().to_rfc3339(),
                    objects: BTreeMap::new(),
                });
                let etag = format!("\"{}\"", md5_hex(ctx.raw_body));
                let ct = header_str(ctx.headers, "content-type")
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let sse =
                    header_str(ctx.headers, "x-amz-server-side-encryption").map(str::to_string);
                let kms_key_id =
                    header_str(ctx.headers, "x-amz-server-side-encryption-aws-kms-key-id")
                        .map(str::to_string);
                let meta = extract_s3_meta(ctx.headers);
                let obj = S3Object {
                    bytes: Bytes::copy_from_slice(ctx.raw_body),
                    content_type: ct,
                    etag: etag.clone(),
                    last_modified: Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
                    sse: sse.clone(),
                    kms_key_id: kms_key_id.clone(),
                    meta,
                };
                b.objects.insert(key, obj);
                let mut extra = vec![("etag", etag)];
                if let Some(s) = sse {
                    extra.push(("x-amz-server-side-encryption", s));
                }
                if let Some(k) = kms_key_id {
                    extra.push(("x-amz-server-side-encryption-aws-kms-key-id", k));
                }
                let refs: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::new(),
                    &ctx.req_id,
                    &refs,
                )
            }
            "CopyObject" => {
                let (Some(dst_bucket), Some(dst_key)) = (bucket, key) else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequest",
                        "Destination bucket and key required.",
                        &ctx.req_id,
                    );
                };
                let copy_src = header_str(ctx.headers, "x-amz-copy-source").unwrap_or("");
                let clean_src =
                    wire::percent_decode_path(copy_src.split('?').next().unwrap_or(copy_src));
                let clean_src = clean_src.trim_start_matches('/');
                let Some((src_bucket, src_key)) = clean_src.split_once('/') else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidArgument",
                        "Invalid x-amz-copy-source.",
                        &ctx.req_id,
                    );
                };
                let cloned = store
                    .s3_buckets
                    .get(src_bucket)
                    .and_then(|b| b.objects.get(src_key))
                    .cloned();
                let Some(mut obj) = cloned else {
                    return s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchKey",
                        "The specified source key does not exist.",
                        &ctx.req_id,
                    );
                };
                obj.last_modified = Utc::now().to_rfc3339();
                let etag = obj.etag.clone();
                let last_mod = obj.last_modified.clone();
                store
                    .s3_buckets
                    .entry(dst_bucket)
                    .or_insert_with(|| S3Bucket {
                        created_at: Utc::now().to_rfc3339(),
                        objects: BTreeMap::new(),
                    })
                    .objects
                    .insert(dst_key, obj);
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <CopyObjectResult><LastModified>{}</LastModified><ETag>{}</ETag></CopyObjectResult>",
                    xml_escape(&last_mod),
                    xml_escape(&etag)
                );
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::from(xml),
                    &ctx.req_id,
                    &[],
                )
            }
            "GetObject" | "HeadObject" => {
                let (Some(bucket), Some(key)) = (bucket, key) else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequest",
                        "Bucket and key required.",
                        &ctx.req_id,
                    );
                };
                let Some(b) = store.s3_buckets.get(&bucket) else {
                    return s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchBucket",
                        "The specified bucket does not exist.",
                        &ctx.req_id,
                    );
                };
                let Some(obj) = b.objects.get(&key) else {
                    return s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchKey",
                        "The specified key does not exist.",
                        &ctx.req_id,
                    );
                };
                let len_str = obj.bytes.len().to_string();
                let mut extra: Vec<(String, String)> = vec![
                    ("etag".into(), obj.etag.clone()),
                    ("last-modified".into(), obj.last_modified.clone()),
                ];
                if ctx.method == Method::HEAD {
                    extra.push(("content-length".into(), len_str));
                }
                if let Some(ref s) = obj.sse {
                    extra.push(("x-amz-server-side-encryption".into(), s.clone()));
                }
                if let Some(ref k) = obj.kms_key_id {
                    extra.push((
                        "x-amz-server-side-encryption-aws-kms-key-id".into(),
                        k.clone(),
                    ));
                }
                for (mk, mv) in &obj.meta {
                    extra.push((mk.clone(), mv.clone()));
                }
                let refs: Vec<(&str, &str)> = extra
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                let payload = if ctx.method == Method::HEAD || ctx.operation == "HeadObject" {
                    Bytes::new()
                } else {
                    obj.bytes.clone()
                };
                s3_response(
                    StatusCode::OK,
                    &obj.content_type,
                    payload,
                    &ctx.req_id,
                    &refs,
                )
            }
            "DeleteObject" => {
                if let (Some(bucket), Some(key)) = (bucket, key)
                    && let Some(b) = store.s3_buckets.get_mut(&bucket)
                {
                    b.objects.remove(&key);
                }
                s3_response(
                    StatusCode::NO_CONTENT,
                    "application/xml",
                    Bytes::new(),
                    &ctx.req_id,
                    &[],
                )
            }
            "DeleteObjects" => {
                let mut deleted_xml = String::new();
                if let Some(bucket) = bucket {
                    let body_str = String::from_utf8_lossy(ctx.raw_body);
                    let keys = extract_xml_tags(&body_str, "Key");
                    if let Some(b) = store.s3_buckets.get_mut(&bucket) {
                        for k in &keys {
                            b.objects.remove(k);
                        }
                    }
                    for k in keys {
                        deleted_xml
                            .push_str(&format!("<Deleted><Key>{}</Key></Deleted>", xml_escape(&k)));
                    }
                }
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{deleted_xml}</DeleteResult>"
                );
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::from(xml),
                    &ctx.req_id,
                    &[],
                )
            }
            "ListObjectsV2" | "ListObjects" => {
                let Some(bucket) = bucket else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidBucketName",
                        "Missing bucket name.",
                        &ctx.req_id,
                    );
                };
                let Some(b) = store.s3_buckets.get(&bucket) else {
                    return s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchBucket",
                        "The specified bucket does not exist.",
                        &ctx.req_id,
                    );
                };
                let prefix = wire::query_param(ctx.path, "prefix").unwrap_or_default();
                let max_keys = wire::query_param(ctx.path, "max-keys")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1000);
                let mut contents = String::new();
                let mut count = 0usize;
                for (k, obj) in b
                    .objects
                    .iter()
                    .filter(|(k, _)| k.starts_with(&prefix))
                    .take(max_keys)
                {
                    count += 1;
                    contents.push_str(&format!(
                        "<Contents><Key>{}</Key><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                        xml_escape(k),
                        xml_escape(&Utc::now().to_rfc3339()),
                        xml_escape(&obj.etag),
                        obj.bytes.len()
                    ));
                }
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                     <Name>{}</Name><Prefix>{}</Prefix><KeyCount>{count}</KeyCount><MaxKeys>{max_keys}</MaxKeys>\
                     <IsTruncated>false</IsTruncated>{contents}</ListBucketResult>",
                    xml_escape(&bucket),
                    xml_escape(&prefix)
                );
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::from(xml),
                    &ctx.req_id,
                    &[],
                )
            }
            "CreateMultipartUpload" => {
                let (Some(bucket), Some(key)) = (bucket, key) else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequest",
                        "Bucket and key required.",
                        &ctx.req_id,
                    );
                };
                store
                    .s3_buckets
                    .entry(bucket.clone())
                    .or_insert_with(|| S3Bucket {
                        created_at: Utc::now().to_rfc3339(),
                        objects: BTreeMap::new(),
                    });
                let upload_id = self.next_uuid();
                let ct = header_str(ctx.headers, "content-type")
                    .unwrap_or("application/octet-stream")
                    .to_string();
                let sse =
                    header_str(ctx.headers, "x-amz-server-side-encryption").map(str::to_string);
                let kms_key_id =
                    header_str(ctx.headers, "x-amz-server-side-encryption-aws-kms-key-id")
                        .map(str::to_string);
                let meta = extract_s3_meta(ctx.headers);
                store.s3_uploads.insert(
                    upload_id.clone(),
                    S3MultipartUpload {
                        bucket: bucket.clone(),
                        key: key.clone(),
                        content_type: ct,
                        sse: sse.clone(),
                        kms_key_id: kms_key_id.clone(),
                        meta,
                        initiated_at: Utc::now().to_rfc3339(),
                        parts: BTreeMap::new(),
                    },
                );
                let mut extra = Vec::new();
                if let Some(s) = sse {
                    extra.push(("x-amz-server-side-encryption", s));
                }
                if let Some(k) = kms_key_id {
                    extra.push(("x-amz-server-side-encryption-aws-kms-key-id", k));
                }
                let refs: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                     <Bucket>{}</Bucket><Key>{}</Key><UploadId>{upload_id}</UploadId>\
                     </InitiateMultipartUploadResult>",
                    xml_escape(&bucket),
                    xml_escape(&key)
                );
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::from(xml),
                    &ctx.req_id,
                    &refs,
                )
            }
            "UploadPart" => {
                let (Some(bucket), Some(key)) = (bucket, key) else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequest",
                        "Bucket and key required.",
                        &ctx.req_id,
                    );
                };
                let Some(upload_id) =
                    wire::query_param(ctx.path, "uploadId").filter(|s| !s.is_empty())
                else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidArgument",
                        "Missing uploadId query parameter.",
                        &ctx.req_id,
                    );
                };
                let Some(part_number) = wire::query_param(ctx.path, "partNumber")
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|n| (1..=10_000).contains(n))
                else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidArgument",
                        "partNumber must be an integer between 1 and 10000.",
                        &ctx.req_id,
                    );
                };
                let Some(upload) = store
                    .s3_uploads
                    .get_mut(&upload_id)
                    .filter(|u| u.bucket == bucket && u.key == key)
                else {
                    return s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchUpload",
                        "The specified multipart upload does not exist.",
                        &ctx.req_id,
                    );
                };
                let md5 = md5_bytes(ctx.raw_body);
                let etag = format!("\"{}\"", capture::hex(&md5));
                let sse = upload.sse.clone();
                let kms_key_id = upload.kms_key_id.clone();
                upload.parts.insert(
                    part_number,
                    S3UploadPart {
                        bytes: Bytes::copy_from_slice(ctx.raw_body),
                        etag: etag.clone(),
                        md5,
                        last_modified: Utc::now().to_rfc3339(),
                    },
                );
                let mut extra = vec![("etag", etag)];
                if let Some(s) = sse {
                    extra.push(("x-amz-server-side-encryption", s));
                }
                if let Some(k) = kms_key_id {
                    extra.push(("x-amz-server-side-encryption-aws-kms-key-id", k));
                }
                let refs: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::new(),
                    &ctx.req_id,
                    &refs,
                )
            }
            "UploadPartCopy" => {
                let (Some(bucket), Some(key)) = (bucket, key) else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequest",
                        "Bucket and key required.",
                        &ctx.req_id,
                    );
                };
                let Some(upload_id) =
                    wire::query_param(ctx.path, "uploadId").filter(|s| !s.is_empty())
                else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidArgument",
                        "Missing uploadId query parameter.",
                        &ctx.req_id,
                    );
                };
                let Some(part_number) = wire::query_param(ctx.path, "partNumber")
                    .and_then(|s| s.parse::<u32>().ok())
                    .filter(|n| (1..=10_000).contains(n))
                else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidArgument",
                        "partNumber must be an integer between 1 and 10000.",
                        &ctx.req_id,
                    );
                };
                let copy_src = header_str(ctx.headers, "x-amz-copy-source").unwrap_or("");
                let clean_src =
                    wire::percent_decode_path(copy_src.split('?').next().unwrap_or(copy_src));
                let clean_src = clean_src.trim_start_matches('/');
                let Some((src_bucket, src_key)) = clean_src.split_once('/') else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidArgument",
                        "Invalid x-amz-copy-source.",
                        &ctx.req_id,
                    );
                };
                let Some(src_bytes) = store
                    .s3_buckets
                    .get(src_bucket)
                    .and_then(|b| b.objects.get(src_key))
                    .map(|o| o.bytes.clone())
                else {
                    return s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchKey",
                        "The specified source key does not exist.",
                        &ctx.req_id,
                    );
                };
                let slice =
                    if let Some(range_hdr) = header_str(ctx.headers, "x-amz-copy-source-range") {
                        let Some((start, end)) = range_hdr
                            .strip_prefix("bytes=")
                            .and_then(|r| r.split_once('-'))
                            .and_then(|(s, e)| {
                                Some((s.parse::<usize>().ok()?, e.parse::<usize>().ok()?))
                            })
                            .filter(|(s, e)| s <= e && *e < src_bytes.len())
                        else {
                            return s3_error(
                                StatusCode::BAD_REQUEST,
                                "InvalidArgument",
                                "Invalid x-amz-copy-source-range.",
                                &ctx.req_id,
                            );
                        };
                        src_bytes.slice(start..=end)
                    } else {
                        src_bytes
                    };
                let Some(upload) = store
                    .s3_uploads
                    .get_mut(&upload_id)
                    .filter(|u| u.bucket == bucket && u.key == key)
                else {
                    return s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchUpload",
                        "The specified multipart upload does not exist.",
                        &ctx.req_id,
                    );
                };
                let md5 = md5_bytes(&slice);
                let etag = format!("\"{}\"", capture::hex(&md5));
                let last_modified = Utc::now().to_rfc3339();
                upload.parts.insert(
                    part_number,
                    S3UploadPart {
                        bytes: slice,
                        etag: etag.clone(),
                        md5,
                        last_modified: last_modified.clone(),
                    },
                );
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <CopyPartResult><LastModified>{}</LastModified><ETag>{}</ETag></CopyPartResult>",
                    xml_escape(&last_modified),
                    xml_escape(&etag)
                );
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::from(xml),
                    &ctx.req_id,
                    &[],
                )
            }
            "ListParts" => {
                let (Some(bucket), Some(key)) = (bucket, key) else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequest",
                        "Bucket and key required.",
                        &ctx.req_id,
                    );
                };
                let upload_id = wire::query_param(ctx.path, "uploadId").unwrap_or_default();
                let Some(upload) = store
                    .s3_uploads
                    .get(&upload_id)
                    .filter(|u| u.bucket == bucket && u.key == key)
                else {
                    return s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchUpload",
                        "The specified multipart upload does not exist.",
                        &ctx.req_id,
                    );
                };
                let mut parts_xml = String::new();
                for (pn, part) in &upload.parts {
                    parts_xml.push_str(&format!(
                        "<Part><PartNumber>{pn}</PartNumber><LastModified>{}</LastModified><ETag>{}</ETag><Size>{}</Size></Part>",
                        xml_escape(&part.last_modified),
                        xml_escape(&part.etag),
                        part.bytes.len()
                    ));
                }
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <ListPartsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                     <Bucket>{}</Bucket><Key>{}</Key><UploadId>{}</UploadId>\
                     <IsTruncated>false</IsTruncated>{parts_xml}</ListPartsResult>",
                    xml_escape(&bucket),
                    xml_escape(&key),
                    xml_escape(&upload_id)
                );
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::from(xml),
                    &ctx.req_id,
                    &[],
                )
            }
            "ListMultipartUploads" => {
                let Some(bucket) = bucket else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidBucketName",
                        "Missing bucket name.",
                        &ctx.req_id,
                    );
                };
                let mut uploads_xml = String::new();
                for (uid, up) in store.s3_uploads.iter().filter(|(_, u)| u.bucket == bucket) {
                    uploads_xml.push_str(&format!(
                        "<Upload><Key>{}</Key><UploadId>{}</UploadId><Initiated>{}</Initiated></Upload>",
                        xml_escape(&up.key),
                        xml_escape(uid),
                        xml_escape(&up.initiated_at)
                    ));
                }
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <ListMultipartUploadsResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                     <Bucket>{}</Bucket><IsTruncated>false</IsTruncated>{uploads_xml}\
                     </ListMultipartUploadsResult>",
                    xml_escape(&bucket)
                );
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::from(xml),
                    &ctx.req_id,
                    &[],
                )
            }
            "AbortMultipartUpload" => {
                let (Some(bucket), Some(key)) = (bucket, key) else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequest",
                        "Bucket and key required.",
                        &ctx.req_id,
                    );
                };
                let upload_id = wire::query_param(ctx.path, "uploadId").unwrap_or_default();
                let matches_target = store
                    .s3_uploads
                    .get(&upload_id)
                    .is_some_and(|u| u.bucket == bucket && u.key == key);
                if !matches_target {
                    return s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchUpload",
                        "The specified multipart upload does not exist.",
                        &ctx.req_id,
                    );
                }
                store.s3_uploads.remove(&upload_id);
                s3_response(
                    StatusCode::NO_CONTENT,
                    "application/xml",
                    Bytes::new(),
                    &ctx.req_id,
                    &[],
                )
            }
            "CompleteMultipartUpload" => {
                let (Some(bucket), Some(key)) = (bucket, key) else {
                    return s3_error(
                        StatusCode::BAD_REQUEST,
                        "InvalidRequest",
                        "Bucket and key required.",
                        &ctx.req_id,
                    );
                };
                let upload_id = wire::query_param(ctx.path, "uploadId").unwrap_or_default();
                let Some(upload) = store
                    .s3_uploads
                    .get(&upload_id)
                    .filter(|u| u.bucket == bucket && u.key == key)
                else {
                    return s3_error(
                        StatusCode::NOT_FOUND,
                        "NoSuchUpload",
                        "The specified multipart upload does not exist.",
                        &ctx.req_id,
                    );
                };
                let body_xml = String::from_utf8_lossy(ctx.raw_body);
                let requested_parts = parse_complete_multipart_parts(&body_xml);
                let selected: Vec<(u32, Option<String>)> = match requested_parts {
                    Ok(list) if !list.is_empty() => list,
                    Ok(_) => upload.parts.keys().map(|&pn| (pn, None)).collect(),
                    Err((code, msg)) => {
                        return s3_error(StatusCode::BAD_REQUEST, code, msg, &ctx.req_id);
                    }
                };
                let mut prev_pn = 0u32;
                let mut total_len = 0usize;
                for (pn, expected_etag) in &selected {
                    if *pn <= prev_pn {
                        return s3_error(
                            StatusCode::BAD_REQUEST,
                            "InvalidPartOrder",
                            "The list of parts was not in ascending order.",
                            &ctx.req_id,
                        );
                    }
                    prev_pn = *pn;
                    let Some(part) = upload.parts.get(pn) else {
                        return s3_error(
                            StatusCode::BAD_REQUEST,
                            "InvalidPart",
                            "One or more of the specified parts could not be found.",
                            &ctx.req_id,
                        );
                    };
                    if let Some(exp) = expected_etag
                        && !etags_match(exp, &part.etag)
                    {
                        return s3_error(
                            StatusCode::BAD_REQUEST,
                            "InvalidPart",
                            "One or more of the specified parts had a mismatched ETag.",
                            &ctx.req_id,
                        );
                    }
                    total_len += part.bytes.len();
                }

                let mut assembled = Vec::with_capacity(total_len);
                let mut md5_concat = Vec::with_capacity(selected.len() * 16);
                for (pn, _) in &selected {
                    let part = &upload.parts[pn];
                    assembled.extend_from_slice(&part.bytes);
                    md5_concat.extend_from_slice(&part.md5);
                }
                let etag = if selected.is_empty() {
                    format!("\"{}\"", md5_hex(&[]))
                } else {
                    format!("\"{}-{}\"", md5_hex(&md5_concat), selected.len())
                };
                let completed = store.s3_uploads.remove(&upload_id).expect("checked upload");
                let sse = completed.sse.clone();
                let kms_key_id = completed.kms_key_id.clone();
                let obj = S3Object {
                    bytes: Bytes::from(assembled),
                    content_type: completed.content_type,
                    etag: etag.clone(),
                    last_modified: Utc::now().format("%a, %d %b %Y %H:%M:%S GMT").to_string(),
                    sse: sse.clone(),
                    kms_key_id: kms_key_id.clone(),
                    meta: completed.meta,
                };
                store
                    .s3_buckets
                    .entry(bucket.clone())
                    .or_insert_with(|| S3Bucket {
                        created_at: Utc::now().to_rfc3339(),
                        objects: BTreeMap::new(),
                    })
                    .objects
                    .insert(key.clone(), obj);
                let mut extra = Vec::new();
                if let Some(s) = sse {
                    extra.push(("x-amz-server-side-encryption", s));
                }
                if let Some(k) = kms_key_id {
                    extra.push(("x-amz-server-side-encryption-aws-kms-key-id", k));
                }
                let refs: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (*k, v.as_str())).collect();
                let location = format!("{}/{}/{}", self.endpoint, bucket, key);
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <CompleteMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                     <Location>{}</Location><Bucket>{}</Bucket><Key>{}</Key><ETag>{}</ETag>\
                     </CompleteMultipartUploadResult>",
                    xml_escape(&location),
                    xml_escape(&bucket),
                    xml_escape(&key),
                    xml_escape(&etag)
                );
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::from(xml),
                    &ctx.req_id,
                    &refs,
                )
            }
            "GetBucketLocation" => {
                let xml = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                     <LocationConstraint xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{}</LocationConstraint>",
                    xml_escape(ctx.region)
                );
                s3_response(
                    StatusCode::OK,
                    "application/xml",
                    Bytes::from(xml),
                    &ctx.req_id,
                    &[],
                )
            }
            _ => s3_response(
                StatusCode::OK,
                "application/xml",
                Bytes::new(),
                &ctx.req_id,
                &[],
            ),
        }
    }

    // =========================================================================
    // DynamoDB (JSON 1.0 & Smithy RPCv2 CBOR)
    // =========================================================================

    fn handle_dynamodb(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let body = parse_rpc_body(ctx);
        let mut store = self.lock_store();
        let reg = ctx.region.to_string();

        match ctx.operation {
            "CreateTable" => {
                let Some(name) = str_field(&body, "TableName") else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ValidationException",
                        "TableName is required.",
                    );
                };
                let key_attrs: Vec<String> = body
                    .get("KeySchema")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|el| {
                                el.get("AttributeName")
                                    .and_then(|a| a.as_str())
                                    .map(str::to_string)
                            })
                            .collect()
                    })
                    .filter(|v: &Vec<String>| !v.is_empty())
                    .unwrap_or_else(|| vec!["id".to_string()]);
                let mut indexes = BTreeMap::new();
                for field in ["GlobalSecondaryIndexes", "LocalSecondaryIndexes"] {
                    if let Some(arr) = body.get(field).and_then(|v| v.as_array()) {
                        for idx in arr {
                            if let Some(iname) = str_field(idx, "IndexName") {
                                let ikeys: Vec<String> = idx
                                    .get("KeySchema")
                                    .and_then(|v| v.as_array())
                                    .map(|ks| {
                                        ks.iter()
                                            .filter_map(|el| str_field(el, "AttributeName"))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                if !ikeys.is_empty() {
                                    indexes.insert(iname, ikeys);
                                }
                            }
                        }
                    }
                }
                let attr_defs = body
                    .get("AttributeDefinitions")
                    .cloned()
                    .unwrap_or_else(|| json!([]));
                let arn = format!("arn:aws:dynamodb:{reg}:{ACCOUNT_ID}:table/{name}");
                let epoch = Utc::now().timestamp() as f64;
                let table = DdbTable {
                    name: name.clone(),
                    arn: arn.clone(),
                    key_attrs: key_attrs.clone(),
                    indexes,
                    attr_defs: attr_defs.clone(),
                    created_epoch: epoch,
                    items: BTreeMap::new(),
                };
                store.ddb_tables.insert((reg, name.clone()), table);
                rpc_ok(
                    ctx,
                    json!({
                        "TableDescription": ddb_table_desc(&name, &arn, &key_attrs, &attr_defs, epoch, 0)
                    }),
                )
            }
            "DescribeTable" => {
                let name = str_field(&body, "TableName").unwrap_or_default();
                let Some(t) = store.ddb_tables.get(&(reg, name)) else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ResourceNotFoundException",
                        "Requested resource not found",
                    );
                };
                rpc_ok(
                    ctx,
                    json!({
                        "Table": ddb_table_desc(&t.name, &t.arn, &t.key_attrs, &t.attr_defs, t.created_epoch, t.items.len())
                    }),
                )
            }
            "DeleteTable" => {
                let name = str_field(&body, "TableName").unwrap_or_default();
                let Some(t) = store.ddb_tables.remove(&(reg, name)) else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ResourceNotFoundException",
                        "Requested resource not found",
                    );
                };
                rpc_ok(
                    ctx,
                    json!({
                        "TableDescription": ddb_table_desc(&t.name, &t.arn, &t.key_attrs, &t.attr_defs, t.created_epoch, t.items.len())
                    }),
                )
            }
            "ListTables" => {
                let names: Vec<String> = store
                    .ddb_tables
                    .keys()
                    .filter(|(r, _)| r == &reg)
                    .map(|(_, n)| n.clone())
                    .collect();
                rpc_ok(ctx, json!({ "TableNames": names }))
            }
            "PutItem" => {
                let name = str_field(&body, "TableName").unwrap_or_default();
                let Some(t) = store.ddb_tables.get_mut(&(reg, name)) else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ResourceNotFoundException",
                        "Requested resource not found",
                    );
                };
                let item = body.get("Item").cloned().unwrap_or_else(|| json!({}));
                let key = ddb_item_key(&item, &t.key_attrs);
                let old = t.items.insert(key, item);
                if str_field(&body, "ReturnValues").as_deref() == Some("ALL_OLD")
                    && let Some(prev) = old
                {
                    return rpc_ok(ctx, json!({ "Attributes": prev }));
                }
                rpc_ok(ctx, json!({}))
            }
            "GetItem" => {
                let name = str_field(&body, "TableName").unwrap_or_default();
                let Some(t) = store.ddb_tables.get(&(reg, name)) else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ResourceNotFoundException",
                        "Requested resource not found",
                    );
                };
                let key_obj = body.get("Key").cloned().unwrap_or_else(|| json!({}));
                let key = ddb_item_key(&key_obj, &t.key_attrs);
                match t.items.get(&key) {
                    Some(item) => rpc_ok(ctx, json!({ "Item": item })),
                    None => rpc_ok(ctx, json!({})),
                }
            }
            "DeleteItem" => {
                let name = str_field(&body, "TableName").unwrap_or_default();
                let Some(t) = store.ddb_tables.get_mut(&(reg, name)) else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ResourceNotFoundException",
                        "Requested resource not found",
                    );
                };
                let key_obj = body.get("Key").cloned().unwrap_or_else(|| json!({}));
                let key = ddb_item_key(&key_obj, &t.key_attrs);
                let removed = t.items.remove(&key);
                if str_field(&body, "ReturnValues").as_deref() == Some("ALL_OLD")
                    && let Some(prev) = removed
                {
                    return rpc_ok(ctx, json!({ "Attributes": prev }));
                }
                rpc_ok(ctx, json!({}))
            }
            "UpdateItem" => {
                let name = str_field(&body, "TableName").unwrap_or_default();
                let Some(t) = store.ddb_tables.get_mut(&(reg, name)) else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ResourceNotFoundException",
                        "Requested resource not found",
                    );
                };
                let key_obj = body.get("Key").cloned().unwrap_or_else(|| json!({}));
                let key = ddb_item_key(&key_obj, &t.key_attrs);
                let mut current = t
                    .items
                    .get(&key)
                    .cloned()
                    .unwrap_or_else(|| key_obj.clone());
                apply_ddb_update(&mut current, &body);
                t.items.insert(key, current.clone());
                rpc_ok(ctx, json!({ "Attributes": current }))
            }
            "Query" | "Scan" => {
                let name = str_field(&body, "TableName").unwrap_or_default();
                let Some(t) = store.ddb_tables.get(&(reg, name)) else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ResourceNotFoundException",
                        "Requested resource not found",
                    );
                };
                match execute_ddb_query_or_scan(t, &body, ctx.operation == "Query") {
                    Ok(payload) => rpc_ok(ctx, payload),
                    Err(msg) => {
                        rpc_error(ctx, StatusCode::BAD_REQUEST, "ValidationException", &msg)
                    }
                }
            }
            "BatchGetItem" => {
                let mut responses = Map::new();
                if let Some(req_items) = body.get("RequestItems").and_then(|v| v.as_object()) {
                    for (tname, spec) in req_items {
                        let Some(t) = store.ddb_tables.get(&(reg.clone(), tname.clone())) else {
                            return rpc_error(
                                ctx,
                                StatusCode::BAD_REQUEST,
                                "ResourceNotFoundException",
                                &format!("Table `{tname}` not found"),
                            );
                        };
                        let mut found = Vec::new();
                        if let Some(keys) = spec.get("Keys").and_then(|k| k.as_array()) {
                            for k_obj in keys {
                                let k = ddb_item_key(k_obj, &t.key_attrs);
                                if let Some(item) = t.items.get(&k) {
                                    found.push(item.clone());
                                }
                            }
                        }
                        responses.insert(tname.clone(), Value::Array(found));
                    }
                }
                rpc_ok(
                    ctx,
                    json!({ "Responses": responses, "UnprocessedKeys": {} }),
                )
            }
            "BatchWriteItem" => {
                if let Some(req_items) = body.get("RequestItems").and_then(|v| v.as_object()) {
                    for (tname, ops) in req_items {
                        let Some(t) = store.ddb_tables.get_mut(&(reg.clone(), tname.clone()))
                        else {
                            return rpc_error(
                                ctx,
                                StatusCode::BAD_REQUEST,
                                "ResourceNotFoundException",
                                &format!("Table `{tname}` not found"),
                            );
                        };
                        if let Some(arr) = ops.as_array() {
                            for entry in arr {
                                if let Some(put) =
                                    entry.get("PutRequest").and_then(|p| p.get("Item"))
                                {
                                    let k = ddb_item_key(put, &t.key_attrs);
                                    t.items.insert(k, put.clone());
                                } else if let Some(del) =
                                    entry.get("DeleteRequest").and_then(|d| d.get("Key"))
                                {
                                    let k = ddb_item_key(del, &t.key_attrs);
                                    t.items.remove(&k);
                                }
                            }
                        }
                    }
                }
                rpc_ok(ctx, json!({ "UnprocessedItems": {} }))
            }
            "TransactGetItems" => {
                let mut responses = Vec::new();
                if let Some(arr) = body.get("TransactItems").and_then(|v| v.as_array()) {
                    for entry in arr {
                        if let Some(get) = entry.get("Get") {
                            let tname = str_field(get, "TableName").unwrap_or_default();
                            let item = store.ddb_tables.get(&(reg.clone(), tname)).and_then(|t| {
                                let k = ddb_item_key(get.get("Key")?, &t.key_attrs);
                                t.items.get(&k).cloned()
                            });
                            responses.push(match item {
                                Some(it) => json!({ "Item": it }),
                                None => json!({}),
                            });
                        }
                    }
                }
                rpc_ok(ctx, json!({ "Responses": responses }))
            }
            "TransactWriteItems" => {
                if let Some(arr) = body.get("TransactItems").and_then(|v| v.as_array()) {
                    for entry in arr {
                        if let Some(put) = entry.get("Put") {
                            let tname = str_field(put, "TableName").unwrap_or_default();
                            if let Some(t) = store.ddb_tables.get_mut(&(reg.clone(), tname))
                                && let Some(it) = put.get("Item")
                            {
                                let k = ddb_item_key(it, &t.key_attrs);
                                t.items.insert(k, it.clone());
                            }
                        } else if let Some(del) = entry.get("Delete") {
                            let tname = str_field(del, "TableName").unwrap_or_default();
                            if let Some(t) = store.ddb_tables.get_mut(&(reg.clone(), tname))
                                && let Some(key_obj) = del.get("Key")
                            {
                                let k = ddb_item_key(key_obj, &t.key_attrs);
                                t.items.remove(&k);
                            }
                        }
                    }
                }
                rpc_ok(ctx, json!({}))
            }
            "DescribeTimeToLive" => rpc_ok(
                ctx,
                json!({
                    "TimeToLiveDescription": { "TimeToLiveStatus": "DISABLED" }
                }),
            ),
            "DescribeContinuousBackups" => rpc_ok(
                ctx,
                json!({
                    "ContinuousBackupsDescription": { "ContinuousBackupsStatus": "ENABLED" }
                }),
            ),
            "ListTagsOfResource" => rpc_ok(ctx, json!({ "Tags": [] })),
            _ => rpc_ok(ctx, json!({})),
        }
    }

    // =========================================================================
    // SQS (JSON 1.0, Smithy RPCv2 CBOR, & Legacy AWS Query XML)
    // =========================================================================

    fn handle_sqs(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let is_query = is_query_protocol(ctx);
        let form = if is_query {
            parse_query_params(ctx.path, ctx.raw_body)
        } else {
            BTreeMap::new()
        };
        let body = if is_query {
            Value::Null
        } else {
            parse_rpc_body(ctx)
        };
        let reg = ctx.region.to_string();
        let mut store = self.lock_store();

        let queue_name_from_url = |url: &str| -> String {
            url.trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("default")
                .to_string()
        };
        let resolve_qname = |body: &Value, form: &BTreeMap<String, String>| -> Option<String> {
            str_field(body, "QueueName")
                .or_else(|| form.get("QueueName").cloned())
                .or_else(|| {
                    str_field(body, "QueueUrl")
                        .or_else(|| form.get("QueueUrl").cloned())
                        .map(|u| queue_name_from_url(&u))
                })
                .or_else(|| {
                    let p = ctx.path.split('?').next().unwrap_or("");
                    let segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
                    (segs.len() >= 2 && segs[0] != "service")
                        .then(|| segs.last().unwrap().to_string())
                })
        };

        match ctx.operation {
            "CreateQueue" => {
                let qname = resolve_qname(&body, &form).unwrap_or_else(|| "queue".into());
                let qurl = format!("{}/{ACCOUNT_ID}/{qname}", self.endpoint);
                let qarn = format!("arn:aws:sqs:{reg}:{ACCOUNT_ID}:{qname}");
                store
                    .sqs_queues
                    .entry((reg, qname))
                    .or_insert_with(|| SqsQueue {
                        arn: qarn,
                        attrs: BTreeMap::new(),
                        messages: VecDeque::new(),
                    });
                if is_query {
                    query_xml_ok(
                        "CreateQueue",
                        "http://queue.amazonaws.com/doc/2012-11-05/",
                        &format!("<QueueUrl>{}</QueueUrl>", xml_escape(&qurl)),
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({ "QueueUrl": qurl }))
                }
            }
            "GetQueueUrl" => {
                let qname = resolve_qname(&body, &form).unwrap_or_default();
                if !store.sqs_queues.contains_key(&(reg, qname.clone())) {
                    return sqs_missing_queue(ctx, is_query);
                }
                let qurl = format!("{}/{ACCOUNT_ID}/{qname}", self.endpoint);
                if is_query {
                    query_xml_ok(
                        "GetQueueUrl",
                        "http://queue.amazonaws.com/doc/2012-11-05/",
                        &format!("<QueueUrl>{}</QueueUrl>", xml_escape(&qurl)),
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({ "QueueUrl": qurl }))
                }
            }
            "ListQueues" => {
                let prefix = str_field(&body, "QueueNamePrefix")
                    .or_else(|| form.get("QueueNamePrefix").cloned())
                    .unwrap_or_default();
                let urls: Vec<String> = store
                    .sqs_queues
                    .keys()
                    .filter(|(r, n)| r == &reg && n.starts_with(&prefix))
                    .map(|(_, n)| format!("{}/{ACCOUNT_ID}/{n}", self.endpoint))
                    .collect();
                if is_query {
                    let inner: String = urls
                        .iter()
                        .map(|u| format!("<QueueUrl>{}</QueueUrl>", xml_escape(u)))
                        .collect();
                    query_xml_ok(
                        "ListQueues",
                        "http://queue.amazonaws.com/doc/2012-11-05/",
                        &inner,
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({ "QueueUrls": urls }))
                }
            }
            "GetQueueAttributes" => {
                let qname = resolve_qname(&body, &form).unwrap_or_default();
                let Some(q) = store.sqs_queues.get(&(reg, qname)) else {
                    return sqs_missing_queue(ctx, is_query);
                };
                let mut attrs = q.attrs.clone();
                attrs.insert("QueueArn".into(), q.arn.clone());
                attrs.insert(
                    "ApproximateNumberOfMessages".into(),
                    q.messages.len().to_string(),
                );
                attrs.insert("ApproximateNumberOfMessagesNotVisible".into(), "0".into());
                if is_query {
                    let inner: String = attrs
                        .iter()
                        .map(|(k, v)| {
                            format!(
                                "<Attribute><Name>{}</Name><Value>{}</Value></Attribute>",
                                xml_escape(k),
                                xml_escape(v)
                            )
                        })
                        .collect();
                    query_xml_ok(
                        "GetQueueAttributes",
                        "http://queue.amazonaws.com/doc/2012-11-05/",
                        &inner,
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({ "Attributes": attrs }))
                }
            }
            "SendMessage" => {
                let qname = resolve_qname(&body, &form).unwrap_or_else(|| "default".into());
                let msg_body = str_field(&body, "MessageBody")
                    .or_else(|| form.get("MessageBody").cloned())
                    .unwrap_or_default();
                let md5 = md5_hex(msg_body.as_bytes());
                let msg_id = self.next_uuid();
                let receipt = self.next_uuid();
                let msg_attrs = body
                    .get("MessageAttributes")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let q = store
                    .sqs_queues
                    .entry((reg.clone(), qname.clone()))
                    .or_insert_with(|| SqsQueue {
                        arn: format!("arn:aws:sqs:{reg}:{ACCOUNT_ID}:{qname}"),
                        attrs: BTreeMap::new(),
                        messages: VecDeque::new(),
                    });
                q.messages.push_back(SqsMsg {
                    id: msg_id.clone(),
                    receipt,
                    body: msg_body,
                    md5: md5.clone(),
                    attrs: msg_attrs,
                });
                if is_query {
                    query_xml_ok(
                        "SendMessage",
                        "http://queue.amazonaws.com/doc/2012-11-05/",
                        &format!(
                            "<MD5OfMessageBody>{md5}</MD5OfMessageBody><MessageId>{msg_id}</MessageId>"
                        ),
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(
                        ctx,
                        json!({
                            "MD5OfMessageBody": md5,
                            "MessageId": msg_id
                        }),
                    )
                }
            }
            "SendMessageBatch" => {
                let qname = resolve_qname(&body, &form).unwrap_or_else(|| "default".into());
                let mut successful = Vec::new();
                let entries = body
                    .get("Entries")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                for entry in entries {
                    let id = str_field(&entry, "Id").unwrap_or_else(|| "0".into());
                    let mbody = str_field(&entry, "MessageBody").unwrap_or_default();
                    let md5 = md5_hex(mbody.as_bytes());
                    let msg_id = self.next_uuid();
                    let receipt = self.next_uuid();
                    let q = store
                        .sqs_queues
                        .entry((reg.clone(), qname.clone()))
                        .or_insert_with(|| SqsQueue {
                            arn: format!("arn:aws:sqs:{reg}:{ACCOUNT_ID}:{qname}"),
                            attrs: BTreeMap::new(),
                            messages: VecDeque::new(),
                        });
                    q.messages.push_back(SqsMsg {
                        id: msg_id.clone(),
                        receipt,
                        body: mbody,
                        md5: md5.clone(),
                        attrs: entry
                            .get("MessageAttributes")
                            .cloned()
                            .unwrap_or_else(|| json!({})),
                    });
                    successful.push(json!({
                        "Id": id,
                        "MessageId": msg_id,
                        "MD5OfMessageBody": md5
                    }));
                }
                rpc_ok(ctx, json!({ "Successful": successful, "Failed": [] }))
            }
            "ReceiveMessage" => {
                let qname = resolve_qname(&body, &form).unwrap_or_default();
                let max_n = body
                    .get("MaxNumberOfMessages")
                    .and_then(|v| v.as_u64())
                    .or_else(|| form.get("MaxNumberOfMessages").and_then(|s| s.parse().ok()))
                    .unwrap_or(1)
                    .clamp(1, 10) as usize;
                let mut out_msgs = Vec::new();
                if let Some(q) = store.sqs_queues.get_mut(&(reg, qname)) {
                    for m in q.messages.iter().take(max_n) {
                        let mut obj = json!({
                            "MessageId": m.id,
                            "ReceiptHandle": m.receipt,
                            "MD5OfBody": m.md5,
                            "Body": m.body,
                        });
                        if m.attrs.as_object().is_some_and(|o| !o.is_empty()) {
                            obj["MessageAttributes"] = m.attrs.clone();
                        }
                        out_msgs.push(obj);
                    }
                }
                if is_query {
                    let mut inner = String::new();
                    for m in &out_msgs {
                        inner.push_str(&format!(
                            "<Message><MessageId>{}</MessageId><ReceiptHandle>{}</ReceiptHandle><MD5OfBody>{}</MD5OfBody><Body>{}</Body></Message>",
                            xml_escape(m["MessageId"].as_str().unwrap_or("")),
                            xml_escape(m["ReceiptHandle"].as_str().unwrap_or("")),
                            xml_escape(m["MD5OfBody"].as_str().unwrap_or("")),
                            xml_escape(m["Body"].as_str().unwrap_or("")),
                        ));
                    }
                    query_xml_ok(
                        "ReceiveMessage",
                        "http://queue.amazonaws.com/doc/2012-11-05/",
                        &inner,
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({ "Messages": out_msgs }))
                }
            }
            "DeleteMessage" => {
                let qname = resolve_qname(&body, &form).unwrap_or_default();
                let handle = str_field(&body, "ReceiptHandle")
                    .or_else(|| form.get("ReceiptHandle").cloned())
                    .unwrap_or_default();
                if let Some(q) = store.sqs_queues.get_mut(&(reg, qname)) {
                    q.messages.retain(|m| m.receipt != handle);
                }
                if is_query {
                    query_xml_ok(
                        "DeleteMessage",
                        "http://queue.amazonaws.com/doc/2012-11-05/",
                        "",
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({}))
                }
            }
            "DeleteMessageBatch" => {
                let qname = resolve_qname(&body, &form).unwrap_or_default();
                let mut successful = Vec::new();
                if let Some(entries) = body.get("Entries").and_then(|v| v.as_array()) {
                    for e in entries {
                        let id = str_field(e, "Id").unwrap_or_default();
                        let rh = str_field(e, "ReceiptHandle").unwrap_or_default();
                        if let Some(q) = store.sqs_queues.get_mut(&(reg.clone(), qname.clone())) {
                            q.messages.retain(|m| m.receipt != rh);
                        }
                        successful.push(json!({ "Id": id }));
                    }
                }
                rpc_ok(ctx, json!({ "Successful": successful, "Failed": [] }))
            }
            "PurgeQueue" => {
                let qname = resolve_qname(&body, &form).unwrap_or_default();
                if let Some(q) = store.sqs_queues.get_mut(&(reg, qname)) {
                    q.messages.clear();
                }
                if is_query {
                    query_xml_ok(
                        "PurgeQueue",
                        "http://queue.amazonaws.com/doc/2012-11-05/",
                        "",
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({}))
                }
            }
            "DeleteQueue" => {
                let qname = resolve_qname(&body, &form).unwrap_or_default();
                store.sqs_queues.remove(&(reg, qname));
                if is_query {
                    query_xml_ok(
                        "DeleteQueue",
                        "http://queue.amazonaws.com/doc/2012-11-05/",
                        "",
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({}))
                }
            }
            _ => {
                if is_query {
                    query_xml_ok(
                        ctx.operation,
                        "http://queue.amazonaws.com/doc/2012-11-05/",
                        "",
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({}))
                }
            }
        }
    }

    // =========================================================================
    // SNS (Query XML & JSON)
    // =========================================================================

    fn handle_sns(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let is_query = is_query_protocol(ctx);
        let form = parse_query_params(ctx.path, ctx.raw_body);
        let body = if is_query {
            Value::Null
        } else {
            parse_rpc_body(ctx)
        };
        let reg = ctx.region.to_string();
        let ns = "http://sns.amazonaws.com/doc/2010-03-31/";
        let mut store = self.lock_store();

        match ctx.operation {
            "CreateTopic" => {
                let name = form
                    .get("Name")
                    .cloned()
                    .or_else(|| str_field(&body, "Name"))
                    .unwrap_or_else(|| "topic".into());
                let arn = format!("arn:aws:sns:{reg}:{ACCOUNT_ID}:{name}");
                store
                    .sns_topics
                    .entry((reg, name))
                    .or_insert_with(|| SnsTopic {
                        arn: arn.clone(),
                        subscriptions: Vec::new(),
                    });
                if is_query {
                    query_xml_ok(
                        "CreateTopic",
                        ns,
                        &format!("<TopicArn>{}</TopicArn>", xml_escape(&arn)),
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({ "TopicArn": arn }))
                }
            }
            "ListTopics" => {
                let arns: Vec<String> = store
                    .sns_topics
                    .iter()
                    .filter(|((r, _), _)| r == &reg)
                    .map(|(_, t)| t.arn.clone())
                    .collect();
                if is_query {
                    let members: String = arns
                        .iter()
                        .map(|a| format!("<member><TopicArn>{}</TopicArn></member>", xml_escape(a)))
                        .collect();
                    query_xml_ok(
                        "ListTopics",
                        ns,
                        &format!("<Topics>{members}</Topics>"),
                        &ctx.req_id,
                    )
                } else {
                    let topics: Vec<Value> =
                        arns.into_iter().map(|a| json!({ "TopicArn": a })).collect();
                    rpc_ok(ctx, json!({ "Topics": topics }))
                }
            }
            "Subscribe" => {
                let topic_arn = form
                    .get("TopicArn")
                    .cloned()
                    .or_else(|| str_field(&body, "TopicArn"))
                    .unwrap_or_else(|| format!("arn:aws:sns:{reg}:{ACCOUNT_ID}:topic"));
                let sub_arn = format!("{topic_arn}:{}", self.next_uuid());
                if let Some((_, t)) = store
                    .sns_topics
                    .iter_mut()
                    .find(|(_, t)| t.arn == topic_arn)
                {
                    t.subscriptions.push(sub_arn.clone());
                }
                if is_query {
                    query_xml_ok(
                        "Subscribe",
                        ns,
                        &format!(
                            "<SubscriptionArn>{}</SubscriptionArn>",
                            xml_escape(&sub_arn)
                        ),
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({ "SubscriptionArn": sub_arn }))
                }
            }
            "Publish" => {
                let msg_id = self.next_uuid();
                if is_query {
                    query_xml_ok(
                        "Publish",
                        ns,
                        &format!("<MessageId>{msg_id}</MessageId>"),
                        &ctx.req_id,
                    )
                } else {
                    rpc_ok(ctx, json!({ "MessageId": msg_id }))
                }
            }
            "PublishBatch" => {
                let mut ok = Vec::new();
                for (k, v) in &form {
                    if k.ends_with(".Id") {
                        ok.push((v.clone(), self.next_uuid()));
                    }
                }
                if let Some(arr) = body
                    .get("PublishBatchRequestEntries")
                    .and_then(|v| v.as_array())
                {
                    for e in arr {
                        if let Some(id) = str_field(e, "Id") {
                            ok.push((id, self.next_uuid()));
                        }
                    }
                }
                if is_query {
                    let members: String = ok
                        .iter()
                        .map(|(id, mid)| {
                            format!(
                                "<member><Id>{}</Id><MessageId>{mid}</MessageId></member>",
                                xml_escape(id)
                            )
                        })
                        .collect();
                    query_xml_ok(
                        "PublishBatch",
                        ns,
                        &format!("<Successful>{members}</Successful><Failed/>"),
                        &ctx.req_id,
                    )
                } else {
                    let s: Vec<Value> = ok
                        .into_iter()
                        .map(|(id, mid)| json!({ "Id": id, "MessageId": mid }))
                        .collect();
                    rpc_ok(ctx, json!({ "Successful": s, "Failed": [] }))
                }
            }
            "DeleteTopic" => {
                let arn = form
                    .get("TopicArn")
                    .cloned()
                    .or_else(|| str_field(&body, "TopicArn"))
                    .unwrap_or_default();
                store.sns_topics.retain(|_, t| t.arn != arn);
                if is_query {
                    query_xml_ok("DeleteTopic", ns, "", &ctx.req_id)
                } else {
                    rpc_ok(ctx, json!({}))
                }
            }
            _ => {
                if is_query {
                    query_xml_ok(ctx.operation, ns, "", &ctx.req_id)
                } else {
                    rpc_ok(ctx, json!({}))
                }
            }
        }
    }

    // =========================================================================
    // Secrets Manager (JSON 1.1)
    // =========================================================================

    fn handle_secrets(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let body = parse_rpc_body(ctx);
        let reg = ctx.region.to_string();
        let mut store = self.lock_store();

        let resolve_name =
            |raw: &str| -> String { raw.rsplit(':').next().unwrap_or(raw).to_string() };

        match ctx.operation {
            "CreateSecret" | "PutSecretValue" | "UpdateSecret" => {
                let id = str_field(&body, "Name")
                    .or_else(|| str_field(&body, "SecretId"))
                    .unwrap_or_else(|| "secret".into());
                let name = resolve_name(&id);
                let arn = format!("arn:aws:secretsmanager:{reg}:{ACCOUNT_ID}:secret:{name}-AbCdEf");
                let ver = self.next_uuid();
                let rec = SecretRecord {
                    arn: arn.clone(),
                    name: name.clone(),
                    version_id: ver.clone(),
                    secret_string: str_field(&body, "SecretString"),
                    secret_binary: str_field(&body, "SecretBinary"),
                    created_epoch: Utc::now().timestamp() as f64,
                };
                store.secrets.insert((reg, name.clone()), rec);
                rpc_ok(
                    ctx,
                    json!({
                        "ARN": arn,
                        "Name": name,
                        "VersionId": ver,
                        "VersionStages": ["AWSCURRENT"]
                    }),
                )
            }
            "GetSecretValue" => {
                let id = str_field(&body, "SecretId").unwrap_or_default();
                let found = store
                    .secrets
                    .iter()
                    .find(|((r, n), rec)| {
                        r == &reg
                            && (n == &id
                                || rec.arn == id
                                || id.starts_with(&format!(
                                    "{}:secret:{n}",
                                    &rec.arn[..rec.arn.find(":secret:").unwrap_or(0)]
                                )))
                    })
                    .map(|(_, rec)| rec.clone());
                let Some(rec) = found else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ResourceNotFoundException",
                        "Secrets Manager can't find the specified secret.",
                    );
                };
                let mut out = json!({
                    "ARN": rec.arn,
                    "Name": rec.name,
                    "VersionId": rec.version_id,
                    "VersionStages": ["AWSCURRENT"],
                    "CreatedDate": rec.created_epoch,
                });
                if let Some(s) = rec.secret_string {
                    out["SecretString"] = Value::String(s);
                }
                if let Some(b) = rec.secret_binary {
                    out["SecretBinary"] = Value::String(b);
                }
                rpc_ok(ctx, out)
            }
            "DescribeSecret" => {
                let id = str_field(&body, "SecretId").unwrap_or_default();
                let name = resolve_name(&id);
                let Some(rec) = store.secrets.get(&(reg, name)) else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ResourceNotFoundException",
                        "Secret not found.",
                    );
                };
                rpc_ok(
                    ctx,
                    json!({
                        "ARN": rec.arn,
                        "Name": rec.name,
                        "CreatedDate": rec.created_epoch
                    }),
                )
            }
            "DeleteSecret" => {
                let id = str_field(&body, "SecretId").unwrap_or_default();
                let name = resolve_name(&id);
                let rec = store.secrets.remove(&(reg.clone(), name.clone()));
                let arn = rec.as_ref().map(|r| r.arn.clone()).unwrap_or_else(|| {
                    format!("arn:aws:secretsmanager:{reg}:{ACCOUNT_ID}:secret:{name}")
                });
                rpc_ok(
                    ctx,
                    json!({
                        "ARN": arn,
                        "Name": name,
                        "DeletionDate": Utc::now().timestamp() as f64
                    }),
                )
            }
            "ListSecrets" => {
                let list: Vec<Value> = store
                    .secrets
                    .iter()
                    .filter(|((r, _), _)| r == &reg)
                    .map(|(_, rec)| json!({ "ARN": rec.arn, "Name": rec.name, "CreatedDate": rec.created_epoch }))
                    .collect();
                rpc_ok(ctx, json!({ "SecretList": list }))
            }
            _ => rpc_ok(ctx, json!({})),
        }
    }

    // =========================================================================
    // SSM Parameter Store (JSON 1.1 & RPCv2 CBOR)
    // =========================================================================

    fn handle_ssm(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let body = parse_rpc_body(ctx);
        let reg = ctx.region.to_string();
        let mut store = self.lock_store();

        match ctx.operation {
            "PutParameter" => {
                let name = str_field(&body, "Name").unwrap_or_else(|| "/param".into());
                let val = str_field(&body, "Value").unwrap_or_default();
                let ptype = str_field(&body, "Type").unwrap_or_else(|| "String".into());
                let clean = name.trim_start_matches('/');
                let arn = format!("arn:aws:ssm:{reg}:{ACCOUNT_ID}:parameter/{clean}");
                let next_ver = store
                    .ssm_params
                    .get(&(reg.clone(), name.clone()))
                    .map(|p| p.version + 1)
                    .unwrap_or(1);
                store.ssm_params.insert(
                    (reg, name),
                    SsmParam {
                        name: str_field(&body, "Name").unwrap_or_else(|| "/param".into()),
                        r#type: ptype,
                        value: val,
                        version: next_ver,
                        arn,
                        last_modified: Utc::now().timestamp() as f64,
                    },
                );
                rpc_ok(ctx, json!({ "Version": next_ver, "Tier": "Standard" }))
            }
            "GetParameter" => {
                let name = str_field(&body, "Name").unwrap_or_default();
                let Some(p) = store.ssm_params.get(&(reg, name)) else {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ParameterNotFound",
                        "Parameter not found.",
                    );
                };
                rpc_ok(ctx, json!({ "Parameter": ssm_param_json(p) }))
            }
            "GetParameters" => {
                let names: Vec<String> = body
                    .get("Names")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut valid = Vec::new();
                let mut invalid = Vec::new();
                for n in names {
                    match store.ssm_params.get(&(reg.clone(), n.clone())) {
                        Some(p) => valid.push(ssm_param_json(p)),
                        None => invalid.push(n),
                    }
                }
                rpc_ok(
                    ctx,
                    json!({ "Parameters": valid, "InvalidParameters": invalid }),
                )
            }
            "GetParametersByPath" => {
                let path = str_field(&body, "Path").unwrap_or_else(|| "/".into());
                let params: Vec<Value> = store
                    .ssm_params
                    .iter()
                    .filter(|((r, n), _)| r == &reg && n.starts_with(&path))
                    .map(|(_, p)| ssm_param_json(p))
                    .collect();
                rpc_ok(ctx, json!({ "Parameters": params }))
            }
            "DeleteParameter" => {
                let name = str_field(&body, "Name").unwrap_or_default();
                if store.ssm_params.remove(&(reg, name)).is_none() {
                    return rpc_error(
                        ctx,
                        StatusCode::BAD_REQUEST,
                        "ParameterNotFound",
                        "Parameter not found.",
                    );
                }
                rpc_ok(ctx, json!({}))
            }
            "DeleteParameters" => {
                let names: Vec<String> = body
                    .get("Names")
                    .and_then(|v| v.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut deleted = Vec::new();
                let mut invalid = Vec::new();
                for n in names {
                    if store.ssm_params.remove(&(reg.clone(), n.clone())).is_some() {
                        deleted.push(n);
                    } else {
                        invalid.push(n);
                    }
                }
                rpc_ok(
                    ctx,
                    json!({ "DeletedParameters": deleted, "InvalidParameters": invalid }),
                )
            }
            _ => rpc_ok(ctx, json!({})),
        }
    }

    // =========================================================================
    // KMS (JSON 1.1)
    // =========================================================================

    fn handle_kms(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let body = parse_rpc_body(ctx);
        let reg = ctx.region.to_string();
        let mut store = self.lock_store();

        match ctx.operation {
            "CreateKey" => {
                let key_id = self.next_uuid();
                let arn = format!("arn:aws:kms:{reg}:{ACCOUNT_ID}:key/{key_id}");
                let desc = str_field(&body, "Description").unwrap_or_default();
                let epoch = Utc::now().timestamp() as f64;
                let rec = KmsKeyRecord {
                    key_id: key_id.clone(),
                    arn: arn.clone(),
                    description: desc.clone(),
                    created_epoch: epoch,
                };
                store.kms_keys.insert(key_id.clone(), rec);
                rpc_ok(
                    ctx,
                    json!({
                        "KeyMetadata": {
                            "AWSAccountId": ACCOUNT_ID,
                            "KeyId": key_id,
                            "Arn": arn,
                            "CreationDate": epoch,
                            "Enabled": true,
                            "Description": desc,
                            "KeyState": "Enabled",
                            "KeyUsage": "ENCRYPT_DECRYPT"
                        }
                    }),
                )
            }
            "CreateAlias" => {
                if let (Some(alias), Some(kid)) = (
                    str_field(&body, "AliasName"),
                    str_field(&body, "TargetKeyId"),
                ) {
                    store.kms_aliases.insert(alias, kid);
                }
                rpc_ok(ctx, json!({}))
            }
            "DescribeKey" => {
                let raw_id = str_field(&body, "KeyId").unwrap_or_else(|| "default".into());
                let resolved = store.kms_aliases.get(&raw_id).cloned().unwrap_or(raw_id);
                let kid = resolved.rsplit('/').next().unwrap_or(&resolved).to_string();
                let (arn, desc, created) = store
                    .kms_keys
                    .get(&kid)
                    .map(|k| (k.arn.clone(), k.description.clone(), k.created_epoch))
                    .unwrap_or_else(|| {
                        (
                            format!("arn:aws:kms:{reg}:{ACCOUNT_ID}:key/{kid}"),
                            String::new(),
                            0.0,
                        )
                    });
                rpc_ok(
                    ctx,
                    json!({
                        "KeyMetadata": {
                            "AWSAccountId": ACCOUNT_ID,
                            "KeyId": kid,
                            "Arn": arn,
                            "Description": desc,
                            "CreationDate": created,
                            "Enabled": true,
                            "KeyState": "Enabled",
                            "KeyUsage": "ENCRYPT_DECRYPT"
                        }
                    }),
                )
            }
            "ListKeys" => {
                let keys: Vec<Value> = store
                    .kms_keys
                    .values()
                    .map(|k| json!({ "KeyId": k.key_id, "KeyArn": k.arn }))
                    .collect();
                rpc_ok(ctx, json!({ "Keys": keys, "Truncated": false }))
            }
            "Encrypt" => {
                let raw_id = str_field(&body, "KeyId").unwrap_or_else(|| "sandbox-key".into());
                let arn = if raw_id.starts_with("arn:") {
                    raw_id
                } else {
                    format!("arn:aws:kms:{reg}:{ACCOUNT_ID}:key/{raw_id}")
                };
                let pt_b64 = str_field(&body, "Plaintext").unwrap_or_default();
                let envelope = json!({ "k": arn, "p": pt_b64 }).to_string();
                let ct_b64 = lenslet_core::trace::b64_encode(envelope.as_bytes());
                rpc_ok(
                    ctx,
                    json!({
                        "CiphertextBlob": ct_b64,
                        "KeyId": arn,
                        "EncryptionAlgorithm": "SYMMETRIC_DEFAULT"
                    }),
                )
            }
            "Decrypt" => {
                let ct_b64 = str_field(&body, "CiphertextBlob").unwrap_or_default();
                let decoded = lenslet_core::trace::b64_decode(&ct_b64).unwrap_or_default();
                if let Ok(env) = serde_json::from_slice::<Value>(&decoded)
                    && let (Some(k), Some(p)) = (
                        env.get("k").and_then(|v| v.as_str()),
                        env.get("p").and_then(|v| v.as_str()),
                    )
                {
                    return rpc_ok(
                        ctx,
                        json!({
                            "KeyId": k,
                            "Plaintext": p,
                            "EncryptionAlgorithm": "SYMMETRIC_DEFAULT"
                        }),
                    );
                }
                let fallback_arn = format!("arn:aws:kms:{reg}:{ACCOUNT_ID}:key/sandbox-key");
                rpc_ok(
                    ctx,
                    json!({
                        "KeyId": fallback_arn,
                        "Plaintext": ct_b64,
                        "EncryptionAlgorithm": "SYMMETRIC_DEFAULT"
                    }),
                )
            }
            "GenerateDataKey" => {
                let raw_id = str_field(&body, "KeyId").unwrap_or_else(|| "sandbox-key".into());
                let arn = if raw_id.starts_with("arn:") {
                    raw_id
                } else {
                    format!("arn:aws:kms:{reg}:{ACCOUNT_ID}:key/{raw_id}")
                };
                let pt_b64 = lenslet_core::trace::b64_encode(&[0x42u8; 32]);
                let envelope = json!({ "k": arn, "p": pt_b64 }).to_string();
                let ct_b64 = lenslet_core::trace::b64_encode(envelope.as_bytes());
                rpc_ok(
                    ctx,
                    json!({
                        "KeyId": arn,
                        "Plaintext": pt_b64,
                        "CiphertextBlob": ct_b64
                    }),
                )
            }
            _ => rpc_ok(ctx, json!({})),
        }
    }

    // =========================================================================
    // STS & IAM (Query XML)
    // =========================================================================

    fn handle_sts(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let form = parse_query_params(ctx.path, ctx.raw_body);
        let ns = "https://sts.amazonaws.com/doc/2011-06-15/";

        match ctx.operation {
            "GetCallerIdentity" => {
                let inner = format!(
                    "<Arn>arn:aws:iam::{ACCOUNT_ID}:user/lenslet-sandbox</Arn>\
                     <UserId>AIDALENSLETSANDBOX</UserId>\
                     <Account>{ACCOUNT_ID}</Account>"
                );
                query_xml_ok("GetCallerIdentity", ns, &inner, &ctx.req_id)
            }
            "AssumeRole" | "AssumeRoleWithWebIdentity" | "AssumeRoleWithSAML" => {
                let role_arn = form
                    .get("RoleArn")
                    .cloned()
                    .unwrap_or_else(|| format!("arn:aws:iam::{ACCOUNT_ID}:role/sandbox-role"));
                let session_name = form
                    .get("RoleSessionName")
                    .cloned()
                    .unwrap_or_else(|| "lenslet-session".into());
                let role_name = role_arn.rsplit('/').next().unwrap_or("sandbox-role");
                let assumed_arn =
                    format!("arn:aws:sts::{ACCOUNT_ID}:assumed-role/{role_name}/{session_name}");
                let n = self.next_seq();
                let akid = format!("ASIALENSLET{n:09X}");
                let exp = (Utc::now() + chrono::Duration::hours(1)).format("%Y-%m-%dT%H:%M:%SZ");
                let inner = format!(
                    "<Credentials>\
                       <AccessKeyId>{akid}</AccessKeyId>\
                       <SecretAccessKey>lenslet-sandbox-secret-{n}</SecretAccessKey>\
                       <SessionToken>lenslet-sandbox-token-{n}</SessionToken>\
                       <Expiration>{exp}</Expiration>\
                     </Credentials>\
                     <AssumedRoleUser>\
                       <AssumedRoleId>AROALENSLET:{}</AssumedRoleId>\
                       <Arn>{}</Arn>\
                     </AssumedRoleUser>",
                    xml_escape(&session_name),
                    xml_escape(&assumed_arn)
                );
                query_xml_ok(ctx.operation, ns, &inner, &ctx.req_id)
            }
            _ => query_xml_ok(ctx.operation, ns, "", &ctx.req_id),
        }
    }

    fn handle_iam(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let form = parse_query_params(ctx.path, ctx.raw_body);
        let ns = "https://iam.amazonaws.com/doc/2010-05-08/";
        let mut store = self.lock_store();

        match ctx.operation {
            "CreateRole" => {
                let rname = form
                    .get("RoleName")
                    .cloned()
                    .unwrap_or_else(|| "role".into());
                let arn = format!("arn:aws:iam::{ACCOUNT_ID}:role/{rname}");
                store.iam_roles.insert(rname.clone(), arn.clone());
                let inner = format!(
                    "<Role><Path>/</Path><RoleName>{}</RoleName><RoleId>AROALENSLET</RoleId><Arn>{}</Arn><CreateDate>{}</CreateDate></Role>",
                    xml_escape(&rname),
                    xml_escape(&arn),
                    Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
                );
                query_xml_ok("CreateRole", ns, &inner, &ctx.req_id)
            }
            "GetRole" => {
                let rname = form
                    .get("RoleName")
                    .cloned()
                    .unwrap_or_else(|| "role".into());
                let arn = store
                    .iam_roles
                    .get(&rname)
                    .cloned()
                    .unwrap_or_else(|| format!("arn:aws:iam::{ACCOUNT_ID}:role/{rname}"));
                let inner = format!(
                    "<Role><Path>/</Path><RoleName>{}</RoleName><RoleId>AROALENSLET</RoleId><Arn>{}</Arn><CreateDate>{}</CreateDate></Role>",
                    xml_escape(&rname),
                    xml_escape(&arn),
                    Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
                );
                query_xml_ok("GetRole", ns, &inner, &ctx.req_id)
            }
            _ => query_xml_ok(ctx.operation, ns, "", &ctx.req_id),
        }
    }

    // =========================================================================
    // Lambda, Logs, EventBridge, CloudWatch, Kinesis, Step Functions
    // =========================================================================

    fn handle_lambda(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let body = parse_rpc_body(ctx);
        let reg = ctx.region.to_string();
        let mut store = self.lock_store();
        let bare = ctx.path.split('?').next().unwrap_or("");
        let segs: Vec<&str> = bare.split('/').filter(|s| !s.is_empty()).collect();
        let path_fn = segs.get(2).copied().map(wire::percent_decode_path);

        match ctx.operation {
            "CreateFunction" => {
                let name = str_field(&body, "FunctionName").unwrap_or_else(|| "fn".into());
                let arn = format!("arn:aws:lambda:{reg}:{ACCOUNT_ID}:function:{name}");
                let rec = LambdaRecord {
                    name: name.clone(),
                    arn: arn.clone(),
                    role: str_field(&body, "Role")
                        .unwrap_or_else(|| format!("arn:aws:iam::{ACCOUNT_ID}:role/lambda-role")),
                    runtime: str_field(&body, "Runtime").unwrap_or_else(|| "python3.12".into()),
                    handler: str_field(&body, "Handler").unwrap_or_else(|| "index.handler".into()),
                    last_modified: Utc::now().to_rfc3339(),
                };
                store.lambda_fns.insert((reg, name), rec.clone());
                json_status(
                    StatusCode::CREATED,
                    json!({
                        "FunctionName": rec.name,
                        "FunctionArn": rec.arn,
                        "Runtime": rec.runtime,
                        "Role": rec.role,
                        "Handler": rec.handler,
                        "State": "Active",
                        "Version": "$LATEST",
                        "LastModified": rec.last_modified
                    }),
                    &ctx.req_id,
                    &[],
                )
            }
            "GetFunction" => {
                let name = path_fn.unwrap_or_else(|| "fn".into());
                let Some(rec) = store.lambda_fns.get(&(reg, name)) else {
                    return rpc_error(
                        ctx,
                        StatusCode::NOT_FOUND,
                        "ResourceNotFoundException",
                        "Function not found.",
                    );
                };
                rpc_ok(
                    ctx,
                    json!({
                        "Configuration": {
                            "FunctionName": rec.name,
                            "FunctionArn": rec.arn,
                            "Runtime": rec.runtime,
                            "Role": rec.role,
                            "Handler": rec.handler,
                            "State": "Active"
                        },
                        "Code": { "RepositoryType": "S3", "Location": format!("{}/code", self.endpoint) }
                    }),
                )
            }
            "ListFunctions" => {
                let fns: Vec<Value> = store
                    .lambda_fns
                    .iter()
                    .filter(|((r, _), _)| r == &reg)
                    .map(|(_, rec)| {
                        json!({
                            "FunctionName": rec.name,
                            "FunctionArn": rec.arn,
                            "Runtime": rec.runtime,
                            "Role": rec.role,
                            "Handler": rec.handler,
                            "State": "Active"
                        })
                    })
                    .collect();
                rpc_ok(ctx, json!({ "Functions": fns }))
            }
            "DeleteFunction" => {
                if let Some(name) = path_fn {
                    store.lambda_fns.remove(&(reg, name));
                }
                s3_response(
                    StatusCode::NO_CONTENT,
                    "application/json",
                    Bytes::new(),
                    &ctx.req_id,
                    &[],
                )
            }
            "Invoke" => {
                let echoed =
                    serde_json::from_slice::<Value>(ctx.raw_body).unwrap_or_else(|_| json!({}));
                json_status(
                    StatusCode::OK,
                    json!({ "statusCode": 200, "sandbox": true, "input": echoed }),
                    &ctx.req_id,
                    &[("x-amz-executed-version", "$LATEST")],
                )
            }
            _ => rpc_ok(ctx, json!({})),
        }
    }

    fn handle_logs(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let body = parse_rpc_body(ctx);
        let reg = ctx.region.to_string();
        let mut store = self.lock_store();

        match ctx.operation {
            "CreateLogGroup" => {
                let g = str_field(&body, "logGroupName").unwrap_or_else(|| "/aws/sandbox".into());
                store.log_groups.entry((reg, g)).or_default();
                rpc_ok(ctx, json!({}))
            }
            "CreateLogStream" => {
                let g = str_field(&body, "logGroupName").unwrap_or_else(|| "/aws/sandbox".into());
                let s = str_field(&body, "logStreamName").unwrap_or_else(|| "default".into());
                store
                    .log_groups
                    .entry((reg, g))
                    .or_default()
                    .entry(s)
                    .or_default();
                rpc_ok(ctx, json!({}))
            }
            "PutLogEvents" => {
                let g = str_field(&body, "logGroupName").unwrap_or_else(|| "/aws/sandbox".into());
                let s = str_field(&body, "logStreamName").unwrap_or_else(|| "default".into());
                let evs = body
                    .get("logEvents")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                store
                    .log_groups
                    .entry((reg, g))
                    .or_default()
                    .entry(s)
                    .or_default()
                    .extend(evs);
                rpc_ok(ctx, json!({ "nextSequenceToken": "1" }))
            }
            "GetLogEvents" | "FilterLogEvents" => {
                let g = str_field(&body, "logGroupName").unwrap_or_default();
                let mut events = Vec::new();
                if let Some(streams) = store.log_groups.get(&(reg, g)) {
                    for evs in streams.values() {
                        events.extend(evs.iter().cloned());
                    }
                }
                rpc_ok(
                    ctx,
                    json!({
                        "events": events,
                        "nextForwardToken": "f/1",
                        "nextBackwardToken": "b/1"
                    }),
                )
            }
            "DescribeLogGroups" => {
                let groups: Vec<Value> = store
                    .log_groups
                    .keys()
                    .filter(|(r, _)| r == &reg)
                    .map(|(_, g)| json!({ "logGroupName": g, "arn": format!("arn:aws:logs:{reg}:{ACCOUNT_ID}:log-group:{g}:*") }))
                    .collect();
                rpc_ok(ctx, json!({ "logGroups": groups }))
            }
            _ => rpc_ok(ctx, json!({})),
        }
    }

    fn handle_events(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let body = parse_rpc_body(ctx);
        let reg = ctx.region.to_string();
        let mut store = self.lock_store();

        match ctx.operation {
            "PutEvents" => {
                let count = body
                    .get("Entries")
                    .and_then(|v| v.as_array())
                    .map_or(1, Vec::len);
                let entries: Vec<Value> = (0..count)
                    .map(|_| json!({ "EventId": self.next_uuid() }))
                    .collect();
                rpc_ok(ctx, json!({ "FailedEntryCount": 0, "Entries": entries }))
            }
            "PutRule" => {
                let name = str_field(&body, "Name").unwrap_or_else(|| "rule".into());
                let arn = format!("arn:aws:events:{reg}:{ACCOUNT_ID}:rule/{name}");
                store.eb_rules.insert(
                    (reg, name.clone()),
                    json!({ "Name": name, "Arn": arn, "State": "ENABLED" }),
                );
                rpc_ok(ctx, json!({ "RuleArn": arn }))
            }
            "ListRules" => {
                let rules: Vec<Value> = store
                    .eb_rules
                    .iter()
                    .filter(|((r, _), _)| r == &reg)
                    .map(|(_, v)| v.clone())
                    .collect();
                rpc_ok(ctx, json!({ "Rules": rules }))
            }
            "PutTargets" | "RemoveTargets" => {
                rpc_ok(ctx, json!({ "FailedEntryCount": 0, "FailedEntries": [] }))
            }
            _ => rpc_ok(ctx, json!({})),
        }
    }

    fn handle_cloudwatch(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        if is_query_protocol(ctx) {
            query_xml_ok(
                ctx.operation,
                "http://monitoring.amazonaws.com/doc/2010-08-01/",
                "",
                &ctx.req_id,
            )
        } else {
            rpc_ok(ctx, json!({ "Metrics": [], "MetricDataResults": [] }))
        }
    }

    fn handle_kinesis(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let body = parse_rpc_body(ctx);
        let reg = ctx.region.to_string();
        let mut store = self.lock_store();

        match ctx.operation {
            "CreateStream" => {
                let name = str_field(&body, "StreamName").unwrap_or_else(|| "stream".into());
                store.kinesis_streams.entry((reg, name)).or_default();
                rpc_ok(ctx, json!({}))
            }
            "PutRecord" => {
                let name = str_field(&body, "StreamName").unwrap_or_else(|| "stream".into());
                let seq = self.next_seq().to_string();
                store
                    .kinesis_streams
                    .entry((reg, name))
                    .or_default()
                    .push(body.clone());
                rpc_ok(
                    ctx,
                    json!({ "ShardId": "shardId-000000000000", "SequenceNumber": seq }),
                )
            }
            "PutRecords" => {
                let n = body
                    .get("Records")
                    .and_then(|v| v.as_array())
                    .map_or(0, Vec::len);
                let recs: Vec<Value> = (0..n)
                    .map(|_| json!({ "ShardId": "shardId-000000000000", "SequenceNumber": self.next_seq().to_string() }))
                    .collect();
                rpc_ok(ctx, json!({ "FailedRecordCount": 0, "Records": recs }))
            }
            "GetShardIterator" => rpc_ok(ctx, json!({ "ShardIterator": "sandbox-iterator-0" })),
            "GetRecords" => rpc_ok(
                ctx,
                json!({ "Records": [], "NextShardIterator": "sandbox-iterator-1", "MillisBehindLatest": 0 }),
            ),
            _ => rpc_ok(ctx, json!({})),
        }
    }

    fn handle_sfn(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        let body = parse_rpc_body(ctx);
        let reg = ctx.region.to_string();
        let mut store = self.lock_store();
        let now = Utc::now().timestamp() as f64;

        match ctx.operation {
            "CreateStateMachine" => {
                let name = str_field(&body, "name").unwrap_or_else(|| "machine".into());
                let arn = format!("arn:aws:states:{reg}:{ACCOUNT_ID}:stateMachine:{name}");
                store.sfn_machines.insert(
                    (reg, name.clone()),
                    json!({ "name": name, "stateMachineArn": arn }),
                );
                rpc_ok(ctx, json!({ "stateMachineArn": arn, "creationDate": now }))
            }
            "StartExecution" => {
                let sm_arn = str_field(&body, "stateMachineArn").unwrap_or_else(|| {
                    format!("arn:aws:states:{reg}:{ACCOUNT_ID}:stateMachine:default")
                });
                let sm_name = sm_arn.rsplit(':').next().unwrap_or("default");
                let exec_name = str_field(&body, "name").unwrap_or_else(|| self.next_uuid());
                let exec_arn =
                    format!("arn:aws:states:{reg}:{ACCOUNT_ID}:execution:{sm_name}:{exec_name}");
                let input = str_field(&body, "input").unwrap_or_else(|| "{}".into());
                store.sfn_executions.insert(
                    exec_arn.clone(),
                    json!({
                        "executionArn": exec_arn,
                        "stateMachineArn": sm_arn,
                        "name": exec_name,
                        "status": "SUCCEEDED",
                        "startDate": now,
                        "stopDate": now,
                        "input": input,
                        "output": input,
                    }),
                );
                rpc_ok(ctx, json!({ "executionArn": exec_arn, "startDate": now }))
            }
            "DescribeExecution" => {
                let arn = str_field(&body, "executionArn").unwrap_or_default();
                let val = store.sfn_executions.get(&arn).cloned().unwrap_or_else(|| {
                    json!({
                        "executionArn": arn,
                        "status": "SUCCEEDED",
                        "startDate": now,
                        "stopDate": now,
                        "output": "{}"
                    })
                });
                rpc_ok(ctx, val)
            }
            _ => rpc_ok(ctx, json!({})),
        }
    }

    fn handle_generic(&self, ctx: &ReqCtx<'_>) -> Response<BoxBody> {
        if is_query_protocol(ctx) {
            query_xml_ok(
                ctx.operation,
                "https://amazonaws.com/doc/2012-10-17/",
                "",
                &ctx.req_id,
            )
        } else {
            rpc_ok(ctx, json!({}))
        }
    }
}

// =============================================================================
// Protocol & Cryptography Helpers
// =============================================================================

fn parse_rpc_body(ctx: &ReqCtx<'_>) -> Value {
    if ctx.raw_body.is_empty() {
        return json!({});
    }
    if ctx.content.cbor
        && let Some(val) = lenslet_core::cbor::decode_cbor(ctx.raw_body)
    {
        return val;
    }
    serde_json::from_slice(ctx.raw_body).unwrap_or_else(|_| json!({}))
}

fn is_query_protocol(ctx: &ReqCtx<'_>) -> bool {
    if ctx.content.cbor || ctx.headers.contains_key("x-amz-target") {
        return false;
    }
    ctx.content.content_type.as_deref() == Some("application/x-www-form-urlencoded")
        || ctx.raw_body.trim_ascii_start().starts_with(b"Action=")
        || wire::query_param(ctx.path, "Action").is_some()
}

fn parse_query_params(path: &str, body: &[u8]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut ingest = |raw: &str| {
        for pair in raw.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            map.insert(wire::percent_decode_query(k), wire::percent_decode_query(v));
        }
    };
    if let Some((_, q)) = path.split_once('?') {
        ingest(q);
    }
    if let Ok(s) = std::str::from_utf8(body) {
        ingest(s.trim());
    }
    map
}

fn rpc_ok(ctx: &ReqCtx<'_>, val: Value) -> Response<BoxBody> {
    if ctx.content.cbor {
        let bytes = lenslet_core::cbor::encode_cbor(&val);
        let mut resp = Response::new(
            Full::new(Bytes::from(bytes))
                .map_err(|never| match never {})
                .boxed(),
        );
        *resp.status_mut() = StatusCode::OK;
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/cbor"),
        );
        h.insert(
            "smithy-protocol",
            header::HeaderValue::from_static("rpc-v2-cbor"),
        );
        if let Ok(v) = header::HeaderValue::from_str(&ctx.req_id) {
            h.insert("x-amzn-requestid", v);
        }
        return resp;
    }
    let ct = match ctx.content.content_type.as_deref() {
        Some("application/x-amz-json-1.0") => "application/x-amz-json-1.0",
        Some("application/x-amz-json-1.1") => "application/x-amz-json-1.1",
        _ => "application/json",
    };
    let mut resp = Response::new(
        Full::new(Bytes::from(val.to_string()))
            .map_err(|never| match never {})
            .boxed(),
    );
    *resp.status_mut() = StatusCode::OK;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, header::HeaderValue::from_static(ct));
    if let Ok(v) = header::HeaderValue::from_str(&ctx.req_id) {
        h.insert("x-amzn-requestid", v);
    }
    resp
}

fn json_status(
    status: StatusCode,
    val: Value,
    req_id: &str,
    extra: &[(&str, &str)],
) -> Response<BoxBody> {
    let mut resp = Response::new(
        Full::new(Bytes::from(val.to_string()))
            .map_err(|never| match never {})
            .boxed(),
    );
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    if let Ok(v) = header::HeaderValue::from_str(req_id) {
        h.insert("x-amzn-requestid", v);
    }
    for (k, v) in extra {
        if let (Ok(hn), Ok(hv)) = (
            header::HeaderName::from_bytes(k.as_bytes()),
            header::HeaderValue::from_str(v),
        ) {
            h.insert(hn, hv);
        }
    }
    resp
}

fn rpc_error(ctx: &ReqCtx<'_>, status: StatusCode, code: &str, message: &str) -> Response<BoxBody> {
    let payload = json!({
        "__type": code,
        "message": message,
        "Message": message,
    });
    if ctx.content.cbor {
        let bytes = lenslet_core::cbor::encode_cbor(&payload);
        let mut resp = Response::new(
            Full::new(Bytes::from(bytes))
                .map_err(|never| match never {})
                .boxed(),
        );
        *resp.status_mut() = status;
        let h = resp.headers_mut();
        h.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/cbor"),
        );
        h.insert(
            "smithy-protocol",
            header::HeaderValue::from_static("rpc-v2-cbor"),
        );
        if let Ok(v) = header::HeaderValue::from_str(code) {
            h.insert("x-amzn-errortype", v);
        }
        if let Ok(v) = header::HeaderValue::from_str(&ctx.req_id) {
            h.insert("x-amzn-requestid", v);
        }
        return resp;
    }
    let ct = match ctx.content.content_type.as_deref() {
        Some("application/x-amz-json-1.0") => "application/x-amz-json-1.0",
        Some("application/x-amz-json-1.1") => "application/x-amz-json-1.1",
        _ => "application/json",
    };
    let mut resp = Response::new(
        Full::new(Bytes::from(payload.to_string()))
            .map_err(|never| match never {})
            .boxed(),
    );
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, header::HeaderValue::from_static(ct));
    if let Ok(v) = header::HeaderValue::from_str(code) {
        h.insert("x-amzn-errortype", v);
    }
    if let Ok(v) = header::HeaderValue::from_str(&ctx.req_id) {
        h.insert("x-amzn-requestid", v);
    }
    resp
}

fn sqs_missing_queue(ctx: &ReqCtx<'_>, is_query: bool) -> Response<BoxBody> {
    if is_query {
        let xml = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <ErrorResponse xmlns=\"http://queue.amazonaws.com/doc/2012-11-05/\">\
             <Error><Type>Sender</Type><Code>AWS.SimpleQueueService.NonExistentQueue</Code>\
             <Message>The specified queue does not exist.</Message></Error>\
             <RequestId>{}</RequestId></ErrorResponse>",
            ctx.req_id
        );
        s3_response(
            StatusCode::BAD_REQUEST,
            "text/xml; charset=utf-8",
            Bytes::from(xml),
            &ctx.req_id,
            &[],
        )
    } else {
        rpc_error(
            ctx,
            StatusCode::BAD_REQUEST,
            "com.amazonaws.sqs#QueueDoesNotExist",
            "The specified queue does not exist.",
        )
    }
}

fn query_xml_ok(op: &str, ns: &str, result_inner: &str, req_id: &str) -> Response<BoxBody> {
    let result_block = if result_inner.is_empty() {
        format!("<{op}Result/>")
    } else {
        format!("<{op}Result>{result_inner}</{op}Result>")
    };
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <{op}Response xmlns=\"{ns}\">{result_block}\
         <ResponseMetadata><RequestId>{req_id}</RequestId></ResponseMetadata>\
         </{op}Response>"
    );
    let mut resp = Response::new(
        Full::new(Bytes::from(xml))
            .map_err(|never| match never {})
            .boxed(),
    );
    *resp.status_mut() = StatusCode::OK;
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/xml; charset=utf-8"),
    );
    if let Ok(v) = header::HeaderValue::from_str(req_id) {
        h.insert("x-amzn-requestid", v);
    }
    resp
}

fn s3_response(
    status: StatusCode,
    content_type: &str,
    body: Bytes,
    req_id: &str,
    extra: &[(&str, &str)],
) -> Response<BoxBody> {
    let mut resp = Response::new(Full::new(body).map_err(|never| match never {}).boxed());
    *resp.status_mut() = status;
    let h = resp.headers_mut();
    if let Ok(ct) = header::HeaderValue::from_str(content_type) {
        h.insert(header::CONTENT_TYPE, ct);
    }
    if let Ok(v) = header::HeaderValue::from_str(req_id) {
        h.insert("x-amz-request-id", v);
    }
    for (k, v) in extra {
        if let (Ok(hn), Ok(hv)) = (
            header::HeaderName::from_bytes(k.as_bytes()),
            header::HeaderValue::from_str(v),
        ) {
            h.insert(hn, hv);
        }
    }
    resp
}

fn s3_error(status: StatusCode, code: &str, message: &str, req_id: &str) -> Response<BoxBody> {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <Error><Code>{}</Code><Message>{}</Message><RequestId>{}</RequestId></Error>",
        xml_escape(code),
        xml_escape(message),
        xml_escape(req_id)
    );
    s3_response(status, "application/xml", Bytes::from(xml), req_id, &[])
}

fn parse_s3_target(headers: &HeaderMap, path: &str) -> (Option<String>, Option<String>) {
    let bare = path.split('?').next().unwrap_or(path);
    let host = header_str(headers, "host")
        .map(wire::strip_port)
        .unwrap_or("");
    let vhost_bucket = host
        .strip_suffix(".localhost")
        .or_else(|| host.split(".s3.").next().filter(|_| host.contains(".s3.")))
        .or_else(|| host.split(".s3-").next().filter(|_| host.contains(".s3-")))
        .or_else(|| host.strip_suffix(".s3"))
        .filter(|b| !b.is_empty());

    let trimmed = bare.strip_prefix('/').unwrap_or(bare);
    if let Some(b) = vhost_bucket {
        let key = (!trimmed.is_empty()).then(|| wire::percent_decode_path(trimmed));
        return (Some(b.to_string()), key);
    }
    if trimmed.is_empty() {
        return (None, None);
    }
    match trimmed.split_once('/') {
        Some((b, k)) => (
            (!b.is_empty()).then(|| wire::percent_decode_path(b)),
            (!k.is_empty()).then(|| wire::percent_decode_path(k)),
        ),
        None => (Some(wire::percent_decode_path(trimmed)), None),
    }
}

fn ddb_table_desc(
    name: &str,
    arn: &str,
    key_attrs: &[String],
    attr_defs: &Value,
    created: f64,
    item_count: usize,
) -> Value {
    let ks: Vec<Value> = key_attrs
        .iter()
        .enumerate()
        .map(|(i, a)| json!({ "AttributeName": a, "KeyType": if i == 0 { "HASH" } else { "RANGE" } }))
        .collect();
    json!({
        "TableName": name,
        "TableArn": arn,
        "TableStatus": "ACTIVE",
        "CreationDateTime": created,
        "ItemCount": item_count,
        "TableSizeBytes": item_count * 64,
        "KeySchema": ks,
        "AttributeDefinitions": attr_defs,
    })
}

fn ddb_item_key(item: &Value, key_attrs: &[String]) -> String {
    let mut parts = Vec::with_capacity(key_attrs.len());
    for attr in key_attrs {
        let val = item.get(attr).cloned().unwrap_or(Value::Null);
        parts.push(val);
    }
    Value::Array(parts).to_string()
}

fn apply_ddb_update(item: &mut Value, req: &Value) {
    let Some(obj) = item.as_object_mut() else {
        return;
    };
    if let Some(updates) = req.get("AttributeUpdates").and_then(|v| v.as_object()) {
        for (k, spec) in updates {
            let action = str_field(spec, "Action").unwrap_or_else(|| "PUT".into());
            if action == "DELETE" && spec.get("Value").is_none() {
                obj.remove(k);
            } else if let Some(val) = spec.get("Value") {
                obj.insert(k.clone(), val.clone());
            }
        }
    }
    // Support common `UpdateExpression: "SET #s = :v, total = :t"` patterns.
    if let Some(expr) = str_field(req, "UpdateExpression") {
        let names = req
            .get("ExpressionAttributeNames")
            .and_then(|v| v.as_object());
        let values = req
            .get("ExpressionAttributeValues")
            .and_then(|v| v.as_object());
        if let Some(set_clause) = expr
            .trim()
            .strip_prefix("SET ")
            .or_else(|| expr.trim().strip_prefix("set "))
        {
            for assign in set_clause.split(',') {
                if let Some((lhs, rhs)) = assign.split_once('=') {
                    let raw_attr = lhs.trim();
                    let attr = names
                        .and_then(|m| m.get(raw_attr))
                        .and_then(|v| v.as_str())
                        .unwrap_or(raw_attr);
                    let rhs_trim = rhs.trim();
                    if let Some(val) = values.and_then(|m| m.get(rhs_trim)) {
                        obj.insert(attr.to_string(), val.clone());
                    }
                }
            }
        }
    }
}

fn ssm_param_json(p: &SsmParam) -> Value {
    json!({
        "Name": p.name,
        "Type": p.r#type,
        "Value": p.value,
        "Version": p.version,
        "ARN": p.arn,
        "DataType": "text",
        "LastModifiedDate": p.last_modified
    })
}

fn str_field(val: &Value, key: &str) -> Option<String> {
    val.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn extract_xml_tags(xml: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(s) = rest.find(&open) {
        let after = &rest[s + open.len()..];
        let Some(e) = after.find(&close) else {
            break;
        };
        out.push(after[..e].to_string());
        rest = &after[e + close.len()..];
    }
    out
}

fn extract_s3_meta(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(k, v)| {
            let name = k.as_str().to_ascii_lowercase();
            name.starts_with("x-amz-meta-")
                .then(|| Some((name, v.to_str().ok()?.to_string())))
                .flatten()
        })
        .collect()
}

type CompletePartSpec = (u32, Option<String>);

fn parse_complete_multipart_parts(
    xml: &str,
) -> Result<Vec<CompletePartSpec>, (&'static str, &'static str)> {
    let blocks = extract_xml_tags(xml, "Part");
    if blocks.is_empty() {
        if xml.trim().is_empty() {
            return Ok(Vec::new());
        }
        let nums = extract_xml_tags(xml, "PartNumber");
        if nums.is_empty() {
            return Err((
                "MalformedXML",
                "CompleteMultipartUpload body did not contain valid <Part> elements.",
            ));
        }
    }
    let mut out = Vec::with_capacity(blocks.len());
    for block in blocks {
        let Some(raw_pn) = extract_xml_tags(&block, "PartNumber").into_iter().next() else {
            return Err(("MalformedXML", "Missing <PartNumber> in <Part>."));
        };
        let Ok(pn) = raw_pn.trim().parse::<u32>() else {
            return Err(("InvalidArgument", "Invalid <PartNumber> value."));
        };
        if !(1..=10_000).contains(&pn) {
            return Err(("InvalidArgument", "PartNumber must be between 1 and 10000."));
        }
        let etag = extract_xml_tags(&block, "ETag")
            .into_iter()
            .next()
            .map(|s| normalize_etag(&s))
            .filter(|s| !s.is_empty());
        out.push((pn, etag));
    }
    Ok(out)
}

fn normalize_etag(raw: &str) -> String {
    raw.replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .trim()
        .trim_matches('"')
        .trim()
        .to_ascii_lowercase()
}

fn etags_match(expected: &str, actual: &str) -> bool {
    normalize_etag(expected) == normalize_etag(actual)
}

// =============================================================================
// DynamoDB Query & Scan Predicate Evaluator
// =============================================================================

fn execute_ddb_query_or_scan(
    table: &DdbTable,
    req: &Value,
    is_query: bool,
) -> Result<Value, String> {
    let active_keys: &[String] = match str_field(req, "IndexName") {
        Some(idx_name) => table
            .indexes
            .get(&idx_name)
            .map(Vec::as_slice)
            .ok_or_else(|| format!("The table does not have the specified index: {idx_name}"))?,
        None => table.key_attrs.as_slice(),
    };

    let names = req
        .get("ExpressionAttributeNames")
        .and_then(|v| v.as_object());
    let values = req
        .get("ExpressionAttributeValues")
        .and_then(|v| v.as_object());

    let key_expr = str_field(req, "KeyConditionExpression")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let key_conds = req.get("KeyConditions").and_then(|v| v.as_object());
    if is_query && key_expr.is_none() && key_conds.is_none_or(|m| m.is_empty()) {
        return Err(
            "Either the KeyConditions or KeyConditionExpression parameter must be specified in the request."
                .to_string(),
        );
    }

    let parsed_key_expr = match key_expr {
        Some(ref src) => Some(parse_ddb_expr(src)?),
        None => None,
    };
    let filter_expr = match str_field(req, "FilterExpression")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        Some(src) => Some(parse_ddb_expr(&src)?),
        None => None,
    };
    let legacy_filter = req
        .get(if is_query {
            "QueryFilter"
        } else {
            "ScanFilter"
        })
        .and_then(|v| v.as_object());
    let legacy_or =
        str_field(req, "ConditionalOperator").is_some_and(|op| op.eq_ignore_ascii_case("OR"));

    let mut candidates: Vec<&Value> = Vec::with_capacity(table.items.len());
    for item in table.items.values() {
        // Sparse index semantics: items missing an index key attribute are not projected.
        if active_keys.iter().any(|k| item.get(k).is_none()) {
            continue;
        }
        if is_query {
            if let Some(ref ast) = parsed_key_expr
                && !eval_ddb_expr(ast, item, names, values)?
            {
                continue;
            }
            if let Some(kc) = key_conds
                && !eval_ddb_condition_map(kc, false, item)?
            {
                continue;
            }
        }
        candidates.push(item);
    }

    // Sort deterministically by active HASH + RANGE key attributes (numeric `N` sorted numerically).
    candidates.sort_by(|a, b| compare_ddb_items_by_keys(a, b, active_keys));
    if is_query && req.get("ScanIndexForward").and_then(|v| v.as_bool()) == Some(false) {
        candidates.reverse();
    }

    // Resume after `ExclusiveStartKey` if pagination cursor was provided.
    if let Some(start_key) = req.get("ExclusiveStartKey").filter(|v| v.is_object()) {
        let target = ddb_item_key(start_key, &table.key_attrs);
        if let Some(pos) = candidates
            .iter()
            .position(|it| ddb_item_key(it, &table.key_attrs) == target)
        {
            candidates.drain(..=pos);
        }
    }

    let limit = req
        .get("Limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);
    let has_more = limit.is_some_and(|lim| candidates.len() > lim);
    let examined_slice = match limit {
        Some(lim) => &candidates[..candidates.len().min(lim)],
        None => &candidates[..],
    };
    let scanned_count = examined_slice.len();
    let last_evaluated_key = if has_more {
        examined_slice
            .last()
            .map(|last| extract_ddb_key_object(last, &table.key_attrs, active_keys))
    } else {
        None
    };

    let mut matched: Vec<Value> = Vec::with_capacity(scanned_count);
    for item in examined_slice {
        if let Some(ref ast) = filter_expr
            && !eval_ddb_expr(ast, item, names, values)?
        {
            continue;
        }
        if let Some(lf) = legacy_filter
            && !eval_ddb_condition_map(lf, legacy_or, item)?
        {
            continue;
        }
        matched.push((*item).clone());
    }

    let count = matched.len();
    let count_only = str_field(req, "Select").is_some_and(|s| s.eq_ignore_ascii_case("COUNT"));
    let mut out = if count_only {
        json!({
            "Count": count,
            "ScannedCount": scanned_count,
        })
    } else {
        json!({
            "Items": matched,
            "Count": count,
            "ScannedCount": scanned_count,
        })
    };
    if let Some(lek) = last_evaluated_key
        && let Some(obj) = out.as_object_mut()
    {
        obj.insert("LastEvaluatedKey".to_string(), lek);
    }
    Ok(out)
}

fn extract_ddb_key_object(item: &Value, table_keys: &[String], index_keys: &[String]) -> Value {
    let mut map = Map::new();
    for k in table_keys.iter().chain(index_keys.iter()) {
        if let Some(v) = item.get(k) {
            map.insert(k.clone(), v.clone());
        }
    }
    Value::Object(map)
}

fn compare_ddb_items_by_keys(a: &Value, b: &Value, keys: &[String]) -> std::cmp::Ordering {
    for k in keys {
        let ord = match (a.get(k), b.get(k)) {
            (Some(va), Some(vb)) => ddb_compare_attr(va, vb).unwrap_or(std::cmp::Ordering::Equal),
            (None, Some(_)) => std::cmp::Ordering::Less,
            (Some(_), None) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    std::cmp::Ordering::Equal
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum DdbToken {
    LParen,
    RParen,
    Comma,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Ident(String),
}

#[derive(Clone, Debug)]
enum DdbOperand {
    Path(String),
    ValueRef(String),
    Size(String),
}

#[derive(Clone, Debug, Copy)]
enum DdbCmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug)]
enum DdbExpr {
    Or(Box<DdbExpr>, Box<DdbExpr>),
    And(Box<DdbExpr>, Box<DdbExpr>),
    Not(Box<DdbExpr>),
    Compare(DdbOperand, DdbCmpOp, DdbOperand),
    Between(DdbOperand, DdbOperand, DdbOperand),
    In(DdbOperand, Vec<DdbOperand>),
    AttrExists(String),
    AttrNotExists(String),
    BeginsWith(DdbOperand, DdbOperand),
    Contains(DdbOperand, DdbOperand),
    AttrType(DdbOperand, DdbOperand),
}

fn tokenize_ddb_expr(src: &str) -> Result<Vec<DdbToken>, String> {
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match b {
            b'(' => {
                out.push(DdbToken::LParen);
                i += 1;
            }
            b')' => {
                out.push(DdbToken::RParen);
                i += 1;
            }
            b',' => {
                out.push(DdbToken::Comma);
                i += 1;
            }
            b'=' => {
                out.push(DdbToken::Eq);
                i += 1;
            }
            b'!' if bytes.get(i + 1) == Some(&b'=') => {
                out.push(DdbToken::Ne);
                i += 2;
            }
            b'<' if bytes.get(i + 1) == Some(&b'>') => {
                out.push(DdbToken::Ne);
                i += 2;
            }
            b'<' if bytes.get(i + 1) == Some(&b'=') => {
                out.push(DdbToken::Le);
                i += 2;
            }
            b'<' => {
                out.push(DdbToken::Lt);
                i += 1;
            }
            b'>' if bytes.get(i + 1) == Some(&b'=') => {
                out.push(DdbToken::Ge);
                i += 2;
            }
            b'>' => {
                out.push(DdbToken::Gt);
                i += 1;
            }
            c if is_ddb_ident_char(c) => {
                let start = i;
                i += 1;
                while i < bytes.len() && is_ddb_ident_char(bytes[i]) {
                    i += 1;
                }
                out.push(DdbToken::Ident(src[start..i].to_string()));
            }
            _ => {
                return Err(format!(
                    "Invalid condition expression syntax near `{}`",
                    &src[i..]
                ));
            }
        }
    }
    Ok(out)
}

fn is_ddb_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'#' | b':' | b'.' | b'[' | b']')
}

struct DdbParser {
    tokens: Vec<DdbToken>,
    pos: usize,
}

impl DdbParser {
    fn peek(&self) -> Option<&DdbToken> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<DdbToken> {
        let tok = self.tokens.get(self.pos).cloned()?;
        self.pos += 1;
        Some(tok)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        if matches!(self.peek(), Some(DdbToken::Ident(s)) if s.eq_ignore_ascii_case(kw)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, expected: &DdbToken) -> Result<(), String> {
        match self.next() {
            Some(ref tok) if tok == expected => Ok(()),
            other => Err(format!(
                "Invalid condition expression: expected {expected:?}, got {other:?}"
            )),
        }
    }

    fn parse_or(&mut self) -> Result<DdbExpr, String> {
        let mut lhs = self.parse_and()?;
        while self.eat_kw("OR") {
            let rhs = self.parse_and()?;
            lhs = DdbExpr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<DdbExpr, String> {
        let mut lhs = self.parse_not()?;
        while self.eat_kw("AND") {
            let rhs = self.parse_not()?;
            lhs = DdbExpr::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> Result<DdbExpr, String> {
        if self.eat_kw("NOT") {
            return Ok(DdbExpr::Not(Box::new(self.parse_not()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<DdbExpr, String> {
        if self.peek() == Some(&DdbToken::LParen) {
            self.pos += 1;
            let inner = self.parse_or()?;
            self.expect(&DdbToken::RParen)?;
            return Ok(inner);
        }
        let Some(DdbToken::Ident(head)) = self.next() else {
            return Err("Invalid condition expression: expected identifier or `(`".to_string());
        };
        if self.peek() == Some(&DdbToken::LParen) {
            self.pos += 1;
            match head.as_str() {
                "attribute_exists" => {
                    let path = self.expect_ident()?;
                    self.expect(&DdbToken::RParen)?;
                    return Ok(DdbExpr::AttrExists(path));
                }
                "attribute_not_exists" => {
                    let path = self.expect_ident()?;
                    self.expect(&DdbToken::RParen)?;
                    return Ok(DdbExpr::AttrNotExists(path));
                }
                "begins_with" => {
                    let lhs = self.parse_operand()?;
                    self.expect(&DdbToken::Comma)?;
                    let rhs = self.parse_operand()?;
                    self.expect(&DdbToken::RParen)?;
                    return Ok(DdbExpr::BeginsWith(lhs, rhs));
                }
                "contains" => {
                    let lhs = self.parse_operand()?;
                    self.expect(&DdbToken::Comma)?;
                    let rhs = self.parse_operand()?;
                    self.expect(&DdbToken::RParen)?;
                    return Ok(DdbExpr::Contains(lhs, rhs));
                }
                "attribute_type" => {
                    let lhs = self.parse_operand()?;
                    self.expect(&DdbToken::Comma)?;
                    let rhs = self.parse_operand()?;
                    self.expect(&DdbToken::RParen)?;
                    return Ok(DdbExpr::AttrType(lhs, rhs));
                }
                "size" => {
                    let path = self.expect_ident()?;
                    self.expect(&DdbToken::RParen)?;
                    return self.parse_predicate_tail(DdbOperand::Size(path));
                }
                other => {
                    return Err(format!(
                        "Invalid condition expression: unsupported function `{other}`"
                    ));
                }
            }
        }
        let lhs = classify_ddb_ident(head);
        self.parse_predicate_tail(lhs)
    }

    fn parse_predicate_tail(&mut self, lhs: DdbOperand) -> Result<DdbExpr, String> {
        if self.eat_kw("BETWEEN") {
            let lo = self.parse_operand()?;
            if !self.eat_kw("AND") {
                return Err("Invalid BETWEEN expression: missing `AND`".to_string());
            }
            let hi = self.parse_operand()?;
            return Ok(DdbExpr::Between(lhs, lo, hi));
        }
        if self.eat_kw("IN") {
            self.expect(&DdbToken::LParen)?;
            let mut list = vec![self.parse_operand()?];
            while self.peek() == Some(&DdbToken::Comma) {
                self.pos += 1;
                list.push(self.parse_operand()?);
            }
            self.expect(&DdbToken::RParen)?;
            return Ok(DdbExpr::In(lhs, list));
        }
        let op = match self.next() {
            Some(DdbToken::Eq) => DdbCmpOp::Eq,
            Some(DdbToken::Ne) => DdbCmpOp::Ne,
            Some(DdbToken::Lt) => DdbCmpOp::Lt,
            Some(DdbToken::Le) => DdbCmpOp::Le,
            Some(DdbToken::Gt) => DdbCmpOp::Gt,
            Some(DdbToken::Ge) => DdbCmpOp::Ge,
            other => {
                return Err(format!(
                    "Invalid condition expression: expected comparison operator, got {other:?}"
                ));
            }
        };
        let rhs = self.parse_operand()?;
        Ok(DdbExpr::Compare(lhs, op, rhs))
    }

    fn parse_operand(&mut self) -> Result<DdbOperand, String> {
        let ident = self.expect_ident()?;
        if ident == "size" && self.peek() == Some(&DdbToken::LParen) {
            self.pos += 1;
            let path = self.expect_ident()?;
            self.expect(&DdbToken::RParen)?;
            return Ok(DdbOperand::Size(path));
        }
        Ok(classify_ddb_ident(ident))
    }

    fn expect_ident(&mut self) -> Result<String, String> {
        match self.next() {
            Some(DdbToken::Ident(s)) => Ok(s),
            other => Err(format!(
                "Invalid condition expression: expected identifier, got {other:?}"
            )),
        }
    }
}

fn classify_ddb_ident(ident: String) -> DdbOperand {
    if ident.starts_with(':') {
        DdbOperand::ValueRef(ident)
    } else {
        DdbOperand::Path(ident)
    }
}

fn parse_ddb_expr(src: &str) -> Result<DdbExpr, String> {
    let tokens = tokenize_ddb_expr(src)?;
    if tokens.is_empty() {
        return Err("Condition expression must not be empty.".to_string());
    }
    let mut parser = DdbParser { tokens, pos: 0 };
    let ast = parser.parse_or()?;
    if parser.pos != parser.tokens.len() {
        return Err(format!(
            "Invalid condition expression: unexpected trailing token `{:?}`",
            parser.tokens[parser.pos]
        ));
    }
    Ok(ast)
}

fn resolve_ddb_path<'a>(
    item: &'a Value,
    raw_path: &str,
    names: Option<&Map<String, Value>>,
) -> Result<Option<&'a Value>, String> {
    let mut cur = item;
    let mut first = true;
    for seg in raw_path.split('.') {
        if seg.is_empty() {
            return Err(format!("Invalid document path `{raw_path}`"));
        }
        let (field_part, rest_indices) = match seg.find('[') {
            Some(idx) => (&seg[..idx], &seg[idx..]),
            None => (seg, ""),
        };
        let attr_name = if field_part.starts_with('#') {
            names
                .and_then(|m| m.get(field_part))
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    format!(
                        "An expression attribute name used in the document path is not defined; attribute name: {field_part}"
                    )
                })?
        } else if let Some(mapped) = names
            .and_then(|m| m.get(field_part))
            .and_then(|v| v.as_str())
        {
            mapped
        } else {
            field_part
        };

        let container = if first {
            first = false;
            cur.as_object()
        } else {
            cur.get("M").and_then(|m| m.as_object())
        };
        let Some(next) = container.and_then(|m| m.get(attr_name)) else {
            return Ok(None);
        };
        cur = next;

        let mut idx_slice = rest_indices;
        while !idx_slice.is_empty() {
            let Some(after_open) = idx_slice.strip_prefix('[') else {
                return Err(format!("Invalid list index in `{raw_path}`"));
            };
            let Some((num_str, rem)) = after_open.split_once(']') else {
                return Err(format!("Unclosed list index in `{raw_path}`"));
            };
            let index: usize = num_str
                .parse()
                .map_err(|_| format!("Invalid list index `{num_str}` in `{raw_path}`"))?;
            let Some(elem) = cur
                .get("L")
                .and_then(|l| l.as_array())
                .and_then(|arr| arr.get(index))
            else {
                return Ok(None);
            };
            cur = elem;
            idx_slice = rem;
        }
    }
    Ok(Some(cur))
}

fn resolve_ddb_operand(
    op: &DdbOperand,
    item: &Value,
    names: Option<&Map<String, Value>>,
    values: Option<&Map<String, Value>>,
) -> Result<Option<Value>, String> {
    match op {
        DdbOperand::ValueRef(placeholder) => {
            let v = values.and_then(|m| m.get(placeholder)).ok_or_else(|| {
                format!(
                    "Value provided in ExpressionAttributeValues unused or missing: {placeholder}"
                )
            })?;
            Ok(Some(v.clone()))
        }
        DdbOperand::Path(p) => Ok(resolve_ddb_path(item, p, names)?.cloned()),
        DdbOperand::Size(p) => {
            let Some(val) = resolve_ddb_path(item, p, names)? else {
                return Ok(None);
            };
            let sz = ddb_attr_size(val)?;
            Ok(Some(json!({ "N": sz.to_string() })))
        }
    }
}

fn ddb_attr_size(val: &Value) -> Result<usize, String> {
    if let Some(s) = val.get("S").and_then(|v| v.as_str()) {
        return Ok(s.len());
    }
    if let Some(b64) = val.get("B").and_then(|v| v.as_str()) {
        return Ok(decode_b64_len(b64));
    }
    for set_key in ["SS", "NS", "BS", "L"] {
        if let Some(arr) = val.get(set_key).and_then(|v| v.as_array()) {
            return Ok(arr.len());
        }
    }
    if let Some(m) = val.get("M").and_then(|v| v.as_object()) {
        return Ok(m.len());
    }
    Err("The function `size` is not supported for this attribute type.".to_string())
}

fn eval_ddb_expr(
    expr: &DdbExpr,
    item: &Value,
    names: Option<&Map<String, Value>>,
    values: Option<&Map<String, Value>>,
) -> Result<bool, String> {
    match expr {
        DdbExpr::Or(a, b) => {
            Ok(eval_ddb_expr(a, item, names, values)? || eval_ddb_expr(b, item, names, values)?)
        }
        DdbExpr::And(a, b) => {
            Ok(eval_ddb_expr(a, item, names, values)? && eval_ddb_expr(b, item, names, values)?)
        }
        DdbExpr::Not(inner) => Ok(!eval_ddb_expr(inner, item, names, values)?),
        DdbExpr::AttrExists(p) => Ok(resolve_ddb_path(item, p, names)?.is_some()),
        DdbExpr::AttrNotExists(p) => Ok(resolve_ddb_path(item, p, names)?.is_none()),
        DdbExpr::Compare(lhs, op, rhs) => {
            let lval = resolve_ddb_operand(lhs, item, names, values)?;
            let rval = resolve_ddb_operand(rhs, item, names, values)?;
            let (Some(lv), Some(rv)) = (lval, rval) else {
                return Ok(false);
            };
            Ok(ddb_eval_cmp(&lv, *op, &rv))
        }
        DdbExpr::Between(val_op, lo_op, hi_op) => {
            let v = resolve_ddb_operand(val_op, item, names, values)?;
            let lo = resolve_ddb_operand(lo_op, item, names, values)?;
            let hi = resolve_ddb_operand(hi_op, item, names, values)?;
            let (Some(v), Some(lo), Some(hi)) = (v, lo, hi) else {
                return Ok(false);
            };
            Ok(
                ddb_compare_attr(&v, &lo).is_some_and(|o| o != std::cmp::Ordering::Less)
                    && ddb_compare_attr(&v, &hi).is_some_and(|o| o != std::cmp::Ordering::Greater),
            )
        }
        DdbExpr::In(lhs, list) => {
            let mut resolved_list = Vec::with_capacity(list.len());
            for el in list {
                resolved_list.push(resolve_ddb_operand(el, item, names, values)?);
            }
            let Some(lv) = resolve_ddb_operand(lhs, item, names, values)? else {
                return Ok(false);
            };
            Ok(resolved_list
                .into_iter()
                .flatten()
                .any(|rv| ddb_eval_cmp(&lv, DdbCmpOp::Eq, &rv)))
        }
        DdbExpr::BeginsWith(lhs, rhs) => {
            let lval = resolve_ddb_operand(lhs, item, names, values)?;
            let rval = resolve_ddb_operand(rhs, item, names, values)?;
            let (Some(lv), Some(rv)) = (lval, rval) else {
                return Ok(false);
            };
            Ok(ddb_begins_with(&lv, &rv))
        }
        DdbExpr::Contains(lhs, rhs) => {
            let lval = resolve_ddb_operand(lhs, item, names, values)?;
            let rval = resolve_ddb_operand(rhs, item, names, values)?;
            let (Some(lv), Some(rv)) = (lval, rval) else {
                return Ok(false);
            };
            Ok(ddb_contains(&lv, &rv))
        }
        DdbExpr::AttrType(lhs, rhs) => {
            let lval = resolve_ddb_operand(lhs, item, names, values)?;
            let rval = resolve_ddb_operand(rhs, item, names, values)?;
            let (Some(lv), Some(rv)) = (lval, rval) else {
                return Ok(false);
            };
            let Some(expected_type) = rv.get("S").and_then(|v| v.as_str()) else {
                return Err(
                    "attribute_type second argument must be a string type descriptor.".into(),
                );
            };
            Ok(lv
                .as_object()
                .is_some_and(|m| m.contains_key(expected_type)))
        }
    }
}

fn eval_ddb_condition_map(
    conds: &Map<String, Value>,
    combine_or: bool,
    item: &Value,
) -> Result<bool, String> {
    if conds.is_empty() {
        return Ok(true);
    }
    let mut any_match = false;
    for (attr, spec) in conds {
        let op = str_field(spec, "ComparisonOperator").unwrap_or_else(|| "EQ".to_string());
        let vals = spec
            .get("AttributeValueList")
            .and_then(|v| v.as_array())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let actual = item.get(attr);
        let matched = match op.as_str() {
            "NULL" => actual.is_none(),
            "NOT_NULL" => actual.is_some(),
            "EQ" => actual
                .zip(vals.first())
                .is_some_and(|(a, b)| ddb_eval_cmp(a, DdbCmpOp::Eq, b)),
            "NE" => match (actual, vals.first()) {
                (Some(a), Some(b)) => ddb_eval_cmp(a, DdbCmpOp::Ne, b),
                (None, Some(_)) => true,
                _ => false,
            },
            "LT" => actual
                .zip(vals.first())
                .is_some_and(|(a, b)| ddb_eval_cmp(a, DdbCmpOp::Lt, b)),
            "LE" => actual
                .zip(vals.first())
                .is_some_and(|(a, b)| ddb_eval_cmp(a, DdbCmpOp::Le, b)),
            "GT" => actual
                .zip(vals.first())
                .is_some_and(|(a, b)| ddb_eval_cmp(a, DdbCmpOp::Gt, b)),
            "GE" => actual
                .zip(vals.first())
                .is_some_and(|(a, b)| ddb_eval_cmp(a, DdbCmpOp::Ge, b)),
            "BEGINS_WITH" => actual
                .zip(vals.first())
                .is_some_and(|(a, b)| ddb_begins_with(a, b)),
            "CONTAINS" => actual
                .zip(vals.first())
                .is_some_and(|(a, b)| ddb_contains(a, b)),
            "NOT_CONTAINS" => actual
                .zip(vals.first())
                .is_some_and(|(a, b)| !ddb_contains(a, b)),
            "BETWEEN" if vals.len() >= 2 => actual.is_some_and(|a| {
                ddb_compare_attr(a, &vals[0]).is_some_and(|o| o != std::cmp::Ordering::Less)
                    && ddb_compare_attr(a, &vals[1])
                        .is_some_and(|o| o != std::cmp::Ordering::Greater)
            }),
            "IN" => actual.is_some_and(|a| vals.iter().any(|v| ddb_eval_cmp(a, DdbCmpOp::Eq, v))),
            other => {
                return Err(format!("Unsupported DynamoDB ComparisonOperator `{other}`"));
            }
        };
        if combine_or {
            if matched {
                any_match = true;
            }
        } else if !matched {
            return Ok(false);
        }
    }
    Ok(if combine_or { any_match } else { true })
}

fn ddb_eval_cmp(lhs: &Value, op: DdbCmpOp, rhs: &Value) -> bool {
    match op {
        DdbCmpOp::Eq => ddb_attr_eq(lhs, rhs),
        DdbCmpOp::Ne => !ddb_attr_eq(lhs, rhs),
        DdbCmpOp::Lt => ddb_compare_attr(lhs, rhs) == Some(std::cmp::Ordering::Less),
        DdbCmpOp::Le => {
            matches!(
                ddb_compare_attr(lhs, rhs),
                Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
            )
        }
        DdbCmpOp::Gt => ddb_compare_attr(lhs, rhs) == Some(std::cmp::Ordering::Greater),
        DdbCmpOp::Ge => {
            matches!(
                ddb_compare_attr(lhs, rhs),
                Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
            )
        }
    }
}

fn ddb_attr_eq(a: &Value, b: &Value) -> bool {
    if let (Some(na), Some(nb)) = (
        a.get("N")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok()),
        b.get("N")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok()),
    ) {
        return na == nb;
    }
    a == b
}

fn ddb_compare_attr(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    if let (Some(sa), Some(sb)) = (
        a.get("S").and_then(|v| v.as_str()),
        b.get("S").and_then(|v| v.as_str()),
    ) {
        return Some(sa.cmp(sb));
    }
    if let (Some(na), Some(nb)) = (
        a.get("N")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok()),
        b.get("N")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok()),
    ) {
        return na.partial_cmp(&nb);
    }
    if let (Some(ba), Some(bb)) = (
        a.get("B").and_then(|v| v.as_str()),
        b.get("B").and_then(|v| v.as_str()),
    ) {
        return Some(decode_b64_bytes(ba).cmp(&decode_b64_bytes(bb)));
    }
    None
}

fn ddb_begins_with(a: &Value, b: &Value) -> bool {
    if let (Some(sa), Some(sb)) = (
        a.get("S").and_then(|v| v.as_str()),
        b.get("S").and_then(|v| v.as_str()),
    ) {
        return sa.starts_with(sb);
    }
    if let (Some(ba), Some(bb)) = (
        a.get("B").and_then(|v| v.as_str()),
        b.get("B").and_then(|v| v.as_str()),
    ) {
        return decode_b64_bytes(ba).starts_with(&decode_b64_bytes(bb));
    }
    false
}

fn ddb_contains(container: &Value, target: &Value) -> bool {
    if let (Some(sa), Some(sb)) = (
        container.get("S").and_then(|v| v.as_str()),
        target.get("S").and_then(|v| v.as_str()),
    ) {
        return sa.contains(sb);
    }
    if let (Some(ss), Some(sb)) = (
        container.get("SS").and_then(|v| v.as_array()),
        target.get("S").and_then(|v| v.as_str()),
    ) {
        return ss.iter().any(|el| el.as_str() == Some(sb));
    }
    if let (Some(ns), Some(nb)) = (
        container.get("NS").and_then(|v| v.as_array()),
        target
            .get("N")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok()),
    ) {
        return ns
            .iter()
            .filter_map(|el| el.as_str()?.parse::<f64>().ok())
            .any(|n| n == nb);
    }
    if let (Some(bs), Some(bb)) = (
        container.get("BS").and_then(|v| v.as_array()),
        target.get("B").and_then(|v| v.as_str()),
    ) {
        let target_bytes = decode_b64_bytes(bb);
        return bs
            .iter()
            .filter_map(|el| el.as_str())
            .any(|el| decode_b64_bytes(el) == target_bytes);
    }
    if let Some(list) = container.get("L").and_then(|v| v.as_array()) {
        return list.iter().any(|el| ddb_attr_eq(el, target));
    }
    false
}

fn decode_b64_bytes(input: &str) -> Vec<u8> {
    fn sextet(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let clean: Vec<u8> = input
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        .collect();
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let Some(s0) = chunk.first().copied().and_then(sextet) else {
            return input.as_bytes().to_vec();
        };
        let Some(s1) = chunk.get(1).copied().and_then(sextet) else {
            return input.as_bytes().to_vec();
        };
        out.push((s0 << 2) | (s1 >> 4));
        if let Some(b2) = chunk.get(2).copied() {
            let Some(s2) = sextet(b2) else {
                return input.as_bytes().to_vec();
            };
            out.push((s1 << 4) | (s2 >> 2));
            if let Some(b3) = chunk.get(3).copied() {
                let Some(s3) = sextet(b3) else {
                    return input.as_bytes().to_vec();
                };
                out.push((s2 << 6) | s3);
            }
        }
    }
    out
}

fn decode_b64_len(input: &str) -> usize {
    decode_b64_bytes(input).len()
}

/// Pure safe Rust RFC 1321 MD5 digest (lowercase hex) so S3 `ETag` and SQS
/// `MD5OfMessageBody` pass strict AWS SDK client-side checksum validation.
pub(crate) fn md5_hex(data: &[u8]) -> String {
    capture::hex(&md5_bytes(data))
}

fn md5_bytes(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            let o = i * 4;
            *word = u32::from_le_bytes([chunk[o], chunk[o + 1], chunk[o + 2], chunk[o + 3]]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    digest[0..4].copy_from_slice(&a0.to_le_bytes());
    digest[4..8].copy_from_slice(&b0.to_le_bytes());
    digest[8..12].copy_from_slice(&c0.to_le_bytes());
    digest[12..16].copy_from_slice(&d0.to_le_bytes());
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_rfc1321_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[tokio::test]
    async fn sandbox_s3_dynamodb_sqs_sts_and_cbor_roundtrip() {
        let sb = Sandbox::new("http://127.0.0.1:9297");
        let call = |method: Method,
                    path: &str,
                    svc: &str,
                    target: Option<&str>,
                    ct: &str,
                    extra: &[(&str, &str)],
                    body: &[u8]| {
            let mut h = HeaderMap::new();
            h.insert("content-type", ct.parse().unwrap());
            if let Some(t) = target {
                h.insert("x-amz-target", t.parse().unwrap());
            }
            for (k, v) in extra {
                h.insert(
                    header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    v.parse().unwrap(),
                );
            }
            let c = capture::content(&h);
            let fb = wire::fallback_operation(svc, method.as_str(), path, &h);
            let adv = target.and_then(|t| t.rsplit('.').next());
            sb.handle(
                &method,
                path,
                &h,
                svc,
                Some("us-east-1"),
                adv,
                &fb,
                &c,
                body,
            )
        };

        // 1. S3 CreateBucket + PutObject + GetObject + HeadObject
        let r = call(
            Method::PUT,
            "/shop-receipts",
            "s3",
            None,
            "application/xml",
            &[],
            b"",
        );
        assert_eq!(r.status(), StatusCode::OK);
        let r = call(
            Method::PUT,
            "/shop-receipts/orders/1.json",
            "s3",
            None,
            "application/json",
            &[("x-amz-meta-env", "local")],
            br#"{"total":42.5}"#,
        );
        assert_eq!(r.status(), StatusCode::OK);
        let r = call(
            Method::GET,
            "/shop-receipts/orders/1.json",
            "s3",
            None,
            "application/octet-stream",
            &[],
            b"",
        );
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(r.headers().get("x-amz-meta-env").unwrap(), "local");
        let got = r.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&got[..], br#"{"total":42.5}"#);

        // 2. DynamoDB missing table -> ResourceNotFoundException, then CreateTable + PutItem + GetItem
        let miss = call(
            Method::POST,
            "/",
            "dynamodb",
            Some("DynamoDB_20120810.GetItem"),
            "application/x-amz-json-1.0",
            &[],
            br#"{"TableName":"orders","Key":{"id":{"S":"1"}}}"#,
        );
        assert_eq!(miss.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            miss.headers().get("x-amzn-errortype").unwrap(),
            "ResourceNotFoundException"
        );

        let ct = call(
            Method::POST,
            "/",
            "dynamodb",
            Some("DynamoDB_20120810.CreateTable"),
            "application/x-amz-json-1.0",
            &[],
            br#"{"TableName":"orders","KeySchema":[{"AttributeName":"id","KeyType":"HASH"}]}"#,
        );
        assert_eq!(ct.status(), StatusCode::OK);
        let put = call(
            Method::POST,
            "/",
            "dynamodb",
            Some("DynamoDB_20120810.PutItem"),
            "application/x-amz-json-1.0",
            &[],
            br#"{"TableName":"orders","Item":{"id":{"S":"ord-1"},"status":{"S":"paid"}}}"#,
        );
        assert_eq!(put.status(), StatusCode::OK);

        // Read back via Smithy RPCv2 CBOR!
        let cbor_req = lenslet_core::cbor::encode_cbor(&json!({
            "TableName": "orders",
            "Key": { "id": { "S": "ord-1" } }
        }));
        let cbor_resp = call(
            Method::POST,
            "/service/DynamoDB_20120810/operation/GetItem",
            "dynamodb",
            Some("DynamoDB_20120810.GetItem"),
            "application/cbor",
            &[("smithy-protocol", "rpc-v2-cbor")],
            &cbor_req,
        );
        assert_eq!(cbor_resp.status(), StatusCode::OK);
        let cbor_bytes = cbor_resp.into_body().collect().await.unwrap().to_bytes();
        let decoded = lenslet_core::cbor::decode_cbor(&cbor_bytes).unwrap();
        assert_eq!(decoded["Item"]["status"]["S"], "paid");

        // 3. SQS CreateQueue + SendMessage + ReceiveMessage
        let q = call(
            Method::POST,
            "/",
            "sqs",
            Some("AmazonSQS.CreateQueue"),
            "application/x-amz-json-1.0",
            &[],
            br#"{"QueueName":"jobs"}"#,
        );
        assert_eq!(q.status(), StatusCode::OK);
        let send = call(
            Method::POST,
            "/",
            "sqs",
            Some("AmazonSQS.SendMessage"),
            "application/x-amz-json-1.0",
            &[],
            br#"{"QueueUrl":"http://127.0.0.1:9297/000000000000/jobs","MessageBody":"hello"}"#,
        );
        let send_json: Value =
            serde_json::from_slice(&send.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(send_json["MD5OfMessageBody"], md5_hex(b"hello"));

        let recv = call(
            Method::POST,
            "/",
            "sqs",
            Some("AmazonSQS.ReceiveMessage"),
            "application/x-amz-json-1.0",
            &[],
            br#"{"QueueUrl":"http://127.0.0.1:9297/000000000000/jobs"}"#,
        );
        let recv_json: Value =
            serde_json::from_slice(&recv.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(recv_json["Messages"][0]["Body"], "hello");

        // 4. STS AssumeRole Query XML
        let sts = call(
            Method::POST,
            "/",
            "sts",
            None,
            "application/x-www-form-urlencoded",
            &[],
            b"Action=AssumeRole&RoleArn=arn%3Aaws%3Aiam%3A%3A000000000000%3Arole%2Fworker&RoleSessionName=test",
        );
        assert_eq!(sts.status(), StatusCode::OK);
        let sts_xml =
            String::from_utf8(sts.into_body().collect().await.unwrap().to_bytes().to_vec())
                .unwrap();
        assert!(sts_xml.contains("<AccessKeyId>ASIALENSLET"));
        assert!(sts_xml.contains("assumed-role/worker/test"));
    }

    #[tokio::test]
    async fn s3_multipart_upload_stages_parts_and_assembles_on_complete() {
        let sb = Sandbox::new("http://127.0.0.1:9297");
        let call = |method: Method, path: &str, extra: &[(&str, &str)], body: &[u8]| {
            let mut h = HeaderMap::new();
            h.insert("content-type", "application/octet-stream".parse().unwrap());
            for (k, v) in extra {
                h.insert(
                    header::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                    v.parse().unwrap(),
                );
            }
            let c = capture::content(&h);
            let fb = wire::fallback_operation("s3", method.as_str(), path, &h);
            sb.handle(
                &method,
                path,
                &h,
                "s3",
                Some("us-east-1"),
                None,
                &fb,
                &c,
                body,
            )
        };

        // Initiate multipart upload with metadata + SSE headers.
        let init = call(
            Method::POST,
            "/media-bucket/videos/demo.bin?uploads",
            &[
                ("content-type", "video/mp4"),
                ("x-amz-meta-author", "alice"),
            ],
            b"",
        );
        assert_eq!(init.status(), StatusCode::OK);
        let init_xml = String::from_utf8(
            init.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        let upload_id = extract_xml_tags(&init_xml, "UploadId").pop().unwrap();

        // Upload part 2 first, then part 1 (out-of-order arrival must assemble in partNumber order).
        let p2 = call(
            Method::PUT,
            &format!("/media-bucket/videos/demo.bin?partNumber=2&uploadId={upload_id}"),
            &[],
            b"-world-part-two",
        );
        assert_eq!(p2.status(), StatusCode::OK);
        let etag2 = p2
            .headers()
            .get("etag")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let p1 = call(
            Method::PUT,
            &format!("/media-bucket/videos/demo.bin?partNumber=1&uploadId={upload_id}"),
            &[],
            b"hello-part-one",
        );
        assert_eq!(p1.status(), StatusCode::OK);
        let etag1 = p1
            .headers()
            .get("etag")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        // Object MUST NOT be visible via GetObject before CompleteMultipartUpload.
        let premature = call(Method::GET, "/media-bucket/videos/demo.bin", &[], b"");
        assert_eq!(premature.status(), StatusCode::NOT_FOUND);

        // CompleteMultipartUpload with XML-escaped &quot; ETags assembles parts 1 + 2 in order.
        let complete_xml = format!(
            "<CompleteMultipartUpload>\
               <Part><PartNumber>1</PartNumber><ETag>{}</ETag></Part>\
               <Part><PartNumber>2</PartNumber><ETag>{}</ETag></Part>\
             </CompleteMultipartUpload>",
            xml_escape(&etag1),
            xml_escape(&etag2),
        );
        let done = call(
            Method::POST,
            &format!("/media-bucket/videos/demo.bin?uploadId={upload_id}"),
            &[("content-type", "application/xml")],
            complete_xml.as_bytes(),
        );
        assert_eq!(done.status(), StatusCode::OK);
        let done_xml = String::from_utf8(
            done.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .to_vec(),
        )
        .unwrap();
        assert!(done_xml.contains("-2&quot;</ETag>"), "{done_xml}");

        let got = call(Method::GET, "/media-bucket/videos/demo.bin", &[], b"");
        assert_eq!(got.status(), StatusCode::OK);
        assert_eq!(got.headers().get("content-type").unwrap(), "video/mp4");
        assert_eq!(got.headers().get("x-amz-meta-author").unwrap(), "alice");
        let bytes = got.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"hello-part-one-world-part-two");
    }

    #[tokio::test]
    async fn dynamodb_query_and_scan_evaluate_key_and_filter_predicates() {
        let sb = Sandbox::new("http://127.0.0.1:9297");
        let ddb = |op: &str, payload: Value| {
            let mut h = HeaderMap::new();
            h.insert(
                "content-type",
                "application/x-amz-json-1.0".parse().unwrap(),
            );
            h.insert(
                "x-amz-target",
                format!("DynamoDB_20120810.{op}").parse().unwrap(),
            );
            let c = capture::content(&h);
            let raw = payload.to_string();
            sb.handle(
                &Method::POST,
                "/",
                &h,
                "dynamodb",
                Some("us-east-1"),
                Some(op),
                op,
                &c,
                raw.as_bytes(),
            )
        };

        let created = ddb(
            "CreateTable",
            json!({
                "TableName": "tenant_orders",
                "KeySchema": [
                    { "AttributeName": "pk", "KeyType": "HASH" },
                    { "AttributeName": "seq", "KeyType": "RANGE" }
                ]
            }),
        );
        assert_eq!(created.status(), StatusCode::OK);

        for (pk, seq, status, total) in [
            ("tenant#acme", "2", "paid", "45.5"),
            ("tenant#acme", "10", "paid", "120.0"),
            ("tenant#acme", "25", "draft", "999.0"),
            ("tenant#other", "10", "paid", "500.0"),
        ] {
            let r = ddb(
                "PutItem",
                json!({
                    "TableName": "tenant_orders",
                    "Item": {
                        "pk": { "S": pk },
                        "seq": { "N": seq },
                        "status": { "S": status },
                        "total": { "N": total }
                    }
                }),
            );
            assert_eq!(r.status(), StatusCode::OK);
        }

        // Query isolates tenant#acme, evaluates numeric BETWEEN (2..=25 includes 10),
        // applies FilterExpression (`#st = :paid AND total >= :min`), and reverses order (`ScanIndexForward: false`).
        let q = ddb(
            "Query",
            json!({
                "TableName": "tenant_orders",
                "KeyConditionExpression": "#pk = :t AND seq BETWEEN :lo AND :hi",
                "FilterExpression": "#st = :paid AND total >= :min",
                "ExpressionAttributeNames": {
                    "#pk": "pk",
                    "#st": "status"
                },
                "ExpressionAttributeValues": {
                    ":t": { "S": "tenant#acme" },
                    ":lo": { "N": "1" },
                    ":hi": { "N": "30" },
                    ":paid": { "S": "paid" },
                    ":min": { "N": "50" }
                },
                "ScanIndexForward": false
            }),
        );
        assert_eq!(q.status(), StatusCode::OK);
        let q_json: Value =
            serde_json::from_slice(&q.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(
            q_json["ScannedCount"], 3,
            "3 items matched key condition for tenant#acme"
        );
        assert_eq!(
            q_json["Count"], 1,
            "only seq=10 passed FilterExpression total >= 50 & status=paid"
        );
        assert_eq!(q_json["Items"][0]["pk"]["S"], "tenant#acme");
        assert_eq!(q_json["Items"][0]["seq"]["N"], "10");

        // Verify numeric sort order on `ScanIndexForward: false` without filter (25 -> 10 -> 2).
        let q_order = ddb(
            "Query",
            json!({
                "TableName": "tenant_orders",
                "KeyConditionExpression": "pk = :t",
                "ExpressionAttributeValues": { ":t": { "S": "tenant#acme" } },
                "ScanIndexForward": false
            }),
        );
        let order_json: Value =
            serde_json::from_slice(&q_order.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let seqs: Vec<&str> = order_json["Items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|it| it["seq"]["N"].as_str().unwrap())
            .collect();
        assert_eq!(seqs, vec!["25", "10", "2"]);

        // Scan with `begins_with` + `attribute_not_exists` works across partitions.
        let scan = ddb(
            "Scan",
            json!({
                "TableName": "tenant_orders",
                "FilterExpression": "begins_with(pk, :prefix) AND NOT #st = :draft",
                "ExpressionAttributeNames": { "#st": "status" },
                "ExpressionAttributeValues": {
                    ":prefix": { "S": "tenant#other" },
                    ":draft": { "S": "draft" }
                }
            }),
        );
        let scan_json: Value =
            serde_json::from_slice(&scan.into_body().collect().await.unwrap().to_bytes()).unwrap();
        assert_eq!(scan_json["ScannedCount"], 4);
        assert_eq!(scan_json["Count"], 1);
        assert_eq!(scan_json["Items"][0]["pk"]["S"], "tenant#other");

        // Malformed expression fails explicitly with 400 ValidationException instead of returning unfiltered items.
        let bad = ddb(
            "Query",
            json!({
                "TableName": "tenant_orders",
                "KeyConditionExpression": "pk = :missing_placeholder"
            }),
        );
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            bad.headers().get("x-amzn-errortype").unwrap(),
            "ValidationException"
        );
    }
}
