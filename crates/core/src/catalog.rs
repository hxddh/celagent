//! Comprehensive AWS REST-URI operation catalog, extended service ARN builder,
//! and human-readable semantic target extraction (`target_summary`).
//!
//! Eliminates two major gaps:
//! 1. REST-JSON / REST-XML services where SDKs do not send `x-amz-target` or `?x-id=`
//!    (API Gateway v1/v2, CloudFront, Route53, EKS, ECS, S3 Control, OpenSearch,
//!    IoT, Batch, SESv2, GuardDuty, Backup, SageMaker Runtime, Bedrock Agent, etc.).
//! 2. Replacing useless `POST /` rows with high-signal semantic targets
//!    (`orders · id="ord-42"`, `s3://shop-assets/receipts/1001.json`, `queue/fulfilment`).

use http::HeaderMap;
use serde_json::Value;

use crate::{trace::Trace, wire};

/// Resolve a REST-JSON or REST-XML operation name from `(service, method, path, headers)`
/// across 40+ AWS REST services when neither `x-amz-target` nor `?x-id=` was sent.
pub fn resolve_rest_operation(
    service: &str,
    method: &str,
    path: &str,
    _headers: &HeaderMap,
) -> Option<String> {
    let bare = path.split('?').next().unwrap_or(path);
    let segs: Vec<&str> = bare.split('/').filter(|s| !s.is_empty()).collect();

    let op: &str = match service {
        // API Gateway v1 (`apigateway`)
        "apigateway" => match (method, segs.as_slice()) {
            ("POST", ["restapis"]) => "CreateRestApi",
            ("GET", ["restapis"]) => "GetRestApis",
            ("GET", ["restapis", _]) => "GetRestApi",
            ("DELETE", ["restapis", _]) => "DeleteRestApi",
            ("GET", ["restapis", _, "resources"]) => "GetResources",
            ("POST", ["restapis", _, "resources", _]) => "CreateResource",
            ("PUT", ["restapis", _, "resources", _, "methods", _]) => "PutMethod",
            ("PUT", ["restapis", _, "resources", _, "methods", _, "integration"]) => {
                "PutIntegration"
            }
            ("POST", ["restapis", _, "deployments"]) => "CreateDeployment",
            ("GET", ["restapis", _, "stages"]) => "GetStages",
            ("POST", ["apikeys"]) => "CreateApiKey",
            ("GET", ["apikeys"]) => "GetApiKeys",
            ("POST", ["domainnames"]) => "CreateDomainName",
            ("GET", ["domainnames"]) => "GetDomainNames",
            _ => return None,
        },
        // API Gateway v2 (`apigatewayv2`)
        "apigatewayv2" => match (method, segs.as_slice()) {
            ("POST", ["v2", "apis"]) => "CreateApi",
            ("GET", ["v2", "apis"]) => "GetApis",
            ("GET", ["v2", "apis", _]) => "GetApi",
            ("PATCH", ["v2", "apis", _]) => "UpdateApi",
            ("DELETE", ["v2", "apis", _]) => "DeleteApi",
            ("POST", ["v2", "apis", _, "routes"]) => "CreateRoute",
            ("GET", ["v2", "apis", _, "routes"]) => "GetRoutes",
            ("DELETE", ["v2", "apis", _, "routes", _]) => "DeleteRoute",
            ("POST", ["v2", "apis", _, "integrations"]) => "CreateIntegration",
            ("GET", ["v2", "apis", _, "integrations"]) => "GetIntegrations",
            ("POST", ["v2", "apis", _, "stages"]) => "CreateStage",
            ("GET", ["v2", "apis", _, "stages"]) => "GetStages",
            ("POST", ["v2", "domainnames"]) => "CreateDomainName",
            ("GET", ["v2", "domainnames"]) => "GetDomainNames",
            _ => return None,
        },
        // Amazon EKS (`eks`)
        "eks" => match (method, segs.as_slice()) {
            ("POST", ["clusters"]) => "CreateCluster",
            ("GET", ["clusters"]) => "ListClusters",
            ("GET", ["clusters", _]) => "DescribeCluster",
            ("DELETE", ["clusters", _]) => "DeleteCluster",
            ("POST", ["clusters", _, "node-groups"]) => "CreateNodegroup",
            ("GET", ["clusters", _, "node-groups"]) => "ListNodegroups",
            ("GET", ["clusters", _, "node-groups", _]) => "DescribeNodegroup",
            ("DELETE", ["clusters", _, "node-groups", _]) => "DeleteNodegroup",
            ("POST", ["clusters", _, "addons"]) => "CreateAddon",
            ("GET", ["clusters", _, "addons"]) => "ListAddons",
            _ => return None,
        },
        // AWS Batch (`batch`)
        "batch" => match (method, segs.as_slice()) {
            ("POST", ["v1", "submitjob"]) => "SubmitJob",
            ("POST", ["v1", "describejobs"]) => "DescribeJobs",
            ("POST", ["v1", "listjobs"]) => "ListJobs",
            ("POST", ["v1", "terminatejob"]) => "TerminateJob",
            ("POST", ["v1", "canceljob"]) => "CancelJob",
            ("POST", ["v1", "registerjobdefinition"]) => "RegisterJobDefinition",
            ("POST", ["v1", "createjobqueue"]) => "CreateJobQueue",
            ("POST", ["v1", "createcomputeenvironment"]) => "CreateComputeEnvironment",
            _ => return None,
        },
        // CloudFront (`cloudfront`)
        "cloudfront" => match (method, segs.as_slice()) {
            ("POST", [_, "distribution"]) => "CreateDistribution",
            ("GET", [_, "distribution"]) => "ListDistributions",
            ("GET", [_, "distribution", _]) => "GetDistribution",
            ("GET", [_, "distribution", _, "config"]) => "GetDistributionConfig",
            ("PUT", [_, "distribution", _, "config"]) => "UpdateDistribution",
            ("DELETE", [_, "distribution", _]) => "DeleteDistribution",
            ("POST", [_, "distribution", _, "invalidation"]) => "CreateInvalidation",
            ("GET", [_, "distribution", _, "invalidation"]) => "ListInvalidations",
            _ => return None,
        },
        // Route 53 (`route53`)
        "route53" => match (method, segs.as_slice()) {
            ("POST", [_, "hostedzone"]) => "CreateHostedZone",
            ("GET", [_, "hostedzone"]) => "ListHostedZones",
            ("GET", [_, "hostedzone", _]) => "GetHostedZone",
            ("DELETE", [_, "hostedzone", _]) => "DeleteHostedZone",
            ("POST", [_, "hostedzone", _, "rrset"]) => "ChangeResourceRecordSets",
            ("GET", [_, "hostedzone", _, "rrset"]) => "ListResourceRecordSets",
            ("GET", [_, "change", _]) => "GetChange",
            _ => return None,
        },
        // S3 Control (`s3-control`)
        "s3-control" => match (method, segs.as_slice()) {
            ("PUT", ["v20180820", "configuration", "publicAccessBlock"]) => "PutPublicAccessBlock",
            ("GET", ["v20180820", "configuration", "publicAccessBlock"]) => "GetPublicAccessBlock",
            ("DELETE", ["v20180820", "configuration", "publicAccessBlock"]) => {
                "DeletePublicAccessBlock"
            }
            ("PUT", ["v20180820", "accesspoint", _]) => "CreateAccessPoint",
            ("GET", ["v20180820", "accesspoint", _]) => "GetAccessPoint",
            ("GET", ["v20180820", "accesspoint"]) => "ListAccessPoints",
            ("DELETE", ["v20180820", "accesspoint", _]) => "DeleteAccessPoint",
            ("POST", ["v20180820", "jobs"]) => "CreateJob",
            ("GET", ["v20180820", "jobs"]) => "ListJobs",
            _ => return None,
        },
        // SES v2 (`sesv2` / `email` REST v2 paths)
        "sesv2" | "email" if segs.first() == Some(&"v2") => match (method, segs.as_slice()) {
            ("POST", ["v2", "email", "outbound-emails"]) => "SendEmail",
            ("POST", ["v2", "email", "outbound-bulk-emails"]) => "SendBulkEmail",
            ("POST", ["v2", "email", "identities"]) => "CreateEmailIdentity",
            ("GET", ["v2", "email", "identities"]) => "ListEmailIdentities",
            ("GET", ["v2", "email", "identities", _]) => "GetEmailIdentity",
            ("DELETE", ["v2", "email", "identities", _]) => "DeleteEmailIdentity",
            ("POST", ["v2", "email", "configuration-sets"]) => "CreateConfigurationSet",
            ("GET", ["v2", "email", "configuration-sets"]) => "ListConfigurationSets",
            ("GET", ["v2", "email", "account"]) => "GetAccount",
            _ => return None,
        },
        // EventBridge Scheduler (`scheduler`)
        "scheduler" => match (method, segs.as_slice()) {
            ("POST", ["schedules", _]) => "CreateSchedule",
            ("GET", ["schedules", _]) => "GetSchedule",
            ("PUT", ["schedules", _]) => "UpdateSchedule",
            ("DELETE", ["schedules", _]) => "DeleteSchedule",
            ("GET", ["schedules"]) => "ListSchedules",
            ("POST", ["schedule-groups", _]) => "CreateScheduleGroup",
            ("GET", ["schedule-groups"]) => "ListScheduleGroups",
            _ => return None,
        },
        // EventBridge Pipes (`pipes`)
        "pipes" => match (method, segs.as_slice()) {
            ("POST", ["v1", "pipes", _]) => "CreatePipe",
            ("GET", ["v1", "pipes", _]) => "DescribePipe",
            ("PUT", ["v1", "pipes", _]) => "UpdatePipe",
            ("DELETE", ["v1", "pipes", _]) => "DeletePipe",
            ("GET", ["v1", "pipes"]) => "ListPipes",
            ("POST", ["v1", "pipes", _, "start"]) => "StartPipe",
            ("POST", ["v1", "pipes", _, "stop"]) => "StopPipe",
            _ => return None,
        },
        // RDS Data API (`rds-data`)
        "rds-data" => match (method, segs.as_slice()) {
            ("POST", ["Execute"]) => "ExecuteStatement",
            ("POST", ["BatchExecute"]) => "BatchExecuteStatement",
            ("POST", ["BeginTransaction"]) => "BeginTransaction",
            ("POST", ["CommitTransaction"]) => "CommitTransaction",
            ("POST", ["RollbackTransaction"]) => "RollbackTransaction",
            _ => return None,
        },
        // OpenSearch (`es` / `opensearch`)
        "es" | "opensearch" => match (method, segs.as_slice()) {
            ("POST", ["2021-01-01", "opensearch", "domain"]) => "CreateDomain",
            ("GET", ["2021-01-01", "opensearch", "domain"]) => "ListDomainNames",
            ("GET", ["2021-01-01", "opensearch", "domain", _]) => "DescribeDomain",
            ("DELETE", ["2021-01-01", "opensearch", "domain", _]) => "DeleteDomain",
            _ => return None,
        },
        // AWS IoT Core & IoT Data (`iot` / `iotdata`)
        "iot" => match (method, segs.as_slice()) {
            ("POST", ["things", _]) => "CreateThing",
            ("GET", ["things", _]) => "DescribeThing",
            ("DELETE", ["things", _]) => "DeleteThing",
            ("GET", ["things"]) => "ListThings",
            ("POST", ["policies", _]) => "CreatePolicy",
            ("GET", ["policies"]) => "ListPolicies",
            _ => return None,
        },
        "iotdata" | "iot-data" => match (method, segs.as_slice()) {
            ("POST", ["topics", ..]) => "Publish",
            ("GET", ["things", _, "shadow"]) => "GetThingShadow",
            ("POST", ["things", _, "shadow"]) => "UpdateThingShadow",
            ("DELETE", ["things", _, "shadow"]) => "DeleteThingShadow",
            _ => return None,
        },
        // SageMaker Runtime (`sagemaker` / `sagemaker-runtime`)
        "sagemaker-runtime" | "sagemaker" if segs.first() == Some(&"endpoints") => {
            match (method, segs.as_slice()) {
                ("POST", ["endpoints", _, "invocations"]) => "InvokeEndpoint",
                ("POST", ["endpoints", _, "invocations-response-stream"]) => {
                    "InvokeEndpointWithResponseStream"
                }
                ("POST", ["endpoints", _, "async-invocations"]) => "InvokeEndpointAsync",
                _ => return None,
            }
        }
        // Bedrock Agent Runtime (`bedrock-agent-runtime` / `bedrock-agent`)
        "bedrock-agent-runtime" => match (method, segs.as_slice()) {
            ("POST", ["knowledgebases", _, "retrieve"]) => "Retrieve",
            ("POST", ["knowledgebases", _, "retrieveAndGenerate"]) => "RetrieveAndGenerate",
            ("POST", ["agents", _, "agentAliases", _, "sessions", _, "text"]) => "InvokeAgent",
            _ => return None,
        },
        // AWS Backup (`backup`)
        "backup" => match (method, segs.as_slice()) {
            ("PUT", ["backup-vaults", _]) => "CreateBackupVault",
            ("GET", ["backup-vaults", _]) => "DescribeBackupVault",
            ("GET", ["backup-vaults"]) => "ListBackupVaults",
            ("DELETE", ["backup-vaults", _]) => "DeleteBackupVault",
            ("PUT", ["backup", "plans"]) => "CreateBackupPlan",
            ("GET", ["backup", "plans"]) => "ListBackupPlans",
            _ => return None,
        },
        // AWS X-Ray (`xray`)
        "xray" => match (method, segs.as_slice()) {
            ("POST", ["TraceSegments"]) => "PutTraceSegments",
            ("POST", ["TelemetryRecords"]) => "PutTelemetryRecords",
            ("POST", ["Traces"]) => "BatchGetTraces",
            ("POST", ["TraceSummaries"]) => "GetTraceSummaries",
            ("POST", ["ServiceGraph"]) => "GetServiceGraph",
            _ => return None,
        },
        // GuardDuty (`guardduty`)
        "guardduty" => match (method, segs.as_slice()) {
            ("POST", ["detector"]) => "CreateDetector",
            ("GET", ["detector"]) => "ListDetectors",
            ("GET", ["detector", _]) => "GetDetector",
            ("DELETE", ["detector", _]) => "DeleteDetector",
            ("POST", ["detector", _, "findings"]) => "ListFindings",
            ("POST", ["detector", _, "findings", "get"]) => "GetFindings",
            _ => return None,
        },
        // Glacier (`glacier`)
        "glacier" => match (method, segs.as_slice()) {
            ("PUT", [_, "vaults", _]) => "CreateVault",
            ("GET", [_, "vaults", _]) => "DescribeVault",
            ("GET", [_, "vaults"]) => "ListVaults",
            ("DELETE", [_, "vaults", _]) => "DeleteVault",
            ("POST", [_, "vaults", _, "archives"]) => "UploadArchive",
            _ => return None,
        },
        // S3 Object Lambda (`s3-object-lambda`)
        "s3-object-lambda" if method == "POST" && bare.contains("WriteGetObjectResponse") => {
            "WriteGetObjectResponse"
        }
        _ => return None,
    };
    Some(op.to_string())
}

