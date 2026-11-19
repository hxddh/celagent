//! Micro benchmark: a synthetic 5,000-call run through load, merge, filter,
//! search, the access table and the per-frame row work. Ignored by default;
//! run it with `cargo test -p lenslet-ui --release bench -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use crate::{
    body::{self, BodyMode},
    feed::{self, LoadResult},
    model::{CallFilter, StatusFilter, format_latency, format_offset, search_tokens},
    store::{AccessBaseline, CallStore, ViewQuery, access_rows, filter_rows},
    testdata::{call_line, synthetic_run},
    theme,
};

fn time<T>(label: &str, runs: u32, mut f: impl FnMut() -> T) -> (Duration, T) {
    let mut out = None;
    let mut best = Duration::MAX;
    for _ in 0..runs {
        let started = Instant::now();
        out = Some(f());
        best = best.min(started.elapsed());
    }
    println!("{label:<44} {:>9.3} ms", best.as_secs_f64() * 1e3);
    (best, out.unwrap())
}

#[test]
#[ignore = "benchmark; run with --release -- --ignored --nocapture"]
fn five_thousand_calls() {
    let dir = std::env::temp_dir().join(format!("lenslet-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("run.jsonl");
    std::fs::write(&path, synthetic_run(5_000, None, true)).unwrap();
    println!(
        "file: {} KiB",
        std::fs::metadata(&path).unwrap().len() / 1024
    );

    let (load, loaded) = time(
        "load 5,000-call run file (off main thread)",
        5,
        || match feed::load(&path) {
            LoadResult::Loaded(loaded) => loaded,
            other => panic!("{other:?}"),
        },
    );
    let (merge, store) = time("store reset (index + aggregates)", 5, || {
        let mut store = CallStore::default();
        store.reset(loaded.calls.clone());
        store
    });
    println!(
        "first rows ready after                         {:>9.3} ms",
        (load + merge).as_secs_f64() * 1e3
    );
    assert_eq!(store.len(), 5_000);

    let mut live = store.clone();
    let mut next = 5_000;
    time("append 10 live calls (incremental)", 20, || {
        let batch: Vec<_> = (next..next + 10)
            .map(|i| crate::model::parse_line(&call_line(i, None)).unwrap())
            .collect();
        next += 10;
        live.ingest(batch)
    });

    let mut query = ViewQuery::default();
    time("filter: all", 20, || filter_rows(store.rows(), &query));
    query.filter = CallFilter {
        status: StatusFilter::Errors,
        ..CallFilter::default()
    };
    let (_, errors) = time("filter: errors", 20, || filter_rows(store.rows(), &query));
    assert!(!errors.is_empty());
    query.filter = CallFilter::default();
    query.tokens = search_tokens("validation orders");
    let (_, hits) = time("search: two tokens over haystacks", 10, || {
        filter_rows(store.rows(), &query)
    });
    assert!(!hits.is_empty());

    let entries = store.access_entries();
    time("access table vs lock", 20, || {
        access_rows(&entries, None, AccessBaseline::Lock(None))
    });

    // What a frame does with data: format the ~40 visible rows.
    let rows = store.rows().clone();
    time("per-frame row prep (40 visible rows)", 50, || {
        let start = rows[0].at_ms().unwrap();
        rows.iter()
            .skip(2_000)
            .take(40)
            .map(|row| {
                (
                    format_offset(row.at_ms().unwrap() - start),
                    format_latency(row.ms),
                    row.service_name().len(),
                )
            })
            .collect::<Vec<_>>()
    });

    let big = crate::model::Body {
        text: Some(serde_json::to_string(&serde_json::json!({
            "Items": (0..4_000).map(|i| serde_json::json!({"id": {"S": format!("order-{i}")}, "n": {"N": i.to_string()}})).collect::<Vec<_>>()
        }))
        .unwrap()),
        ..Default::default()
    };
    let (_, doc) = time("decode + highlight 300 KB body (background)", 3, || {
        body::build(
            Some(&big),
            "application/json",
            BodyMode::Decoded,
            &[],
            &theme::DARK,
        )
    });
    println!("  -> {} lines, virtualized", doc.lines.len());
    let _ = std::fs::remove_dir_all(&dir);
}
