//! The Access view: every permission the run needed, compared with the
//! baseline (the lock by default, or another run), an optional IAM Policy
//! Simulator (`Check policy…`), and a detail workbench to accept permissions
//! into `lenslet.lock`, mark `# test`, or compile least-privilege policies into
//! IAM JSON, Terraform HCL, CloudFormation, AWS SAM, and AWS CDK TypeScript.

use std::{collections::BTreeMap, sync::Arc};

use gpui::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Div, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, MouseButton, MouseDownEvent, PathPromptOptions, Pixels,
    Point, Render, ScrollStrategy, SharedString, UniformListScrollHandle, Window, div, prelude::*,
    px, rgb, uniform_list,
};
use lenslet_core::{
    iac::{CompileOptions, PolicyFormat},
    lock::{Decision, Reason},
};

use super::{
    bodyview::{BodyView, BodyViewEvent},
    session::{Baseline, Session, SessionEvent},
    widgets,
};
use crate::{
    body::BodyDoc,
    fonts, glyph, i18n,
    model::{Access, Trace, ellipsize, format_bytes, operation_label, service_label, status_label},
    prefs::Lang,
    store::{
        AccessDelta, AccessFilter, AccessRow, compile_policy, compile_single_permission,
        policy_json,
    },
    syntax::SyntaxKind,
    theme::{self, Ui, text},
};

const PAGE_ROWS: isize = 20;

pub enum AccessEvent {
    Selected(Option<AccessRow>),
    ContextMenu(Point<Pixels>, AccessRow),
    /// Show the calls that need this permission.
    FilterCalls(Access),
    SelectCall(String),
    Toast(String),
}

pub struct AccessView {
    session: Entity<Session>,
    focus: FocusHandle,
    scroll: UniformListScrollHandle,
    filter: AccessFilter,
    tokens: Vec<String>,
    rows: Arc<Vec<AccessRow>>,
    sim_decisions: Arc<BTreeMap<Access, Decision>>,
    view: Arc<Vec<usize>>,
    selected: Option<Access>,
    _subscription: gpui::Subscription,
}

impl EventEmitter<AccessEvent> for AccessView {}

