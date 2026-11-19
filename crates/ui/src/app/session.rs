//! The run being shown and everything derived from it.
//!
//! `Session` is a model entity: it owns the call store, the run header and
//! report, the lock, the baseline comparison, findings and the run list. All
//! IO happens on the background executor; results are applied with
//! `this.update`, guarded by a generation counter so a stale load (the user
//! already switched runs) is dropped. Views subscribe to [`SessionEvent`]s and
//! re-render only for what they show.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use futures::StreamExt;
use gpui::{Context, EventEmitter, Task};
use lenslet_core::{
    findings::Finding,
    lock::Lock,
    summary::{Report, RunDiff},
};

use crate::{
    demo,
    feed::{self, FeedMode, FeedMsg, LoadResult, Loaded, RunSummary, Source},
    lockfile,
    model::{Access, RunInfo, Trace},
    store::{AccessBaseline, AccessRow, CallStore, ChangeSet, Delta, access_rows},
};

/// Live findings are recomputed at most this often.
const FINDINGS_THROTTLE: Duration = Duration::from_millis(1_500);
/// How often a session that follows the newest run looks for a newer one.
const NEWEST_RUN_POLL: Duration = Duration::from_secs(2);
/// Live batches are applied at most once per frame.
const FRAME: Duration = Duration::from_millis(16);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    Loading,
    /// No run file yet.
    Missing,
    Error(String),
    Ready,
}

/// What the current run is compared with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Baseline {
    Lock,
    Previous,
    Run(PathBuf),
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// A different run (or the first one) is shown; everything was replaced.
    RunChanged,
    /// Rows were added or replaced.
    Rows(Delta),
    /// Access rows, changes, diff, lock or findings were recomputed.
    Derived,
    /// The run list was refreshed.
    Runs,
    /// Something to tell the user.
    Toast(String),
}

pub struct Session {
    pub source: Source,
    pub demo: bool,
    generation: u64,
    pub health: Health,
    pub path: Option<PathBuf>,
    pub run: Option<RunInfo>,
    pub store: CallStore,
    pub feed_mode: Option<FeedMode>,
    pub skipped: usize,
    feed_task: Option<Task<()>>,
    _watch_task: Option<Task<()>>,

    pub lock_path: Option<PathBuf>,
    pub lock: Option<Lock>,
    pub baseline: Baseline,
    /// The finished run the baseline resolves to (not for `Lock`).
    pub baseline_run: Option<(PathBuf, Report)>,
    pub diff: Option<RunDiff>,
    pub changes: ChangeSet,
    pub access_rows: Arc<Vec<AccessRow>>,
    pub findings: Arc<Vec<Finding>>,
    findings_running: bool,
    findings_dirty: bool,
    findings_at: Option<Instant>,
    pub runs: Arc<Vec<RunSummary>>,
}

impl EventEmitter<SessionEvent> for Session {}

impl Session {
    pub fn new(source: Source, demo: bool, cx: &mut Context<Self>) -> Self {
        let mut session = Session {
            source,
            demo,
            generation: 0,
            health: Health::Loading,
            path: None,
            run: None,
            store: CallStore::default(),
            feed_mode: None,
            skipped: 0,
            feed_task: None,
            _watch_task: None,
            lock_path: None,
            lock: None,
            baseline: Baseline::Lock,
            baseline_run: None,
            diff: None,
            changes: ChangeSet::default(),
            access_rows: Arc::new(Vec::new()),
            findings: Arc::new(Vec::new()),
            findings_running: false,
            findings_dirty: false,
            findings_at: None,
            runs: Arc::new(Vec::new()),
        };
        if demo {
            let run = demo::demo_run();
            let calls = demo::demo_rows();
            session.run = Some(run);
            session.store.reset(calls);
            session.health = Health::Ready;
            session.recompute(cx);
            session.schedule_findings(cx);
        } else {
            session.reload(cx);
            session._watch_task = Some(session.watch_newest(cx));
        }
        session
    }

    pub fn is_live(&self) -> bool {
        self.run.as_ref().is_some_and(|run| run.report.is_none())
            && matches!(self.feed_mode, Some(FeedMode::Push | FeedMode::Tail))
    }

