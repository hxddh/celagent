//! The command palette (⌘K / Ctrl+K): every command, run and preference in
//! one filterable list. Entries are built from state already in memory when
//! the palette opens or its query changes — never from disk.

use gpui::{AnyElement, Context, ElementId, FontWeight, Window, div, prelude::*, px, rgb};

use super::{Cmd, LensletApp, MenuEntry, Mode, session::Baseline, widgets};
use crate::{
    i18n,
    model::StatusFilter,
    prefs::{AccentColor, Density, Lang, ThemeMode},
    theme::{self, Ui, text},
};

impl LensletApp {
    pub(super) fn toggle_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette_open {
            self.close_palette(window, cx);
            return;
        }
        self.palette_open = true;
        self.settings_open = false;
        self.overlay = None;
        self.palette_index = 0;
        self.palette_input
            .update(cx, |input, cx| input.clear(window, cx));
        self.refresh_palette(cx);
        cx.notify();
    }

    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = false;
        self.focus_list(window, cx);
        cx.notify();
    }

    pub(super) fn palette_step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.palette_entries.len();
        if count > 0 {
            self.palette_index =
                (self.palette_index as isize + delta).clamp(0, count as isize - 1) as usize;
            cx.notify();
        }
    }

    pub(super) fn palette_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entry = self.palette_entries.get(self.palette_index).cloned();
        self.close_palette(window, cx);
        if let Some(entry) = entry {
            self.run_cmd(entry.cmd, window, cx);
        }
    }

    /// Rebuild the filtered entries (on open and on each query change).
    pub(super) fn refresh_palette(&mut self, cx: &mut Context<Self>) {
        if !self.palette_open {
            return;
        }
        let query = self.palette_input.read(cx).query().to_lowercase();
        let tokens: Vec<&str> = query.split_whitespace().collect();
        let entries = self.palette_all(cx);
        self.palette_entries = entries
            .into_iter()
            .filter(|entry| {
                let hay = entry.title.to_lowercase();
                tokens.iter().all(|token| hay.contains(token))
            })
            .take(60)
            .collect();
        self.palette_index = self
            .palette_index
            .min(self.palette_entries.len().saturating_sub(1));
        cx.notify();
    }

    fn palette_all(&self, cx: &Context<Self>) -> Vec<MenuEntry> {
        let ui = Ui::of(cx);
        let session = self.session.read(cx);
        let mut items: Vec<MenuEntry> = Vec::with_capacity(64);
        let mut add = |title: String, hint: Option<String>, cmd: Cmd| {
            items.push(MenuEntry {
                title: title.into(),
                hint,
                cmd,
            });
        };
        add(
            ui.t("Show Calls", "显示请求").into(),
            Some(i18n::shortcut("1")),
            Cmd::Mode(Mode::Calls),
        );
        add(
            ui.t("Show Access", "显示权限").into(),
            Some(i18n::shortcut("2")),
            Cmd::Mode(Mode::Access),
        );
        add(
            ui.t("Search", "搜索").into(),
            Some("/".into()),
            Cmd::FocusSearch,
        );
        add(
            ui.t("Toggle Inspector", "显示/隐藏详情栏").into(),
            Some(i18n::shortcut("B")),
            Cmd::ToggleInspector,
        );
        add(
            ui.t("Reload Run", "重新加载运行").into(),
            Some(i18n::shortcut("R")),
            Cmd::Reload,
        );
        if self.inspector.read(cx).can_replay(cx) {
            add(
                ui.t("Replay Selected Call", "重放选中的请求").into(),
                Some(i18n::shortcut("⏎")),
                Cmd::Replay,
            );
        }
        if let Some(call) = self.inspector.read(cx).call().cloned() {
            add(
                ui.t("Copy Selected Call as curl", "将选中的请求复制为 curl")
                    .into(),
                Some(i18n::shortcut("⇧C")),
                Cmd::CopyCurl(call),
            );
        }
        add(
            ui.t("Copy IAM Policy JSON", "复制 IAM 策略 JSON").into(),
            Some(i18n::shortcut("⇧E")),
            Cmd::CopyPolicy,
        );
        let not_in_lock: Vec<_> = session
            .access_rows
            .iter()
            .filter(|row| row.used && !row.in_lock)
            .map(|row| row.access.clone())
            .collect();
        if session.can_edit_lock() && !not_in_lock.is_empty() {
            add(
                if ui.zh() {
                    format!("将 {} 项新权限写入 lenslet.lock", not_in_lock.len())
                } else {
                    format!(
                        "Accept {} New Permissions into lenslet.lock",
                        not_in_lock.len()
                    )
                },
                None,
                Cmd::Accept(not_in_lock),
            );
        }
        for (status, en, zh) in [
            (StatusFilter::Errors, "Filter: Errors", "筛选：错误"),
            (
                StatusFilter::Changed,
                "Filter: Changed vs Baseline",
                "筛选：与基线不同",
            ),
            (StatusFilter::Retries, "Filter: Retries", "筛选：重试"),
            (StatusFilter::Slow, "Filter: Slow", "筛选：慢请求"),
            (StatusFilter::Blocked, "Filter: Blocked", "筛选：已拦截"),
        ] {
            add(ui.t(en, zh).into(), None, Cmd::FilterStatus(status));
        }
        for facet in session.store.facets() {
            add(
                format!("{} {}", ui.t("Filter: Service", "筛选：服务"), facet.label),
                None,
                Cmd::FilterService(facet.id),
            );
        }
        add(
            ui.t("Clear All Filters", "清除全部筛选").into(),
            Some("Esc".into()),
            Cmd::ClearFilters,
        );
        add(
            ui.t("Compare with lenslet.lock", "对比 lenslet.lock")
                .into(),
            None,
            Cmd::SetBaseline(Baseline::Lock),
        );
        add(
            ui.t("Compare with Previous Run", "对比上一次运行").into(),
            None,
            Cmd::SetBaseline(Baseline::Previous),
        );
        if session.source.runs_dir().is_some() {
            add(
                ui.t("Previous Run", "上一个运行").into(),
                Some(i18n::shortcut("[")),
                Cmd::PrevRun,
            );
            add(
                ui.t("Next Run", "下一个运行").into(),
                Some(i18n::shortcut("]")),
                Cmd::NextRun,
            );
            add(
                ui.t("Follow Newest Run", "跟随最新运行").into(),
                None,
                Cmd::FollowLatest,
            );
        }
        for run in session.runs.iter().take(20) {
            add(
                format!(
                    "{} {} · {}",
                    ui.t("Open Run", "打开运行"),
                    run.id,
                    run.label()
                ),
                None,
                Cmd::PinRun(run.path.clone()),
            );
        }
        for run in session.runs.iter().filter(|r| r.ended()).take(10) {
            add(
                format!("{} {}", ui.t("Compare with Run", "对比运行"), run.id),
                None,
                Cmd::SetBaseline(Baseline::Run(run.path.clone())),
            );
        }
        for mode in ThemeMode::ALL {
            add(
                format!(
                    "{} {}",
                    ui.t("Appearance:", "外观："),
                    i18n::theme_label(mode, ui.lang)
                ),
                None,
                Cmd::Theme(mode),
            );
        }
        for accent in AccentColor::ALL {
            add(
                format!(
                    "{} {}",
                    ui.t("Accent:", "强调色："),
                    i18n::accent_label(accent, ui.lang)
                ),
                None,
                Cmd::Accent(accent),
            );
        }
        for density in [Density::Comfortable, Density::Compact] {
            add(
                format!(
                    "{} {}",
                    ui.t("Density:", "密度："),
                    i18n::density_label(density, ui.lang)
                ),
                None,
                Cmd::Density(density),
            );
        }
        add("Language: English".into(), None, Cmd::Lang(Lang::En));
        add("语言：中文".into(), None, Cmd::Lang(Lang::Zh));
        add(
            ui.t("Settings…", "设置…").into(),
            Some(i18n::shortcut(",")),
            Cmd::OpenSettings,
        );
        add(
            ui.t("Quit Lenslet", "退出 Lenslet").into(),
            Some(i18n::shortcut("Q")),
            Cmd::Quit,
        );
        items
    }

    pub(super) fn render_palette(&self, ui: &Ui, cx: &mut Context<Self>) -> AnyElement {
        let t = ui.p;
        let mut list = div()
            .id("palette-list")
            .flex()
            .flex_col()
            .max_h(px(380.0))
            .overflow_y_scroll()
            .p(px(4.0));
        if self.palette_entries.is_empty() {
            list = list.child(
                div()
                    .p(px(12.0))
                    .text_size(px(text::MD))
                    .text_color(rgb(t.text_muted))
                    .child(ui.t("No matching command", "没有匹配的命令")),
            );
        }
        for (index, entry) in self.palette_entries.iter().enumerate() {
            let cmd = entry.cmd.clone();
            list = list.child(
                widgets::menu_item(
                    ui,
                    ElementId::named_usize("palette-item", index),
                    entry.title.clone(),
                    entry.hint.clone(),
                    index == self.palette_index,
                    false,
                )
                .on_click(cx.listener(move |app, _, window, cx| {
                    app.close_palette(window, cx);
                    app.run_cmd(cmd.clone(), window, cx);
                })),
            );
        }
        let card = div()
            .id("palette")
            .w(px(560.0))
            .flex()
            .flex_col()
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.panel_raised))
            .shadow(ui.elevation())
            .overflow_hidden()
            .on_mouse_down_out(cx.listener(|app, _, window, cx| app.close_palette(window, cx)))
            .child(self.palette_input.clone())
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .h(px(28.0))
                    .px(px(12.0))
                    .border_t_1()
                    .border_color(rgb(t.border_subtle))
                    .text_size(px(text::XS))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(t.text_muted))
                    .child("↑↓  navigate")
                    .child("⏎  run")
                    .child("esc  close"),
            );
        div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .pt(px(88.0))
            .bg(theme::h(0x000000, if ui.dark() { 0.32 } else { 0.12 }))
            .child(widgets::appear(ui, "palette-anim", card))
            .into_any_element()
    }
}