impl Focusable for AccessView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AccessView {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&session, |this, _, event, cx| match event {
            SessionEvent::Derived | SessionEvent::RunChanged => this.refresh(cx),
            _ => {}
        });
        let mut view = AccessView {
            session,
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            filter: AccessFilter::All,
            tokens: Vec::new(),
            rows: Arc::new(Vec::new()),
            sim_decisions: Arc::new(BTreeMap::new()),
            view: Arc::new(Vec::new()),
            selected: None,
            _subscription: subscription,
        };
        view.refresh(cx);
        view
    }

    pub fn selected(&self) -> Option<AccessRow> {
        let selected = self.selected.as_ref()?;
        self.rows
            .iter()
            .find(|row| &row.access == selected)
            .cloned()
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let session = self.session.read(cx);
        let rows = session.access_rows.clone();
        let sim = session.sim_decisions.clone();
        let same = Arc::ptr_eq(&rows, &self.rows) && Arc::ptr_eq(&sim, &self.sim_decisions);
        self.rows = rows;
        self.sim_decisions = sim;
        if !same {
            self.recompute(cx);
        }
    }

    fn recompute(&mut self, cx: &mut Context<Self>) {
        let view: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| self.filter.matches(row) && row.matches_search(&self.tokens))
            .map(|(index, _)| index)
            .collect();
        self.view = Arc::new(view);
        let still_there = self
            .selected
            .as_ref()
            .is_some_and(|sel| self.view.iter().any(|&ix| &self.rows[ix].access == sel));
        if !still_there {
            let first = self.view.first().map(|&ix| self.rows[ix].access.clone());
            self.selected = first;
        }
        cx.emit(AccessEvent::Selected(self.selected()));
        cx.notify();
    }

    pub fn set_tokens(&mut self, tokens: Vec<String>, cx: &mut Context<Self>) {
        if self.tokens != tokens {
            self.tokens = tokens;
            self.recompute(cx);
        }
    }

    fn set_filter(&mut self, filter: AccessFilter, cx: &mut Context<Self>) {
        self.filter = if self.filter == filter {
            AccessFilter::All
        } else {
            filter
        };
        self.recompute(cx);
    }

    pub fn clear_filter(&mut self, cx: &mut Context<Self>) -> bool {
        if self.filter == AccessFilter::All {
            return false;
        }
        self.filter = AccessFilter::All;
        self.recompute(cx);
        true
    }

    pub fn select(&mut self, access: Access, cx: &mut Context<Self>) {
        if !self.rows.iter().any(|row| row.access == access) {
            return;
        }
        if !self.view.iter().any(|&ix| self.rows[ix].access == access) {
            self.filter = AccessFilter::All;
            self.tokens.clear();
            self.recompute(cx);
        }
        self.selected = Some(access);
        if let Some(position) = self.position() {
            self.scroll.scroll_to_item(position, ScrollStrategy::Center);
        }
        cx.emit(AccessEvent::Selected(self.selected()));
        cx.notify();
    }

    fn position(&self) -> Option<usize> {
        let selected = self.selected.as_ref()?;
        self.view
            .iter()
            .position(|&ix| &self.rows[ix].access == selected)
    }

    pub fn move_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.view.is_empty() {
            return;
        }
        let last = self.view.len() as isize - 1;
        let next = match self.position() {
            Some(position) => (position as isize + delta).clamp(0, last),
            None => 0,
        } as usize;
        self.selected = Some(self.rows[self.view[next]].access.clone());
        self.scroll.scroll_to_item(next, ScrollStrategy::Top);
        cx.emit(AccessEvent::Selected(self.selected()));
        cx.notify();
    }

    pub fn page(&mut self, down: bool, cx: &mut Context<Self>) {
        self.move_by(if down { PAGE_ROWS } else { -PAGE_ROWS }, cx);
    }

    pub fn select_edge(&mut self, last: bool, cx: &mut Context<Self>) {
        let delta = if last { isize::MAX / 2 } else { isize::MIN / 2 };
        self.move_by(delta, cx);
    }

    fn prompt_policy(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        let session = self.session.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                session.update(cx, |s, cx| s.load_sim_policy(path, cx)).ok();
            }
        })
        .detach();
    }

    fn render_subbar(&self, ui: &Ui, cx: &mut Context<Self>) -> Div {
        let t = ui.p;
        let lang = ui.lang;
        let count = |f: AccessFilter| self.rows.iter().filter(|row| f.matches(row)).count();
        let added = count(AccessFilter::Added);
        let removed = count(AccessFilter::Removed);
        let session = self.session.read(cx);
        let vs_lock = session.baseline == Baseline::Lock || session.baseline_run.is_none();
        let editable = session.can_edit_lock();
        let sim_label = session.sim_policy.as_ref().map(|(l, _)| l.clone());
        let entity = cx.entity();
        let chip = |id: &'static str, label: String, n: usize, f: AccessFilter, color: u32| {
            let entity = entity.clone();
            widgets::chip(ui, id, label, Some(n), self.filter == f, color).on_click(
                move |_, _, cx| {
                    entity.update(cx, |view, cx| view.set_filter(f, cx));
                },
            )
        };
        let mut bar = div()
            .flex()
            .items_center()
            .gap(px(8.0))
            .h(px(theme::SUBBAR_H))
            .px(px(theme::ROW_PAD_X - 4.0))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .bg(rgb(t.toolbar))
            .child(chip(
                "a-all",
                ui.t("All", "全部").into(),
                self.rows.len(),
                AccessFilter::All,
                t.accent,
            ))
            .child(chip(
                "a-added",
                if vs_lock {
                    ui.t("+ Not in lock", "+ 未入锁").into()
                } else {
                    ui.t("+ Added", "+ 新增").into()
                },
                added,
                AccessFilter::Added,
                t.amber,
            ))
            .child(chip(
                "a-removed",
                if vs_lock {
                    ui.t("− Unused", "− 未使用").into()
                } else {
                    ui.t("− Removed", "− 移除").into()
                },
                removed,
                AccessFilter::Removed,
                t.text_muted,
            ))
            .child(chip(
                "a-implied",
                ui.t("Implied", "隐含").into(),
                count(AccessFilter::Implied),
                AccessFilter::Implied,
                t.accent,
            ))
            .child(chip(
                "a-test",
                "# test".into(),
                count(AccessFilter::Test),
                AccessFilter::Test,
                t.text_muted,
            ))
            .child(div().flex_1());

        if let Some(label) = sim_label {
            let session = self.session.clone();
            bar = bar.child(
                widgets::chip(
                    ui,
                    "clear-sim-policy",
                    format!("{} {label} ×", ui.t("Policy:", "模拟策略:")),
                    None,
                    true,
                    t.purple,
                )
                .on_click(move |_, _, cx| {
                    session.update(cx, |s, cx| s.clear_sim_policy(cx));
                }),
            );
        } else {
            bar = bar.child(
                widgets::button(ui, "check-policy", ui.t("Check policy…", "模拟策略…"))
                    .tooltip(widgets::tip(
                        ui.t(
                            "Simulate an IAM policy JSON file against this run",
                            "加载 IAM 策略 JSON 文件模拟评估本次运行",
                        ),
                        None,
                    ))
                    .on_click(cx.listener(|view, _, _, cx| view.prompt_policy(cx))),
            );
        }

        let not_in_lock: Vec<(Access, bool, bool)> = self
            .rows
            .iter()
            .filter(|row| row.used && !row.in_lock)
            .map(|row| (row.access.clone(), false, row.implied))
            .collect();
        if editable && !not_in_lock.is_empty() {
            let session = self.session.clone();
            let n = not_in_lock.len();
            bar = bar.child(
                widgets::primary_button(
                    ui,
                    "accept-all",
                    if ui.zh() {
                        format!("全部写入 lock（{n}）")
                    } else {
                        format!("Accept {n} into lock")
                    },
                )
                .tooltip(widgets::tip(
                    ui.t(
                        "Add every permission this run used that the lock lacks",
                        "将本次运行使用但 lock 中缺少的权限全部写入",
                    ),
                    None,
                ))
                .on_click(move |_, _, cx| {
                    let entries = not_in_lock.clone();
                    session.update(cx, |session, cx| session.accept(entries, cx));
                }),
            );
        }
        let rows = self.rows.clone();
        bar.child(
            widgets::button(ui, "copy-policy", ui.t("Copy policy", "复制策略"))
                .tooltip(widgets::tip(
                    ui.t(
                        "IAM policy JSON for what this run used (without # test)",
                        "本次运行所用权限的 IAM 策略 JSON（不含 # test）",
                    ),
                    Some(i18n::shortcut("⇧E")),
                ))
                .on_click(cx.listener(move |_, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(policy_json(&rows)));
                    cx.emit(AccessEvent::Toast(i18n::copied("policy JSON", lang)));
                })),
        )
    }

    fn render_head(&self, ui: &Ui) -> Div {
        let t = ui.p;
        let head = |label: &'static str| {
            div()
                .text_size(px(text::XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(t.text_muted))
                .child(label)
        };
        div()
            .flex()
            .items_center()
            .gap(px(theme::COL_GAP))
            .h(px(theme::TABLE_HEAD_H))
            .px(px(theme::ROW_PAD_X))
            .bg(rgb(t.table_head))
            .border_b_1()
            .border_color(rgb(t.border_subtle))
            .child(div().w(px(14.0)))
            .child(head("ACTION").w(px(220.0)).flex_shrink_0())
            .child(head("RESOURCE").flex_1().min_w(px(0.0)))
            .child(head("CALLS").w(px(56.0)).flex_shrink_0().text_right())
    }
}

