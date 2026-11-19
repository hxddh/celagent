//! Lenslet: a precision workbench over the Lenslet proxy trace.

mod app;
mod demo;
mod fonts;
mod glyph;
mod i18n;
mod prefs;
mod replay;
mod search;
mod syntax;
mod theme;
mod trace;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, point, prelude::*, px, size,
};

use crate::app::{
    CopyCurlAction, ExportPolicyAction, FocusSearch, FollowLatestAction, NextRunAction,
    OpenPalette, OpenSettings, PrevRunAction, QuitApp, ReloadTrace, ReplaySelectedAction,
    SwitchAccess, SwitchCalls, SwitchDiff, ToggleInspector,
};

fn main() {
    Application::new().run(|cx: &mut App| {
        fonts::install(cx);

        cx.bind_keys([
            KeyBinding::new("cmd-k", OpenPalette, None),
            KeyBinding::new("ctrl-k", OpenPalette, None),
            KeyBinding::new("cmd-f", FocusSearch, None),
            KeyBinding::new("ctrl-f", FocusSearch, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("ctrl-,", OpenSettings, None),
            KeyBinding::new("cmd-b", ToggleInspector, None),
            KeyBinding::new("ctrl-b", ToggleInspector, None),
            KeyBinding::new("cmd-r", ReloadTrace, None),
            KeyBinding::new("ctrl-r", ReloadTrace, None),
            KeyBinding::new("cmd-enter", ReplaySelectedAction, None),
            KeyBinding::new("ctrl-enter", ReplaySelectedAction, None),
            KeyBinding::new("cmd-1", SwitchCalls, None),
            KeyBinding::new("ctrl-1", SwitchCalls, None),
            KeyBinding::new("cmd-2", SwitchAccess, None),
            KeyBinding::new("ctrl-2", SwitchAccess, None),
            KeyBinding::new("cmd-3", SwitchDiff, None),
            KeyBinding::new("ctrl-3", SwitchDiff, None),
            KeyBinding::new("cmd-[", PrevRunAction, None),
            KeyBinding::new("cmd-]", NextRunAction, None),
            KeyBinding::new("cmd-shift-c", CopyCurlAction, None),
            KeyBinding::new("cmd-shift-e", ExportPolicyAction, None),
            KeyBinding::new("cmd-q", QuitApp, None),
        ]);
        cx.on_action(|_: &QuitApp, cx| cx.quit());
        cx.set_menus(vec![
            Menu {
                name: "Lenslet".into(),
                items: vec![
                    MenuItem::action("Command Palette…", OpenPalette),
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::action("Quit Lenslet", QuitApp),
                ],
            },
            Menu {
                name: "Edit".into(),
                items: vec![
                    MenuItem::action("Search & Highlight…", FocusSearch),
                    MenuItem::separator(),
                    MenuItem::action("Copy Selected Call as curl", CopyCurlAction),
                    MenuItem::action("Copy Minimum IAM Policy JSON", ExportPolicyAction),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Calls Stream", SwitchCalls),
                    MenuItem::action("IAM Access", SwitchAccess),
                    MenuItem::action("Run Diff", SwitchDiff),
                    MenuItem::separator(),
                    MenuItem::action("Toggle Inspector", ToggleInspector),
                    MenuItem::action("Reload Trace", ReloadTrace),
                ],
            },
            Menu {
                name: "Run".into(),
                items: vec![
                    MenuItem::action("Replay Selected Call", ReplaySelectedAction),
                    MenuItem::separator(),
                    MenuItem::action("Previous Run", PrevRunAction),
                    MenuItem::action("Next Run", NextRunAction),
                    MenuItem::action("Follow Latest Run", FollowLatestAction),
                ],
            },
        ]);

        let saved = prefs::load(&app::home());
        let width = saved.window_width.unwrap_or(1340.0).clamp(960.0, 3840.0);
        let height = saved.window_height.unwrap_or(840.0).clamp(640.0, 2400.0);
        let bounds = Bounds::centered(None, size(px(width), px(height)), cx);

        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(960.0), px(640.0))),
                    window_background: WindowBackgroundAppearance::Opaque,
                    app_id: Some("lenslet".to_string()),
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("Lenslet".into()),
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(14.0), px(14.0))),
                    }),
                    ..Default::default()
                },
                |_, cx| cx.new(app::LensletApp::new),
            )
            .expect("open Lenslet window");

        window
            .update(cx, |view, window, cx| {
                view.observe_window(window, cx);
                window.focus(&view.root_focus());
                cx.activate(true);
            })
            .expect("activate Lenslet");
    });
}
