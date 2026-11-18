//! Lenslet: a single-window live lens over the Lenslet proxy trace.

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
    App, Application, Bounds, WindowBackgroundAppearance, WindowBounds, WindowOptions, prelude::*,
    px, size,
};

fn main() {
    Application::new().run(|cx: &mut App| {
        fonts::install(cx);

        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
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
                view.observe_appearance(window, cx);
                window.focus(&view.root_focus());
                cx.activate(true);
            })
            .expect("activate Lenslet");
    });
}
