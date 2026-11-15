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

/// A lock file: the access it lists, and which of it is test-only.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lock {
    pub access: BTreeSet<Access>,
    /// Lines marked `# test`: needed by the tests themselves (fixtures that
    /// create tables or buckets), kept for `--check`, left out of `policy`.
    pub test: BTreeSet<Access>,
}

/// The lock, or `None` when there is no lock file.
pub fn read(path: &Path) -> Result<Option<Lock>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    Ok(Some(parse(&text)))
}

pub fn parse(text: &str) -> Lock {
    let mut lock = Lock::default();
    for line in text.lines() {
        let (entry, comment) = line.split_once('#').unwrap_or((line, ""));
        let mut fields = entry.split_whitespace();
        let Some(action) = fields.next() else {
            continue;
        };
        let access = Access {
            action: action.to_string(),
            resource: fields.next().unwrap_or("*").to_string(),
        };
        if comment
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word == "test")
        {
            lock.test.insert(access.clone());
        }
        lock.access.insert(access);
    }
    lock
}

/// The lock file text. `test` marks carry over from the previous lock.
pub fn render(access: &BTreeMap<Access, AccessUse>, test: &BTreeSet<Access>) -> String {
    let width = access
        .keys()
        .map(|access| access.action.len())
        .max()
        .unwrap_or(0);
    let mut out = String::from(
        "# AWS access observed by `lenslet run`: action, resource.\n\
         # Regenerate with `lenslet run --update -- <tests>`; review changes like code.\n\
         # Mark a line `# test` when only the tests need it; `lenslet policy` leaves it out.\n",
    );
    for (access, used) in access {
        out.push_str(&format!("{:<width$}  {}", access.action, access.resource));
        let mut notes = Vec::new();
        if test.contains(access) {
            notes.push("test");
        }
        if used.implied {
            notes.push("implied");
        }
        if !notes.is_empty() {
            out.push_str(&format!("  # {}", notes.join(", ")));
        }
        out.push('\n');
    }
    out
}

pub fn write(
    path: &Path,
    access: &BTreeMap<Access, AccessUse>,
    test: &BTreeSet<Access>,
) -> Result<()> {
    fs::write(path, render(access, test)).with_context(|| format!("write {}", path.display()))
}

/// An IAM policy document granting the locked access, without `test` lines.
pub fn policy(lock: &Lock) -> serde_json::Value {
    // One statement per resource, actions sorted: compact and reviewable.
    let mut by_resource: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for entry in lock.access.difference(&lock.test) {
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
            access("dynamodb:CreateTable", "arn:aws:dynamodb:*:*:table/orders"),
            AccessUse::default(),
        );
        used.insert(
            access("kms:Decrypt", "arn:aws:kms:*:*:key/*"),
            AccessUse {
                implied: true,
                ..AccessUse::default()
            },
        );
        let test: BTreeSet<Access> = [access(
            "dynamodb:CreateTable",
            "arn:aws:dynamodb:*:*:table/orders",
        )]
        .into();
        let text = render(&used, &test);
        assert!(
            text.contains("kms:Decrypt           arn:aws:kms:*:*:key/*  # implied\n"),
            "{text}"
        );
        assert!(
            text.contains("dynamodb:CreateTable  arn:aws:dynamodb:*:*:table/orders  # test\n"),
            "{text}"
        );
        let parsed = parse(&text);
        assert_eq!(parsed.access, used.keys().cloned().collect());
        assert_eq!(parsed.test, test, "test marks survive a rewrite");
        assert_eq!(
            parse("s3:ListAllMyBuckets\n# note\n\n").access,
            [access("s3:ListAllMyBuckets", "*")].into()
        );
        assert!(
            parse("s3:GetObject arn  # tested elsewhere\n")
                .test
                .is_empty(),
            "whole word only"
        );
    }

    #[test]
    fn missing_lock_is_none_and_policy_leaves_out_test_lines() {
        let dir = std::env::temp_dir().join(format!("lenslet-lock-{}", std::process::id()));
        assert!(read(&path_in(&dir)).unwrap().is_none());

        let lock = parse(
            "sqs:SendMessage  arn:aws:sqs:*:*:jobs\n\
             sqs:ReceiveMessage  arn:aws:sqs:*:*:jobs\n\
             s3:GetObject  arn:aws:s3:::assets/*\n\
             s3:CreateBucket  arn:aws:s3:::assets  # test\n",
        );
        let policy = policy(&lock);
        let statements = policy["Statement"].as_array().unwrap();
        assert_eq!(statements.len(), 2, "{policy}");
        assert_eq!(statements[0]["Resource"], "arn:aws:s3:::assets/*");
        assert_eq!(statements[1]["Resource"], "arn:aws:sqs:*:*:jobs");
        assert_eq!(
            statements[1]["Action"],
            serde_json::json!(["sqs:ReceiveMessage", "sqs:SendMessage"])
        );
    }
}