impl Render for AccessView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let body: AnyElement = if self.view.is_empty() {
            let health_ready = self.session.read(cx).health == super::session::Health::Ready;
            widgets::empty_state(
                &ui,
                if self.rows.is_empty() {
                    ui.t("No permissions yet", "还没有权限记录")
                } else {
                    ui.t("Nothing matches", "没有匹配项")
                },
                if self.rows.is_empty() && health_ready {
                    ui.t(
                        "Permissions appear as calls that need them arrive.",
                        "当需要权限的调用到达时，这里会列出所需权限。",
                    )
                } else {
                    ui.t("Press Esc to clear the filter.", "按 Esc 清除筛选。")
                },
                None,
            )
            .into_any_element()
        } else {
            let rows = self.rows.clone();
            let sim = self.sim_decisions.clone();
            let view = self.view.clone();
            let selected = self.selected.clone();
            let entity = cx.entity().downgrade();
            uniform_list("access", view.len(), move |range, _window, cx| {
                let _scope = crate::fsguard::RenderScope::enter();
                let ui = Ui::of(cx);
                range
                    .map(|position| {
                        let row = &rows[view[position]];
                        access_row(
                            &ui,
                            row,
                            sim.get(&row.access),
                            position,
                            selected.as_ref() == Some(&row.access),
                            entity.clone(),
                        )
                    })
                    .collect()
            })
            .flex_1()
            .min_h(px(0.0))
            .track_scroll(self.scroll.clone())
            .into_any_element()
        };
        div()
            .id("access-list")
            .size_full()
            .key_context("Access")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.0))
            .min_h(px(0.0))
            .bg(rgb(t.panel))
            .child(self.render_subbar(&ui, cx))
            .child(self.render_head(&ui))
            .child(body)
    }
}

