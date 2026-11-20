//! Lenslet: a native window over Lenslet run files.

mod app;
mod body;
mod daemon;
mod demo;
mod feed;
mod fonts;
mod fsguard;
mod glyph;
mod i18n;
mod icons;
mod lockfile;
mod model;
mod prefs;
mod replay;
mod search;
mod sigv4;
mod sse;
mod store;
mod syntax;
#[cfg(test)]
mod testdata;
mod theme;

#[cfg(test)]
mod bench;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, prelude::*, px, size,
};

use crate::app::*;

/// Context predicates for the key map.
const LIST: &str = "LensletApp && !Search && !Palette && !Body && !ComposerField";
const NOT_TYPING: &str = "LensletApp && !Search && !Palette && !ComposerField";

/// The whole key map. Every `cmd-` binding also gets a `ctrl-` twin off macOS.
fn key_bindings() -> Vec<KeyBinding> {
    type Make = Box<dyn Fn(&str) -> KeyBinding>;
    let primary: Vec<(&str, Make)> = vec![
        (
            "k",
            Box::new(|k| KeyBinding::new(k, OpenPalette, Some("LensletApp"))),
        ),
        (
            "f",
            Box::new(|k| KeyBinding::new(k, FocusSearch, Some("LensletApp"))),
        ),
        (
            ",",
            Box::new(|k| KeyBinding::new(k, OpenSettings, Some("LensletApp"))),
        ),
        (
            "0",
            Box::new(|k| KeyBinding::new(k, ToggleSidebar, Some("LensletApp"))),
        ),
        (
            "b",
            Box::new(|k| KeyBinding::new(k, ToggleInspector, Some("LensletApp"))),
        ),
        (
            "o",
            Box::new(|k| KeyBinding::new(k, OpenFile, Some("LensletApp"))),
        ),
        (
            "s",
            Box::new(|k| KeyBinding::new(k, ExportReport, Some("LensletApp"))),
        ),
        (
            "shift-r",
            Box::new(|k| KeyBinding::new(k, RevealTrace, Some("LensletApp"))),
        ),
        (
            "g",
            Box::new(|k| KeyBinding::new(k, NextMatch, Some("LensletApp"))),
        ),
        (
            "shift-g",
            Box::new(|k| KeyBinding::new(k, PrevMatch, Some("LensletApp"))),
        ),
        (
            "r",
            Box::new(|k| KeyBinding::new(k, ReloadRun, Some("LensletApp"))),
        ),
        (
            "1",
            Box::new(|k| KeyBinding::new(k, ShowCalls, Some("LensletApp"))),
        ),
        (
            "2",
            Box::new(|k| KeyBinding::new(k, ShowAccess, Some("LensletApp"))),
        ),
        (
            "[",
            Box::new(|k| KeyBinding::new(k, PrevRun, Some("LensletApp"))),
        ),
        (
            "]",
            Box::new(|k| KeyBinding::new(k, NextRun, Some("LensletApp"))),
        ),
        (
            "shift-c",
            Box::new(|k| KeyBinding::new(k, CopyCurl, Some(NOT_TYPING))),
        ),
        (
            "shift-e",
            Box::new(|k| KeyBinding::new(k, CopyPolicy, Some("LensletApp"))),
        ),
        (
            "enter",
            Box::new(|k| KeyBinding::new(k, Replay, Some(NOT_TYPING))),
        ),
        (
            "shift-enter",
            Box::new(|k| KeyBinding::new(k, OpenComposer, Some(NOT_TYPING))),
        ),
        ("c", Box::new(|k| KeyBinding::new(k, Copy, Some("Body")))),
        (
            "a",
            Box::new(|k| KeyBinding::new(k, SelectAll, Some("Body"))),
        ),
        ("q", Box::new(|k| KeyBinding::new(k, Quit, None))),
    ];
    let mut bindings = Vec::new();
    for (key, make) in &primary {
        bindings.push(make(&format!("cmd-{key}")));
        if !cfg!(target_os = "macos") {
            bindings.push(make(&format!("ctrl-{key}")));
        }
    }
    bindings.extend([
        KeyBinding::new("/", FocusSearch, Some(NOT_TYPING)),
        KeyBinding::new("j", SelectNext, Some(LIST)),
        KeyBinding::new("k", SelectPrev, Some(LIST)),
        KeyBinding::new("down", SelectNext, Some(LIST)),
        KeyBinding::new("up", SelectPrev, Some(LIST)),
        KeyBinding::new("pagedown", PageDown, Some(LIST)),
        KeyBinding::new("pageup", PageUp, Some(LIST)),
        KeyBinding::new("home", SelectFirst, Some(LIST)),
        KeyBinding::new("end", SelectLast, Some(LIST)),
        KeyBinding::new("enter", Confirm, Some(NOT_TYPING)),
        KeyBinding::new("escape", Cancel, Some(NOT_TYPING)),
        // While typing a search, ↑/↓ still move through the results.
        KeyBinding::new("down", SelectNext, Some("Search")),
        KeyBinding::new("up", SelectPrev, Some("Search")),
        KeyBinding::new("down", SelectNext, Some("Palette")),
        KeyBinding::new("up", SelectPrev, Some("Palette")),
        KeyBinding::new("ctrl-n", SelectNext, Some("Palette")),
        KeyBinding::new("ctrl-p", SelectPrev, Some("Palette")),
        KeyBinding::new("enter", Confirm, Some("Palette")),
        KeyBinding::new("escape", Cancel, Some("Palette")),
    ]);
    bindings
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Lenslet".into(),
            items: vec![
                MenuItem::action("Command Palette…", OpenPalette),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Quit Lenslet", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open Trace File…", OpenFile),
                MenuItem::action("Export Run Summary…", ExportReport),
                MenuItem::action("Reveal Trace File", RevealTrace),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Search", FocusSearch),
                MenuItem::action("Find Next Match", NextMatch),
                MenuItem::action("Find Previous Match", PrevMatch),
                MenuItem::separator(),
                MenuItem::action("Copy Call as curl", CopyCurl),
                MenuItem::action("Copy IAM Policy JSON", CopyPolicy),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Calls", ShowCalls),
                MenuItem::action("Access", ShowAccess),
                MenuItem::separator(),
                MenuItem::action("Toggle Workspace Sidebar", ToggleSidebar),
                MenuItem::action("Toggle Inspector", ToggleInspector),
                MenuItem::action("Reload Run", ReloadRun),
            ],
        },
        Menu {
            name: "Run".into(),
            items: vec![
                MenuItem::action("Replay Selected Call", Replay),
                MenuItem::action("Edit & Replay Call…", OpenComposer),
                MenuItem::separator(),
                MenuItem::action("Previous Run", PrevRun),
                MenuItem::action("Next Run", NextRun),
                MenuItem::action("Follow Newest Run", FollowLatest),
            ],
        },
    ]
}

