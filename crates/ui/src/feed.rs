//! Getting a run into the window, off the main thread.
//!
//! [`load`] reads the newest [`MAX_ROWS`] lines of a run file once (a single
//! backwards scan that concatenates its blocks at the end, so it is linear in
//! what it reads). [`start`] then keeps the run current on a background thread:
//! it subscribes to the proxy's event stream when the run is live and falls
//! back to an incremental file tail when the stream is not there (older CLI,
//! crashed run). Parsing happens on that thread; the window only merges.

use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use crate::{
    fsguard,
    model::{LineRef, MAX_ROWS, Record, Report, RunHeader, Trace, parse_record, parse_records_at},
    sse,
};

/// A reload reads backwards in blocks of this size until it has enough lines.
const TAIL_BLOCK_BYTES: u64 = 1 << 20;
/// Upper bound on what a reload reads, whatever the line sizes.
const MAX_TAIL_BYTES: u64 = 256 << 20;
/// Never let a partially written line grow without bound.
const MAX_CARRY_BYTES: usize = 16 << 20;
/// How often the fallback tail checks the file.
pub const TAIL_INTERVAL: Duration = Duration::from_millis(500);

/// What the window follows: one fixed file, or a run in the runs directory
/// (the newest, unless the viewer picked one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    File(PathBuf),
    Runs {
        dir: PathBuf,
        pinned: Option<PathBuf>,
    },
}

impl Source {
    /// The file to show now. Lists the runs directory: background only.
    pub fn resolve(&self) -> Option<PathBuf> {
        fsguard::assert_io_allowed();
        match self {
            Source::File(path) => Some(path.clone()),
            Source::Runs { dir, pinned } => pinned
                .clone()
                .filter(|path| path.exists())
                .or_else(|| lenslet_core::runs::list(dir).pop()),
        }
    }

    pub fn runs_dir(&self) -> Option<&Path> {
        match self {
            Source::Runs { dir, .. } => Some(dir),
            Source::File(_) => None,
        }
    }

    pub fn follows_latest(&self) -> bool {
        matches!(self, Source::Runs { pinned: None, .. })
    }
}

/// Where an incremental tail resumes.
#[derive(Clone, Debug, Default)]
pub struct TailState {
    pub offset: u64,
    pub file_key: u64,
    pub carry: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct Loaded {
    pub path: PathBuf,
    pub header: Option<RunHeader>,
    pub report: Option<Report>,
    pub calls: Vec<Trace>,
    pub skipped: usize,
    pub tail: TailState,
}

#[derive(Debug)]
pub enum LoadResult {
    /// No run file (yet).
    Missing,
    Error(String),
    Loaded(Box<Loaded>),
}

/// Resolve `source` and read its run. Blocking IO: background only.
pub fn load_source(source: &Source) -> LoadResult {
    match source.resolve() {
        Some(path) => load(&path),
        None => LoadResult::Missing,
    }
}

/// Read the newest [`MAX_ROWS`] calls of a run file, its header and report.
pub fn load(path: &Path) -> LoadResult {
    fsguard::assert_io_allowed();
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return LoadResult::Missing,
        Err(error) => return LoadResult::Error(short_io_error(&error)),
    };
    let metadata = match file.metadata() {
        Ok(metadata) if metadata.is_dir() => {
            return LoadResult::Error("trace path is a directory".into());
        }
        Ok(metadata) => metadata,
        Err(error) => return LoadResult::Error(short_io_error(&error)),
    };
    let len = metadata.len();

