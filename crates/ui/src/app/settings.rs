//! Settings (⌘,): Appearance, Accent, Language, Density and Storage. Renders
//! from memory only; storage stats are read in the background when opened.

use gpui::{
    AnyElement, Context, Div, ElementId, FontWeight, IntoElement, div, prelude::*, px, rgb,
};

use super::{Cmd, LensletApp, widgets};
use crate::{
    i18n,
    model::format_bytes,
    prefs::{AccentColor, Density, Lang, ThemeMode},
    theme::{self, Ui, text},
};

fn row(ui: &Ui, title: &str, detail: &str, control: impl IntoElement) -> Div {
    let t = ui.p;
    div()
        .flex()
        .items_center()
        .gap(px(16.0))
        .py(px(12.0))
        .border_b_1()
        .border_color(rgb(t.border_subtle))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(text::LG))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(t.text))
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(text::SM))
                        .text_color(rgb(t.text_muted))
                        .child(detail.to_string()),
                ),
        )
        .child(control)
}

impl LensletApp {
    pub(super) fn render_settings(&self, ui: &Ui, cx: &mut Context<Self>) -> AnyElement {
        let t = ui.p;
        let prefs = &self.prefs;
        let entity = cx.entity();
        let segmented =
            |ids: &[&'static str], labels: Vec<String>, active: usize, cmds: Vec<Cmd>| {
                let entity = entity.clone();
                let segments = ids
                    .iter()
                    .zip(labels)
                    .enumerate()
                    .map(|(index, (id, label))| widgets::Segment {
                        id,
                        label: label.into(),
                        active: index == active,
                        tooltip: None,
                    })
                    .collect();
                widgets::segmented(ui, segments, move |index, window, cx| {
                    let cmd = cmds[index].clone();
                    entity.update(cx, |app, cx| app.run_cmd(cmd, window, cx));
                })
            };

        let theme_ix = ThemeMode::ALL
            .iter()
            .position(|m| *m == prefs.theme)
            .unwrap_or(0);
        let appearance = segmented(
            &["theme-system", "theme-light", "theme-dark"],
            ThemeMode::ALL
                .iter()
                .map(|m| i18n::theme_label(*m, ui.lang).to_string())
                .collect(),
            theme_ix,
            ThemeMode::ALL.iter().map(|m| Cmd::Theme(*m)).collect(),
        );
        let lang = segmented(
            &["lang-en", "lang-zh"],
            vec!["English".into(), "中文".into()],
            usize::from(prefs.lang == Lang::Zh),
            vec![Cmd::Lang(Lang::En), Cmd::Lang(Lang::Zh)],
        );
        let density = segmented(
            &["density-comfortable", "density-compact"],
            vec![
                i18n::density_label(Density::Comfortable, ui.lang).into(),
                i18n::density_label(Density::Compact, ui.lang).into(),
            ],
            usize::from(prefs.density == Density::Compact),
            vec![
                Cmd::Density(Density::Comfortable),
                Cmd::Density(Density::Compact),
            ],
        );

        let mut accents = div().flex().items_center().gap(px(8.0));
        for (index, accent) in AccentColor::ALL.into_iter().enumerate() {
            let swatch = theme::accent_tokens(ui.appearance, accent).0;
            let active = prefs.accent == accent;
            accents = accents.child(
                div()
                    .id(ElementId::named_usize("accent", index))
                    .size(px(24.0))
                    .rounded_full()
                    .border_2()
                    .border_color(if active {
                        theme::h(t.text, 0.9)
                    } else {
                        theme::h(t.text, 0.0)
                    })
                    .p(px(3.0))
                    .cursor_pointer()
                    .tooltip(widgets::tip(i18n::accent_label(accent, ui.lang), None))
                    .on_click(cx.listener(move |app, _, window, cx| {
                        app.run_cmd(Cmd::Accent(accent), window, cx)
                    }))
                    .child(div().size_full().rounded_full().bg(rgb(swatch))),
            );
        }

        let storage_text = match self.storage {
            Some((count, bytes)) => {
                if ui.zh() {
                    format!("{count} 个运行，{}", format_bytes(bytes as usize))
                } else {
                    format!("{count} runs, {}", format_bytes(bytes as usize))
                }
            }
            None if self.session.read(cx).runs_dir().is_some() => {
                ui.t("Measuring…", "统计中…").into()
            }
            None => ui
                .t("Not available for this source", "当前来源不可用")
                .into(),
        };
        let session = self.session.clone();
        let clear = self.session.read(cx).runs_dir().is_some().then(|| {
            widgets::danger_button(ui, "clear-runs", ui.t("Clear finished runs", "清理已结束的运行"))
                .on_click(cx.listener(move |app, _, _, cx| {
                    session.update(cx, |session, cx| session.clear_finished_runs(cx));
                    app.storage = None;
                    app.open_settings(cx);
                }))
        });

        let card = div()
            .id("settings")
            .w(px(560.0))
            .flex()
            .flex_col()
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.panel_raised))
            .shadow(ui.elevation())
            .on_mouse_down_out(cx.listener(|app, _, _, cx| {
                app.settings_open = false;
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(48.0))
                    .px(px(20.0))
                    .border_b_1()
                    .border_color(rgb(t.border_subtle))
                    .child(
                        div()
                            .text_size(px(text::XL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(ui.t("Settings", "设置")),
                    )
                    .child(div().flex_1())
                    .child(
                        widgets::button(ui, "close-settings", ui.t("Done", "完成")).on_click(
                            cx.listener(|app, _, _, cx| {
                                app.settings_open = false;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(px(20.0))
                    .pb(px(8.0))
                    .child(row(
                        ui,
                        ui.t("Appearance", "外观"),
                        ui.t("Follow the system, or pick one.", "跟随系统或手动选择。"),
                        appearance,
                    ))
                    .child(row(
                        ui,
                        ui.t("Accent", "强调色"),
                        ui.t("Selection and highlights.", "选中与高亮的颜色。"),
                        accents,
                    ))
                    .child(row(
                        ui,
                        ui.t("Language", "语言"),
                        ui.t(
                            "AWS and IAM terms stay in English.",
                            "AWS 与 IAM 术语保持英文。",
                        ),
                        lang,
                    ))
                    .child(row(
                        ui,
                        ui.t("Density", "密度"),
                        ui.t("Row height in lists.", "列表行高。"),
                        density,
                    ))
                    .child(row(
                        ui,
                        ui.t("Storage", "存储"),
                        &format!("~/.lenslet/runs · {storage_text}"),
                        div().children(clear),
                    )),
            );
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::h(0x000000, if ui.dark() { 0.40 } else { 0.18 }))
            .child(widgets::appear(ui, "settings-anim", card))
            .into_any_element()
    }
}
