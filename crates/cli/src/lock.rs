//! `lenslet.lock`: the AWS access a project's runs are known to need.
//!
//! One permission per line, `action  resource`, sorted, so a pull request
//! that widens access shows up as an added line in review. Comments start
//! with `#`. The file is only ever written by `lenslet run --update`.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::{access::Access, summary::AccessUse};

pub const FILE_NAME: &str = "lenslet.lock";

/// `lenslet.lock` in `dir`.
pub fn path_in(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// The locked access, or `None` when there is no lock file.
pub fn read(path: &Path) -> Result<Option<BTreeSet<Access>>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    Ok(Some(parse(&text)))
}

pub fn parse(text: &str) -> BTreeSet<Access> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let action = fields.next()?;
            let resource = fields.next().unwrap_or("*");
            Some(Access {
                action: action.to_string(),
                resource: resource.to_string(),
            })
        })
        .collect()
}

pub fn render(access: &BTreeMap<Access, AccessUse>) -> String {
    let width = access
        .keys()
        .map(|access| access.action.len())
        .max()
        .unwrap_or(0);
    let mut out = String::from(
        "# AWS access observed by `lenslet run`: action, resource.\n\
         # Regenerate with `lenslet run --update -- <tests>`; review changes like code.\n",
    );
    for (access, used) in access {
        out.push_str(&format!("{:<width$}  {}", access.action, access.resource));
        if used.implied {
            out.push_str("  # implied");
        }
        out.push('\n');
    }
    out
}

pub fn write(path: &Path, access: &BTreeMap<Access, AccessUse>) -> Result<()> {
    fs::write(path, render(access)).with_context(|| format!("write {}", path.display()))
}

/// An IAM policy document granting exactly the locked access.
pub fn policy(access: &BTreeSet<Access>) -> serde_json::Value {
    // One statement per resource, actions sorted: compact and reviewable.
    let mut by_resource: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for entry in access {
        by_resource
            .entry(entry.resource.as_str())
            .or_default()
            .push(entry.action.as_str());
    }
    let statements: Vec<serde_json::Value> = by_resource
        .into_iter()
        .map(|(resource, actions)| {
            serde_json::json!({
                "Effect": "Allow",
                "Action": actions,
                "Resource": resource,
            })
        })
        .collect();
    serde_json::json!({ "Version": "2012-10-17", "Statement": statements })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(action: &str, resource: &str) -> Access {
        Access {
            action: action.into(),
            resource: resource.into(),
        }
    }

    #[test]
    fn round_trips_through_a_reviewable_text_file() {
        let mut used = BTreeMap::new();
        used.insert(
            access("sqs:SendMessage", "arn:aws:sqs:*:*:jobs"),
            AccessUse::default(),
        );
        used.insert(
            access("kms:Decrypt", "arn:aws:kms:*:*:key/*"),
            AccessUse {
                implied: true,
                ..AccessUse::default()
            },
        );
        let text = render(&used);
        assert!(text.contains("kms:Decrypt      arn:aws:kms:*:*:key/*  # implied\n"));
        assert!(
            text.find("kms:").unwrap() < text.find("sqs:").unwrap(),
            "sorted"
        );
        let parsed = parse(&text);
        assert_eq!(parsed, used.keys().cloned().collect());
        assert_eq!(
            parse("s3:ListAllMyBuckets\n# note\n\n"),
            [access("s3:ListAllMyBuckets", "*")].into()
        );
    }

    #[test]
    fn missing_lock_is_none_and_policy_groups_by_resource() {
        let dir = std::env::temp_dir().join(format!("lenslet-lock-{}", std::process::id()));
        assert!(read(&path_in(&dir)).unwrap().is_none());

        let locked: BTreeSet<Access> = [
            access("sqs:SendMessage", "arn:aws:sqs:*:*:jobs"),
            access("sqs:ReceiveMessage", "arn:aws:sqs:*:*:jobs"),
            access("s3:GetObject", "arn:aws:s3:::assets/*"),
        ]
        .into();
        let policy = policy(&locked);
        let statements = policy["Statement"].as_array().unwrap();
        assert_eq!(statements.len(), 2);
        assert_eq!(statements[1]["Resource"], "arn:aws:sqs:*:*:jobs");
        assert_eq!(
            statements[1]["Action"],
            serde_json::json!(["sqs:ReceiveMessage", "sqs:SendMessage"])
        );
    }
}