    pub fn header_endpoint(&self) -> Option<&str> {
        self.run
            .as_ref()
            .map(|run| run.header.endpoint.as_str())
            .filter(|endpoint| !endpoint.is_empty())
    }

    pub fn run_start_ms(&self) -> Option<i64> {
        self.run
            .as_ref()
            .and_then(|run| crate::model::parse_rfc3339_ms(&run.header.started))
            .or_else(|| self.store.rows().first().and_then(|row| row.at_ms()))
    }

    pub fn run_span_ms(&self) -> i64 {
        if let Some(report) = self
            .run
            .as_ref()
            .and_then(|run| run.report.as_ref())
            .filter(|report| report.duration_ms > 0)
        {
            return report.duration_ms as i64;
        }
        let Some(start) = self.run_start_ms() else {
            return 1;
        };
        let end = self
            .store
            .rows()
            .iter()
            .rev()
            .take(256)
            .filter_map(|row| row.at_ms().map(|at| at + row.ms as i64))
            .max()
            .unwrap_or(start + 1);
        (end - start).max(1)
    }

    // -- Loading -----------------------------------------------------------

    /// Load the source again from scratch (off the main thread).
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if self.demo {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.feed_task = None;
        self.feed_mode = None;
        self.health = Health::Loading;
        cx.notify();
        let source = self.source.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { feed::load_source(&source) })
                .await;
            this.update(cx, |session, cx| {
                if session.generation == generation {
                    session.apply_load(result, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Show another source (a picked run, or back to following the newest).
    pub fn switch(&mut self, source: Source, cx: &mut Context<Self>) {
        if self.demo {
            return;
        }
        self.source = source;
        self.store.reset(Vec::new());
        self.run = None;
        self.path = None;
        self.diff = None;
        self.findings = Arc::new(Vec::new());
        self.access_rows = Arc::new(Vec::new());
        cx.emit(SessionEvent::RunChanged);
        self.reload(cx);
    }

    fn apply_load(&mut self, result: LoadResult, cx: &mut Context<Self>) {
        match result {
            LoadResult::Missing => {
                self.health = Health::Missing;
                self.path = None;
                self.run = None;
                self.store.reset(Vec::new());
                cx.emit(SessionEvent::RunChanged);
            }
            LoadResult::Error(error) => {
                self.health = Health::Error(error);
                cx.emit(SessionEvent::RunChanged);
            }
            LoadResult::Loaded(loaded) => {
                let tail = loaded.tail.clone();
                let path = loaded.path.clone();
                self.install_loaded(*loaded, cx);
                crate::fsguard::milestone_once("run loaded and merged");
                self.start_feed(path, tail, cx);
            }
        }
        self.refresh_runs(cx);
        cx.notify();
    }

    fn install_loaded(&mut self, loaded: Loaded, cx: &mut Context<Self>) {
        let path_changed = self.path.as_ref() != Some(&loaded.path);
        self.path = Some(loaded.path);
        self.skipped = loaded.skipped;
        self.run = Some(RunInfo {
            header: loaded.header.unwrap_or_default(),
            report: loaded.report,
        });
        self.store.reset(loaded.calls);
        self.health = Health::Ready;
        self.findings = Arc::new(Vec::new());
        self.findings_at = None;
        if path_changed {
            self.baseline_run = None;
        }
        cx.emit(SessionEvent::RunChanged);
        self.refresh_lock(cx);
        self.resolve_baseline(cx);
        self.recompute(cx);
        self.schedule_findings(cx);
    }

    /// Keep the run current: event stream when live, file tail otherwise.
    fn start_feed(&mut self, path: PathBuf, tail: feed::TailState, cx: &mut Context<Self>) {
        let ended = self.run.as_ref().is_none_or(|run| run.report.is_some());
        if ended {
            self.feed_mode = Some(FeedMode::Done);
            return;
        }
        let endpoint = self.header_endpoint().map(str::to_string);
        let generation = self.generation;
        let mut rx = feed::start(path, endpoint, tail);
        self.feed_task = Some(cx.spawn(async move |this, cx| {
            while let Some(first) = rx.next().await {
                let mut batch = vec![first];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                let alive = this.update(cx, |session, cx| {
                    if session.generation != generation {
                        return false;
                    }
                    session.apply_feed(batch, cx);
                    true
                });
                if !matches!(alive, Ok(true)) {
                    break;
                }
                cx.background_executor().timer(FRAME).await;
            }
        }));
    }

    /// Apply one frame's worth of feed messages.
    fn apply_feed(&mut self, batch: Vec<FeedMsg>, cx: &mut Context<Self>) {
        let mut calls: Vec<Trace> = Vec::new();
        let mut ended = false;
        for msg in batch {
            match msg {
                FeedMsg::Calls(more) => calls.extend(more),
                FeedMsg::Header(header) => {
                    if let Some(run) = self.run.as_mut() {
                        run.header = header;
                    }
                }
                FeedMsg::End(report) => {
                    if let Some(run) = self.run.as_mut() {
                        run.report = Some(*report);
                    }
                    ended = true;
                }
                FeedMsg::Reset(loaded) => {
                    calls.clear();
                    self.install_loaded(*loaded, cx);
                }
                FeedMsg::Mode(mode) => {
                    self.feed_mode = Some(mode);
                    cx.notify();
                }
                FeedMsg::Error(error) => {
                    self.health = Health::Error(error);
                    cx.notify();
                }
            }
        }
        if !calls.is_empty() {
            if self.health != Health::Ready {
                self.health = Health::Ready;
            }
            let delta = self.store.ingest(calls);
            if delta.changed {
                cx.emit(SessionEvent::Rows(delta));
                self.recompute(cx);
                self.schedule_findings(cx);
            }
        }
        if ended {
            self.feed_mode = Some(FeedMode::Done);
            self.refresh_runs(cx);
            self.recompute(cx);
            self.findings_at = None;
            self.schedule_findings(cx);
            cx.notify();
        }
    }

    /// While following the newest run, switch when a newer one appears.
    fn watch_newest(&mut self, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(NEWEST_RUN_POLL).await;
                let Ok(Some(dir)) = this.update(cx, |session, _| {
                    session
                        .source
                        .follows_latest()
                        .then(|| session.source.runs_dir().map(Path::to_path_buf))
                        .flatten()
                }) else {
                    if this.upgrade().is_none() {
                        break;
                    }
                    continue;
                };
                let newest = cx
                    .background_executor()
                    .spawn(async move { feed::newest_run(&dir) })
                    .await;
                let gone = this
                    .update(cx, |session, cx| {
                        let stale = newest.is_some()
                            && newest != session.path
                            && session.source.follows_latest()
                            && session.health != Health::Loading;
                        if stale {
                            session.reload(cx);
                        }
                    })
                    .is_err();
                if gone {
                    break;
                }
            }
        })
    }

    // -- Runs, lock and baseline -------------------------------------------

    pub fn refresh_runs(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.runs_dir() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let runs = cx
                .background_executor()
                .spawn(async move { feed::list_runs(&dir) })
                .await;
            this.update(cx, |session, cx| {
                session.runs = Arc::new(runs);
                cx.emit(SessionEvent::Runs);
                if session.baseline == Baseline::Previous {
                    session.resolve_baseline(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn runs_dir(&self) -> Option<PathBuf> {
        if self.demo {
            return None;
        }
        match &self.source {
            Source::Runs { dir, .. } => Some(dir.clone()),
            Source::File(path) => path.parent().map(Path::to_path_buf),
        }
    }

    /// Re-read the lock file in the background.
    pub fn refresh_lock(&mut self, cx: &mut Context<Self>) {
        self.lock_path = if self.demo {
            None
        } else {
            self.run
                .as_ref()
                .and_then(|run| lockfile::lock_path(&run.header))
        };
        let Some(path) = self.lock_path.clone() else {
            self.lock = None;
            return;
        };
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let lock = cx
                .background_executor()
                .spawn(async move { lockfile::read(&path).ok().flatten() })
                .await;
            this.update(cx, |session, cx| {
                if session.generation == generation {
                    session.lock = lock;
                    session.recompute(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn set_baseline(&mut self, baseline: Baseline, cx: &mut Context<Self>) {
        if self.baseline != baseline {
            self.baseline = baseline;
            self.baseline_run = None;
            self.resolve_baseline(cx);
            self.recompute(cx);
        }
    }

    /// The run the "previous run" baseline means: the newest finished run
    /// before this one, preferring the same command in the same directory.
    pub fn previous_run(&self) -> Option<&RunSummary> {
        let current = self.path.as_deref()?;
        let header = self.run.as_ref().map(|run| &run.header);
        let earlier = || {
            self.runs
                .iter()
                .filter(move |run| run.ended() && run.path.as_path() < current)
        };
        earlier()
            .find(|run| {
                header.is_some_and(|h| {
                    run.header
                        .as_ref()
                        .is_some_and(|rh| rh.command == h.command && rh.cwd == h.cwd)
                })
            })
            .or_else(|| earlier().next())
    }

    fn resolve_baseline(&mut self, cx: &mut Context<Self>) {
        let path = match &self.baseline {
            Baseline::Lock => {
                self.baseline_run = None;
                return;
            }
            Baseline::Previous => match self.previous_run() {
                Some(run) => run.path.clone(),
                None => {
                    self.baseline_run = None;
                    return;
                }
            },
            Baseline::Run(path) => path.clone(),
        };
        if self
            .baseline_run
            .as_ref()
            .is_some_and(|(loaded, _)| *loaded == path)
        {
            return;
        }
        // The run list already holds finished runs' reports.
        if let Some(report) = self
            .runs
            .iter()
            .find(|run| run.path == path)
            .and_then(|run| run.report.clone())
        {
            self.baseline_run = Some((path, report));
            self.recompute(cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let target = path.clone();
            let report = cx
                .background_executor()
                .spawn(async move { feed::read_report(&target) })
                .await;
            this.update(cx, |session, cx| {
                if let Some(report) = report {
                    session.baseline_run = Some((path, report));
                    session.recompute(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Recompute access rows, changes and the diff from what is in memory.
    /// No IO: runs when inputs change, never in render.
    pub fn recompute(&mut self, cx: &mut Context<Self>) {
        let report = self.run.as_ref().and_then(|run| run.report.as_ref());
        let lock = self.lock.as_ref();
        match (&self.baseline, self.baseline_run.as_ref()) {
            (Baseline::Lock, _) | (_, None) => {
                self.diff = None;
                self.changes = ChangeSet {
                    new_failures: Arc::new(BTreeSet::new()),
                    new_access: true,
                };
                // O(permissions), not O(calls): the store keeps the table.
                let live;
                let used = match report {
                    Some(report) => &report.access,
                    None => {
                        live = self.store.access_entries();
                        &live
                    }
                };
                self.access_rows = Arc::new(access_rows(used, lock, AccessBaseline::Lock(lock)));
            }
            (_, Some((_, base))) => {
                let live;
                let target: &Report = match report {
                    Some(report) => report,
                    None => {
                        let run_id = self
                            .run
                            .as_ref()
                            .map(|run| run.header.run.clone())
                            .unwrap_or_default();
                        live = self.store.live_report(&run_id);
                        &live
                    }
                };
                let diff = RunDiff::compute(base, target);
                let base_access = base.access_set();
                self.changes = ChangeSet {
                    new_failures: Arc::new(diff.new_failures.iter().cloned().collect()),
                    new_access: false,
                };
                self.access_rows = Arc::new(access_rows(
                    &target.access,
                    lock,
                    AccessBaseline::Run(&base_access),
                ));
                self.diff = Some(diff);
            }
        }
        cx.emit(SessionEvent::Derived);
        cx.notify();
    }

    /// Findings: the report's when it has them, else computed from the calls
    /// off the main thread, at most every [`FINDINGS_THROTTLE`] while live.
    pub fn schedule_findings(&mut self, cx: &mut Context<Self>) {
        if let Some(report) = self.run.as_ref().and_then(|run| run.report.as_ref())
            && !report.findings.is_empty()
        {
            self.findings = Arc::new(report.findings.clone());
            cx.emit(SessionEvent::Derived);
            return;
        }
        if self.findings_running {
            self.findings_dirty = true;
            return;
        }
        let wait = self
            .findings_at
            .map(|at| FINDINGS_THROTTLE.saturating_sub(at.elapsed()))
            .unwrap_or_default();
        self.findings_running = true;
        self.findings_dirty = false;
        let generation = self.generation;
        let rows = self.store.rows().clone();
        cx.spawn(async move |this, cx| {
            if !wait.is_zero() {
                cx.background_executor().timer(wait).await;
            }
            let findings = cx
                .background_executor()
                .spawn(async move {
                    let calls: Vec<lenslet_core::trace::Trace> = rows
                        .iter()
                        .filter(|row| !row.pending)
                        .map(|row| row.to_core())
                        .collect();
                    lenslet_core::findings::analyze(calls.iter())
                })
                .await;
            this.update(cx, |session, cx| {
                session.findings_running = false;
                session.findings_at = Some(Instant::now());
                if session.generation == generation {
                    session.findings = Arc::new(findings);
                    cx.emit(SessionEvent::Derived);
                    cx.notify();
                }
                if session.findings_dirty {
                    session.schedule_findings(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    // -- Lock edits ----------------------------------------------------------

    pub fn can_edit_lock(&self) -> bool {
        self.lock_path.is_some()
    }

    /// Accept permissions into the lock (background), then re-read it.
    pub fn accept(&mut self, entries: Vec<(Access, bool)>, cx: &mut Context<Self>) {
        let Some(path) = self.lock_path.clone() else {
            return;
        };
        if entries.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { lockfile::accept(&path, &entries) })
                .await;
            this.update(cx, |session, cx| {
                let message = match result {
                    Ok(added) => format!("Added {added} to lenslet.lock"),
                    Err(error) => format!("lenslet.lock: {error}"),
                };
                cx.emit(SessionEvent::Toast(message));
                session.refresh_lock(cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn set_test_mark(&mut self, access: Access, test: bool, cx: &mut Context<Self>) {
        let Some(path) = self.lock_path.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { lockfile::set_test_mark(&path, &access, test) })
                .await;
            this.update(cx, |session, cx| {
                let message = match result {
                    Ok(_) if test => "Marked `# test` in lenslet.lock".to_string(),
                    Ok(_) => "Removed `# test` in lenslet.lock".to_string(),
                    Err(error) => format!("lenslet.lock: {error}"),
                };
                cx.emit(SessionEvent::Toast(message));
                session.refresh_lock(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Delete finished runs (background).
    pub fn clear_finished_runs(&mut self, cx: &mut Context<Self>) {
        let Some(dir) = self.runs_dir() else {
            return;
        };
        let keep = self.path.clone();
        cx.spawn(async move |this, cx| {
            let removed = cx
                .background_executor()
                .spawn(async move { feed::clear_finished_runs(&dir, keep.as_deref()) })
                .await;
            this.update(cx, |session, cx| {
                cx.emit(SessionEvent::Toast(format!(
                    "Cleared {removed} finished runs"
                )));
                session.refresh_runs(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Step to an older (`-1`) or newer (`+1`) run in the run list.
    pub fn step_run(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(dir) = self.source.runs_dir().map(Path::to_path_buf) else {
            return;
        };
        // `runs` is newest first.
        let paths: Vec<PathBuf> = self.runs.iter().rev().map(|r| r.path.clone()).collect();
        let Some(current) = self
            .path
            .as_ref()
            .and_then(|path| paths.iter().position(|p| p == path))
        else {
            return;
        };
        let target = (current as isize + delta).clamp(0, paths.len() as isize - 1) as usize;
        if target == current {
            return;
        }
        let pinned = (target + 1 < paths.len()).then(|| paths[target].clone());
        self.switch(Source::Runs { dir, pinned }, cx);
    }

    /// `(position, count)` of the shown run in the run list (1-based, oldest first).
    pub fn run_position(&self) -> Option<(usize, usize)> {
        let path = self.path.as_ref()?;
        let index = self.runs.iter().position(|run| &run.path == path)?;
        Some((self.runs.len() - index, self.runs.len()))
    }
}
