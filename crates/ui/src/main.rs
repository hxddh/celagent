//! Lenslet: a precision workbench over the Lenslet proxy trace.

mod app;
mod demo;
mod fonts;
mod glyph;
mod i18n;
mod prefs;
mod search;
mod theme;
mod trace;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, prelude::*, px, size,
};

use crate::app::{OpenSettings, QuitApp, ReloadTrace, ToggleInspector};

fn main() {
    Application::new().run(|cx: &mut App| {
        fonts::install(cx);

        cx.bind_keys([
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("ctrl-,", OpenSettings, None),
            KeyBinding::new("cmd-b", ToggleInspector, None),
            KeyBinding::new("ctrl-b", ToggleInspector, None),
            KeyBinding::new("cmd-r", ReloadTrace, None),
            KeyBinding::new("ctrl-r", ReloadTrace, None),
            KeyBinding::new("cmd-q", QuitApp, None),
        ]);
        cx.on_action(|_: &QuitApp, cx| cx.quit());
        cx.set_menus(vec![
            Menu {
                name: "Lenslet".into(),
                items: vec![
                    MenuItem::action("Settings…", OpenSettings),
                    MenuItem::separator(),
                    MenuItem::action("Quit Lenslet", QuitApp),
                ],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Toggle Inspector", ToggleInspector),
                    MenuItem::action("Reload Trace", ReloadTrace),
                ],
            },
        ]);

        let saved = prefs::load(&app::home());
        let width = saved.window_width.unwrap_or(1320.0).clamp(960.0, 3840.0);
        let height = saved.window_height.unwrap_or(820.0).clamp(640.0, 2400.0);
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
                        ..Default::default()
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