    // Read backwards a block at a time, keeping blocks separately and joining
    // them once: linear in the bytes read.
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut start = len;
    let mut newlines = 0usize;
    while start > 0 && len - start < MAX_TAIL_BYTES {
        let block = TAIL_BLOCK_BYTES.min(start);
        start -= block;
        let mut chunk = vec![0u8; block as usize];
        if let Err(error) = file
            .seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut chunk))
        {
            return LoadResult::Error(short_io_error(&error));
        }
        newlines += chunk.iter().filter(|byte| **byte == b'\n').count();
        blocks.push(chunk);
        if newlines > MAX_ROWS + 2 {
            break;
        }
    }
    let mut bytes = Vec::with_capacity((len - start) as usize);
    for block in blocks.iter().rev() {
        bytes.extend_from_slice(block);
    }
    drop(blocks);

    // Drop a leading partial line, keep an unterminated tail for later.
    let mut body: &[u8] = &bytes;
    let mut body_offset = start;
    if start > 0 {
        match body.iter().position(|byte| *byte == b'\n') {
            Some(ix) => {
                body = &body[ix + 1..];
                body_offset += (ix + 1) as u64;
            }
            None => body = &[],
        }
    }
    let (complete, carry) = match body.iter().rposition(|byte| *byte == b'\n') {
        Some(ix) => (&body[..=ix], body[ix + 1..].to_vec()),
        None => (&[][..], body.to_vec()),
    };
    let source = Arc::new(path.to_path_buf());
    let mut parsed = parse_records_at(complete, body_offset, Some(&source));
    if parsed.calls.len() > MAX_ROWS {
        parsed.calls.drain(..parsed.calls.len() - MAX_ROWS);
    }
    // A long run's header is far before the tail that was read.
    let header = parsed.header.or_else(|| lenslet_core::runs::header(path));
    LoadResult::Loaded(Box::new(Loaded {
        path: path.to_path_buf(),
        header,
        report: parsed.report,
        calls: parsed.calls,
        skipped: parsed.bad,
        tail: TailState {
            offset: len,
            file_key: file_key(&metadata),
            carry,
        },
    }))
}