/// `LENSLET_LOG=1` prints gpui's own log (renderer, platform) to stderr.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "[{}] {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }
    fn flush(&self) {}
}

fn main() {
    fsguard::mark_start();
    if std::env::var_os("LENSLET_LOG").is_some() {
        static LOGGER: StderrLog = StderrLog;
        let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
    }
    Application::new()
        .with_assets(icons::Assets)
        .run(|cx: &mut App| {
            fonts::install(cx);
            cx.bind_keys(key_bindings());
            cx.on_action(|_: &Quit, cx| cx.quit());
            // The native menu bar is a macOS idiom; elsewhere every menu command
            // is in the command palette.
            if cfg!(target_os = "macos") {
                cx.set_menus(menus());
            }

            let saved = prefs::load(&app::home());
            let width = saved.window_width.unwrap_or(1340.0).clamp(960.0, 3840.0);
            let height = saved.window_height.unwrap_or(840.0).clamp(640.0, 2400.0);
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);

            // A unified, transparent titlebar on macOS; normal decorations
            // elsewhere (a transparent one would stack two title bars on Linux).
            let titlebar = if cfg!(target_os = "macos") {
                gpui::TitlebarOptions {
                    title: Some("Lenslet".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(gpui::point(px(14.0), px(14.0))),
                }
            } else {
                gpui::TitlebarOptions {
                    title: Some("Lenslet".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }
            };
            let window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        window_min_size: Some(size(px(960.0), px(640.0))),
                        window_background: WindowBackgroundAppearance::Opaque,
                        app_id: Some("lenslet".to_string()),
                        titlebar: Some(titlebar),
                        ..Default::default()
                    },
                    |window, cx| cx.new(|cx| LensletApp::new(saved, window, cx)),
                )
                .expect("open Lenslet window");
            window
                .update(cx, |_, _, cx| cx.activate(true))
                .expect("activate Lenslet");
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_binding_has_a_ctrl_twin_off_macos() {
        let bindings = key_bindings();
        let cmd = bindings
            .iter()
            .filter(|b| b.keystrokes()[0].modifiers().platform)
            .count();
        if !cfg!(target_os = "macos") {
            let ctrl = bindings
                .iter()
                .filter(|b| b.keystrokes()[0].modifiers().control)
                .count();
            assert!(ctrl >= cmd, "{ctrl} ctrl bindings for {cmd} cmd bindings");
        }
        assert!(cmd >= 14);
    }
}
