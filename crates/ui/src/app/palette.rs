//! Native Command Palette (`⌘K` / `Ctrl+K`): zero-chrome fuzzy launcher for
//! switching modes, runs, AWS service filters, IAM lock curation, replay, and preferences.

use super::*;
use crate::prefs::{AccentColor, Density, Lang, ThemeMode};

#[derive(Clone, Debug)]
pub(super) enum PaletteCmd {
    SwitchPane(Pane),
    FilterStatus(StatusFilter),
    FilterService(Option<String>),
    ClearAllFilters,
    ReplaySelected,
    CopySelectedCurl,
    AcceptAllNewLock,
    ExportPolicyJson,
    FollowLatestRun,
    PinRun(PathBuf),
    TogglePause,
    ToggleInspector,
    SetTheme(ThemeMode),
    SetAccent(AccentColor),
    SetDensity(Density),
    SetLang(Lang),
    OpenSettings,
}

#[derive(Clone)]
pub(super) struct PaletteEntry {
    pub category: &'static str,
    pub title: String,
    pub hint: Option<String>,
    pub cmd: PaletteCmd,
}

impl LensletApp {
    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = true;
        self.settings_open = false;
        self.runs_popover_open = false;
        self.palette_index = 0;
        self.palette_input.update(cx, |input, cx| {
            input.clear(window, cx);
        });
        cx.notify();
    }

    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette_open = false;
        window.focus(&self.root_focus);
        cx.notify();
    }

    pub(super) fn palette_entries(&self, cx: &App) -> Vec<PaletteEntry> {
        let cmd_mod = if cfg!(target_os = "macos") {
            "⌘"
        } else {
            "Ctrl+"
        };
        let mut items = Vec::with_capacity(32);

        // Workspace modes
        items.push(PaletteEntry {
            category: i18n::pick("VIEW", "视图"),
            title: i18n::pick("Switch to Calls Stream", "切换到请求流 (Calls)").into(),
            hint: Some(format!("{cmd_mod}1")),
            cmd: PaletteCmd::SwitchPane(Pane::Calls),
        });
        items.push(PaletteEntry {
            category: i18n::pick("VIEW", "视图"),
            title: i18n::pick("Switch to IAM Access", "切换到 IAM 权限表 (Access)").into(),
            hint: Some(format!("{cmd_mod}2")),
            cmd: PaletteCmd::SwitchPane(Pane::Access),
        });
        items.push(PaletteEntry {
            category: i18n::pick("VIEW", "视图"),
            title: i18n::pick("Switch to Run Diff", "切换到回归对比 (Diff)").into(),
            hint: Some(format!("{cmd_mod}3")),
            cmd: PaletteCmd::SwitchPane(Pane::Diff),
        });
        items.push(PaletteEntry {
            category: i18n::pick("VIEW", "视图"),
            title: i18n::pick("Toggle Inspector Sidebar", "展开 / 收起右侧详情栏").into(),
            hint: Some(format!("{cmd_mod}B")),
            cmd: PaletteCmd::ToggleInspector,
        });

        // Call actions & Replay
        if self.can_replay_selected() {
            items.push(PaletteEntry {
                category: i18n::pick("CALL", "请求操作"),
                title: i18n::pick(
                    "Replay Selected Call Against Emulator",
                    "向本地模拟器重放当前请求 (Replay)",
                )
                .into(),
                hint: Some(format!("{cmd_mod}⏎")),
                cmd: PaletteCmd::ReplaySelected,
            });
        }
        if self.selected.is_some() {
            items.push(PaletteEntry {
                category: i18n::pick("CALL", "请求操作"),
                title: i18n::pick("Copy Selected Call as curl", "复制当前请求为 curl").into(),
                hint: Some("c".into()),
                cmd: PaletteCmd::CopySelectedCurl,
            });
        }

        // IAM Lock & Policy curation
        let access = self.access_rows();
        let new_count = access.iter().filter(|e| e.new).count();
        if new_count > 0 && self.can_edit_lock() {
            items.push(PaletteEntry {
                category: "IAM LOCK",
                title: if i18n::is_zh() {
                    format!("一键将 +{new_count} 项新权限写入 lenslet.lock")
                } else {
                    format!("Accept All +{new_count} New Permissions into lenslet.lock")
                },
                hint: Some("lenslet.lock".into()),
                cmd: PaletteCmd::AcceptAllNewLock,
            });
        }
        if access.iter().any(|e| !e.unused && !e.test) {
            items.push(PaletteEntry {
                category: "IAM LOCK",
                title: i18n::pick(
                    "Copy Minimum IAM Policy JSON",
                    "复制最小权限 IAM Policy JSON",
                )
                .into(),
                hint: None,
                cmd: PaletteCmd::ExportPolicyJson,
            });
        }

        // Stream Filters
        items.push(PaletteEntry {
            category: i18n::pick("FILTER", "筛选"),
            title: i18n::pick("Filter: Error Calls Only", "筛选：仅查看失败请求 (Errors)").into(),
            hint: (self.counts.errors > 0).then(|| self.counts.errors.to_string()),
            cmd: PaletteCmd::FilterStatus(StatusFilter::Errors),
        });
        if self.counts.blocked > 0 {
            items.push(PaletteEntry {
                category: i18n::pick("FILTER", "筛选"),
                title: i18n::pick(
                    "Filter: Blocked by --enforce",
                    "筛选：被 --enforce 拦截的请求",
                )
                .into(),
                hint: Some(self.counts.blocked.to_string()),
                cmd: PaletteCmd::FilterStatus(StatusFilter::Blocked),
            });
        }
        if self.counts.retries > 0 {
            items.push(PaletteEntry {
                category: i18n::pick("FILTER", "筛选"),
                title: i18n::pick("Filter: Retried SDK Calls", "筛选：发生 SDK 重试的请求").into(),
                hint: Some(self.counts.retries.to_string()),
                cmd: PaletteCmd::FilterStatus(StatusFilter::Retries),
            });
        }
        if self.call_filter.is_active() || !self.query.is_empty() {
            items.push(PaletteEntry {
                category: i18n::pick("FILTER", "筛选"),
                title: i18n::pick(
                    "Clear All Active Filters & Search",
                    "清除全部筛选与时间切片",
                )
                .into(),
                hint: Some("ESC".into()),
                cmd: PaletteCmd::ClearAllFilters,
            });
        }
        for facet in &self.facets {
            items.push(PaletteEntry {
                category: "AWS SERVICE",
                title: if i18n::is_zh() {
                    format!("按服务筛选：{}", facet.label)
                } else {
                    format!("Filter Service: {}", facet.label)
                },
                hint: Some(format!("{} calls", facet.count)),
                cmd: PaletteCmd::FilterService(Some(facet.id.clone())),
            });
        }

        // Runs
        items.push(PaletteEntry {
            category: i18n::pick("RUNS", "运行会话"),
            title: i18n::pick("Follow Latest Live Run", "实时跟随最新运行会话").into(),
            hint: None,
            cmd: PaletteCmd::FollowLatestRun,
        });
        items.push(PaletteEntry {
            category: i18n::pick("RUNS", "运行会话"),
            title: if self.paused {
                i18n::pick("Resume Live Stream", "继续实时流").into()
            } else {
                i18n::pick("Pause Live Stream", "暂停实时流").into()
            },
            hint: Some("Space".into()),
            cmd: PaletteCmd::TogglePause,
        });
        if let Source::Runs { dir, .. } = &self.source {
            for summary in trace::list_run_summaries(dir).into_iter().take(10) {
                items.push(PaletteEntry {
                    category: i18n::pick("RUNS", "运行会话"),
                    title: format!(
                        "{} · {}",
                        summary.id,
                        trace::ellipsize(&summary.label(), 36)
                    ),
                    hint: summary
                        .report
                        .as_ref()
                        .map(|r| format!("{} calls", r.calls)),
                    cmd: PaletteCmd::PinRun(summary.path),
                });
            }
        }

        // Settings & Theme
        items.push(PaletteEntry {
            category: i18n::pick("SETTINGS", "偏好"),
            title: i18n::pick("Open Settings…", "打开偏好设置面板…").into(),
            hint: Some(format!("{cmd_mod},")),
            cmd: PaletteCmd::OpenSettings,
        });
        for mode in ThemeMode::ALL {
            items.push(PaletteEntry {
                category: i18n::pick("THEME", "外观"),
                title: format!(
                    "{}: {}",
                    i18n::pick("Appearance", "主题模式"),
                    i18n::theme_label(mode)
                ),
                hint: None,
                cmd: PaletteCmd::SetTheme(mode),
            });
        }
        for acc in AccentColor::ALL {
            items.push(PaletteEntry {
                category: i18n::pick("THEME", "外观"),
                title: format!(
                    "{}: {}",
                    i18n::pick("Accent Color", "强调色"),
                    i18n::accent_label(acc)
                ),
                hint: None,
                cmd: PaletteCmd::SetAccent(acc),
            });
        }
        items.push(PaletteEntry {
            category: i18n::pick("SETTINGS", "偏好"),
            title: i18n::pick("Density: Compact Rows", "行高密度：紧凑").into(),
            hint: None,
            cmd: PaletteCmd::SetDensity(Density::Compact),
        });
        items.push(PaletteEntry {
            category: i18n::pick("SETTINGS", "偏好"),
            title: i18n::pick("Density: Comfortable Rows", "行高密度：标准").into(),
            hint: None,
            cmd: PaletteCmd::SetDensity(Density::Comfortable),
        });
        items.push(PaletteEntry {
            category: i18n::pick("SETTINGS", "偏好"),
            title: "Language: English".into(),
            hint: None,
            cmd: PaletteCmd::SetLang(Lang::En),
        });
        items.push(PaletteEntry {
            category: i18n::pick("SETTINGS", "偏好"),
            title: "语言：简体中文".into(),
            hint: None,
            cmd: PaletteCmd::SetLang(Lang::Zh),
        });

        let q = self.palette_input.read(cx).query().trim().to_lowercase();
        if q.is_empty() {
            return items;
        }
        let tokens: Vec<&str> = q.split_whitespace().collect();
        items
            .into_iter()
            .filter(|entry| {
                let hay = format!(
                    "{} {} {}",
                    entry.category,
                    entry.title,
                    entry.hint.as_deref().unwrap_or("")
                )
                .to_lowercase();
                tokens.iter().all(|t| hay.contains(t))
            })
            .collect()
    }

    pub(super) fn execute_palette_cmd(
        &mut self,
        cmd: PaletteCmd,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette_open = false;
        window.focus(&self.root_focus);
        match cmd {
            PaletteCmd::SwitchPane(pane) => self.set_pane(pane, cx),
            PaletteCmd::FilterStatus(st) => {
                self.pane = Pane::Calls;
                self.call_filter.status = st;
                self.refresh_view();
                self.reveal_selected();
            }
            PaletteCmd::FilterService(svc) => {
                self.pane = Pane::Calls;
                self.call_filter.service = svc;
                self.refresh_view();
                self.reveal_selected();
            }
            PaletteCmd::ClearAllFilters => self.clear_search_and_filter(window, cx),
            PaletteCmd::ReplaySelected => self.trigger_replay_selected(cx),
            PaletteCmd::CopySelectedCurl => self.copy_selected_curl(cx),
            PaletteCmd::AcceptAllNewLock => self.accept_all_new_into_lock(cx),
            PaletteCmd::ExportPolicyJson => {
                let json = trace::export_policy_json(&self.access_rows());
                self.copy_text("policy JSON", json, cx);
            }
            PaletteCmd::FollowLatestRun => self.follow_latest_run(cx),
            PaletteCmd::PinRun(path) => self.pin_run(path, cx),
            PaletteCmd::TogglePause => self.toggle_pause(cx),
            PaletteCmd::ToggleInspector => self.toggle_inspector(cx),
            PaletteCmd::SetTheme(mode) => self.set_theme_mode(mode, cx),
            PaletteCmd::SetAccent(acc) => self.set_accent_color(acc, cx),
            PaletteCmd::SetDensity(d) => self.set_density(d, cx),
            PaletteCmd::SetLang(lang) => self.set_language(lang, cx),
            PaletteCmd::OpenSettings => {
                self.settings_open = true;
            }
        }
        cx.notify();
    }

    pub(super) fn render_command_palette(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = theme::p();
        let entries = self.palette_entries(cx);
        let active_idx = if entries.is_empty() {
            0
        } else {
            self.palette_index.min(entries.len() - 1)
        };

        let mut list = div()
            .id("palette-list")
            .max_h(px(360.0))
            .overflow_y_scroll()
            .scrollbar_width(px(5.0))
            .py(px(4.0))
            .flex()
            .flex_col();

        if entries.is_empty() {
            list = list.child(
                div()
                    .px(px(16.0))
                    .py(px(20.0))
                    .text_size(px(12.0))
                    .text_color(rgb(t.text_muted))
                    .text_center()
                    .child(i18n::pick("No matching commands", "没有匹配的命令")),
            );
        } else {
            for (idx, entry) in entries.into_iter().take(24).enumerate() {
                let selected = idx == active_idx;
                let cmd = entry.cmd.clone();
                list = list.child(
                    div()
                        .id(ElementId::named_usize("palette-item", idx))
                        .flex()
                        .items_center()
                        .gap(px(10.0))
                        .h(px(32.0))
                        .px(px(14.0))
                        .mx(px(5.0))
                        .rounded(px(6.0))
                        .bg(if selected {
                            theme::h(t.accent, 0.14)
                        } else {
                            theme::h(t.accent, 0.0)
                        })
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(t.row_hover)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.execute_palette_cmd(cmd.clone(), window, cx);
                        }))
                        .child(
                            div()
                                .w(px(78.0))
                                .flex_shrink_0()
                                .font_family(fonts::mono())
                                .text_size(px(9.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(if selected {
                                    rgb(t.accent)
                                } else {
                                    rgb(t.text_faint)
                                })
                                .child(entry.category),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .text_size(px(12.0))
                                .font_weight(if selected {
                                    FontWeight::MEDIUM
                                } else {
                                    FontWeight::NORMAL
                                })
                                .text_color(if selected {
                                    rgb(t.text)
                                } else {
                                    rgb(t.text_soft)
                                })
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .overflow_hidden()
                                .child(entry.title),
                        )
                        .when_some(entry.hint, |row, hint| {
                            row.child(
                                div()
                                    .flex_shrink_0()
                                    .font_family(fonts::mono())
                                    .text_size(px(10.5))
                                    .text_color(rgb(t.text_muted))
                                    .child(hint),
                            )
                        }),
                );
            }
        }

        let card = div()
            .id("palette-card")
            .w(px(560.0))
            .flex()
            .flex_col()
            .rounded(px(10.0))
            .border_1()
            .border_color(rgb(t.border_strong))
            .bg(rgb(t.panel))
            .shadow(theme::elevation_shadow())
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(self.palette_input.clone())
            .child(list);

        div()
            .id("palette-backdrop")
            .absolute()
            .inset_0()
            .bg(theme::h(0x000000, 0.46))
            .flex()
            .justify_center()
            .pt(px(68.0))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
            )
            .child(card)
    }
}