/// What the feed thread sends the window.
#[derive(Debug)]
pub enum FeedMsg {
    Calls(Vec<Trace>),
    Header(RunHeader),
    End(Box<Report>),
    /// The file was truncated or replaced: here is all of it again.
    Reset(Box<Loaded>),
    Mode(FeedMode),
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedMode {
    /// Subscribed to the proxy's event stream.
    Push,
    /// Polling the file.
    Tail,
    /// Nothing more will come (the run ended).
    Done,
}

/// Keep `path` current from a background thread. `endpoint` is the run's
/// proxy when it may be live. Dropping the receiver stops the thread.
pub fn start(
    path: PathBuf,
    endpoint: Option<String>,
    tail: TailState,
) -> UnboundedReceiver<FeedMsg> {
    let (tx, rx) = unbounded();
    std::thread::Builder::new()
        .name("lenslet-feed".into())
        .spawn(move || run_feed(path, endpoint, tail, tx))
        .expect("spawn feed thread");
    rx
}

fn run_feed(
    path: PathBuf,
    endpoint: Option<String>,
    tail: TailState,
    tx: UnboundedSender<FeedMsg>,
) {
    let mut ended = false;
    if let Some(endpoint) = endpoint {
        let source = Arc::new(path.clone());
        let mut offset: u64 = 0;
        let mut announced = false;
        let end = sse::subscribe(
            &endpoint,
            || tx.is_closed(),
            |line| {
                if !announced {
                    announced = true;
                    let _ = tx.unbounded_send(FeedMsg::Mode(FeedMode::Push));
                }
                let at = offset;
                offset += line.len() as u64 + 1;
                match parse_record(line) {
                    Some(Record::Call(mut trace)) => {
                        // The stream mirrors the file byte for byte, so the
                        // running offset is where this line lives on disk;
                        // hydration verifies the id and rescans if not.
                        trace.dehydrate(LineRef {
                            path: source.clone(),
                            offset: at,
                            len: line.len(),
                        });
                        tx.unbounded_send(FeedMsg::Calls(vec![*trace])).is_ok()
                    }
                    Some(Record::Run(header)) => tx.unbounded_send(FeedMsg::Header(header)).is_ok(),
                    Some(Record::End(report)) => {
                        ended = true;
                        let _ = tx.unbounded_send(FeedMsg::End(report));
                        false
                    }
                    None => !tx.is_closed(),
                }
            },
        );
        if ended || tx.is_closed() {
            let _ = tx.unbounded_send(FeedMsg::Mode(FeedMode::Done));
            return;
        }
        // No stream (older CLI, finished or crashed run), or it dropped
        // before the report: the file is the source of truth from here.
        let _ = end;
    }
    tail_loop(&path, tail, &tx);
}

fn tail_loop(path: &Path, mut state: TailState, tx: &UnboundedSender<FeedMsg>) {
    let _ = tx.unbounded_send(FeedMsg::Mode(FeedMode::Tail));
    loop {
        match poll_tail(path, &mut state) {
            TailPoll::Idle => {}
            TailPoll::Records(records) => {
                let mut calls = Vec::new();
                let mut ended = false;
                for record in records {
                    match record {
                        Record::Call(trace) => calls.push(*trace),
                        Record::Run(header) => {
                            let _ = tx.unbounded_send(FeedMsg::Header(header));
                        }
                        Record::End(report) => {
                            if !calls.is_empty() {
                                let _ =
                                    tx.unbounded_send(FeedMsg::Calls(std::mem::take(&mut calls)));
                            }
                            let _ = tx.unbounded_send(FeedMsg::End(report));
                            ended = true;
                        }
                    }
                }
                if !calls.is_empty() && tx.unbounded_send(FeedMsg::Calls(calls)).is_err() {
                    return;
                }
                if ended {
                    let _ = tx.unbounded_send(FeedMsg::Mode(FeedMode::Done));
                    return;
                }
            }
            TailPoll::Replaced => match load(path) {
                LoadResult::Loaded(loaded) => {
                    state = loaded.tail.clone();
                    let done = loaded.report.is_some();
                    if tx.unbounded_send(FeedMsg::Reset(loaded)).is_err() || done {
                        return;
                    }
                }
                LoadResult::Missing => {}
                LoadResult::Error(error) => {
                    let _ = tx.unbounded_send(FeedMsg::Error(error));
                }
            },
            TailPoll::Error(error) => {
                if tx.unbounded_send(FeedMsg::Error(error)).is_err() {
                    return;
                }
            }
        }
        std::thread::sleep(TAIL_INTERVAL);
        if tx.is_closed() {
            return;
        }
    }
}

enum TailPoll {
    Idle,
    Records(Vec<Record>),
    Replaced,
    Error(String),
}

/// Read what was appended since `state`, parsing only complete lines.
fn poll_tail(path: &Path, state: &mut TailState) -> TailPoll {
    fsguard::assert_io_allowed();
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return TailPoll::Error(short_io_error(&error)),
    };
    let len = metadata.len();
    let key = file_key(&metadata);
    if len < state.offset || (state.file_key != 0 && key != 0 && state.file_key != key) {
        return TailPoll::Replaced;
    }
    if len == state.offset {
        return TailPoll::Idle;
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => return TailPoll::Error(short_io_error(&error)),
    };
    let mut bytes = Vec::with_capacity((len - state.offset) as usize);
    if let Err(error) = file
        .seek(SeekFrom::Start(state.offset))
        .and_then(|_| file.take(len - state.offset).read_to_end(&mut bytes))
    {
        return TailPoll::Error(short_io_error(&error));
    }
    let base_offset = state.offset - state.carry.len() as u64;
    state.offset += bytes.len() as u64;
    state.carry.extend_from_slice(&bytes);
    let Some(newline) = state.carry.iter().rposition(|byte| *byte == b'\n') else {
        if state.carry.len() > MAX_CARRY_BYTES {
            state.carry.clear();
        }
        return TailPoll::Idle;
    };
    let complete: Vec<u8> = state.carry.drain(..=newline).collect();
    let source = Arc::new(path.to_path_buf());
    let parsed = parse_records_at(&complete, base_offset, Some(&source));
    let mut records: Vec<Record> = Vec::new();
    if let Some(header) = parsed.header {
        records.push(Record::Run(header));
    }
    records.extend(
        parsed
            .calls
            .into_iter()
            .map(|call| Record::Call(Box::new(call))),
    );
    if let Some(report) = parsed.report {
        records.push(Record::End(Box::new(report)));
    }
    TailPoll::Records(records)
}