fn sim_badge(ui: &Ui, decision: &Decision) -> Div {
    let t = ui.p;
    if decision.allowed {
        widgets::badge(ui, "SIM: ALLOW", t.green)
    } else {
        match decision.reason {
            Reason::NeedsContext => widgets::badge(ui, "NEEDS CONTEXT", t.amber),
            Reason::ExplicitDeny => widgets::badge(ui, "EXPLICIT DENY", t.red),
            _ => widgets::badge(ui, "IMPLICIT DENY", t.purple),
        }
    }
}

fn access_row(
    ui: &Ui,
    row: &AccessRow,
    sim: Option<&Decision>,
    position: usize,
    selected: bool,
    view: gpui::WeakEntity<AccessView>,
) -> gpui::Stateful<Div> {
    let t = ui.p;
    let (glyph, color) = match row.delta {
        AccessDelta::Added => ("+", t.amber),
        AccessDelta::Removed => ("−", t.text_muted),
        AccessDelta::Same => ("", t.text_muted),
    };
    let service = row.access.action.split(':').next().unwrap_or("");
    let service_color = ui.service_color(&service_label(service));
    let access = row.access.clone();
    let menu_row = row.clone();
    let click_view = view.clone();
    let mut flags = div().flex().items_center().gap(px(4.0)).flex_shrink_0();
    if let Some(decision) = sim {
        flags = flags.child(sim_badge(ui, decision));
    }
    if row.implied {
        flags = flags.child(widgets::badge(ui, "IMPLIED", t.text_muted));
    }
    if row.test {
        flags = flags.child(widgets::badge(ui, "# test", t.text_muted));
    }
    if row.errors > 0 {
        flags = flags.child(widgets::badge(ui, format!("{} ERR", row.errors), t.red));
    }
    div()
        .id(ElementId::named_usize("access-row", position))
        .w_full()
        .flex()
        .items_center()
        .gap(px(theme::COL_GAP))
        .h(px(32.0))
        .px(px(theme::ROW_PAD_X))
        .border_b_1()
        .border_color(rgb(t.border_subtle))
        .bg(if selected {
            rgb(t.row_selected)
        } else {
            rgb(t.row)
        })
        .hover(move |s| {
            s.bg(if selected {
                rgb(t.row_selected_hover)
            } else {
                rgb(t.row_hover)
            })
        })
        .when(!row.used, |el| el.opacity(0.65))
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            let access = access.clone();
            click_view
                .update(cx, |view, cx| {
                    window.focus(&view.focus);
                    view.selected = Some(access);
                    cx.emit(AccessEvent::Selected(view.selected()));
                    cx.notify();
                })
                .ok();
        })
        .on_mouse_down(
            MouseButton::Right,
            move |event: &MouseDownEvent, window, cx| {
                let row = menu_row.clone();
                view.update(cx, |view, cx| {
                    window.focus(&view.focus);
                    view.selected = Some(row.access.clone());
                    cx.emit(AccessEvent::Selected(Some(row.clone())));
                    cx.emit(AccessEvent::ContextMenu(event.position, row));
                    cx.notify();
                })
                .ok();
            },
        )
        .child(
            div()
                .w(px(14.0))
                .flex_shrink_0()
                .font(fonts::mono_font())
                .text_size(px(text::MD))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(color))
                .child(glyph),
        )
        .child(
            div()
                .w(px(220.0))
                .flex_shrink_0()
                .flex()
                .items_center()
                .gap(px(6.0))
                .overflow_hidden()
                .child(glyph::dot(service_color, 6.0))
                .child(
                    div()
                        .min_w(px(0.0))
                        .font(fonts::mono_font())
                        .text_size(px(text::SM))
                        .text_color(rgb(t.text))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(row.access.action.clone()),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .font(fonts::mono_font())
                        .text_size(px(text::SM))
                        .text_color(rgb(t.text_dim))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(row.access.resource.clone()),
                )
                .child(flags),
        )
        .child(
            div()
                .w(px(56.0))
                .flex_shrink_0()
                .text_right()
                .font(fonts::mono_font())
                .text_size(px(text::SM))
                .text_color(rgb(t.text_muted))
                .child(if row.used {
                    row.calls.to_string()
                } else {
                    "—".into()
                }),
        )
}

