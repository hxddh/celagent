//! The calls of one run, kept incrementally: rows in arrival order with a
//! persistent id→index map, and the aggregates the window shows (counts,
//! service facets, the access table) maintained per append, so a live run
//! costs O(new lines) per batch instead of a pass over every row.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};

use lenslet_core::{
    lock::Lock,
    summary::{AccessEntry, AccessUse, Failure, FailureKind, Report},
};

use crate::model::{
    Access, CallFilter, MAX_ROWS, ServiceFacet, Trace, matches_search, operation_label,
};

/// Counts over every row in the store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub total: usize,
    pub errors: usize,
    pub retries: usize,
    pub blocked: usize,
    pub slow: usize,
    pub pending: usize,
}

#[derive(Clone, Debug, Default)]
struct AccessAgg {
    calls: usize,
    errors: usize,
    direct: usize,
    new: usize,
    unverified: usize,
    via: Vec<String>,
}

/// What one [`CallStore::ingest`] changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Delta {
    /// New rows landed only at the end, starting at this index, and nothing
    /// before them moved: a view can extend itself instead of recomputing.
    pub appended_from: Option<usize>,
    /// Indices of rows replaced in place (a pending call that finished).
    pub replaced: Vec<usize>,
    /// Rows moved (out-of-order insert or eviction): indices are invalid.
    pub reindexed: bool,
    pub changed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CallStore {
    rows: Arc<Vec<Arc<Trace>>>,
    index: HashMap<String, usize>,
    stats: Stats,
    services: BTreeMap<String, (String, usize)>,
    access: BTreeMap<Access, AccessAgg>,
    ever_access: BTreeMap<Access, AccessAgg>,
    revision: u64,
}

fn order_key(trace: &Trace) -> (i64, &str) {
    (trace.at_ms().unwrap_or(i64::MAX), trace.at.as_str())
}

impl CallStore {
    pub fn rows(&self) -> &Arc<Vec<Arc<Trace>>> {
        &self.rows
    }

