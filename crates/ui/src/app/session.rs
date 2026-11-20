//! The run being shown and everything derived from it.
//!
//! `Session` is a model entity: it owns the call store, the run header and
//! report, the lock, the baseline comparison, findings and the run list. All
//! IO happens on the background executor; results are applied with
//! `this.update`, guarded by a generation counter so a stale load (the user
//! already switched runs) is dropped. Views subscribe to [`SessionEvent`]s and
//! re-render only for what they show.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use futures::StreamExt;
use gpui::{Context, EventEmitter, Task};
use lenslet_core::{
    findings::Finding,
    lock::{Decision, Gate, Lock, RequestContext},
    summary::{Report, RunDiff},
};

use crate::{
    demo,
    feed::{self, FeedMode, FeedMsg, LoadResult, Loaded, RunSummary, Source},
    lockfile,
    model::{Access, RunInfo, Trace},
    prefs::Lang,
    store::{AccessBaseline, AccessRow, CallStore, ChangeSet, Delta, access_rows},
    theme::Ui,
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
    pub evicted: usize,
    pub console_log: Arc<str>,
    pub workspace_fallback: Option<PathBuf>,
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
    pub sim_policy: Option<(String, Arc<Gate>)>,
    pub sim_decisions: Arc<BTreeMap<Access, Decision>>,
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
            evicted: 0,
            console_log: Arc::from(""),
            workspace_fallback: None,
            feed_task: None,
            _watch_task: None,
            lock_path: None,
            lock: None,
            baseline: Baseline::Lock,
            baseline_run: None,
            diff: None,
            changes: ChangeSet::default(),
            access_rows: Arc::new(Vec::new()),
            sim_policy: None,
            sim_decisions: Arc::new(BTreeMap::new()),
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

    pub fn set_workspace_fallback(&mut self, dir: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.workspace_fallback != dir {
            self.workspace_fallback = dir;
            self.refresh_lock(cx);
            cx.notify();
        }
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
        let end = self.store.max_end_ms().unwrap_or(start + 1);
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
                self.evicted = 0;
                self.console_log = Arc::from("");
                self.store.reset(Vec::new());
                self.refresh_lock(cx);
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
        self.evicted = loaded.evicted;
        self.run = Some(RunInfo {
            header: loaded.header.unwrap_or_default(),
            report: loaded.report,
        });
        self.store
            .reset_with_cap(loaded.calls, loaded.unbounded, loaded.evicted);
        self.health = Health::Ready;
        self.findings = Arc::new(Vec::new());
        self.findings_at = None;
        if path_changed {
            self.baseline_run = None;
            self.console_log = Arc::from("");
        }
        cx.emit(SessionEvent::RunChanged);
        self.refresh_lock(cx);
        self.refresh_console_log(cx);
        self.resolve_baseline(cx);
        self.recompute(cx);
        self.schedule_findings(cx);
    }

    /// Re-load the current trace file without the 5,000-row rolling window cap.
    pub fn load_all_calls(&mut self, cx: &mut Context<Self>) {
        if self.demo {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let generation = self.generation;
        let lang = Ui::of(cx).lang;
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { feed::load_unbounded(&path) })
                .await;
            this.update(cx, |session, cx| {
                if session.generation != generation {
                    return;
                }
                if let LoadResult::Loaded(loaded) = res {
                    let count = loaded.calls.len();
                    session.install_loaded(*loaded, cx);
                    let msg = match lang {
                        Lang::En => format!("Loaded all {count} calls from trace"),
                        Lang::Zh => format!("已加载全部 {count} 条调用记录"),
                    };
                    cx.emit(SessionEvent::Toast(msg));
                }
            })
            .ok();
        })
        .detach();
    }

    /// Read companion stdout/stderr `.log` file for the active run on the background executor.
    pub fn refresh_console_log(&mut self, cx: &mut Context<Self>) {
        if self.demo {
            return;
        }
        let log_path = self
            .run
            .as_ref()
            .and_then(|r| r.header.log.as_deref())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| self.path.as_deref().map(lenslet_core::runs::log_path_for));
        let Some(log_path) = log_path else {
            return;
        };
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let text = cx
                .background_executor()
                .spawn(async move { crate::daemon::read_run_log(&log_path, 512 * 1024) })
                .await;
            this.update(cx, |session, cx| {
                if session.generation == generation {
                    let next = text.unwrap_or_default();
                    if session.console_log.as_ref() != next {
                        session.console_log = Arc::from(next);
                        cx.emit(SessionEvent::Derived);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
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
                        run.header = *header;
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
            self.evicted = self.evicted.max(self.store.evicted_total());
            if delta.changed {
                cx.emit(SessionEvent::Rows(delta));
                self.recompute(cx);
                self.schedule_findings(cx);
            }
            self.refresh_console_log(cx);
        }
        if ended {
            self.feed_mode = Some(FeedMode::Done);
            self.refresh_console_log(cx);
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
                .or_else(|| {
                    self.workspace_fallback
                        .as_ref()
                        .filter(|d| d.is_dir())
                        .map(|d| d.join("lenslet.lock"))
                })
        };
        let Some(path) = self.lock_path.clone() else {
            self.lock = None;
            self.recompute(cx);
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
        let live_lock = lock.map(|l| Arc::new(l.access.clone()));
        match (&self.baseline, self.baseline_run.as_ref()) {
            (Baseline::Lock, _) | (_, None) => {
                self.diff = None;
                self.changes = ChangeSet {
                    new_failures: Arc::new(BTreeSet::new()),
                    new_access: true,
                    live_lock,
                };
                // O(permissions), not O(calls): cumulative_access_entries keeps
                // early permissions even if >5,000 live calls roll over the window.
                let live;
                let used = match report {
                    Some(report) => &report.access,
                    None => {
                        live = if self.store.rows().len() < crate::model::MAX_ROWS {
                            self.store.access_entries()
                        } else {
                            self.store.cumulative_access_entries()
                        };
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
                    live_lock: None,
                };
                self.access_rows = Arc::new(access_rows(
                    &target.access,
                    lock,
                    AccessBaseline::Run(&base_access),
                ));
                self.diff = Some(diff);
            }
        }
        if self.sim_policy.is_some() {
            self.recompute_sim_decisions();
        }
        cx.emit(SessionEvent::Derived);
        cx.notify();
    }

    fn recompute_sim_decisions(&mut self) {
        let Some((_, gate)) = self.sim_policy.as_ref() else {
            self.sim_decisions = Arc::new(BTreeMap::new());
            return;
        };
        let mut map = BTreeMap::new();
        for row in self.access_rows.iter() {
            let sample = self
                .store
                .rows()
                .iter()
                .find(|c| !c.pending && c.uses(&row.access))
                .map(|c| c.to_core_with_request_body());
            let decision = match sample {
                Some(core_trace) => {
                    let base_ctx = RequestContext::from_trace(&core_trace);
                    let ctx = if row.implied {
                        base_ctx.for_implied(
                            &core_trace.service,
                            core_trace.region.as_deref(),
                            &row.access.action,
                        )
                    } else {
                        base_ctx
                    };
                    gate.decide(&row.access, Some(&ctx))
                }
                None => gate.decide(&row.access, None),
            };
            map.insert(row.access.clone(), decision);
        }
        self.sim_decisions = Arc::new(map);
    }

    /// Load an external IAM policy JSON file to simulate against this run's permissions.
    pub fn load_sim_policy(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let lang = Ui::of(cx).lang;
        let label = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("policy.json")
            .to_string();
        let gate_label = label.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    crate::fsguard::assert_io_allowed();
                    Gate::from_policy_file_simulating(&path, gate_label)
                })
                .await;
            this.update(cx, |session, cx| match loaded {
                Ok(gate) => {
                    session.sim_policy = Some((label.clone(), Arc::new(gate)));
                    session.recompute_sim_decisions();
                    let msg = match lang {
                        Lang::En => format!("Simulated against {label}"),
                        Lang::Zh => format!("已加载策略模拟：{label}"),
                    };
                    cx.emit(SessionEvent::Toast(msg));
                    cx.emit(SessionEvent::Derived);
                    cx.notify();
                }
                Err(error) => {
                    let msg = match lang {
                        Lang::En => format!("Invalid IAM policy: {error}"),
                        Lang::Zh => format!("无效的 IAM 策略：{error}"),
                    };
                    cx.emit(SessionEvent::Toast(msg));
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn clear_sim_policy(&mut self, cx: &mut Context<Self>) {
        self.sim_policy = None;
        self.sim_decisions = Arc::new(BTreeMap::new());
        cx.emit(SessionEvent::Derived);
        cx.notify();
    }

    /// Findings: the report's when the run ended, else computed from the calls
    /// off the main thread, at most every [`FINDINGS_THROTTLE`] while live.
    pub fn schedule_findings(&mut self, cx: &mut Context<Self>) {
        if let Some(report) = self.run.as_ref().and_then(|run| run.report.as_ref()) {
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

    /// Accept permissions into the lock (background), preserving `# implied` & `# test`.
    pub fn accept(&mut self, entries: Vec<(Access, bool, bool)>, cx: &mut Context<Self>) {
        let Some(path) = self.lock_path.clone() else {
            return;
        };
        if entries.is_empty() {
            return;
        }
        let lang = Ui::of(cx).lang;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { lockfile::accept_with_implied(&path, &entries) })
                .await;
            this.update(cx, |session, cx| {
                let message = match (result, lang) {
                    (Ok(added), Lang::En) => format!("Added {added} to lenslet.lock"),
                    (Ok(added), Lang::Zh) => format!("已向 lenslet.lock 写入 {added} 项权限"),
                    (Err(error), _) => format!("lenslet.lock: {error}"),
                };
                cx.emit(SessionEvent::Toast(message));
                session.refresh_lock(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Remove permissions from `lenslet.lock` (background).
    pub fn remove_from_lock(&mut self, entries: Vec<Access>, cx: &mut Context<Self>) {
        let Some(path) = self.lock_path.clone() else {
            return;
        };
        if entries.is_empty() {
            return;
        }
        let lang = Ui::of(cx).lang;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { lockfile::remove(&path, &entries) })
                .await;
            this.update(cx, |session, cx| {
                let message = match (result, lang) {
                    (Ok(removed), Lang::En) => format!("Removed {removed} from lenslet.lock"),
                    (Ok(removed), Lang::Zh) => format!("已从 lenslet.lock 移除 {removed} 项权限"),
                    (Err(error), _) => format!("lenslet.lock: {error}"),
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
        let lang = Ui::of(cx).lang;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { lockfile::set_test_mark(&path, &access, test) })
                .await;
            this.update(cx, |session, cx| {
                let message = match (result, test, lang) {
                    (Ok(_), true, Lang::En) => "Marked `# test` in lenslet.lock".to_string(),
                    (Ok(_), true, Lang::Zh) => "已在 lenslet.lock 标记 `# test`".to_string(),
                    (Ok(_), false, Lang::En) => "Removed `# test` in lenslet.lock".to_string(),
                    (Ok(_), false, Lang::Zh) => "已取消 lenslet.lock 中的 `# test`".to_string(),
                    (Err(error), _, _) => format!("lenslet.lock: {error}"),
                };
                cx.emit(SessionEvent::Toast(message));
                session.refresh_lock(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Pin or unpin a run so retention pruning preserves it.
    pub fn toggle_pin_run(&mut self, run_id: String, pinned: bool, cx: &mut Context<Self>) {
        let Some(dir) = self.runs_dir() else {
            return;
        };
        let lang = Ui::of(cx).lang;
        let id_for_io = run_id.clone();
        cx.spawn(async move |this, cx| {
            let res = cx
                .background_executor()
                .spawn(async move { feed::set_run_pinned(&dir, &id_for_io, pinned) })
                .await;
            this.update(cx, |session, cx| {
                let msg = match (res, pinned, lang) {
                    (Ok(_), true, Lang::En) => format!("Pinned run {run_id}"),
                    (Ok(_), true, Lang::Zh) => format!("已置顶保留运行 {run_id}"),
                    (Ok(_), false, Lang::En) => format!("Unpinned run {run_id}"),
                    (Ok(_), false, Lang::Zh) => format!("已取消置顶 {run_id}"),
                    (Err(e), _, _) => e.to_string(),
                };
                cx.emit(SessionEvent::Toast(msg));
                session.refresh_runs(cx);
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
        let lang = Ui::of(cx).lang;
        let keep = self.path.clone();
        cx.spawn(async move |this, cx| {
            let removed = cx
                .background_executor()
                .spawn(async move { feed::clear_finished_runs(&dir, keep.as_deref()) })
                .await;
            this.update(cx, |session, cx| {
                let msg = match lang {
                    Lang::En => format!("Cleared {removed} finished runs"),
                    Lang::Zh => format!("已清理 {removed} 个已结束运行"),
                };
                cx.emit(SessionEvent::Toast(msg));
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