/// The right-hand workbench of the Access view: permission status, policy
/// simulation verdict, multi-format IaC compiler + export, and triggering calls.
pub struct AccessDetail {
    session: Entity<Session>,
    row: Option<AccessRow>,
    calls: Vec<Arc<Trace>>,
    iac_format: PolicyFormat,
    iac_full_run: bool,
    iac_text: String,
    iac_body: Entity<BodyView>,
    _subscriptions: Vec<gpui::Subscription>,
}

impl EventEmitter<AccessEvent> for AccessDetail {}

impl AccessDetail {
    pub fn new(session: Entity<Session>, cx: &mut Context<Self>) -> Self {
        let iac_body = cx.new(|cx| BodyView::new("iac-preview", cx));
        let sub_body = cx.subscribe(&iac_body, |_, _, event, cx| match event {
            BodyViewEvent::Copied(bytes) => {
                let lang = Ui::of(cx).lang;
                cx.emit(AccessEvent::Toast(i18n::copied(
                    &format_bytes(*bytes),
                    lang,
                )));
            }
        });
        let sub_sess = cx.subscribe(&session, |this, _, event, cx| {
            if matches!(event, SessionEvent::Derived | SessionEvent::RunChanged) {
                this.rebuild_iac(cx);
                cx.notify();
            }
        });
        AccessDetail {
            session,
            row: None,
            calls: Vec::new(),
            iac_format: PolicyFormat::IamJson,
            iac_full_run: false,
            iac_text: String::new(),
            iac_body,
            _subscriptions: vec![sub_body, sub_sess],
        }
    }

    pub fn set_row(&mut self, row: Option<AccessRow>, cx: &mut Context<Self>) {
        if self.row == row {
            return;
        }
        self.calls = match row.as_ref() {
            Some(row) => self
                .session
                .read(cx)
                .store
                .rows()
                .iter()
                .filter(|call| !call.pending && call.uses(&row.access))
                .take(40)
                .cloned()
                .collect(),
            None => Vec::new(),
        };
        self.row = row;
        self.rebuild_iac(cx);
        cx.notify();
    }

    pub fn restyle(&mut self, cx: &mut Context<Self>) {
        self.rebuild_iac(cx);
    }

