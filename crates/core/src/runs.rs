//! Run files in `~/.lenslet/runs`: one `YYYYMMDD-HHMMSS-PID.jsonl` per session,
//! newest last by name. The first line says what the run is, the last line
//! (once it ends) what it did. Only the last [`KEEP_RUNS`] finished runs are
//! kept; a run that is still going is never deleted.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::Utc;

use crate::{
    summary::Report,
    trace::{Record, RunHeader, Trace},
};

pub const KEEP_RUNS: usize = 20;
/// The `end` report is one line; this bounds how far back we look for it.
const MAX_LAST_LINE: u64 = 16 << 20;

/// Set Unix permission bits (no-op off Unix).
#[cfg(unix)]
pub fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("set permissions on {}", path.display()))
}

#[cfg(not(unix))]
pub fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

pub fn dir(home: &Path) -> PathBuf {
    home.join(".lenslet/runs")
}

/// Prepare the runs directory, drop old runs, and name the new run file.
pub fn create(dir: &Path) -> Result<PathBuf> {
    if !dir.exists() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        set_mode(dir, 0o700)?;
    }
    prune(dir, KEEP_RUNS.saturating_sub(1));
    let stamp = Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let pid = std::process::id();
    Ok(unique_run_path(dir, &stamp, pid))
}