/// Extract extended service resource ARNs for services beyond the core 18.
/// `norm` normalizes generated names in Footprint mode and leaves them intact in Concrete mode.
pub fn derive_extra_resources(
    prefix: &str,
    operation: &str,
    trace: &Trace,
    json: Option<&Value>,
    form: Option<&[(String, String)]>,
    arn: impl Fn(&str, &str) -> String,
    norm: impl Fn(&str) -> String,
) -> Option<Vec<String>> {
    let bare_path = trace.path.split('?').next().unwrap_or(&trace.path);
    let segs: Vec<&str> = bare_path.split('/').filter(|s| !s.is_empty()).collect();
    let j_str = |key: &str| -> Option<String> {
        json.and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let f_str = |key: &str| -> Option<String> {
        form.and_then(|items| items.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
            .filter(|s| !s.is_empty())
    };
    let field = |key: &str| j_str(key).or_else(|| f_str(key));

    let single = |res: String| Some(vec![res]);

    match prefix {
        "ecs" => {
            if let Some(td) = field("taskDefinition") {
                if td.starts_with("arn:") {
                    return single(td);
                }
                return single(arn("ecs", &format!("task-definition/{}", norm(&td))));
            }
            if let Some(fam) = field("family") {
                return single(arn("ecs", &format!("task-definition/{}:*", norm(&fam))));
            }
            if let Some(svc) = field("service").or_else(|| field("serviceName")) {
                let cluster = field("cluster")
                    .map(|c| norm(c.rsplit('/').next().unwrap_or(&c)))
                    .unwrap_or_else(|| "default".into());
                return single(arn(
                    "ecs",
                    &format!(
                        "service/{cluster}/{}",
                        norm(svc.rsplit('/').next().unwrap_or(&svc))
                    ),
                ));
            }
            if let Some(cluster) = field("cluster").or_else(|| field("clusterName")) {
                if cluster.starts_with("arn:") {
                    return single(cluster);
                }
                return single(arn("ecs", &format!("cluster/{}", norm(&cluster))));
            }
            None
        }
        "eks" => {
            if let Some(name) = segs
                .get(1)
                .filter(|_| segs.first() == Some(&"clusters"))
                .copied()
                .map(wire::percent_decode_path)
                .or_else(|| field("name"))
                .or_else(|| field("clusterName"))
            {
                return single(arn("eks", &format!("cluster/{}", norm(&name))));
            }
            None
        }
        "cloudformation" => {
            if let Some(stack) = field("StackName") {
                if stack.starts_with("arn:") {
                    return single(stack);
                }
                return single(arn("cloudformation", &format!("stack/{}/*", norm(&stack))));
            }
            None
        }
        "glue" => {
            if let Some(job) = field("JobName").or_else(|| {
                (operation == "CreateJob" || operation == "GetJob").then(|| field("Name"))?
            }) {
                return single(arn("glue", &format!("job/{}", norm(&job))));
            }
            if let Some(db) = field("DatabaseName") {
                if let Some(tbl) = field("TableName").or_else(|| {
                    json.and_then(|v| v.get("TableInput"))
                        .and_then(|t| t.get("Name"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                }) {
                    return single(arn("glue", &format!("table/{}/{}", norm(&db), norm(&tbl))));
                }
                return single(arn("glue", &format!("database/{}", norm(&db))));
            }
            None
        }
        "athena" => {
            let wg = field("WorkGroup").unwrap_or_else(|| "primary".into());
            single(arn("athena", &format!("workgroup/{}", norm(&wg))))
        }
        "rds" => {
            if let Some(cluster) = field("DBClusterIdentifier") {
                return single(arn("rds", &format!("cluster:{}", norm(&cluster))));
            }
            if let Some(db) = field("DBInstanceIdentifier") {
                return single(arn("rds", &format!("db:{}", norm(&db))));
            }
            None
        }
        "apigateway" => {
            if segs.first() == Some(&"restapis") {
                let rest = if let Some(id) = segs.get(1) {
                    format!("/restapis/{}", norm(id))
                } else {
                    "/restapis".into()
                };
                return single(arn("apigateway", &rest));
            }
            if segs.first() == Some(&"v2") && segs.get(1) == Some(&"apis") {
                let rest = if let Some(id) = segs.get(2) {
                    format!("/apis/{}", norm(id))
                } else {
                    "/apis".into()
                };
                return single(arn("apigateway", &rest));
            }
            None
        }
        "cloudfront" => {
            if let Some(dist) = segs
                .get(2)
                .filter(|_| segs.get(1) == Some(&"distribution"))
                .copied()
            {
                return single(format!("arn:aws:cloudfront::*:distribution/{}", norm(dist)));
            }
            None
        }
        "route53" => {
            if let Some(zone) = segs
                .get(2)
                .filter(|_| segs.get(1) == Some(&"hostedzone"))
                .copied()
            {
                return single(format!("arn:aws:route53:::hostedzone/{}", norm(zone)));
            }
            None
        }
        "ses" => {
            if let Some(id) = field("FromEmailAddress")
                .or_else(|| field("Source"))
                .or_else(|| field("EmailIdentity"))
                .or_else(|| {
                    segs.get(3)
                        .filter(|_| segs.get(2) == Some(&"identities"))
                        .map(|s| wire::percent_decode_path(s))
                })
            {
                return single(arn("ses", &format!("identity/{id}")));
            }
            None
        }
        "sagemaker" => {
            if let Some(ep) = segs
                .get(1)
                .filter(|_| segs.first() == Some(&"endpoints"))
                .map(|s| wire::percent_decode_path(s))
                .or_else(|| field("EndpointName"))
            {
                return single(arn("sagemaker", &format!("endpoint/{}", norm(&ep))));
            }
            None
        }
        "es" | "opensearch" => {
            if let Some(dom) = segs
                .get(3)
                .filter(|_| segs.get(2) == Some(&"domain"))
                .map(|s| wire::percent_decode_path(s))
                .or_else(|| field("DomainName"))
            {
                return single(arn("es", &format!("domain/{}", norm(&dom))));
            }
            None
        }
        "pipes" => {
            if let Some(pipe) = segs
                .get(2)
                .filter(|_| segs.get(1) == Some(&"pipes"))
                .map(|s| wire::percent_decode_path(s))
                .or_else(|| field("Name"))
            {
                return single(arn("pipes", &format!("pipe/{}", norm(&pipe))));
            }
            None
        }
        "backup" => {
            if let Some(vault) = segs
                .get(1)
                .filter(|_| segs.first() == Some(&"backup-vaults"))
                .map(|s| wire::percent_decode_path(s))
                .or_else(|| field("BackupVaultName"))
            {
                return single(arn("backup", &format!("backup-vault:{}", norm(&vault))));
            }
            None
        }
        _ => None,
    }
}

/// Compute a crisp, human-readable one-line semantic target for any AWS call
/// (e.g. `orders · id="ord-42"`, `s3://shop-assets/receipts/1001.json`, `queue/fulfilment`).
/// Replaces meaningless `POST /` rows in tables, Inspector headers, and CLI summaries.
pub fn target_summary(trace: &Trace) -> String {
    let body_text = trace
        .request
        .body
        .as_ref()
        .and_then(|b| b.text.as_deref())
        .unwrap_or("")
        .trim();
    let json: Option<Value> = if body_text.starts_with('{') {
        serde_json::from_str(body_text).ok()
    } else {
        None
    };

    match trace.service.as_str() {
        "s3" => return s3_target_summary(trace),
        "dynamodb" | "streams.dynamodb" | "dynamodbstreams" => {
            if let Some(j) = json.as_ref()
                && let Some(summary) = dynamodb_target_summary(&trace.operation, j)
            {
                return summary;
            }
        }
        "sqs" => {
            if let Some(j) = json.as_ref() {
                if let Some(q) = j
                    .get("QueueUrl")
                    .or_else(|| j.get("QueueName"))
                    .and_then(Value::as_str)
                {
                    let name = q.trim_end_matches('/').rsplit('/').next().unwrap_or(q);
                    if let Some(entries) = j.get("Entries").and_then(Value::as_array) {
                        return format!("queue/{name} · {} msgs", entries.len());
                    }
                    return format!("queue/{name}");
                }
            } else if let Some(q) =
                form_param(body_text, "QueueUrl").or_else(|| form_param(body_text, "QueueName"))
            {
                let name = q.trim_end_matches('/').rsplit('/').next().unwrap_or(&q);
                return format!("queue/{name}");
            }
        }
        "sns" => {
            let topic = json
                .as_ref()
                .and_then(|j| {
                    j.get("TopicArn")
                        .or_else(|| j.get("TargetArn"))
                        .or_else(|| j.get("Name"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .or_else(|| {
                    form_param(body_text, "TopicArn")
                        .or_else(|| form_param(body_text, "TargetArn"))
                        .or_else(|| form_param(body_text, "Name"))
                });
            if let Some(t) = topic {
                let name = t.rsplit(':').next().unwrap_or(&t);
                return format!("topic/{name}");
            }
        }
        "lambda" => {
            let bare = trace.path.split('?').next().unwrap_or(&trace.path);
            let segs: Vec<&str> = bare.split('/').filter(|s| !s.is_empty()).collect();
            if let Some(&fname) = segs.get(2).filter(|_| segs.get(1) == Some(&"functions")) {
                let decoded = wire::percent_decode_path(fname);
                let short = decoded.rsplit(':').next().unwrap_or(&decoded);
                return format!("fn/{short}");
            }
            if let Some(j) = json.as_ref()
                && let Some(fname) = j.get("FunctionName").and_then(Value::as_str)
            {
                return format!("fn/{fname}");
            }
        }
        "kms" => {
            if let Some(j) = json.as_ref() {
                if let Some(kid) = j
                    .get("KeyId")
                    .or_else(|| j.get("TargetKeyId"))
                    .or_else(|| j.get("AliasName"))
                    .and_then(Value::as_str)
                {
                    let short = kid.rsplit('/').next().unwrap_or(kid);
                    return format!("key/{short}");
                }
                if let Some(spec) = j
                    .get("KeySpec")
                    .or_else(|| j.get("CustomerMasterKeySpec"))
                    .and_then(Value::as_str)
                {
                    return spec.to_string();
                }
                if trace.operation == "CreateKey" {
                    return "SYMMETRIC_DEFAULT".to_string();
                }
            }
        }
        "iam" | "sts" => {
            let val = json
                .as_ref()
                .and_then(|j| {
                    j.get("RoleName")
                        .or_else(|| j.get("RoleArn"))
                        .or_else(|| j.get("UserName"))
                        .or_else(|| j.get("PolicyName"))
                        .or_else(|| j.get("PolicyArn"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .or_else(|| {
                    form_param(body_text, "RoleName")
                        .or_else(|| form_param(body_text, "RoleArn"))
                        .or_else(|| form_param(body_text, "UserName"))
                        .or_else(|| form_param(body_text, "PolicyName"))
                        .or_else(|| form_param(body_text, "PolicyArn"))
                });
            if let Some(v) = val {
                if let Some(rest) = v.split(':').nth(5) {
                    return rest.to_string();
                }
                return format!("role/{v}");
            }
        }
        "secretsmanager" => {
            if let Some(j) = json.as_ref()
                && let Some(id) = j
                    .get("SecretId")
                    .or_else(|| j.get("Name"))
                    .and_then(Value::as_str)
            {
                let short = id.rsplit(":secret:").next().unwrap_or(id);
                return format!("secret/{short}");
            }
        }
        "ssm" => {
            if let Some(j) = json.as_ref() {
                if let Some(name) = j.get("Name").and_then(Value::as_str) {
                    return format!("param/{}", name.trim_start_matches('/'));
                }
                if let Some(names) = j.get("Names").and_then(Value::as_array) {
                    let list: Vec<&str> = names.iter().filter_map(Value::as_str).take(2).collect();
                    if !list.is_empty() {
                        return format!("params: {} ({} total)", list.join(", "), names.len());
                    }
                }
            }
        }
        "events" => {
            if let Some(j) = json.as_ref() {
                if let Some(entries) = j.get("Entries").and_then(Value::as_array) {
                    let bus = entries
                        .first()
                        .and_then(|e| e.get("EventBusName"))
                        .and_then(Value::as_str)
                        .unwrap_or("default");
                    return format!("bus/{bus} · {} events", entries.len());
                }
                if let Some(rule) = j
                    .get("Name")
                    .or_else(|| j.get("Rule"))
                    .and_then(Value::as_str)
                {
                    return format!("rule/{rule}");
                }
            }
        }
        "states" => {
            if let Some(j) = json.as_ref()
                && let Some(arn) = j
                    .get("stateMachineArn")
                    .or_else(|| j.get("executionArn"))
                    .or_else(|| j.get("name"))
                    .and_then(Value::as_str)
            {
                let short = arn.rsplit(':').next().unwrap_or(arn);
                return format!("sm/{short}");
            }
        }
        "logs" => {
            if let Some(j) = json.as_ref()
                && let Some(lg) = j.get("logGroupName").and_then(Value::as_str)
            {
                if let Some(ls) = j.get("logStreamName").and_then(Value::as_str) {
                    return format!("{lg} · {ls}");
                }
                return lg.to_string();
            }
        }
        "bedrock" | "bedrock-runtime" => {
            let bare = trace.path.split('?').next().unwrap_or(&trace.path);
            let segs: Vec<&str> = bare.split('/').filter(|s| !s.is_empty()).collect();
            if segs.first() == Some(&"model")
                && let Some(mid) = segs.get(1)
            {
                return format!("model/{}", wire::percent_decode_path(mid));
            }
        }
        _ => {}
    }

    // Fallback: derive from first concrete/footprint resource ARN if not `*`.
    if let Some(first) = trace.access.first()
        && first.resource != "*"
    {
        if let Some(s3) = first.resource.strip_prefix("arn:aws:s3:::") {
            return format!("s3://{s3}");
        }
        if let Some(rest) = first.resource.splitn(6, ':').nth(5)
            && !rest.is_empty()
        {
            return rest.to_string();
        }
    }

    let path = trace.path.trim();
    if !path.is_empty() && path != "/" {
        format!("{} {path}", trace.method.to_ascii_uppercase())
    } else if let Some(reg) = trace.region.as_deref().filter(|r| !r.is_empty()) {
        format!("{} · {reg}", trace.service.to_ascii_uppercase())
    } else {
        format!(
            "{} {}",
            trace.method.to_ascii_uppercase(),
            if path.is_empty() { "/" } else { path }
        )
    }
}

fn s3_target_summary(trace: &Trace) -> String {
    let (bare, _query) = trace.path.split_once('?').unwrap_or((&trace.path, ""));
    let host = trace.host.split(':').next().unwrap_or(&trace.host);
    let vhost_bucket = host
        .strip_suffix(".localhost")
        .or_else(|| host.split(".s3.").next().filter(|_| host.contains(".s3.")))
        .or_else(|| host.split(".s3-").next().filter(|_| host.contains(".s3-")));

    let (bucket, key) = if let Some(b) = vhost_bucket {
        (
            b.to_string(),
            wire::percent_decode_path(bare.trim_start_matches('/')),
        )
    } else {
        let trimmed = bare.trim_start_matches('/');
        match trimmed.split_once('/') {
            Some((b, k)) => (wire::percent_decode_path(b), wire::percent_decode_path(k)),
            None => (wire::percent_decode_path(trimmed), String::new()),
        }
    };

    if bucket.is_empty() {
        return "s3://".to_string();
    }
    let mut out = if key.is_empty() {
        format!("s3://{bucket}")
    } else {
        format!("s3://{bucket}/{key}")
    };
    if let Some(prefix) = wire::query_param(&trace.path, "prefix").filter(|p| !p.is_empty()) {
        out.push_str(&format!(" · prefix=\"{prefix}\""));
    }
    out
}

fn dynamodb_target_summary(operation: &str, json: &Value) -> Option<String> {
    if let Some(table) = json.get("TableName").and_then(Value::as_str) {
        let mut out = format!("table/{table}");
        if let Some(idx) = json.get("IndexName").and_then(Value::as_str) {
            out.push_str(&format!("/index/{idx}"));
        }
        if let Some(key_obj) = json
            .get("Key")
            .or_else(|| json.get("Item"))
            .and_then(Value::as_object)
            && let Some((k, v)) = key_obj.iter().next()
            && let Some(scalar) = ddb_scalar_preview(v)
        {
            out.push_str(&format!(" · {k}={scalar}"));
        }
        return Some(out);
    }
    if let Some(req_items) = json.get("RequestItems").and_then(Value::as_object) {
        let tables: Vec<&str> = req_items.keys().map(String::as_str).collect();
        if !tables.is_empty() {
            return Some(format!("tables: {}", tables.join(", ")));
        }
    }
    if let Some(items) = json.get("TransactItems").and_then(Value::as_array) {
        return Some(format!("tx · {} items", items.len()));
    }
    if let Some(stmt) = json.get("Statement").and_then(Value::as_str) {
        let compact: String = stmt.split_whitespace().collect::<Vec<_>>().join(" ");
        let _ = operation;
        return Some(compact);
    }
    None
}

fn ddb_scalar_preview(attr: &Value) -> Option<String> {
    let obj = attr.as_object()?;
    if let Some(s) = obj.get("S").and_then(Value::as_str) {
        let clipped: String = s.chars().take(24).collect();
        return Some(format!("\"{clipped}\""));
    }
    if let Some(n) = obj.get("N").and_then(Value::as_str) {
        return Some(n.to_string());
    }
    if let Some(b) = obj.get("BOOL").and_then(Value::as_bool) {
        return Some(b.to_string());
    }
    None
}

fn form_param(body: &str, key: &str) -> Option<String> {
    if !body.contains('=') {
        return None;
    }
    for pair in body.split('&') {
        let (k, v) = pair.split_once('=')?;
        if k.trim() == key {
            return wire::query_param(&format!("?v={v}"), "v").filter(|s| !s.is_empty());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace::{BodyTrace, Message};

    #[test]
    fn resolves_rest_operations_and_builds_semantic_target_summaries() {
        let headers = HeaderMap::new();
        assert_eq!(
            resolve_rest_operation("apigatewayv2", "POST", "/v2/apis/abc123/routes", &headers)
                .as_deref(),
            Some("CreateRoute")
        );
        assert_eq!(
            resolve_rest_operation("eks", "GET", "/clusters/prod-eks/node-groups", &headers)
                .as_deref(),
            Some("ListNodegroups")
        );
        assert_eq!(
            resolve_rest_operation("sesv2", "POST", "/v2/email/outbound-emails", &headers)
                .as_deref(),
            Some("SendEmail")
        );

        let ddb_call = Trace {
            method: "POST".into(),
            path: "/".into(),
            service: "dynamodb".into(),
            operation: "GetItem".into(),
            request: Message {
                headers: Vec::new(),
                content_type: Some("application/x-amz-json-1.0".into()),
                body: Some(BodyTrace {
                    text: Some(r#"{"TableName":"orders","Key":{"orderId":{"S":"ord-42"}}}"#.into()),
                    hex: None,
                    encoding: None,
                    bytes: 55,
                    truncated: false,
                    binary: false,
                    sha256: String::new(),
                }),
            },
            ..Trace::default()
        };
        assert_eq!(
            target_summary(&ddb_call),
            "table/orders · orderId=\"ord-42\""
        );
    }
}
