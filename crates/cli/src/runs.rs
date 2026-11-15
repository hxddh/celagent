//! Run files: one trace per session in `~/.lenslet/runs`, newest last.
//!
//! Every `lenslet run` and `lenslet proxy` session writes its own
//! `YYYYMMDD-HHMMSS-PID.jsonl` trace plus a `.run.json` sidecar that says what
//! the session was (command, endpoint, upstream, exit code). The window follows
//! the newest run. Only the last [`KEEP_RUNS`] runs are kept, so the directory
//! never grows without bound.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::proxy::set_mode;

pub const KEEP_RUNS: usize = 20;

pub fn dir(home: &Path) -> PathBuf {
    home.join(".lenslet/runs")
}

/// Prepare the runs directory, drop old runs, and name the new trace file.
pub fn create(dir: &Path) -> Result<PathBuf> {
    if !dir.exists() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        set_mode(dir, 0o700)?;
    }
    prune(dir, KEEP_RUNS.saturating_sub(1));
    let stamp = Utc::now().format("%Y%m%d-%H%M%S");
    Ok(dir.join(format!("{stamp}-{}.jsonl", std::process::id())))
}

/// Keep the newest `keep` traces (by name, which sorts by start time).
/// A session that is still running is never deleted, however old: a
/// long-lived `lenslet proxy` must not lose its trace to newer runs.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut traces: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    traces.sort();
    let excess = traces.len().saturating_sub(keep);
    let finished = traces.into_iter().filter(|trace| !is_active(trace));
    for trace in finished.take(excess) {
        let _ = fs::remove_file(info_path(&trace));
        let _ = fs::remove_file(trace);
    }
}

/// A session whose sidecar has not recorded an end and whose process lives.
fn is_active(trace: &Path) -> bool {
    let Some(info) = fs::read_to_string(info_path(trace))
        .ok()
        .and_then(|text| serde_json::from_str::<RunInfo>(&text).ok())
    else {
        return false;
    };
    info.ended.is_none() && process_alive(info.pid)
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // `kill -0` only checks that the process exists and is ours to signal.
    pid != 0
        && std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

#[cfg(not(unix))]
fn process_alive(pid: u32) -> bool {
    // No cheap liveness check: keep anything that has not recorded an end.
    pid != 0
}

/// What a session was, next to its trace.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunInfo {
    /// `run` or `proxy`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    pub pid: u32,
    /// What SDKs point at, e.g. `http://127.0.0.1:53211`.
    pub endpoint: String,
    pub upstream: String,
    pub started: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// `runs/X.jsonl` → `runs/X.run.json`.
pub fn info_path(trace: &Path) -> PathBuf {
    trace.with_extension("run.json")
}

/// Write the sidecar atomically (owner-only), so a reader never sees half.
pub fn write_info(trace: &Path, info: &RunInfo) -> Result<()> {
    let path = info_path(trace);
    let temporary = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(info).context("encode run info")?;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("write {}", temporary.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("write {}", temporary.display()))?;
    fs::rename(&temporary, &path).with_context(|| format!("write {}", path.display()))
}

pub fn now() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_a_bounded_set_of_runs_with_their_sidecars() {
        let dir = std::env::temp_dir().join(format!("lenslet-runs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for index in 0..25 {
            let trace = dir.join(format!("20261115-0000{index:02}-1.jsonl"));
            fs::write(&trace, "").unwrap();
            write_info(&trace, &RunInfo::default()).unwrap();
        }
        // The oldest session is a proxy that is still running (this process).
        let live = dir.join("20261114-000000-1.jsonl");
        fs::write(&live, "").unwrap();
        let live_info = RunInfo {
            kind: "proxy".into(),
            pid: std::process::id(),
            ..RunInfo::default()
        };
        write_info(&live, &live_info).unwrap();
        let next = create(&dir).unwrap();
        assert!(next.extension().is_some_and(|ext| ext == "jsonl"));
        let mut left: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let traces = left.iter().filter(|name| name.ends_with(".jsonl")).count();
        let infos = left
            .iter()
            .filter(|name| name.ends_with(".run.json"))
            .count();
        assert_eq!(traces, KEEP_RUNS - 1, "room is left for the new run");
        assert_eq!(infos, KEEP_RUNS - 1);
        assert!(live.exists(), "a running session is never pruned");
        assert_eq!(left[0], "20261114-000000-1.jsonl");
        assert!(
            left[2].starts_with("20261115-000007"),
            "oldest finished runs go first: {left:?}"
        );

        let info = RunInfo {
            kind: "run".into(),
            exit_code: Some(3),
            ..RunInfo::default()
        };
        write_info(&next, &info).unwrap();
        let read: RunInfo =
            serde_json::from_str(&fs::read_to_string(info_path(&next)).unwrap()).unwrap();
        assert_eq!(read.exit_code, Some(3));
        fs::remove_dir_all(&dir).unwrap();
    }
}