fn short_io_error(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "permission denied".to_string(),
        io::ErrorKind::NotFound => "trace file not found".to_string(),
        _ => error.to_string(),
    }
}

#[cfg(unix)]
fn file_key(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn file_key(_metadata: &fs::Metadata) -> u64 {
    0
}

/// A finished-or-not run in the runs directory, for the run switcher & sidebar.
#[derive(Clone, Debug)]
pub struct RunSummary {
    pub path: PathBuf,
    pub id: String,
    pub header: Option<RunHeader>,
    pub report: Option<Report>,
    pub bytes: u64,
    pub pinned: bool,
}

impl RunSummary {
    pub fn label(&self) -> String {
        match self.header.as_ref() {
            Some(header) if !header.command.is_empty() => format!("$ {}", header.command.join(" ")),
            Some(_) => "proxy session".into(),
            None => self.id.clone(),
        }
    }

    pub fn project_name(&self) -> String {
        let Some(cwd) = self
            .header
            .as_ref()
            .map(|h| h.cwd.as_str())
            .filter(|s| !s.is_empty())
        else {
            return "workspace".into();
        };
        Path::new(cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| cwd.to_string())
    }

    pub fn ended(&self) -> bool {
        self.report.is_some()
    }
}

/// Every run in `dir`, newest first, with its header, report, and pin state. Background only.
pub fn list_runs(dir: &Path) -> Vec<RunSummary> {
    fsguard::assert_io_allowed();
    let pinned = lenslet_core::runs::read_pinned(dir);
    lenslet_core::runs::list(dir)
        .into_iter()
        .rev()
        .map(|path| {
            let id = lenslet_core::runs::run_id(&path);
            let is_pinned = pinned.contains(&id);
            RunSummary {
                id,
                header: lenslet_core::runs::header(&path),
                report: lenslet_core::runs::report(&path),
                bytes: fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                pinned: is_pinned,
                path,
            }
        })
        .collect()
}

pub fn set_run_pinned(dir: &Path, id: &str, pin: bool) -> anyhow::Result<bool> {
    fsguard::assert_io_allowed();
    lenslet_core::runs::set_pinned(dir, id, pin)
}

/// Just the newest run file's path (cheap: one directory listing).
pub fn newest_run(dir: &Path) -> Option<PathBuf> {
    fsguard::assert_io_allowed();
    lenslet_core::runs::list(dir).pop()
}

/// `(file_count, total_bytes)` for the runs directory. Background only.
pub fn storage_stats(dir: &Path) -> (usize, u64) {
    fsguard::assert_io_allowed();
    let files = lenslet_core::runs::list(dir);
    let bytes = files
        .iter()
        .filter_map(|path| fs::metadata(path).ok().map(|m| m.len()))
        .sum();
    (files.len(), bytes)
}

/// Delete finished run files (never `keep`, pinned runs, or a run still going). Background only.
pub fn clear_finished_runs(dir: &Path, keep: Option<&Path>) -> usize {
    fsguard::assert_io_allowed();
    let pinned = lenslet_core::runs::read_pinned(dir);
    let mut removed = 0;
    for path in lenslet_core::runs::list(dir) {
        if keep.is_some_and(|keep| keep == path) {
            continue;
        }
        if pinned.contains(&lenslet_core::runs::run_id(&path)) {
            continue;
        }
        if lenslet_core::runs::report(&path).is_some() && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// A run's report, read from its last line. Background only.
pub fn read_report(path: &Path) -> Option<Report> {
    fsguard::assert_io_allowed();
    lenslet_core::runs::report(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdata::{header_line, synthetic_run};
    use futures::StreamExt;
    use std::io::Write;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lenslet-feed-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("run.jsonl")
    }

    fn loaded(path: &Path) -> Box<Loaded> {
        match load(path) {
            LoadResult::Loaded(loaded) => loaded,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn loads_the_tail_of_a_long_run_with_its_header_and_hydrates_bodies() {
        let path = temp("long");
        let big = "x".repeat(20_000);
        let text = synthetic_run(MAX_ROWS + 300, Some(&big), true);
        fs::write(&path, &text).unwrap();
        let loaded = loaded(&path);
        assert_eq!(loaded.calls.len(), MAX_ROWS);
        assert!(loaded.header.is_some(), "header read separately");
        assert!(loaded.report.is_some());
        let last = loaded.calls.last().unwrap();
        assert!(last.is_dehydrated());
        let full = last.hydrated().unwrap();
        assert_eq!(
            full.request.unwrap().body.unwrap().text.unwrap().len(),
            big.len()
        );
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn tails_appends_holds_partial_lines_and_detects_truncation() {
        let path = temp("tail");
        let text = synthetic_run(3, None, false);
        fs::write(&path, &text).unwrap();
        let mut state = loaded(&path).tail;
        assert!(matches!(poll_tail(&path, &mut state), TailPoll::Idle));

        let more = synthetic_run(5, None, false);
        let extra: Vec<&str> = more.lines().skip(4).collect();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        // A partial line (split inside a UTF-8 character) is held back.
        let line = extra[0].replace("\"operation\":\"", "\"operation\":\"é");
        let split = line.find('é').unwrap() + 1;
        file.write_all(&line.as_bytes()[..split]).unwrap();
        file.flush().unwrap();
        assert!(matches!(poll_tail(&path, &mut state), TailPoll::Idle));
        file.write_all(&line.as_bytes()[split..]).unwrap();
        file.write_all(b"\n").unwrap();
        file.flush().unwrap();
        match poll_tail(&path, &mut state) {
            TailPoll::Records(records) => {
                assert_eq!(records.len(), 1);
                let Record::Call(call) = &records[0] else {
                    panic!()
                };
                assert!(call.operation.starts_with('é'));
                let full = call.hydrated().unwrap();
                assert_eq!(full.id, call.id, "offset points at the line");
            }
            _ => panic!("expected the appended call"),
        }
        fs::write(&path, header_line()).unwrap();
        assert!(matches!(poll_tail(&path, &mut state), TailPoll::Replaced));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn pushes_live_records_from_the_event_stream_and_falls_back_to_the_tail() {
        // Live: everything arrives over the stream, ending with the report.
        let path = temp("push");
        let text = synthetic_run(4, None, true);
        fs::write(&path, &text).unwrap();
        let events: Vec<String> = text.lines().map(str::to_string).collect();
        let endpoint = crate::sse::tests::fake_sse_server(events, true);
        let mut rx = start(path.clone(), Some(endpoint), TailState::default());
        let msgs: Vec<FeedMsg> = futures::executor::block_on(async {
            let mut out = Vec::new();
            while let Some(msg) = rx.next().await {
                out.push(msg);
            }
            out
        });
        assert!(matches!(msgs[0], FeedMsg::Mode(FeedMode::Push)));
        let calls: Vec<&Trace> = msgs
            .iter()
            .filter_map(|m| match m {
                FeedMsg::Calls(c) => Some(c),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[2].hydrated().unwrap().id, calls[2].id);
        assert!(msgs.iter().any(|m| matches!(m, FeedMsg::End(_))));
        assert!(matches!(msgs.last(), Some(FeedMsg::Mode(FeedMode::Done))));

        // No stream (older CLI): tail the file until the report shows up.
        let path = temp("fallback");
        let partial = synthetic_run(2, None, false);
        fs::write(&path, &partial).unwrap();
        let state = loaded(&path).tail;
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", dead.local_addr().unwrap());
        drop(dead);
        let mut rx = start(path.clone(), Some(endpoint), state);
        let full = synthetic_run(2, None, true);
        fs::write(&path, &full).unwrap();
        let msgs: Vec<FeedMsg> = futures::executor::block_on(async {
            let mut out = Vec::new();
            while let Some(msg) = rx.next().await {
                out.push(msg);
            }
            out
        });
        assert!(matches!(msgs[0], FeedMsg::Mode(FeedMode::Tail)));
        assert!(msgs.iter().any(|m| matches!(m, FeedMsg::End(_))));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
