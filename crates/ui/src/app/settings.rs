//! Dedicated Settings modal (`⌘,` / `⚙`): Appearance, Accent Color, Language,
//! Inspector Formatting, Storage Management, and Keyboard Shortcuts.

use super::*;
use crate::prefs::{AccentColor, Density, Lang, ThemeMode, TimeFormat};

impl LensletApp {
    pub(super) fn render_settings_modal(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme::p();
        let runs_dir = self.runs_dir();
        let (run_count, run_bytes) = trace::storage_stats(&runs_dir);
        let dir_label = display_trace_path(Some(&runs_dir));

        let theme_seg =
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .children(ThemeMode::ALL.into_iter().map(|mode| {
                    let active = self.prefs.theme == mode;
                    let id = match mode {
                        ThemeMode::System => "pref-theme-system",
                        ThemeMode::Light => "pref-theme-light",
                        ThemeMode::Dark => "pref-theme-dark",
                    };
                    setting_pill(id, i18n::theme_label(mode), active)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_theme_mode(mode, cx)))
                }));

        let accent_row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.0))
            .children(AccentColor::ALL.into_iter().map(|acc| {
                let active = self.prefs.accent == acc;
                let swatch = theme::accent_swatch(acc);
                let id = match acc {
                    AccentColor::Sky => "pref-acc-sky",
                    AccentColor::Indigo => "pref-acc-indigo",
                    AccentColor::Violet => "pref-acc-violet",
                    AccentColor::Emerald => "pref-acc-emerald",
                    AccentColor::Amber => "pref-acc-amber",
                    AccentColor::Rose => "pref-acc-rose",
                };
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(26.0))
                    .px(px(9.0))
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(if active { rgb(swatch) } else { rgb(t.border) })
                    .bg(if active {
                        theme::h(swatch, 0.12)
                    } else {
                        theme::h(t.text, 0.02)
                    })
                    .text_size(px(11.5))
                    .font_weight(if active {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::NORMAL
                    })
                    .text_color(if active { rgb(t.text) } else { rgb(t.text_dim) })
                    .cursor_pointer()
                    .hover(move |s| s.bg(theme::h(swatch, 0.08)).text_color(rgb(t.text)))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_accent_color(acc, cx)))
                    .child(glyph::dot(rgb(swatch), 8.0))
                    .child(i18n::accent_label(acc))
            }));

        let density_seg = div().flex().items_center().gap(px(4.0)).children(
            [Density::Comfortable, Density::Compact]
                .into_iter()
                .map(|d| {
                    let active = self.prefs.density == d;
                    let id = match d {
                        Density::Comfortable => "pref-density-comfy",
                        Density::Compact => "pref-density-compact",
                    };
                    setting_pill(id, i18n::density_label(d), active)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_density(d, cx)))
                }),
        );

        let lang_seg = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(
                setting_pill("pref-lang-zh", "简体中文", self.prefs.lang == Lang::Zh)
                    .on_click(cx.listener(|this, _, _, cx| this.set_language(Lang::Zh, cx))),
            )
            .child(
                setting_pill("pref-lang-en", "English", self.prefs.lang == Lang::En)
                    .on_click(cx.listener(|this, _, _, cx| this.set_language(Lang::En, cx))),
            );

        let time_seg = div().flex().items_center().gap(px(4.0)).children(
            [TimeFormat::Relative, TimeFormat::Clock]
                .into_iter()
                .map(|fmt| {
                    let active = self.prefs.time_format == fmt;
                    let id = match fmt {
                        TimeFormat::Relative => "pref-time-rel",
                        TimeFormat::Clock => "pref-time-clock",
                    };
                    setting_pill(id, i18n::time_format_label(fmt), active)
                        .on_click(cx.listener(move |this, _, _, cx| this.set_time_format(fmt, cx)))
                }),
        );

        let body_seg = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(
                setting_pill(
                    "pref-body-fmt",
                    i18n::pick("Formatted", "自动格式化"),
                    !self.prefs.raw_body_default,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_raw_body_default(false, cx))),
            )
            .child(
                setting_pill(
                    "pref-body-raw",
                    i18n::pick("Raw Wire", "原始报文"),
                    self.prefs.raw_body_default,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_raw_body_default(true, cx))),
            );

        let wrap_seg = div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(
                setting_pill(
                    "pref-wrap-on",
                    i18n::pick("Wrap Lines", "自动换行"),
                    self.prefs.word_wrap,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_word_wrap(true, cx))),
            )
            .child(
                setting_pill(
                    "pref-wrap-off",
                    i18n::pick("Horizontal Scroll", "横向滚动"),
                    !self.prefs.word_wrap,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_word_wrap(false, cx))),
            );

        let storage_info = if i18n::is_zh() {
            format!(
                "{dir_label} · {run_count} 个运行文件 · {}",
                format_bytes(run_bytes as usize)
            )
        } else {
            format!(
                "{dir_label} · {run_count} {} · {}",
                if run_count == 1 { "run" } else { "runs" },
                format_bytes(run_bytes as usize)
            )
        };
        let copy_dir_str = runs_dir.display().to_string();
        let storage_actions = div()
            .flex()
            .items_center()
            .gap(px(6.0))
            .child(
                action_button("settings-copy-dir", i18n::pick("Copy Path", "复制路径")).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.copy_text("path", copy_dir_str.clone(), cx);
                    }),
                ),
            )
            .child(
                action_button(
                    "settings-clear-runs",
                    i18n::pick("Clear Finished Runs", "清理历史运行记录"),
                )
                .on_click(cx.listener(|this, _, _, cx| this.clear_historical_runs(cx))),
            );

        let cmd = if cfg!(target_os = "macos") {
            "⌘"
        } else {
            "Ctrl+"
        };
        let shortcuts = [
            (
                format!("{cmd}K"),
                i18n::pick("Command Palette", "打开极速命令面板"),
            ),
            (
                format!("{cmd}F  /  /"),
                i18n::pick(
                    "Search stream & highlight payloads",
                    "搜索过滤并高亮报文命中词",
                ),
            ),
            (
                format!("{cmd}⏎"),
                i18n::pick(
                    "Replay selected call against emulator",
                    "向模拟器重放当前请求",
                ),
            ),
            (
                format!("{cmd}1  {cmd}2  {cmd}3"),
                i18n::pick("Switch Calls / Access / Diff", "切换请求流 / 权限 / 对比"),
            ),
            (
                format!("{cmd},"),
                i18n::pick("Open Settings", "打开设置面板"),
            ),
            (
                format!("{cmd}B"),
                i18n::pick("Toggle Inspector", "展开/收起详情栏"),
            ),
            (
                "Space".to_string(),
                i18n::pick("Pause / Resume live tail", "暂停 / 继续实时流"),
            ),
            (
                "[  /  ]".to_string(),
                i18n::pick("Previous / Next run", "切换上一 / 下一次运行"),
            ),
            (
                "1  2  3".to_string(),
                i18n::pick("Switch Inspector tabs", "切换检查器子标签"),
            ),
            (
                "c".to_string(),
                i18n::pick("Copy selected call as curl", "复制当前请求为 curl"),
            ),
        ];
        let shortcut_grid = div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .p(px(10.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(t.border_subtle))
            .bg(rgb(t.code))
            .children(shortcuts.into_iter().map(|(k, desc)| {
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(11.5))
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(10.5))
                            .px(px(6.0))
                            .py(px(1.5))
                            .rounded(px(4.0))
                            .border_1()
                            .border_color(rgb(t.border))
                            .bg(rgb(t.panel))
                            .text_color(rgb(t.text_soft))
                            .child(k),
                    )
                    .child(div().text_color(rgb(t.text_dim)).child(desc))
            }));

        let card = div()
            .id("settings-card")
            .w(px(560.0))
            .max_h(px(640.0))
            .flex()
            .flex_col()
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(t.border_strong))
            .bg(rgb(t.panel))
            .shadow(theme::elevation_shadow())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(18.0))
                    .h(px(46.0))
                    .border_b_1()
                    .border_color(rgb(t.border_subtle))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(glyph::gear_icon(theme::h(t.accent, 0.95)))
                            .child(
                                div()
                                    .text_size(px(14.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(t.text))
                                    .child(i18n::pick("Settings", "偏好设置")),
                            ),
                    )
                    .child(
                        action_button("close-settings", i18n::pick("Done", "完成")).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.settings_open = false;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .scrollbar_width(px(6.0))
                    .p(px(18.0))
                    .flex()
                    .flex_col()
                    .gap(px(18.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(micro_label(i18n::pick("APPEARANCE", "外观与主题")))
                            .child(setting_row(
                                i18n::pick("Theme Mode", "外观模式"),
                                i18n::pick(
                                    "Follow OS appearance or pin Light / Dark.",
                                    "跟随操作系统深浅色，或固定浅色 / 深色表面。",
                                ),
                                theme_seg,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(6.0))
                                    .py(px(4.0))
                                    .child(
                                        div()
                                            .text_size(px(12.0))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(rgb(t.text_soft))
                                            .child(i18n::pick("Accent Color", "主体强调色")),
                                    )
                                    .child(accent_row),
                            )
                            .child(setting_row(
                                i18n::pick("Row Density", "列表密度"),
                                i18n::pick(
                                    "Compact fits ~35% more calls vertically.",
                                    "紧凑模式可在同一屏幕高度内多展示约 35% 的请求行。",
                                ),
                                density_seg,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.0))
                            .child(micro_label(i18n::pick(
                                "LANGUAGE & INSPECTOR",
                                "语言与检查器显示",
                            )))
                            .child(setting_row(
                                i18n::pick("Interface Language", "界面语言"),
                                i18n::pick(
                                    "AWS, IAM, and HTTP technical parameters stay canonical.",
                                    "仅汉化桌面端界面框架；AWS、IAM 与 HTTP 专业参数保持英文原名。",
                                ),
                                lang_seg,
                            ))
                            .child(setting_row(
                                i18n::pick("Time Column", "时间列主格式"),
                                i18n::pick(
                                    "Primary timestamp shown on each call row.",
                                    "请求流首列优先展示相对运行起点偏移，或本地时钟时间。",
                                ),
                                time_seg,
                            ))
                            .child(setting_row(
                                i18n::pick("Default Payload View", "默认 Body 展示"),
                                i18n::pick(
                                    "Auto-indent JSON/XML or show raw wire text.",
                                    "默认自动缩进 JSON/XML，或直接显示未格化的原始报文。",
                                ),
                                body_seg,
                            ))
                            .child(setting_row(
                                i18n::pick("Line Wrapping", "代码自动换行"),
                                i18n::pick(
                                    "Wrap long JSON lines and header values to fit Inspector.",
                                    "在检查器内自动折行显示长 JSON 与 Header 值。",
                                ),
                                wrap_seg,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(micro_label(i18n::pick("STORAGE & RUNS", "运行记录与存储")))
                            .child(setting_row(
                                i18n::pick("Local Run History", "本地运行记录目录"),
                                &storage_info,
                                storage_actions,
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(micro_label(i18n::pick("KEYBOARD SHORTCUTS", "快捷键参考")))
                            .child(shortcut_grid),
                    )
                    .child(
                        div()
                            .font_family(fonts::mono())
                            .text_size(px(10.0))
                            .text_color(rgb(t.text_faint))
                            .child(format!(
                                "Lenslet {} · ~/.lenslet/ui.json",
                                env!("CARGO_PKG_VERSION")
                            )),
                    ),
            );

        div()
            .id("settings-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x0000_0078))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.settings_open = false;
                    cx.notify();
                }),
            )
            .child(card)
    }
}

fn setting_row(title: &str, subtitle: &str, control: impl IntoElement) -> Div {
    let t = theme::p();
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(12.0))
        .py(px(6.0))
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
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(t.text_soft))
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .line_height(px(15.0))
                        .text_color(rgb(t.text_muted))
                        .child(subtitle.to_string()),
                ),
        )
        .child(div().flex_shrink_0().child(control))
}

fn setting_pill(id: &'static str, label: &'static str, active: bool) -> Stateful<Div> {
    let t = theme::p();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .h(px(25.0))
        .px(px(10.0))
        .rounded(px(5.0))
        .border_1()
        .border_color(if active { rgb(t.accent) } else { rgb(t.border) })
        .bg(if active {
            theme::h(t.accent, 0.14)
        } else {
            theme::h(t.text, 0.02)
        })
        .text_size(px(11.0))
        .font_weight(if active {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .text_color(if active {
            rgb(t.accent)
        } else {
            rgb(t.text_dim)
        })
        .cursor_pointer()
        .hover(move |s| s.bg(theme::h(t.text, 0.06)).text_color(rgb(t.text)))
        .child(label)
}
