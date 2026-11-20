//! Production Infrastructure-as-Code (IaC) IAM policy compiler.
//!
//! Compiles observed permissions (excluding `# test` fixtures) into deployable,
//! least-privilege artifacts across five production formats:
//! - `iam`: AWS IAM Policy JSON with clean `Sid`s, parameterized or explicit
//!   region/account, and synthesized `iam:PassedToService` / `kms:ViaService` conditions.
//! - `terraform`: HashiCorp Terraform `data "aws_iam_policy_document"` HCL.
//! - `cfn`: AWS CloudFormation `PolicyDocument` YAML (`!Sub`).
//! - `sam`: AWS SAM `Policies` YAML snippet (`!Sub`).
//! - `cdk`: AWS CDK TypeScript `iam.PolicyStatement` array.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::access::Access;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PolicyFormat {
    #[default]
    IamJson,
    Terraform,
    CloudFormation,
    Sam,
    CdkTypeScript,
}

impl PolicyFormat {
    pub const ALL: [PolicyFormat; 5] = [
        PolicyFormat::IamJson,
        PolicyFormat::Terraform,
        PolicyFormat::CloudFormation,
        PolicyFormat::Sam,
        PolicyFormat::CdkTypeScript,
    ];

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "iam" | "json" | "policy" => Some(Self::IamJson),
            "tf" | "hcl" | "terraform" => Some(Self::Terraform),
            "cfn" | "cloudformation" | "yaml" => Some(Self::CloudFormation),
            "sam" => Some(Self::Sam),
            "cdk" | "ts" | "typescript" => Some(Self::CdkTypeScript),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::IamJson => "IAM JSON",
            Self::Terraform => "Terraform HCL",
            Self::CloudFormation => "CloudFormation",
            Self::Sam => "AWS SAM",
            Self::CdkTypeScript => "AWS CDK (TS)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::IamJson => "json",
            Self::Terraform => "tf",
            Self::CloudFormation | Self::Sam => "yaml",
            Self::CdkTypeScript => "ts",
        }
    }
}

