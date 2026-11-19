//! Lenslet's fact engine, shared by the `lenslet` CLI and the window.
//!
//! Everything here is a pure function of a run's records: the run-file schema,
//! IAM access derivation, the lock and the policy gate, failure explanations,
//! findings, reports and run diffs. The proxy computes each fact once and
//! writes it down; readers parse the same types, so they always agree.

pub mod access;
pub mod findings;
pub mod lock;
pub mod problem;
pub mod runs;
pub mod summary;
pub mod trace;
pub mod wire;
