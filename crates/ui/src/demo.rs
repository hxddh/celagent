//! Demo data for screenshots and `LENSLET_DEMO=1`: a real run of a small shop
//! script against a local emulator, recorded by `lenslet run --check` (paths
//! scrubbed). It is only used when demo mode is explicitly requested.

use crate::model::{self as trace, RunInfo, Trace};

const RUN: &str = include_str!("demo.jsonl");

pub fn demo_rows() -> Vec<Trace> {
    trace::parse_lines(RUN)
}

pub fn demo_run() -> RunInfo {
    trace::parse_run(RUN)
}