/// Rich metadata for one permission fed into the IaC compiler.
#[derive(Clone, Debug, Default)]
pub struct PolicyInput {
    pub access: Access,
    pub implied: bool,
    /// Caller operations that needed this permission, e.g. `["LAMBDA CreateFunction"]`.
    pub via: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct CompileOptions {
    pub format: PolicyFormat,
    /// Optional explicit region (e.g. `us-east-1`) replacing `*` in IAM JSON.
    pub region: Option<String>,
    /// Optional explicit 12-digit AWS account ID replacing `*` in IAM JSON.
    pub account: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SynthesizedCondition {
    None,
    PassedToService(Vec<String>),
    KmsViaService(Vec<String>),
}

#[derive(Clone, Debug)]
struct CompiledStatement {
    sid: String,
    actions: Vec<String>,
    resource: String,
    condition: SynthesizedCondition,
}

/// Compile a single permission into `format` (for Inspector / AccessDetail preview).
pub fn compile_single(input: &PolicyInput, format: PolicyFormat) -> String {
    compile(
        std::slice::from_ref(input),
        &CompileOptions {
            format,
            ..CompileOptions::default()
        },
    )
}

/// Compile a set of permissions into the target IaC format.
pub fn compile(inputs: &[PolicyInput], opts: &CompileOptions) -> String {
    let stmts = build_statements(inputs, opts);
    match opts.format {
        PolicyFormat::IamJson => serde_json::to_string_pretty(&render_iam_json(&stmts))
            .unwrap_or_else(|_| "{}".to_string()),
        PolicyFormat::Terraform => render_terraform(&stmts),
        PolicyFormat::CloudFormation => render_cfn(&stmts, false),
        PolicyFormat::Sam => render_cfn(&stmts, true),
        PolicyFormat::CdkTypeScript => render_cdk(&stmts),
    }
}

/// Build the structured IAM JSON `Value` with `Sid`s and least-privilege conditions.
pub fn compile_iam_value(inputs: &[PolicyInput], opts: &CompileOptions) -> Value {
    let stmts = build_statements(inputs, opts);
    render_iam_json(&stmts)
}

fn build_statements(inputs: &[PolicyInput], opts: &CompileOptions) -> Vec<CompiledStatement> {
    // Group by `(parameterized_resource, condition)` so statements stay minimal and sound.
    let mut groups: BTreeMap<(String, SynthesizedCondition), BTreeSet<String>> = BTreeMap::new();
    for item in inputs {
        let resource = parameterize_arn(&item.access.resource, opts);
        let condition = infer_condition(item, opts);
        groups
            .entry((resource, condition))
            .or_default()
            .insert(item.access.action.clone());
    }

    let mut used_sids: BTreeSet<String> = BTreeSet::new();
    groups
        .into_iter()
        .map(|((resource, condition), actions_set)| {
            let actions: Vec<String> = actions_set.into_iter().collect();
            let base_sid = make_sid(&actions, &resource, &condition);
            let mut sid = base_sid.clone();
            let mut seq = 2usize;
            while !used_sids.insert(sid.clone()) {
                sid = format!("{base_sid}{seq}");
                seq += 1;
            }
            CompiledStatement {
                sid,
                actions,
                resource,
                condition,
            }
        })
        .collect()
}

fn infer_condition(item: &PolicyInput, opts: &CompileOptions) -> SynthesizedCondition {
    if item.via.is_empty() {
        return SynthesizedCondition::None;
    }
    if item.access.action.eq_ignore_ascii_case("iam:PassRole") {
        let mut principals = BTreeSet::new();
        for caller in &item.via {
            let svc = caller
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if svc.is_empty() || svc == "iam" {
                continue;
            }
            let principal = match svc.as_str() {
                "ecs" => "ecs-tasks.amazonaws.com".to_string(),
                "monitoring" | "cloudwatch" => "cloudwatch.amazonaws.com".to_string(),
                "email" | "ses" | "sesv2" => "ses.amazonaws.com".to_string(),
                "bedrock-runtime" | "bedrock" => "bedrock.amazonaws.com".to_string(),
                other => format!("{other}.amazonaws.com"),
            };
            principals.insert(principal);
        }
        if !principals.is_empty() {
            return SynthesizedCondition::PassedToService(principals.into_iter().collect());
        }
    } else if item.implied && item.access.action.to_ascii_lowercase().starts_with("kms:") {
        let mut via_services = BTreeSet::new();
        let region_token = match opts.format {
            PolicyFormat::IamJson => opts.region.as_deref().unwrap_or("*").to_string(),
            PolicyFormat::Terraform => "${data.aws_region.current.name}".to_string(),
            PolicyFormat::CloudFormation | PolicyFormat::Sam => "${AWS::Region}".to_string(),
            PolicyFormat::CdkTypeScript => "${cdk.Stack.of(this).region}".to_string(),
        };
        for caller in &item.via {
            let svc = caller
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            if svc.is_empty() || svc == "kms" {
                continue;
            }
            let norm_svc = match svc.as_str() {
                "streams.dynamodb" | "dynamodbstreams" => "dynamodb",
                "email" | "sesv2" => "ses",
                other => other,
            };
            via_services.insert(format!("{norm_svc}.{region_token}.amazonaws.com"));
        }
        if !via_services.is_empty() {
            return SynthesizedCondition::KmsViaService(via_services.into_iter().collect());
        }
    }
    SynthesizedCondition::None
}

fn parameterize_arn(arn: &str, opts: &CompileOptions) -> String {
    let Some(rest) = arn.strip_prefix("arn:") else {
        return arn.to_string();
    };
    let mut parts = rest.splitn(5, ':');
    let (Some(part), Some(svc), Some(reg), Some(acc), Some(res)) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return arn.to_string();
    };
    // Global services (`s3`, `iam`, `sts`, `route53`, `cloudfront`) keep empty region/account where AWS requires.
    let global_no_reg = matches!(svc, "s3" | "iam" | "sts" | "cloudfront" | "route53");
    let s3_no_acc = matches!(svc, "s3" | "route53");

    match opts.format {
        PolicyFormat::IamJson => {
            let out_reg = if global_no_reg {
                reg
            } else if reg == "*" {
                opts.region.as_deref().unwrap_or("*")
            } else {
                reg
            };
            let out_acc = if s3_no_acc {
                acc
            } else if acc == "*" {
                opts.account.as_deref().unwrap_or("*")
            } else {
                acc
            };
            format!("arn:{part}:{svc}:{out_reg}:{out_acc}:{res}")
        }
        PolicyFormat::Terraform => {
            let out_part = "${data.aws_partition.current.partition}";
            let out_reg = if global_no_reg {
                ""
            } else if reg == "*" {
                "${data.aws_region.current.name}"
            } else {
                reg
            };
            let out_acc = if s3_no_acc {
                ""
            } else if acc == "*" {
                "${data.aws_caller_identity.current.account_id}"
            } else {
                acc
            };
            format!("arn:{out_part}:{svc}:{out_reg}:{out_acc}:{res}")
        }
        PolicyFormat::CloudFormation | PolicyFormat::Sam => {
            let out_part = "${AWS::Partition}";
            let out_reg = if global_no_reg {
                ""
            } else if reg == "*" {
                "${AWS::Region}"
            } else {
                reg
            };
            let out_acc = if s3_no_acc {
                ""
            } else if acc == "*" {
                "${AWS::AccountId}"
            } else {
                acc
            };
            format!("arn:{out_part}:{svc}:{out_reg}:{out_acc}:{res}")
        }
        PolicyFormat::CdkTypeScript => {
            let out_part = "${cdk.Stack.of(this).partition}";
            let out_reg = if global_no_reg {
                ""
            } else if reg == "*" {
                "${cdk.Stack.of(this).region}"
            } else {
                reg
            };
            let out_acc = if s3_no_acc {
                ""
            } else if acc == "*" {
                "${cdk.Stack.of(this).account}"
            } else {
                acc
            };
            format!("arn:{out_part}:{svc}:{out_reg}:{out_acc}:{res}")
        }
    }
}

fn make_sid(actions: &[String], resource: &str, cond: &SynthesizedCondition) -> String {
    let prefix = actions
        .first()
        .and_then(|a| a.split(':').next())
        .unwrap_or("Aws");
    let svc_pascal = pascal_case(prefix);
    let res_tail = resource
        .rsplit([':', '/'])
        .find(|seg| !seg.is_empty() && *seg != "*" && !seg.starts_with("${"))
        .map(pascal_case)
        .unwrap_or_else(|| "Access".to_string());
    let base = match cond {
        SynthesizedCondition::PassedToService(_) => format!("PassRole{res_tail}"),
        SynthesizedCondition::KmsViaService(_) => "KmsViaService".to_string(),
        SynthesizedCondition::None => format!("{svc_pascal}{res_tail}"),
    };
    let clean: String = base.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if clean.is_empty() {
        "AllowAccess".into()
    } else {
        clean
    }
}

fn pascal_case(input: &str) -> String {
    let mut out = String::new();
    for part in input.split(|c: char| !c.is_ascii_alphanumeric()) {
        if part.is_empty() {
            continue;
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            for ch in chars {
                out.push(ch);
            }
        }
    }
    out
}

fn single_or_list(items: &[String]) -> Value {
    if items.len() == 1 {
        Value::String(items[0].clone())
    } else {
        json!(items)
    }
}

fn render_iam_json(stmts: &[CompiledStatement]) -> Value {
    let list: Vec<Value> = stmts
        .iter()
        .map(|s| {
            let mut obj = serde_json::Map::new();
            obj.insert("Sid".into(), Value::String(s.sid.clone()));
            obj.insert("Effect".into(), Value::String("Allow".into()));
            obj.insert("Action".into(), json!(s.actions));
            obj.insert("Resource".into(), Value::String(s.resource.clone()));
            match &s.condition {
                SynthesizedCondition::None => {}
                SynthesizedCondition::PassedToService(svcs) => {
                    obj.insert(
                        "Condition".into(),
                        json!({
                            "StringEquals": {
                                "iam:PassedToService": single_or_list(svcs)
                            }
                        }),
                    );
                }
                SynthesizedCondition::KmsViaService(svcs) => {
                    let op = if svcs.iter().any(|v| v.contains('*')) {
                        "StringLike"
                    } else {
                        "StringEquals"
                    };
                    obj.insert(
                        "Condition".into(),
                        json!({
                            op: {
                                "kms:ViaService": single_or_list(svcs)
                            }
                        }),
                    );
                }
            }
            Value::Object(obj)
        })
        .collect();
    json!({
        "Version": "2012-10-17",
        "Statement": list,
    })
}

fn render_terraform(stmts: &[CompiledStatement]) -> String {
    let mut out = String::from(
        "data \"aws_partition\" \"current\" {}\ndata \"aws_region\" \"current\" {}\ndata \"aws_caller_identity\" \"current\" {}\n\ndata \"aws_iam_policy_document\" \"lenslet\" {\n",
    );
    for s in stmts {
        out.push_str("  statement {\n");
        out.push_str(&format!("    sid    = \"{}\"\n", s.sid));
        out.push_str("    effect = \"Allow\"\n");
        if s.actions.len() == 1 {
            out.push_str(&format!("    actions = [\"{}\"]\n", s.actions[0]));
        } else {
            out.push_str("    actions = [\n");
            for act in &s.actions {
                out.push_str(&format!("      \"{act}\",\n"));
            }
            out.push_str("    ]\n");
        }
        out.push_str(&format!("    resources = [\"{}\"]\n", s.resource));
        match &s.condition {
            SynthesizedCondition::None => {}
            SynthesizedCondition::PassedToService(svcs) => {
                out.push_str("    condition {\n      test     = \"StringEquals\"\n      variable = \"iam:PassedToService\"\n");
                out.push_str(&format!(
                    "      values   = [{}]\n    }}\n",
                    svcs.iter()
                        .map(|v| format!("\"{v}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            SynthesizedCondition::KmsViaService(svcs) => {
                out.push_str("    condition {\n      test     = \"StringEquals\"\n      variable = \"kms:ViaService\"\n");
                out.push_str(&format!(
                    "      values   = [{}]\n    }}\n",
                    svcs.iter()
                        .map(|v| format!("\"{v}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        out.push_str("  }\n");
    }
    out.push_str("}\n");
    out
}

fn render_cfn(stmts: &[CompiledStatement], sam: bool) -> String {
    let mut out = if sam {
        String::from("Policies:\n  - Version: '2012-10-17'\n    Statement:\n")
    } else {
        String::from("PolicyDocument:\n  Version: '2012-10-17'\n  Statement:\n")
    };
    let pad = if sam { "      " } else { "    " };
    for s in stmts {
        out.push_str(&format!("{pad}- Sid: {}\n", s.sid));
        out.push_str(&format!("{pad}  Effect: Allow\n"));
        if s.actions.len() == 1 {
            out.push_str(&format!("{pad}  Action: {}\n", s.actions[0]));
        } else {
            out.push_str(&format!("{pad}  Action:\n"));
            for act in &s.actions {
                out.push_str(&format!("{pad}    - {act}\n"));
            }
        }
        if s.resource.contains("${") {
            out.push_str(&format!("{pad}  Resource: !Sub '{}'\n", s.resource));
        } else {
            out.push_str(&format!("{pad}  Resource: '{}'\n", s.resource));
        }
        match &s.condition {
            SynthesizedCondition::None => {}
            SynthesizedCondition::PassedToService(svcs) => {
                out.push_str(&format!("{pad}  Condition:\n{pad}    StringEquals:\n"));
                if svcs.len() == 1 {
                    out.push_str(&format!("{pad}      iam:PassedToService: '{}'\n", svcs[0]));
                } else {
                    out.push_str(&format!("{pad}      iam:PassedToService:\n"));
                    for v in svcs {
                        out.push_str(&format!("{pad}        - '{v}'\n"));
                    }
                }
            }
            SynthesizedCondition::KmsViaService(svcs) => {
                out.push_str(&format!("{pad}  Condition:\n{pad}    StringEquals:\n"));
                if svcs.len() == 1 {
                    out.push_str(&format!("{pad}      kms:ViaService: !Sub '{}'\n", svcs[0]));
                } else {
                    out.push_str(&format!("{pad}      kms:ViaService:\n"));
                    for v in svcs {
                        out.push_str(&format!("{pad}        - !Sub '{v}'\n"));
                    }
                }
            }
        }
    }
    out
}

fn render_cdk(stmts: &[CompiledStatement]) -> String {
    let mut out = String::from(
        "import * as cdk from 'aws-cdk-lib';\nimport * as iam from 'aws-cdk-lib/aws-iam';\n\nexport const lensletPolicyStatements: iam.PolicyStatement[] = [\n",
    );
    for s in stmts {
        out.push_str("  new iam.PolicyStatement({\n");
        out.push_str(&format!("    sid: '{}',\n", s.sid));
        out.push_str("    effect: iam.Effect.ALLOW,\n");
        out.push_str(&format!(
            "    actions: [{}],\n",
            s.actions
                .iter()
                .map(|a| format!("'{a}'"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let res_lit = if s.resource.contains("${") {
            format!("`{}`", s.resource)
        } else {
            format!("'{}'", s.resource)
        };
        out.push_str(&format!("    resources: [{res_lit}],\n"));
        match &s.condition {
            SynthesizedCondition::None => {}
            SynthesizedCondition::PassedToService(svcs) => {
                out.push_str(&format!(
                    "    conditions: {{ StringEquals: {{ 'iam:PassedToService': [{}] }} }},\n",
                    svcs.iter()
                        .map(|v| format!("'{v}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            SynthesizedCondition::KmsViaService(svcs) => {
                out.push_str(&format!(
                    "    conditions: {{ StringEquals: {{ 'kms:ViaService': [{}] }} }},\n",
                    svcs.iter()
                        .map(|v| format!("`{v}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        }
        out.push_str("  }),\n");
    }
    out.push_str("];\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesizes_least_privilege_conditions_and_compiles_all_iac_formats() {
        let inputs = vec![
            PolicyInput {
                access: Access::new("dynamodb:GetItem", "arn:aws:dynamodb:*:*:table/orders"),
                implied: false,
                via: vec!["DYNAMODB GetItem".into()],
            },
            PolicyInput {
                access: Access::new("iam:PassRole", "arn:aws:iam::*:role/shop-lambda"),
                implied: true,
                via: vec!["LAMBDA CreateFunction".into()],
            },
            PolicyInput {
                access: Access::new("kms:Decrypt", "arn:aws:kms:*:*:key/*"),
                implied: true,
                via: vec!["S3 GetObject".into()],
            },
        ];

        let iam_json = compile(
            &inputs,
            &CompileOptions {
                format: PolicyFormat::IamJson,
                region: Some("us-east-1".into()),
                account: Some("123456789012".into()),
            },
        );
        assert!(iam_json.contains("arn:aws:dynamodb:us-east-1:123456789012:table/orders"));
        assert!(iam_json.contains("\"iam:PassedToService\": \"lambda.amazonaws.com\""));
        assert!(iam_json.contains("\"kms:ViaService\": \"s3.us-east-1.amazonaws.com\""));

        let tf = compile(
            &inputs,
            &CompileOptions {
                format: PolicyFormat::Terraform,
                ..CompileOptions::default()
            },
        );
        assert!(tf.contains("data \"aws_iam_policy_document\" \"lenslet\""));
        assert!(tf.contains("${data.aws_caller_identity.current.account_id}:table/orders"));
        assert!(tf.contains("variable = \"iam:PassedToService\""));

        let cfn = compile(
            &inputs,
            &CompileOptions {
                format: PolicyFormat::CloudFormation,
                ..CompileOptions::default()
            },
        );
        assert!(cfn.contains(
            "!Sub 'arn:${AWS::Partition}:dynamodb:${AWS::Region}:${AWS::AccountId}:table/orders'"
        ));

        let cdk = compile(
            &inputs,
            &CompileOptions {
                format: PolicyFormat::CdkTypeScript,
                ..CompileOptions::default()
            },
        );
        assert!(cdk.contains("new iam.PolicyStatement"));
    }
}
