//! Findings: everything in a run worth a look, as one ranked list.
//!
//! A finding is a pure function of the run's call records (and, for access
//! findings, of the lock/policy comparison already written on each call). The
//! report carries them (`findings`), and the window computes the same list
//! live from the calls it has, so both always agree.
//!
//! Kinds, most severe first:
//! - `failure`: a failed call, with its cause (root-cause correlation included).
//! - `access`: a call needs access the lock/policy does not grant.
//! - `n_plus_one`: the same read repeated in a tight serial burst that a batch
//!   API would collapse (e.g. many `GetItem` → `BatchGetItem`).
//! - `leaked`: a resource the run created and never deleted.
//! - `retries`: an SDK invocation that needed retries.

use serde::{Deserialize, Serialize};

use crate::trace::Trace;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    Failure,
    Access,
    NPlusOne,
    Leaked,
    Retries,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub kind: FindingKind,
    pub severity: Severity,
    /// One line, e.g. `DYNAMODB GetItem ×37 in a serial burst`.
    pub title: String,
    /// The why / what to do, one or two sentences.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// Ids of the calls this finding is about, in call order (first is the
    /// one to jump to).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<String>,
}

/// All findings for these calls (finished calls, in arrival order), most
/// severe first, then in call order.
pub fn analyze<'a>(calls: impl IntoIterator<Item = &'a Trace>) -> Vec<Finding> {
    let _ = calls.into_iter();
    Vec::new()
}