    fn rebuild_iac(&mut self, cx: &mut Context<Self>) {
        let ui = Ui::of(cx);
        let Some(row) = self.row.as_ref() else {
            self.iac_text.clear();
            self.iac_body
                .update(cx, |b, cx| b.set_doc(Arc::new(BodyDoc::default()), cx));
            return;
        };
        let opts = CompileOptions {
            format: self.iac_format,
            ..CompileOptions::default()
        };
        let code = if self.iac_full_run {
            let rows = self.session.read(cx).access_rows.clone();
            compile_policy(&rows, &opts)
        } else {
            compile_single_permission(row, &opts)
        };
        let kind = match self.iac_format {
            PolicyFormat::IamJson => SyntaxKind::Json,
            _ => SyntaxKind::Iac,
        };
        let doc = Arc::new(BodyDoc::from_text(code.clone(), kind, &[], &ui.p));
        self.iac_text = code;
        self.iac_body.update(cx, |b, cx| b.set_doc(doc, cx));
    }

    fn export_iac(&mut self, cx: &mut Context<Self>) {
        let code = self.iac_text.clone();
        if code.is_empty() {
            return;
        }
        let ext = match self.iac_format {
            PolicyFormat::IamJson => "lenslet-policy.json",
            PolicyFormat::Terraform => "lenslet-policy.tf",
            PolicyFormat::CloudFormation => "lenslet-cfn-policy.yaml",
            PolicyFormat::Sam => "lenslet-sam-policy.yaml",
            PolicyFormat::CdkTypeScript => "lenslet-policy.ts",
        };
        let dir = self
            .session
            .read(cx)
            .run
            .as_ref()
            .filter(|r| !r.header.cwd.is_empty())
            .map(|r| std::path::PathBuf::from(&r.header.cwd))
            .unwrap_or_else(super::home);
        let lang = Ui::of(cx).lang;
        let rx = cx.prompt_for_new_path(&dir, Some(ext));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = rx.await {
                let target = path.clone();
                let res = cx
                    .background_executor()
                    .spawn(async move {
                        crate::fsguard::assert_io_allowed();
                        std::fs::write(&target, code)
                    })
                    .await;
                this.update(cx, |_, cx| {
                    let msg = match (res, lang) {
                        (Ok(()), Lang::En) => format!("Exported {}", path.display()),
                        (Ok(()), Lang::Zh) => format!("已导出 {}", path.display()),
                        (Err(e), _) => format!("Export error: {e}"),
                    };
                    cx.emit(AccessEvent::Toast(msg));
                })
                .ok();
            }
        })
        .detach();
    }
}