fn unique_run_path(dir: &Path, stamp: &str, pid: u32) -> PathBuf {
    let base = dir.join(format!("{stamp}-{pid}.jsonl"));
    if !base.exists() {
        return base;
    }
    // `_` (0x5f) sorts after `.` (0x2e), preserving chronological order when
    // two runs share a second and PID (e.g. PID 1 in containers, or `init`).
    for seq in 2u32.. {
        let candidate = dir.join(format!("{stamp}-{pid}_{seq:02}.jsonl"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// `…/20261115-054628-15569.jsonl` → `20261115-054628-15569`.
pub fn run_id(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Run files, oldest first.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut runs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    runs.sort();
    runs
}

/// Resolve a run specifier (path, `latest`/`prev` alias, exact run ID, or unique ID prefix) against `dir`.
pub fn resolve(dir: &Path, spec: &str) -> Result<PathBuf> {
    let candidate = Path::new(spec);
    if candidate.is_file() {
        return Ok(candidate.to_path_buf());
    }
    let all = list(dir);
    if spec.eq_ignore_ascii_case("latest") {
        if let Some(finished_latest) = all.iter().rev().find(|p| report(p).is_some()) {
            return Ok(finished_latest.clone());
        }
        if let Some(any_latest) = all.last() {
            return Ok(any_latest.clone());
        }
        anyhow::bail!("no runs yet; try `lenslet run -- <command>`");
    }
    if spec.eq_ignore_ascii_case("prev") || spec.eq_ignore_ascii_case("previous") {
        let finished: Vec<&PathBuf> = all.iter().filter(|p| report(p).is_some()).collect();
        if finished.len() < 2 {
            anyhow::bail!("need at least 2 finished runs for `{spec}`; try `lenslet runs`");
        }
        return Ok(finished[finished.len() - 2].clone());
    }
    if let Some(exact) = all.iter().find(|p| run_id(p) == spec) {
        return Ok(exact.clone());
    }
    let matches: Vec<PathBuf> = all
        .into_iter()
        .filter(|p| {
            let id = run_id(p);
            id.starts_with(spec) || id.contains(spec)
        })
        .collect();
    match matches.as_slice() {
        [single] => Ok(single.clone()),
        [] => anyhow::bail!("no run matching `{spec}`; try `lenslet runs`"),
        many => {
            let names: Vec<String> = many.iter().rev().take(5).map(|p| run_id(p)).collect();
            anyhow::bail!(
                "ambiguous run `{spec}` (matches {}); specify more characters",
                names.join(", ")
            )
        }
    }
}

fn prune(dir: &Path, keep: usize) {
    let runs = list(dir);
    let excess = runs.len().saturating_sub(keep);
    let finished = runs.into_iter().filter(|run| !is_active(run));
    for run in finished.take(excess) {
        let _ = fs::remove_file(run);
    }
}

pub fn header(path: &Path) -> Option<RunHeader> {
    let mut line = String::new();
    BufReader::new(File::open(path).ok()?)
        .read_line(&mut line)
        .ok()?;
    match serde_json::from_str(&line).ok()? {
        Record::Run(header) => Some(header),
        _ => None,
    }
}

/// A run's finished calls in arrival order: the last line of each call id
/// (a final line replaces its `pending` one). Calls still pending when the
/// run ended are left out.
pub fn calls(path: &Path) -> Result<Vec<Trace>> {
    let file = File::open(path).with_context(|| format!("read {}", path.display()))?;
    let mut order: Vec<String> = Vec::new();
    let mut latest: HashMap<String, Trace> = HashMap::new();
    for line in BufReader::new(file).lines() {
        let line = line.with_context(|| format!("read {}", path.display()))?;
        let Ok(Record::Call(call)) = serde_json::from_str::<Record>(&line) else {
            continue;
        };
        if !latest.contains_key(&call.id) {
            order.push(call.id.clone());
        }
        latest.insert(call.id.clone(), *call);
    }
    Ok(order
        .into_iter()
        .filter_map(|id| latest.remove(&id))
        .filter(|call| !call.pending)
        .collect())
}

/// The run's `end` report, when it has ended.
pub fn report(path: &Path) -> Option<Report> {
    match serde_json::from_str(&last_line(path)?).ok()? {
        Record::End(report) => Some(*report),
        _ => None,
    }
}

fn last_line(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut start = len;
    let mut tail: Vec<u8> = Vec::new();
    // Read backwards until the tail holds a whole last line.
    while start > 0 && len - start < MAX_LAST_LINE {
        let block = (64 * 1024u64).min(start);
        start -= block;
        let mut chunk = vec![0u8; block as usize];
        file.seek(SeekFrom::Start(start)).ok()?;
        file.read_exact(&mut chunk).ok()?;
        chunk.extend_from_slice(&tail);
        tail = chunk;
        let body = tail.strip_suffix(b"\n").unwrap_or(&tail);
        if body.contains(&b'\n') {
            break;
        }
    }
    let body = tail.strip_suffix(b"\n").unwrap_or(&tail);
    let line = match body.iter().rposition(|byte| *byte == b'\n') {
        Some(ix) => &body[ix + 1..],
        None if start == 0 => body,
        None => return None,
    };
    Some(String::from_utf8_lossy(line).into_owned())
}

/// Not ended, and its process is alive.
pub fn is_active(path: &Path) -> bool {
    report(path).is_none() && header(path).is_some_and(|header| process_alive(header.pid))
}

/// The most recent finished run of the same command in the same directory,
/// before `current`: what "did this get worse?" compares with.
pub fn previous(dir: &Path, current: &Path, command: &[String], cwd: &str) -> Option<Report> {
    list(dir)
        .into_iter()
        .rev()
        .filter(|run| run.as_path() < current)
        .filter(|run| {
            header(run).is_some_and(|header| header.command == command && header.cwd == cwd)
        })
        .find_map(|run| report(&run))
}

#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    if pid == 0 {
        return false;
    }
    let Ok(target) = fs::metadata(format!("/proc/{pid}")) else {
        return false;
    };
    let Ok(me) = fs::metadata("/proc/self") else {
        return true;
    };
    target.uid() == me.uid() || me.uid() == 0
}

#[cfg(all(unix, not(target_os = "linux")))]
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

pub fn now() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_run(dir: &Path, name: &str, pid: u32, command: &[&str], end: Option<i32>) -> PathBuf {
        let path = dir.join(format!("{name}.jsonl"));
        let header = serde_json::json!({
            "type": "run", "version": 3, "run": name, "command": command, "cwd": "/w",
            "pid": pid, "endpoint": "http://127.0.0.1:1", "upstream": "u", "started": "s",
        });
        let mut text = format!("{header}\n");
        text.push_str(r#"{"type":"call","id":"a","at":"t","method":"GET","path":"/","host":"h","service":"s3","operation":"GET","status":200,"ms":1,"authorization":false,"request":{"headers":[]},"response":{"headers":[]}}"#);
        text.push('\n');
        if let Some(code) = end {
            let report = Report {
                run: name.into(),
                exit_code: Some(code),
                ..Report::default()
            };
            let mut value = serde_json::to_value(&report).unwrap();
            value["type"] = "end".into();
            text.push_str(&format!("{value}\n"));
        }
        fs::write(&path, text).unwrap();
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lenslet-runs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn keeps_a_bounded_set_of_finished_runs_and_every_live_one() {
        let dir = temp_dir("prune");
        for index in 0..25 {
            write_run(
                &dir,
                &format!("20261115-0000{index:02}-1"),
                0,
                &["t"],
                Some(0),
            );
        }
        // The oldest session is a proxy that is still running (this process).
        let live = write_run(&dir, "20261114-000000-1", std::process::id(), &[], None);
        assert!(is_active(&live));
        let next = create(&dir).unwrap();
        assert!(next.extension().is_some_and(|ext| ext == "jsonl"));
        let left: Vec<String> = list(&dir).iter().map(|path| run_id(path)).collect();
        assert!(live.exists(), "a running session is never pruned");
        assert_eq!(left[0], "20261114-000000-1");
        assert_eq!(left.len(), KEEP_RUNS - 1, "room is left for the new run");
        assert!(
            left[1].starts_with("20261115-000007"),
            "oldest finished runs go first: {left:?}"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_the_header_the_report_and_the_previous_run() {
        let dir = temp_dir("read");
        let one = write_run(&dir, "20261115-000001-1", 0, &["npm", "test"], Some(1));
        write_run(&dir, "20261115-000002-1", 0, &["pytest"], Some(0));
        let three = write_run(&dir, "20261115-000003-1", 0, &["npm", "test"], None);
        assert_eq!(header(&one).unwrap().command, ["npm", "test"]);
        assert_eq!(report(&one).unwrap().exit_code, Some(1));
        assert!(report(&three).is_none(), "still running");
        let command = vec!["npm".to_string(), "test".to_string()];
        assert_eq!(
            previous(&dir, &three, &command, "/w").unwrap().run,
            "20261115-000001-1"
        );
        assert!(previous(&dir, &three, &command, "/elsewhere").is_none());
        assert!(previous(&dir, &one, &command, "/w").is_none());

        // A long last line is still found.
        let big = dir.join("big.jsonl");
        fs::write(&big, format!("first\n{}\n", "x".repeat(300_000))).unwrap();
        assert_eq!(last_line(&big).unwrap().len(), 300_000);
        fs::write(&big, "only line\n").unwrap();
        assert_eq!(last_line(&big).unwrap(), "only line");

        // Same-second PID-1 runs never overwrite or sort out of order.
        let first = unique_run_path(&dir, "20261118-120000", 1);
        fs::write(&first, "").unwrap();
        let second = unique_run_path(&dir, "20261118-120000", 1);
        fs::write(&second, "").unwrap();
        assert_ne!(first, second);
        assert!(first < second, "{first:?} should sort before {second:?}");

        fs::remove_dir_all(&dir).unwrap();
    }
}
