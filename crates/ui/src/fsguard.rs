//! A debug-only tripwire that keeps filesystem IO out of render paths, and an
//! opt-in frame-build profiler.
//!
//! Every view's `render` (and every virtualized list's row builder) holds a
//! [`RenderScope`]; every helper in the window that touches the disk calls
//! [`assert_io_allowed`]. In debug builds and tests, IO during a render
//! panics, so a regression is caught the first time it runs.
//!
//! With `LENSLET_PROFILE=1`, the time spent inside render scopes is summed
//! per frame and printed by the root view: how long building the window's
//! elements took.

use std::{
    cell::Cell,
    sync::OnceLock,
    time::{Duration, Instant},
};

thread_local! {
    static IN_RENDER: Cell<u32> = const { Cell::new(0) };
    static FRAME_NS: Cell<u64> = const { Cell::new(0) };
}

fn profiling() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LENSLET_PROFILE").is_some())
}

/// Held for the duration of a `render`.
pub struct RenderScope {
    started: Option<Instant>,
}

impl RenderScope {
    #[must_use]
    pub fn enter() -> Self {
        let outer = IN_RENDER.with(|depth| {
            depth.set(depth.get() + 1);
            depth.get() == 1
        });
        RenderScope {
            started: (outer && profiling()).then(Instant::now),
        }
    }
}

impl Drop for RenderScope {
    fn drop(&mut self) {
        IN_RENDER.with(|depth| depth.set(depth.get().saturating_sub(1)));
        if let Some(started) = self.started {
            let ns = started.elapsed().as_nanos() as u64;
            FRAME_NS.with(|total| total.set(total.get() + ns));
        }
    }
}

/// Whether the current thread is inside a `render`.
pub fn in_render() -> bool {
    IN_RENDER.with(|depth| depth.get() > 0)
}

/// Call at the top of every function that reads or writes the filesystem.
#[track_caller]
pub fn assert_io_allowed() {
    debug_assert!(
        !in_render(),
        "filesystem IO inside a render path; compute it when inputs change instead"
    );
}

static STARTED: OnceLock<Instant> = OnceLock::new();

/// Mark process start (called first thing in `main`).
pub fn mark_start() {
    STARTED.get_or_init(Instant::now);
}

/// With `LENSLET_PROFILE`, print how long after start `what` happened.
pub fn milestone(what: &str) {
    if profiling()
        && let Some(started) = STARTED.get()
    {
        eprintln!(
            "lenslet: {what} after {:.1} ms",
            started.elapsed().as_secs_f64() * 1e3
        );
    }
}

/// [`milestone`], but only the first time for each `what`.
pub fn milestone_once(what: &'static str) {
    if !profiling() {
        return;
    }
    static SEEN: std::sync::Mutex<Vec<&'static str>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.contains(&what) {
        seen.push(what);
        milestone(what);
    }
}

/// With `LENSLET_PROFILE`, print and reset the element-build time of the
/// previous frame. Called by the root view at the start of each frame.
pub fn report_frame() {
    if !profiling() {
        return;
    }
    let ns = FRAME_NS.with(|total| total.replace(0));
    if ns > 0 {
        eprintln!(
            "lenslet: frame build {:.3} ms",
            Duration::from_nanos(ns).as_secs_f64() * 1e3
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_inside_render_panics_in_debug() {
        assert_io_allowed();
        {
            let _scope = RenderScope::enter();
            assert!(in_render());
            let caught = std::panic::catch_unwind(assert_io_allowed);
            assert!(caught.is_err(), "IO during render must trip the guard");
        }
        assert!(!in_render());
        assert_io_allowed();
    }
}