impl Render for AccessDetail {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let lang = ui.lang;
        let container = div()
            .id("access-detail")
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(t.panel_raised));
        let Some(row) = self.row.clone() else {
            return container.child(widgets::empty_state(
                &ui,
                ui.t("Select a permission", "选择一项权限"),
                "",
                None,
            ));
        };
        let session = self.session.read(cx);
        let editable = session.can_edit_lock();
        let sim_decision = session.sim_decisions.get(&row.access).cloned();
        let (service, action) = row
            .access
            .action
            .split_once(':')
            .unwrap_or(("", row.access.action.as_str()));
        let service_color = ui.service_color(&service_label(service));

        let mut badges = div().flex().flex_wrap().items_center().gap(px(6.0));
        badges = badges.child(widgets::badge(
            ui_ref(&ui),
            service_label(service),
            color_of(service_color),
        ));
        badges = badges.child(match (row.delta, row.in_lock) {
            (AccessDelta::Added, false) => widgets::badge(&ui, "NOT IN LOCK", t.amber),
            (AccessDelta::Added, true) => widgets::badge(&ui, "NEW VS BASELINE", t.amber),
            (AccessDelta::Removed, _) => widgets::badge(&ui, "UNUSED", t.text_muted),
            (AccessDelta::Same, true) => widgets::badge(&ui, "IN LOCK", t.green),
            (AccessDelta::Same, false) => widgets::badge(&ui, "ALLOWED", t.green),
        });
        if row.implied {
            badges = badges.child(widgets::badge(&ui, "IMPLIED", t.accent));
        }
        if row.test {
            badges = badges.child(widgets::badge(&ui, "# test", t.text_muted));
        }
        if let Some(ref dec) = sim_decision {
            badges = badges.child(sim_badge(&ui, dec));
        }

        let mut actions = div().flex().flex_wrap().items_center().gap(px(8.0));
        if editable && row.used && !row.in_lock {
            let session = self.session.clone();
            let access = row.access.clone();
            let implied = row.implied;
            actions = actions.child(
                widgets::primary_button(&ui, "accept", ui.t("Accept into lock", "写入 lock"))
                    .on_click(move |_, _, cx| {
                        let entries = vec![(access.clone(), false, implied)];
                        session.update(cx, |session, cx| session.accept(entries, cx));
                    }),
            );
        } else if row.used && !row.in_lock {
            actions = actions.child(
                widgets::primary_button(&ui, "accept", ui.t("Accept into lock", "写入 lock"))
                    .opacity(0.45)
                    .cursor_default()
                    .tooltip(widgets::tip(
                        ui.t(
                            "This run has no lenslet.lock to write: it is checked against an IAM policy, or its project folder is unknown",
                            "此运行没有可写的 lenslet.lock：它按 IAM policy 检查，或项目目录未知",
                        ),
                        None,
                    )),
            );
        }
        if editable && (row.in_lock || row.used) {
            let session = self.session.clone();
            let access = row.access.clone();
            let test = !row.test;
            actions = actions.child(
                widgets::button(
                    &ui,
                    "mark-test",
                    if row.test {
                        ui.t("Unmark # test", "取消 # test")
                    } else {
                        ui.t("Mark # test", "标记 # test")
                    },
                )
                .tooltip(widgets::tip(
                    ui.t(
                        "Test-only permissions stay in the lock but out of `lenslet policy`",
                        "仅测试需要的权限保留在 lock 中，但不进入 `lenslet policy`",
                    ),
                    None,
                ))
                .on_click(move |_, _, cx| {
                    let access = access.clone();
                    session.update(cx, |session, cx| session.set_test_mark(access, test, cx));
                }),
            );
        }
        if row.used {
            let access = row.access.clone();
            actions = actions.child(
                widgets::button(&ui, "show-calls", ui.t("Show calls", "查看调用")).on_click(
                    cx.listener(move |_, _, _, cx| {
                        cx.emit(AccessEvent::FilterCalls(access.clone()));
                    }),
                ),
            );
        }

        // Simulation context banner when NeedsContext / Denied
        let sim_card = sim_decision.and_then(|dec| match dec.reason {
            Reason::NeedsContext => Some(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .p(px(10.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme::h(t.amber, 0.40))
                    .bg(theme::h(t.amber, 0.06))
                    .child(widgets::label(
                        &ui,
                        ui.t("POLICY NEEDS CLOUD CONTEXT", "策略包含云端运行时上下文条件"),
                    ))
                    .child(widgets::mono(&ui, dec.missing_context.join(", "))),
            ),
            Reason::ExplicitDeny => Some(
                div()
                    .p(px(10.0))
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(theme::h(t.red, 0.40))
                    .bg(theme::h(t.red, 0.06))
                    .child(widgets::label(
                        &ui,
                        ui.t(
                            "EXPLICITLY DENIED BY SIMULATED POLICY",
                            "被所选策略显式拒绝 (Explicit Deny)",
                        ),
                    )),
            ),
            _ => None,
        });

        // Multi-Format IaC Policy Compiler section
        let formats = [
            (PolicyFormat::IamJson, "IAM JSON"),
            (PolicyFormat::Terraform, "Terraform"),
            (PolicyFormat::CloudFormation, "CFN"),
            (PolicyFormat::Sam, "SAM"),
            (PolicyFormat::CdkTypeScript, "CDK"),
        ];
        let mut fmt_row = div().flex().flex_wrap().items_center().gap(px(4.0));
        for (fmt, label) in formats {
            let active = self.iac_format == fmt;
            fmt_row = fmt_row.child(
                widgets::chip(&ui, label, label, None, active, t.accent).on_click(cx.listener(
                    move |detail, _, _, cx| {
                        detail.iac_format = fmt;
                        detail.rebuild_iac(cx);
                        cx.notify();
                    },
                )),
            );
        }
        let ent = cx.entity();
        let scope_seg = widgets::segmented(
            &ui,
            vec![
                widgets::Segment {
                    id: "iac-one",
                    label: ui.t("This Permission", "当前权限").into(),
                    active: !self.iac_full_run,
                    tooltip: None,
                },
                widgets::Segment {
                    id: "iac-all",
                    label: ui.t("Full Run", "全部权限").into(),
                    active: self.iac_full_run,
                    tooltip: None,
                },
            ],
            move |ix, _, cx| {
                ent.update(cx, |detail, cx| {
                    detail.iac_full_run = ix == 1;
                    detail.rebuild_iac(cx);
                    cx.notify();
                });
            },
        );
        let code_copy = self.iac_text.clone();
        let iac_section = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(widgets::label(
                        &ui,
                        ui.t("LEAST-PRIVILEGE IAC COMPILER", "最小权限 IaC 编译器"),
                    ))
                    .child(div().flex_1())
                    .child(scope_seg),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(fmt_row)
                    .child(div().flex_1())
                    .child(
                        widgets::button(&ui, "iac-copy", ui.t("Copy", "复制")).on_click(
                            cx.listener(move |_, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(code_copy.clone()));
                                cx.emit(AccessEvent::Toast(i18n::copied("IaC policy", lang)));
                            }),
                        ),
                    )
                    .child(
                        widgets::button(&ui, "iac-export", ui.t("Export…", "导出…"))
                            .on_click(cx.listener(|detail, _, _, cx| detail.export_iac(cx))),
                    ),
            )
            .child(
                div()
                    .h(px(210.0))
                    .flex()
                    .flex_col()
                    .child(super::cached(&self.iac_body)),
            );

        let mut calls = div().flex().flex_col();
        for (index, call) in self.calls.iter().enumerate() {
            let id = call.id.clone();
            let target_str = call.semantic_target();
            calls = calls.child(
                div()
                    .id(ElementId::named_usize("access-call", index))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .h(px(28.0))
                    .px(px(8.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(rgb(t.row_hover)))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(AccessEvent::SelectCall(id.clone()));
                    }))
                    .child(
                        div()
                            .w(px(36.0))
                            .font(fonts::mono_font())
                            .text_size(px(text::SM))
                            .text_color(ui.status_color(call.status))
                            .child(status_label(call.status)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(text::SM))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(t.text_soft))
                            .child(format!("{} {}", call.service_name(), operation_label(call))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .font(fonts::mono_font())
                            .text_size(px(text::XS))
                            .text_color(rgb(t.text_muted))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(ellipsize(&target_str, 44)),
                    ),
            );
        }
        if self.calls.is_empty() {
            calls = calls.child(
                div()
                    .text_size(px(text::MD))
                    .text_color(rgb(t.text_muted))
                    .child(if row.used {
                        ui.t(
                            "The calls that needed it are no longer in view.",
                            "相关调用已不在当前列表中。",
                        )
                    } else {
                        ui.t("No call in this run needed it.", "本次运行没有调用需要它。")
                    }),
            );
        }

        let via: SharedString = row.via.join(", ").into();
        container.child(
            div()
                .id("access-detail-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .gap(px(16.0))
                .p(px(16.0))
                .child(badges)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .child(
                            div()
                                .font(fonts::mono_font())
                                .text_size(px(text::XL))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(t.text))
                                .child(action.to_string()),
                        )
                        .child(
                            div()
                                .font(fonts::mono_font())
                                .text_size(px(text::MD))
                                .line_height(px(18.0))
                                .text_color(rgb(t.text_dim))
                                .child(row.access.resource.clone()),
                        ),
                )
                .child(actions)
                .children(sim_card)
                .when(!via.is_empty(), |el| {
                    el.child(widgets::meta_row(
                        &ui,
                        ui.t("Needed by", "调用方"),
                        widgets::mono(&ui, via.clone()),
                    ))
                })
                .child(iac_section)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.0))
                        .child(widgets::label(&ui, ui.t("CALLS", "调用")))
                        .child(calls),
                ),
        )
    }
}

fn ui_ref(ui: &Ui) -> &Ui {
    ui
}

fn color_of(color: gpui::Rgba) -> u32 {
    ((color.r * 255.0).round() as u32) << 16
        | ((color.g * 255.0).round() as u32) << 8
        | (color.b * 255.0).round() as u32
}
