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
            ui.t("Run Command Through Lenslet…", "在 Lenslet 中发起运行…")
                .into(),
            Some(i18n::shortcut("N")),
            Cmd::OpenLauncher,
        );
        add(
            ui.t("Compose New Local AWS Request…", "新建本地 AWS 请求…")
                .into(),
            Some(i18n::shortcut("⇧N")),
            Cmd::NewRequest,
        );
        add(
            ui.t("Open Workspace Folder…", "打开项目工作区目录…").into(),
            Some(i18n::shortcut("⌥O")),
            Cmd::OpenWorkspace,
        );
        add(
            ui.t(
                "Generate GitHub Actions CI Workflow (.github/workflows/lenslet.yml)",
                "生成 GitHub Actions CI 工作流 (.github/workflows/lenslet.yml)",
            )
            .into(),
            None,
            Cmd::InitCiWorkflow,
        );
        if self.managed_run.is_some() {
            add(
                ui.t("Stop Active Command Run", "停止当前运行的命令").into(),
                Some(i18n::shortcut(".")),
                Cmd::StopActiveRun,
            );
        }
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
            ui.t(
                "Show Output & Run Telemetry",
                "显示终端输出与全量运行元数据",
            )
            .into(),
            Some(i18n::shortcut("3")),
            Cmd::Mode(Mode::Console),
        );
        add(
            ui.t("Search", "搜索").into(),
            Some("/".into()),
            Cmd::FocusSearch,
        );
        add(
            ui.t("Toggle Workspace Sidebar", "显示/隐藏工作区侧栏")
                .into(),
            Some(i18n::shortcut("0")),
            Cmd::ToggleSidebar,
        );
        add(
            ui.t("Toggle Inspector", "显示/隐藏详情栏").into(),
            Some(i18n::shortcut("B")),
            Cmd::ToggleInspector,
        );
        add(
            ui.t("Open Trace File…", "打开追踪文件 (.jsonl)…").into(),
            Some(i18n::shortcut("O")),
            Cmd::OpenFile,
        );
        add(
            ui.t("Export Run Summary JSON…", "导出运行摘要 JSON…")
                .into(),
            Some(i18n::shortcut("S")),
            Cmd::ExportReport,
        );
        if session.path.is_some() {
            add(
                ui.t(
                    "Reveal Trace File in File Manager",
                    "在文件管理器中显示追踪文件",
                )
                .into(),
                Some(i18n::shortcut("⇧R")),
                Cmd::RevealTrace,
            );
        }
        if self.managed_proxy.is_some() {
            add(
                ui.t("Stop Managed Local Proxy", "停止托管本地代理").into(),
                None,
                Cmd::StopProxy,
            );
        } else if !self.env_probe.running {
            add(
                ui.t("Start Local Proxy (:9297)", "启动本地代理 (:9297)")
                    .into(),
                None,
                Cmd::StartProxy,
            );
        }
        add(
            ui.t("Copy Proxy Shell Environment", "复制代理终端环境变量")
                .into(),
            None,
            Cmd::CopyProxyEnv(crate::daemon::ShellFormat::Sh),
        );
        if session.evicted > 0 && !session.store.is_unbounded() {
            add(
                if ui.zh() {
                    format!("无损加载全部调用记录（+{}）", session.evicted)
                } else {
                    format!("Load All Calls Without Cap (+{})", session.evicted)
                },
                None,
                Cmd::LoadAllCalls,
            );
        }
        if self.inspector.read(cx).call().is_some() {
            add(
                ui.t("Save Selected Payload…", "保存当前请求/响应报文…")
                    .into(),
                Some(i18n::shortcut("⌥S")),
                Cmd::SavePayload,
            );
        }
        add(
            ui.t("About Lenslet", "关于 Lenslet").into(),
            None,
            Cmd::OpenAbout,
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
            add(
                ui.t(
                    "Edit & Replay Selected Call (SigV4 Composer)",
                    "编辑并重放选中的请求 (SigV4 Composer)",
                )
                .into(),
                Some(i18n::shortcut("⇧⏎")),
                Cmd::OpenComposer,
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
            .map(|row| (row.access.clone(), row.implied))
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
            ui.t(
                "Install CLI to PATH (~/.local/bin)",
                "安装 CLI 到 PATH (~/.local/bin)",
            )
            .into(),
            None,
            Cmd::InstallCli,
        );
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
                    .child(ui.t("↑↓  navigate", "↑↓  选择"))
                    .child(ui.t("⏎  run", "⏎  执行"))
                    .child(ui.t("esc  close", "esc  关闭")),
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
