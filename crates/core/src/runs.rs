//! Run files in `~/.lenslet/runs`: one `YYYYMMDD-HHMMSS-PID.jsonl` per session,
//! newest last by name. The first line says what the run is, the last line
//! (once it ends) what it did. Only the last [`KEEP_RUNS`] finished runs are
//! kept; a run that is still going is never deleted.

use std::{
    collections::{BTreeSet, HashMap},
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

/// Companion stdout/stderr log path (`<run>.log`) for a run file (`<run>.jsonl`).
pub fn log_path_for(trace_path: &Path) -> PathBuf {
    trace_path.with_extension("log")
}

/// Content-addressed sidecar blob directory (`<trace_dir>/blobs`) holding exact
/// raw wire bodies for binary, compressed, or multi-megabyte payloads.
/// Accepts either a run `.jsonl` file path or the runs directory itself.
pub fn blobs_dir(trace_or_dir: &Path) -> PathBuf {
    if trace_or_dir.extension().is_none() && trace_or_dir.is_dir() {
        return trace_or_dir.join("blobs");
    }
    trace_or_dir
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join("blobs")
}

fn valid_sha256_hex(sha256: &str) -> bool {
    sha256.len() == 64 && sha256.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Persist `bytes` into `<blobs_dir>/<sha256>.bin` (owner-only `0600`) when not
/// already present, returning the relative blob filename `<sha256>.bin`.
pub fn write_blob(blobs_dir: &Path, sha256: &str, bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || !valid_sha256_hex(sha256) {
        return None;
    }
    let name = format!("{}.bin", sha256.to_ascii_lowercase());
    let dest = blobs_dir.join(&name);
    if dest.is_file() {
        return Some(name);
    }
    if !blobs_dir.exists() {
        fs::create_dir_all(blobs_dir).ok()?;
        let _ = set_mode(blobs_dir, 0o700);
    }
    let tmp = blobs_dir.join(format!(
        ".{}.{}.{}.tmp",
        name,
        std::process::id(),
        Utc::now().timestamp_subsec_nanos()
    ));
    fs::write(&tmp, bytes).ok()?;
    let _ = set_mode(&tmp, 0o600);
    if fs::rename(&tmp, &dest).is_err() && !dest.is_file() {
        let _ = fs::remove_file(&tmp);
        return None;
    }
    let _ = fs::remove_file(&tmp);
    Some(name)
}

/// Read exact wire bytes from `<trace_dir>/blobs/<blob_name>`, guarding strictly
/// against path traversal.
pub fn read_blob(trace_path: &Path, blob_name: &str) -> Option<Vec<u8>> {
    let stem = blob_name.strip_suffix(".bin")?;
    if !valid_sha256_hex(stem) {
        return None;
    }
    fs::read(blobs_dir(trace_path).join(blob_name)).ok()
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

pub fn pinned_file(dir: &Path) -> PathBuf {
    dir.join("pinned.json")
}

pub fn read_pinned(dir: &Path) -> BTreeSet<String> {
    let Ok(text) = fs::read_to_string(pinned_file(dir)) else {
        return BTreeSet::new();
    };
    serde_json::from_str::<BTreeSet<String>>(&text).unwrap_or_default()
}

pub fn set_pinned(dir: &Path, id: &str, pin: bool) -> Result<bool> {
    if !dir.exists() {
        fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        set_mode(dir, 0o700)?;
    }
    let mut set = read_pinned(dir);
    let changed = if pin {
        set.insert(id.to_string())
    } else {
        set.remove(id)
    };
    if changed {
        let dest = pinned_file(dir);
        let tmp = dir.join(format!(".pinned.{}.tmp", std::process::id()));
        let body = serde_json::to_string_pretty(&set)?;
        fs::write(&tmp, body).with_context(|| format!("write {}", tmp.display()))?;
        set_mode(&tmp, 0o600)?;
        fs::rename(&tmp, &dest).with_context(|| format!("rename {}", dest.display()))?;
    }
    Ok(changed)
}

fn prune(dir: &Path, keep: usize) {
    let runs = list(dir);
    let excess = runs.len().saturating_sub(keep);
    if excess == 0 {
        return;
    }
    let pinned = read_pinned(dir);
    // Protect active and pinned runs unconditionally, plus up to 2 newest
    // finished runs per distinct workspace `cwd` when multiple projects share
    // `~/.lenslet/runs` so one noisy test loop never wipes another repo's history.
    let mut per_cwd_seen: HashMap<String, usize> = HashMap::new();
    let mut distinct_cwds: BTreeSet<String> = BTreeSet::new();
    let mut run_cwds: HashMap<PathBuf, String> = HashMap::new();
    for path in &runs {
        if let Some(h) = header(path)
            && !h.cwd.is_empty()
        {
            distinct_cwds.insert(h.cwd.clone());
            run_cwds.insert(path.clone(), h.cwd);
        }
    }
    let mut protected: BTreeSet<PathBuf> = BTreeSet::new();
    if distinct_cwds.len() > 1 {
        for path in runs.iter().rev() {
            if let Some(cwd) = run_cwds.get(path) {
                let count = per_cwd_seen.entry(cwd.clone()).or_default();
                if *count < 2 {
                    *count += 1;
                    protected.insert(path.clone());
                }
            }
        }
    }
    let candidates: Vec<PathBuf> = runs
        .iter()
        .filter(|run| {
            !is_active(run) && !pinned.contains(&run_id(run)) && !protected.contains(*run)
        })
        .take(excess)
        .cloned()
        .collect();
    for run in &candidates {
        let _ = fs::remove_file(log_path_for(run));
        let _ = fs::remove_file(run);
    }
    if !candidates.is_empty() {
        prune_orphan_blobs(dir);
    }
}

pub fn prune_orphan_blobs(dir: &Path) {
    let bdir = dir.join("blobs");
    let Ok(entries) = fs::read_dir(&bdir) else {
        return;
    };
    let blob_files: Vec<(String, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(name, _)| name.ends_with(".bin"))
        .collect();
    if blob_files.is_empty() {
        return;
    }
    let mut referenced = BTreeSet::new();
    for run in list(dir) {
        let Ok(file) = File::open(&run) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            for (name, _) in &blob_files {
                if !referenced.contains(name) && line.contains(name.as_str()) {
                    referenced.insert(name.clone());
                }
            }
        }
    }
    for (name, path) in blob_files {
        if !referenced.contains(&name) {
            let _ = fs::remove_file(path);
        }
    }
}

pub fn header(path: &Path) -> Option<RunHeader> {
    let mut line = String::new();
    BufReader::new(File::open(path).ok()?)
        .read_line(&mut line)
        .ok()?;
    match serde_json::from_str(&line).ok()? {
        Record::Run(header) => Some(*header),
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

    #[test]
    fn pinned_runs_and_other_workspaces_survive_pruning() {
        let dir = temp_dir("pinned");
        let pinned_path = write_run(&dir, "20261110-000000-1", 0, &["baseline"], Some(0));
        assert!(set_pinned(&dir, "20261110-000000-1", true).unwrap());
        assert!(read_pinned(&dir).contains("20261110-000000-1"));

        // Write one finished run in `/repo-a`, then 25 finished runs in `/w`.
        let other_repo = dir.join("20261111-000000-1.jsonl");
        fs::write(
            &other_repo,
            format!(
                "{}\n{}\n",
                serde_json::json!({
                    "type": "run", "version": 3, "run": "20261111-000000-1",
                    "command": ["cargo", "test"], "cwd": "/repo-a", "pid": 0,
                    "endpoint": "http://127.0.0.1:1", "upstream": "u", "started": "s",
                }),
                serde_json::json!({
                    "type": "end", "version": 3, "run": "20261111-000000-1", "exit_code": 0,
                    "ended": "e", "duration_ms": 1, "calls": 0, "errors": 0, "unfinished": 0,
                    "services": {}, "failures": [], "access": [], "lock": null,
                    "access_added": [], "access_unused": [], "regressions": null,
                    "trace": "", "endpoint": "", "upstream": ""
                }),
            ),
        )
        .unwrap();
        for index in 0..25 {
            write_run(
                &dir,
                &format!("20261115-0000{index:02}-1"),
                0,
                &["t"],
                Some(0),
            );
        }
        let _ = create(&dir).unwrap();
        assert!(pinned_path.exists(), "pinned run must never be pruned");
        assert!(
            other_repo.exists(),
            "secondary workspace run survives noisy loop"
        );
        assert!(set_pinned(&dir, "20261110-000000-1", false).unwrap());
        assert!(!read_pinned(&dir).contains("20261110-000000-1"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sidecar_blobs_roundtrip_and_orphan_blobs_are_pruned() {
        let dir = temp_dir("blobs");
        let bdir = blobs_dir(&dir);
        let kept_hash = "a".repeat(64);
        let orphan_hash = "b".repeat(64);
        let payload = b"lossless binary s3 object bytes \x00\xff\x7f";
        write_blob(&bdir, &kept_hash, payload).unwrap();
        write_blob(&bdir, &orphan_hash, b"orphan").unwrap();

        let run_path = dir.join("20261119-000000-1.jsonl");
        fs::write(
            &run_path,
            format!(
                "{}\n{}\n{}\n",
                serde_json::json!({
                    "type": "run", "version": 3, "run": "20261119-000000-1",
                    "command": ["pytest"], "cwd": "/w", "pid": 1,
                    "endpoint": "http://127.0.0.1:1", "upstream": "u", "started": "s"
                }),
                serde_json::json!({
                    "type": "call", "id": "c1", "at": "2026-11-19T00:00:00Z",
                    "method": "PUT", "path": "/b/k", "status": 200, "ms": 2,
                    "service": "s3", "operation": "PutObject",
                    "request": { "body": { "bytes": payload.len(), "binary": true, "blob": format!("{kept_hash}.bin") } }
                }),
                serde_json::json!({
                    "type": "end", "version": 3, "run": "20261119-000000-1", "exit_code": 0,
                    "ended": "e", "duration_ms": 2, "calls": 1, "errors": 0, "unfinished": 0,
                    "services": {}, "failures": [], "access": [], "lock": null,
                    "access_added": [], "access_unused": [], "regressions": null,
                    "trace": "", "endpoint": "", "upstream": ""
                }),
            ),
        )
        .unwrap();

        assert_eq!(
            read_blob(&run_path, &format!("{kept_hash}.bin")).unwrap(),
            payload
        );
        prune_orphan_blobs(&dir);
        assert!(bdir.join(format!("{kept_hash}.bin")).exists());
        assert!(!bdir.join(format!("{orphan_hash}.bin")).exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