    #[cfg(test)]
    pub fn position(&self, id: &str) -> Option<usize> {
        self.index.get(id).copied()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Services present, sorted by label.
    pub fn facets(&self) -> Vec<ServiceFacet> {
        let mut facets: Vec<ServiceFacet> = self
            .services
            .iter()
            .filter(|(_, (_, count))| *count > 0)
            .map(|(id, (label, count))| ServiceFacet {
                id: id.clone(),
                label: label.clone(),
                count: *count,
            })
            .collect();
        facets.sort_by(|a, b| a.label.cmp(&b.label).then_with(|| a.id.cmp(&b.id)));
        facets
    }

    fn contribute(&mut self, row: &Trace, add: bool, evict: bool) {
        let step = |value: &mut usize, on: bool| {
            if on {
                if add {
                    *value += 1;
                } else {
                    *value = value.saturating_sub(1);
                }
            }
        };
        step(&mut self.stats.total, true);
        step(&mut self.stats.errors, row.is_error());
        step(&mut self.stats.retries, row.is_retry());
        step(&mut self.stats.blocked, row.blocked);
        step(&mut self.stats.slow, row.is_slow());
        step(&mut self.stats.pending, row.pending);
        let facet = self
            .services
            .entry(row.service_key().to_string())
            .or_insert_with(|| (row.service_name().to_string(), 0));
        step(&mut facet.1, true);
        if row.pending {
            return;
        }
        let via_label = add.then(|| format!("{} {}", row.service_name(), operation_label(row)));
        for (call_access, implied) in row.call_access() {
            let key = call_access.key();
            let is_new = row.is_access_new(call_access);
            let agg = self.access.entry(key.clone()).or_default();
            step(&mut agg.calls, true);
            step(&mut agg.errors, row.is_error());
            step(&mut agg.direct, !implied);
            step(&mut agg.new, is_new);
            step(&mut agg.unverified, call_access.unverified);
            if let Some(via) = via_label.as_ref()
                && agg.via.len() < 3
                && !agg.via.contains(via)
            {
                agg.via.push(via.clone());
            }
            if !evict {
                let ever = self.ever_access.entry(key).or_default();
                step(&mut ever.calls, true);
                step(&mut ever.errors, row.is_error());
                step(&mut ever.direct, !implied);
                step(&mut ever.new, is_new);
                step(&mut ever.unverified, call_access.unverified);
                if let Some(via) = via_label.as_ref()
                    && ever.via.len() < 3
                    && !ever.via.contains(via)
                {
                    ever.via.push(via.clone());
                }
            }
        }
        if !add {
            self.access.retain(|_, agg| agg.calls > 0);
            if !evict {
                self.ever_access.retain(|_, agg| agg.calls > 0);
            }
        }
    }

    /// Replace everything with `rows` (a fresh load).
    pub fn reset(&mut self, rows: Vec<Trace>) -> Delta {
        *self = CallStore {
            revision: self.revision + 1,
            ..CallStore::default()
        };
        let mut delta = self.ingest(rows);
        delta.reindexed = true;
        delta.appended_from = None;
        delta.changed = true;
        delta
    }

    /// Fold new lines in. A line whose id is already present replaces that
    /// row (a pending call that finished); an identical repeat (a replayed
    /// event-stream record) is ignored. Rows stay in request-arrival order and
    /// only the newest [`MAX_ROWS`] are kept.
    pub fn ingest(&mut self, incoming: Vec<Trace>) -> Delta {
        let mut delta = Delta {
            appended_from: Some(self.rows.len()),
            ..Delta::default()
        };
        let mut reorder_from: Option<usize> = None;
        for trace in incoming {
            if let Some(&at) = self.index.get(&trace.id) {
                let old = self.rows[at].clone();
                if old.pending == trace.pending && old.status == trace.status {
                    continue;
                }
                let mut trace = trace;
                if trace.line_ref.is_none() {
                    trace.line_ref = old.line_ref.clone();
                }
                self.contribute(&old, false, false);
                self.contribute(&trace, true, false);
                Arc::make_mut(&mut self.rows)[at] = Arc::new(trace);
                delta.replaced.push(at);
                delta.changed = true;
                continue;
            }
            self.contribute(&trace, true, false);
            let rows = Arc::make_mut(&mut self.rows);
            let key = order_key(&trace);
            let mut pos = rows.len();
            while pos > 0 && order_key(&rows[pos - 1]) > key {
                pos -= 1;
            }
            let id = trace.id.clone();
            if pos == rows.len() {
                rows.push(Arc::new(trace));
                self.index.insert(id, pos);
            } else {
                rows.insert(pos, Arc::new(trace));
                reorder_from = Some(reorder_from.map_or(pos, |from| from.min(pos)));
                self.index.insert(id, pos);
            }
            delta.changed = true;
        }
        if let Some(from) = reorder_from {
            for (index, row) in self.rows.iter().enumerate().skip(from) {
                self.index.insert(row.id.clone(), index);
            }
            delta.reindexed = true;
        }
        if self.rows.len() > MAX_ROWS {
            let excess = self.rows.len() - MAX_ROWS;
            let evicted: Vec<Arc<Trace>> = Arc::make_mut(&mut self.rows).drain(..excess).collect();
            for row in &evicted {
                self.contribute(row, false, true);
                self.index.remove(&row.id);
            }
            for (index, row) in self.rows.iter().enumerate() {
                self.index.insert(row.id.clone(), index);
            }
            delta.reindexed = true;
        }
        if delta.reindexed {
            delta.appended_from = None;
            delta.replaced.clear();
        }
        if delta.changed {
            self.revision += 1;
        }
        delta
    }

    /// Ids of the calls in the same SDK invocation as `trace`, in order.
    pub fn invocation_peers(&self, trace: &Trace) -> Vec<Arc<Trace>> {
        // A replay re-sends the original invocation id but is not a retry.
        let Some(id) = trace
            .invocation_id
            .as_deref()
            .filter(|_| trace.replay_of.is_none())
        else {
            return Vec::new();
        };
        self.rows
            .iter()
            .filter(|row| row.replay_of.is_none() && row.invocation_id.as_deref() == Some(id))
            .cloned()
            .collect()
    }

    /// The access table of the calls currently in the window.
    pub fn access_entries(&self) -> Vec<AccessEntry> {
        entries_from_map(&self.access)
    }

    /// Cumulative access table across the entire live session (never evicted
    /// when `MAX_ROWS` rolls over).
    pub fn cumulative_access_entries(&self) -> Vec<AccessEntry> {
        entries_from_map(&self.ever_access)
    }

    /// A report for a run that has not ended yet, from the calls seen so far:
    /// enough for `RunDiff` and the access table.
    pub fn live_report(&self, run: &str) -> Report {
        let mut services = BTreeMap::new();
        let mut failures = Vec::new();
        for row in self.rows.iter().filter(|row| !row.pending) {
            *services.entry(row.service.clone()).or_insert(0usize) += 1;
            if let Some(problem) = row.problem.as_ref() {
                failures.push(Failure {
                    id: row.id.clone(),
                    service: row.service.clone(),
                    operation: row.operation.clone(),
                    status: row.status,
                    code: problem.code.clone(),
                    message: problem.message.clone(),
                    hint: problem.hint.clone(),
                    attempt: row.attempt,
                    max_attempts: row.max_attempts,
                    blocked: row.blocked,
                });
            }
        }
        Report {
            run: run.to_string(),
            calls: self.stats.total,
            errors: self.stats.errors,
            retries: self.stats.retries,
            blocked: self.stats.blocked,
            unfinished: self.stats.pending,
            services,
            failures,
            access: self.cumulative_access_entries(),
            ..Report::default()
        }
    }
}

fn entries_from_map(map: &BTreeMap<Access, AccessAgg>) -> Vec<AccessEntry> {
    map.iter()
        .map(|(access, agg)| AccessEntry {
            action: access.action.clone(),
            resource: access.resource.clone(),
            used: AccessUse {
                calls: agg.calls,
                errors: agg.errors,
                implied: agg.direct == 0,
                unverified: agg.unverified > 0,
                via: agg.via.clone(),
            },
            new: agg.new > 0,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Views over the store
// ---------------------------------------------------------------------------

/// What makes a call "changed" relative to the baseline.
#[derive(Clone, Debug, Default)]
pub struct ChangeSet {
    /// Failure kinds that are new in this run.
    pub new_failures: Arc<BTreeSet<FailureKind>>,
    /// Count calls needing access the lock lacks as changed.
    pub new_access: bool,
}

impl ChangeSet {
    pub fn is_changed(&self, trace: &Trace) -> bool {
        (self.new_access && trace.new)
            || (!self.new_failures.is_empty()
                && trace
                    .failure_kind()
                    .is_some_and(|kind| self.new_failures.contains(&kind)))
    }
}

/// Everything a calls view filter depends on, cheap to clone into a task.
#[derive(Clone, Debug, Default)]
pub struct ViewQuery {
    pub filter: CallFilter,
    pub tokens: Vec<String>,
    pub run_start_ms: Option<i64>,
    pub changes: ChangeSet,
}

impl ViewQuery {
    pub fn matches(&self, trace: &Trace) -> bool {
        self.filter
            .matches(trace, self.run_start_ms, self.changes.is_changed(trace))
            && matches_search(trace, &self.tokens)
    }
}

/// Indices of the rows that pass `query`.
pub fn filter_rows(rows: &[Arc<Trace>], query: &ViewQuery) -> Vec<u32> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| query.matches(row))
        .map(|(index, _)| index as u32)
        .collect()
}

/// Above this many rows a search runs on the background executor.
pub const BACKGROUND_SEARCH_ROWS: usize = 1_000;

// ---------------------------------------------------------------------------
// Access table
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessDelta {
    /// Used by this run, not in the baseline (lock or run).
    Added,
    /// In the baseline, not used by this run.
    Removed,
    Same,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessRow {
    pub access: Access,
    pub calls: usize,
    pub errors: usize,
    pub implied: bool,
    pub via: Vec<String>,
    pub used: bool,
    pub test: bool,
    pub in_lock: bool,
    pub delta: AccessDelta,
}

impl AccessRow {
    pub fn matches_search(&self, tokens: &[String]) -> bool {
        if tokens.is_empty() {
            return true;
        }
        let mut hay = format!("{} {}", self.access.action, self.access.resource).to_lowercase();
        for via in &self.via {
            hay.push(' ');
            hay.push_str(&via.to_lowercase());
        }
        match self.delta {
            AccessDelta::Added => hay.push_str(" new added"),
            AccessDelta::Removed => hay.push_str(" unused removed"),
            AccessDelta::Same => {}
        }
        if self.implied {
            hay.push_str(" implied");
        }
        if self.test {
            hay.push_str(" test");
        }
        tokens.iter().all(|token| hay.contains(token.as_str()))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccessFilter {
    #[default]
    All,
    Added,
    Removed,
    Implied,
    Test,
}

impl AccessFilter {
    pub fn matches(self, row: &AccessRow) -> bool {
        match self {
            AccessFilter::All => true,
            AccessFilter::Added => row.delta == AccessDelta::Added,
            AccessFilter::Removed => row.delta == AccessDelta::Removed,
            AccessFilter::Implied => row.implied,
            AccessFilter::Test => row.test,
        }
    }
}

/// What the access table is compared with.
pub enum AccessBaseline<'a> {
    /// The lock file (`None`: no lock file; fall back to the `new` flags the
    /// proxy wrote, which also covers `--policy`).
    Lock(Option<&'a Lock>),
    /// Another run's permissions.
    Run(&'a BTreeSet<Access>),
}

/// Merge this run's access with the baseline into the rows the table shows.
pub fn access_rows(
    used: &[AccessEntry],
    lock: Option<&Lock>,
    baseline: AccessBaseline<'_>,
) -> Vec<AccessRow> {
    let mut rows: BTreeMap<Access, AccessRow> = BTreeMap::new();
    for entry in used {
        let access = Access::new(entry.action.clone(), entry.resource.clone());
        let in_lock = lock.is_some_and(|lock| lock.access.contains(&access));
        let delta = match &baseline {
            AccessBaseline::Lock(Some(lock)) => {
                if lock.access.contains(&access) {
                    AccessDelta::Same
                } else {
                    AccessDelta::Added
                }
            }
            AccessBaseline::Lock(None) => {
                if entry.new {
                    AccessDelta::Added
                } else {
                    AccessDelta::Same
                }
            }
            AccessBaseline::Run(base) => {
                if base.contains(&access) {
                    AccessDelta::Same
                } else {
                    AccessDelta::Added
                }
            }
        };
        rows.insert(
            access.clone(),
            AccessRow {
                test: lock.is_some_and(|lock| lock.test.contains(&access)),
                access,
                calls: entry.used.calls,
                errors: entry.used.errors,
                implied: entry.used.implied,
                via: entry.used.via.clone(),
                used: true,
                in_lock,
                delta,
            },
        );
    }
    let removed: Vec<&Access> = match &baseline {
        AccessBaseline::Lock(Some(lock)) => lock.access.iter().collect(),
        AccessBaseline::Lock(None) => Vec::new(),
        AccessBaseline::Run(base) => base.iter().collect(),
    };
    for access in removed {
        rows.entry(access.clone()).or_insert_with(|| AccessRow {
            access: access.clone(),
            calls: 0,
            errors: 0,
            implied: false,
            via: Vec::new(),
            used: false,
            test: lock.is_some_and(|lock| lock.test.contains(access)),
            in_lock: lock.is_some_and(|lock| lock.access.contains(access)),
            delta: AccessDelta::Removed,
        });
    }
    rows.into_values().collect()
}

/// Compile the permissions used in this run (minus `# test`) into any IaC format.
pub fn compile_policy(rows: &[AccessRow], opts: &lenslet_core::iac::CompileOptions) -> String {
    let inputs: Vec<lenslet_core::iac::PolicyInput> = rows
        .iter()
        .filter(|row| row.used && !row.test)
        .map(|row| lenslet_core::iac::PolicyInput {
            access: row.access.clone(),
            implied: row.implied,
            via: row.via.clone(),
        })
        .collect();
    lenslet_core::iac::compile(&inputs, opts)
}

/// Compile a single permission row into any IaC format (preserving its `implied`/`via` conditions).
pub fn compile_single_permission(
    row: &AccessRow,
    opts: &lenslet_core::iac::CompileOptions,
) -> String {
    let mut single = row.clone();
    single.used = true;
    single.test = false;
    compile_policy(&[single], opts)
}

/// The lock a policy is generated from: what this run used, minus `# test`.
pub fn policy_json(rows: &[AccessRow]) -> String {
    compile_policy(rows, &lenslet_core::iac::CompileOptions::default())
}

/// One IAM statement for a single permission.
pub fn statement_json(access: &Access) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "Effect": "Allow",
        "Action": access.action,
        "Resource": access.resource,
    }))
    .unwrap_or_else(|_| "{}".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{StatusFilter, parse_line, search_tokens};

    pub(crate) fn call_line(id: &str, at_ms: u64, status: u16, extra: &str) -> String {
        let secs = at_ms / 1000;
        let millis = at_ms % 1000;
        format!(
            r#"{{"type":"call","id":"{id}","at":"2026-11-15T09:{:02}:{:02}.{millis:03}+00:00","method":"POST","path":"/","host":"h","service":"dynamodb","operation":"GetItem","status":{status},"ms":3,"authorization":true{extra}}}"#,
            (secs / 60) % 60,
            secs % 60
        )
    }

    fn row(id: &str, at_ms: u64, status: u16, extra: &str) -> Trace {
        parse_line(&call_line(id, at_ms, status, extra)).unwrap()
    }

    #[test]
    fn appends_are_incremental_and_pending_rows_are_replaced_in_place() {
        let mut store = CallStore::default();
        let delta = store.ingest(vec![
            row("a", 0, 200, ""),
            row("b", 10, 0, r#","pending":true"#),
        ]);
        assert_eq!(delta.appended_from, Some(0));
        assert_eq!(store.stats().pending, 1);

        let delta = store.ingest(vec![row("b", 10, 500, "")]);
        assert_eq!(delta.replaced, vec![1]);
        assert_eq!(delta.appended_from, Some(2), "nothing appended");
        assert_eq!(store.stats().pending, 0);
        assert_eq!(store.stats().errors, 1);
        assert_eq!(store.len(), 2);

        // A replayed duplicate (event stream) changes nothing.
        let delta = store.ingest(vec![row("a", 0, 200, "")]);
        assert!(!delta.changed);

        let delta = store.ingest(vec![row("c", 20, 200, "")]);
        assert_eq!(delta.appended_from, Some(2));
        assert_eq!(store.position("c"), Some(2));
    }

    #[test]
    fn out_of_order_lines_keep_arrival_order_and_reindex() {
        let mut store = CallStore::default();
        store.ingest(vec![row("a", 0, 200, ""), row("c", 30, 200, "")]);
        let delta = store.ingest(vec![row("b", 10, 200, "")]);
        assert!(delta.reindexed);
        let ids: Vec<&str> = store.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        assert_eq!(store.position("c"), Some(2));
    }

    #[test]
    fn keeps_only_the_newest_rows_and_their_aggregates() {
        let mut store = CallStore::default();
        let batch: Vec<Trace> = (0..MAX_ROWS as u64 + 10)
            .map(|i| {
                row(
                    &format!("id{i}"),
                    i,
                    if i < 10 { 500 } else { 200 },
                    r#","access":[{"action":"dynamodb:GetItem","resource":"t"}]"#,
                )
            })
            .collect();
        store.ingest(batch);
        assert_eq!(store.len(), MAX_ROWS);
        assert_eq!(store.stats().errors, 0, "the failing calls were evicted");
        assert_eq!(store.position("id10"), Some(0));
        let entries = store.access_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].used.calls, MAX_ROWS);
        let cumulative = store.cumulative_access_entries();
        assert_eq!(cumulative[0].used.calls, MAX_ROWS + 10);
    }

    #[test]
    fn filters_and_changes_compose() {
        let mut store = CallStore::default();
        store.ingest(vec![
            row("ok", 0, 200, ""),
            row(
                "bad",
                1,
                404,
                r#","problem":{"code":"NoSuchKey"},"new":true"#,
            ),
        ]);
        let mut query = ViewQuery {
            filter: CallFilter {
                status: StatusFilter::Errors,
                ..CallFilter::default()
            },
            ..ViewQuery::default()
        };
        assert_eq!(filter_rows(store.rows(), &query), vec![1]);
        query.filter.status = StatusFilter::Changed;
        assert!(filter_rows(store.rows(), &query).is_empty());
        query.changes.new_access = true;
        assert_eq!(filter_rows(store.rows(), &query), vec![1]);
        query.filter.status = StatusFilter::All;
        query.tokens = search_tokens("nosuchkey");
        assert_eq!(filter_rows(store.rows(), &query), vec![1]);
    }

    #[test]
    fn access_rows_compare_with_the_lock_or_another_run() {
        let used = vec![
            AccessEntry {
                action: "s3:GetObject".into(),
                resource: "b/*".into(),
                used: AccessUse {
                    calls: 2,
                    ..AccessUse::default()
                },
                new: false,
            },
            AccessEntry {
                action: "s3:PutObject".into(),
                resource: "b/*".into(),
                used: AccessUse::default(),
                new: true,
            },
        ];
        let lock = lenslet_core::lock::parse("s3:GetObject b/*\nsqs:SendMessage q  # test\n");
        let rows = access_rows(&used, Some(&lock), AccessBaseline::Lock(Some(&lock)));
        let deltas: Vec<(&str, AccessDelta)> = rows
            .iter()
            .map(|r| (r.access.action.as_str(), r.delta))
            .collect();
        assert_eq!(
            deltas,
            [
                ("s3:GetObject", AccessDelta::Same),
                ("s3:PutObject", AccessDelta::Added),
                ("sqs:SendMessage", AccessDelta::Removed),
            ]
        );
        assert!(rows[2].test);

        let base: BTreeSet<Access> = [Access::new("s3:PutObject", "b/*")].into();
        let rows = access_rows(&used, Some(&lock), AccessBaseline::Run(&base));
        assert_eq!(rows[0].delta, AccessDelta::Added);
        assert_eq!(rows[1].delta, AccessDelta::Same);

        let rows = access_rows(&used, None, AccessBaseline::Lock(None));
        assert_eq!(rows[1].delta, AccessDelta::Added, "falls back to `new`");

        let policy = policy_json(&access_rows(
            &used,
            Some(&lock),
            AccessBaseline::Lock(Some(&lock)),
        ));
        assert!(policy.contains("s3:GetObject") && !policy.contains("sqs:SendMessage"));
    }
}
