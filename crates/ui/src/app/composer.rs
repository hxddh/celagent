//! Interactive Request Composer & Local SigV4 Re-signer (`Edit & Replay` ⇧⌘⏎ / `New Request` ⇧⌘N).
//!
//! Lets developers either mutate a captured AWS call or craft a brand-new AWS
//! request from scratch using 1-click service templates, edit HTTP method,
//! path/query, target loopback URL, service/region, multiple header overrides
//! (`Key: Value; -RemoveHeader`), and payload body, automatically sign with
//! `AWS4-HMAC-SHA256`, and dispatch strictly over local loopback.

use std::path::PathBuf;

use gpui::{
    AnyElement, App, Context, ElementId, Entity, EventEmitter, FontWeight, Render, Window, div,
    prelude::*, px, rgb,
};

use super::widgets;
use crate::{
    fonts,
    model::{Header, Trace},
    replay::{self, ComposedRequest, ReplayState, ReplayTarget},
    search::SearchInput,
    theme::{self, Ui, text},
};

pub enum ComposerEvent {
    Close,
    Sent(String, ReplayState),
    ScratchResult(String),
}

struct AwsTemplate {
    label: &'static str,
    service: &'static str,
    method: &'static str,
    path: &'static str,
    headers: &'static str,
    body: &'static str,
}

const SCRATCH_TEMPLATES: &[AwsTemplate] = &[
    AwsTemplate {
        label: "S3 ListBuckets",
        service: "s3",
        method: "GET",
        path: "/",
        headers: "",
        body: "",
    },
    AwsTemplate {
        label: "S3 PutObject",
        service: "s3",
        method: "PUT",
        path: "/test-bucket/hello.txt",
        headers: "content-type: text/plain",
        body: "hello from lenslet",
    },
    AwsTemplate {
        label: "DynamoDB ListTables",
        service: "dynamodb",
        method: "POST",
        path: "/",
        headers: "x-amz-target: DynamoDB_20120810.ListTables; content-type: application/x-amz-json-1.0",
        body: "{\"Limit\": 25}",
    },
    AwsTemplate {
        label: "SQS ListQueues",
        service: "sqs",
        method: "POST",
        path: "/",
        headers: "x-amz-target: AmazonSQS.ListQueues; content-type: application/x-amz-json-1.0",
        body: "{}",
    },
    AwsTemplate {
        label: "STS CallerIdentity",
        service: "sts",
        method: "POST",
        path: "/",
        headers: "content-type: application/x-www-form-urlencoded",
        body: "Action=GetCallerIdentity&Version=2011-06-15",
    },
];

pub struct RequestComposer {
    call_id: String,
    scratch_mode: bool,
    host: String,
    encoding: Option<String>,
    base_headers: Vec<Header>,
    via_proxy: bool,
    home: PathBuf,
    resign_sigv4: bool,
    sending: bool,
    last_scratch_status: Option<(u16, String)>,
    target_input: Entity<SearchInput>,
    service_input: Entity<SearchInput>,
    region_input: Entity<SearchInput>,
    method_input: Entity<SearchInput>,
    path_input: Entity<SearchInput>,
    header_input: Entity<SearchInput>,
    body_input: Entity<SearchInput>,
}

impl EventEmitter<ComposerEvent> for RequestComposer {}

impl RequestComposer {
    pub fn new(home: PathBuf, cx: &mut Context<Self>) -> Self {
        let target_input = cx.new(|cx| {
            SearchInput::new_field(
                "http://127.0.0.1:9297",
                "http://127.0.0.1:9297 (loopback only)",
                true,
                cx,
            )
        });
        let service_input =
            cx.new(|cx| SearchInput::new_field("s3", "s3 / dynamodb / sqs", true, cx));
        let region_input = cx.new(|cx| SearchInput::new_field("us-east-1", "us-east-1", true, cx));
        let method_input = cx.new(|cx| SearchInput::new_field("POST", "POST", true, cx));
        let path_input = cx.new(|cx| SearchInput::new_field("/", "/path?query=1", true, cx));
        let header_input = cx.new(|cx| {
            SearchInput::new_field(
                "",
                "x-amz-target: DynamoDB_20120810.GetItem; -x-unwanted-header",
                true,
                cx,
            )
        });
        let body_input = cx.new(|cx| SearchInput::new_field("", "{\"Key\": ...}", true, cx));
        Self {
            call_id: String::new(),
            scratch_mode: false,
            host: String::new(),
            encoding: None,
            base_headers: Vec::new(),
            via_proxy: true,
            home,
            resign_sigv4: true,
            sending: false,
            last_scratch_status: None,
            target_input,
            service_input,
            region_input,
            method_input,
            path_input,
            header_input,
            body_input,
        }
    }

    pub fn load(
        &mut self,
        call: &Trace,
        target: ReplayTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.call_id = call.id.clone();
        self.scratch_mode = false;
        self.last_scratch_status = None;
        self.host = call.host.clone();
        let service = call.service.clone();
        let region = call
            .region
            .clone()
            .unwrap_or_else(|| "us-east-1".to_string());
        self.encoding = call
            .request
            .as_ref()
            .and_then(|r| r.body.as_ref())
            .and_then(|b| b.encoding.clone());
        self.base_headers = call
            .request
            .as_ref()
            .map(|r| r.headers.clone())
            .unwrap_or_default();
        self.via_proxy = target.via_proxy;
        self.resign_sigv4 = call.authorization || !call.presigned();
        self.sending = false;

        let method = call.method.to_ascii_uppercase();
        let path = call.path.clone();
        let target_hdr = self
            .base_headers
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("x-amz-target"))
            .map(|h| format!("{}: {}", h.name, h.value))
            .unwrap_or_default();
        let raw_body = call
            .request
            .as_ref()
            .and_then(|r| r.body.as_ref())
            .and_then(|b| b.text.as_deref())
            .unwrap_or("");
        let compact_body = serde_json::from_str::<serde_json::Value>(raw_body)
            .ok()
            .and_then(|v| serde_json::to_string(&v).ok())
            .unwrap_or_else(|| raw_body.replace(['\r', '\n'], " "));

        self.target_input
            .update(cx, |i, cx| i.set_text(target.url, cx));
        self.service_input
            .update(cx, |i, cx| i.set_text(service, cx));
        self.region_input.update(cx, |i, cx| i.set_text(region, cx));
        self.method_input.update(cx, |i, cx| i.set_text(method, cx));
        self.path_input.update(cx, |i, cx| i.set_text(path, cx));
        self.header_input
            .update(cx, |i, cx| i.set_text(target_hdr, cx));
        self.body_input
            .update(cx, |i, cx| i.set_text(compact_body, cx));
        let focus = self.body_input.read(cx).handle();
        window.focus(&focus);
        cx.notify();
    }

    pub fn load_scratch(
        &mut self,
        target: ReplayTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.call_id.clear();
        self.scratch_mode = true;
        self.last_scratch_status = None;
        self.host.clear();
        self.encoding = None;
        self.base_headers.clear();
        self.via_proxy = target.via_proxy;
        self.resign_sigv4 = true;
        self.sending = false;
        self.target_input
            .update(cx, |i, cx| i.set_text(target.url, cx));
        self.region_input
            .update(cx, |i, cx| i.set_text("us-east-1", cx));
        self.apply_template(&SCRATCH_TEMPLATES[0], cx);
        let focus = self.path_input.read(cx).handle();
        window.focus(&focus);
        cx.notify();
    }

    fn apply_template(&mut self, tpl: &AwsTemplate, cx: &mut Context<Self>) {
        self.service_input
            .update(cx, |i, cx| i.set_text(tpl.service, cx));
        self.method_input
            .update(cx, |i, cx| i.set_text(tpl.method, cx));
        self.path_input.update(cx, |i, cx| i.set_text(tpl.path, cx));
        self.header_input
            .update(cx, |i, cx| i.set_text(tpl.headers, cx));
        self.body_input.update(cx, |i, cx| i.set_text(tpl.body, cx));
        cx.notify();
    }

    pub fn send(&mut self, cx: &mut Context<Self>) {
        if self.sending {
            return;
        }
        let target_url = self.target_input.read(cx).query().trim().to_string();
        if target_url.is_empty() {
            return;
        }
        let target = ReplayTarget {
            url: target_url,
            via_proxy: self.via_proxy && !self.call_id.is_empty(),
        };
        let service = {
            let s = self.service_input.read(cx).query().trim().to_string();
            if s.is_empty() { "s3".into() } else { s }
        };
        let region = {
            let r = self.region_input.read(cx).query().trim().to_string();
            if r.is_empty() { "us-east-1".into() } else { r }
        };
        let method = self.method_input.read(cx).query().trim().to_string();
        let path = {
            let p = self.path_input.read(cx).query().trim().to_string();
            if p.is_empty() {
                "/".into()
            } else if p.starts_with('/') {
                p
            } else {
                format!("/{p}")
            }
        };
        let extra_hdr = self.header_input.read(cx).query().trim().to_string();
        let body = self.body_input.read(cx).query().to_string();

        let mut headers = self.base_headers.clone();
        for part in extra_hdr.split(';') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some(rem) = part.strip_prefix('-') {
                let rem = rem.trim();
                if !rem.is_empty() {
                    headers.retain(|h| !h.name.eq_ignore_ascii_case(rem));
                }
            } else if let Some((name, val)) = part.split_once(':') {
                let name = name.trim();
                let val = val.trim();
                if !name.is_empty() {
                    headers.retain(|h| !h.name.eq_ignore_ascii_case(name));
                    headers.push(Header {
                        name: name.to_string(),
                        value: val.to_string(),
                    });
                }
            }
        }

        let req = ComposedRequest {
            call_id: (!self.call_id.is_empty()).then(|| self.call_id.clone()),
            method,
            path,
            host: self.host.clone(),
            service,
            region,
            encoding: self.encoding.clone(),
            headers,
            body,
            resign_sigv4: self.resign_sigv4,
        };
        let call_id = self.call_id.clone();
        let scratch = self.scratch_mode || call_id.is_empty();
        let home = self.home.clone();
        self.sending = true;
        if !scratch {
            cx.emit(ComposerEvent::Sent(call_id.clone(), ReplayState::Running));
        }
        cx.notify();

        cx.spawn(async move |this, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { replay::send_composed(&req, &target, &home) })
                .await;
            this.update(cx, |composer, cx| {
                composer.sending = false;
                if scratch {
                    match outcome {
                        Ok(done) => {
                            let preview = done
                                .body_text
                                .as_deref()
                                .map(|s| s.lines().next().unwrap_or("").trim().to_string())
                                .unwrap_or_default();
                            composer.last_scratch_status = Some((
                                done.status,
                                format!("HTTP {} · {} ms · {}", done.status, done.ms, preview),
                            ));
                            cx.emit(ComposerEvent::ScratchResult(format!(
                                "Request completed: HTTP {} ({} ms)",
                                done.status, done.ms
                            )));
                        }
                        Err(err) => {
                            composer.last_scratch_status = Some((599, err.clone()));
                            cx.emit(ComposerEvent::ScratchResult(err));
                        }
                    }
                    cx.notify();
                } else {
                    let state = match outcome {
                        Ok(done) => ReplayState::Done(done),
                        Err(err) => ReplayState::Failed(err),
                    };
                    cx.emit(ComposerEvent::Sent(call_id, state));
                    cx.emit(ComposerEvent::Close);
                }
            })
            .ok();
        })
        .detach();
    }

    fn set_method(&mut self, method: &'static str, cx: &mut Context<Self>) {
        self.method_input.update(cx, |i, cx| i.set_text(method, cx));
        cx.notify();
    }
}

impl Render for RequestComposer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _scope = crate::fsguard::RenderScope::enter();
        let ui = Ui::of(cx);
        let t = ui.p;
        let cur_method = self.method_input.read(cx).query().to_ascii_uppercase();

        let mut method_chips = div().flex().items_center().gap(px(4.0));
        for m in ["POST", "GET", "PUT", "DELETE", "HEAD"] {
            let active = cur_method == m;
            method_chips = method_chips.child(
                widgets::chip(&ui, m, m, None, active, t.accent)
                    .on_click(cx.listener(move |comp, _, _, cx| comp.set_method(m, cx))),
            );
        }

        let mut tpl_bar = div().flex().flex_wrap().items_center().gap(px(5.0));
        for (ix, tpl) in SCRATCH_TEMPLATES.iter().enumerate() {
            tpl_bar = tpl_bar.child(
                widgets::chip(
                    &ui,
                    ElementId::named_usize("scratch-tpl", ix),
                    tpl.label,
                    None,
                    false,
                    t.accent,
                )
                .on_click(cx.listener(move |comp, _, _, cx| {
                    comp.apply_template(&SCRATCH_TEMPLATES[ix], cx);
                })),
            );
        }

        let service_str = self.service_input.read(cx).query().to_string();
        let region_str = self.region_input.read(cx).query().to_string();

        let card = div()
            .id("composer-modal")
            .w(px(680.0))
            .flex()
            .flex_col()
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(t.border))
            .bg(rgb(t.panel_raised))
            .shadow(ui.elevation())
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(ComposerEvent::Close)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .h(px(46.0))
                    .px(px(18.0))
                    .border_b_1()
                    .border_color(rgb(t.border_subtle))
                    .child(
                        div()
                            .text_size(px(text::XL))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(t.text))
                            .child(if self.scratch_mode {
                                ui.t("Compose Local AWS Request", "新建本地 AWS 请求")
                            } else {
                                ui.t("Edit & Replay Request", "编辑并重放请求")
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        widgets::button(&ui, "close-composer", ui.t("Close", "关闭"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(ComposerEvent::Close))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.0))
                    .p(px(18.0))
                    .when(self.scratch_mode, |el| {
                        el.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .child(widgets::label(&ui, ui.t("QUICK TEMPLATES", "快速请求模板")))
                                .child(tpl_bar),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().flex_1().child(field_block(
                                &ui,
                                ui.t("TARGET LOOPBACK ENDPOINT", "目标本地回环地址"),
                                self.target_input.clone(),
                            )))
                            .child(div().w(px(120.0)).child(field_block(
                                &ui,
                                "SERVICE",
                                self.service_input.clone(),
                            )))
                            .child(div().w(px(120.0)).child(field_block(
                                &ui,
                                "REGION",
                                self.region_input.clone(),
                            ))),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().w(px(88.0)).child(self.method_input.clone()))
                            .child(method_chips),
                    )
                    .child(field_block(
                        &ui,
                        ui.t("PATH & QUERY", "请求路径与查询参数"),
                        self.path_input.clone(),
                    ))
                    .child(field_block(
                        &ui,
                        ui.t(
                            "HEADERS (`Key: Val; Key2: Val2; -RemoveKey`)",
                            "覆盖/增删 Headers（用 `;` 分隔，`-Header` 移除）",
                        ),
                        self.header_input.clone(),
                    ))
                    .child(field_block(
                        &ui,
                        ui.t("REQUEST BODY", "请求报文体 (JSON / XML / Form)"),
                        self.body_input.clone(),
                    ))
                    .when_some(self.last_scratch_status.as_ref(), |el, (code, msg)| {
                        let c = if *code < 400 { t.green } else { t.red };
                        el.child(
                            div()
                                .p(px(8.0))
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(theme::h(c, 0.4))
                                .bg(theme::h(c, 0.08))
                                .font(fonts::mono_font())
                                .text_size(px(text::XS))
                                .text_color(rgb(c))
                                .child(msg.clone()),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .pt(px(4.0))
                            .child(
                                widgets::chip(
                                    &ui,
                                    "toggle-sigv4",
                                    format!(
                                        "{} ({}/{})",
                                        ui.t("Auto sign AWS SigV4", "自动签名 AWS SigV4"),
                                        service_str,
                                        region_str
                                    ),
                                    None,
                                    self.resign_sigv4,
                                    t.green,
                                )
                                .on_click(cx.listener(
                                    |comp, _, _, cx| {
                                        comp.resign_sigv4 = !comp.resign_sigv4;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(div().flex_1())
                            .child(
                                widgets::primary_button(
                                    &ui,
                                    "composer-send",
                                    if self.sending {
                                        ui.t("Sending…", "发送中…")
                                    } else {
                                        ui.t("Send Request", "发送请求")
                                    },
                                )
                                .on_click(cx.listener(|comp, _, _, cx| comp.send(cx))),
                            ),
                    ),
            );

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme::h(0x000000, if ui.dark() { 0.44 } else { 0.20 }))
            .child(widgets::appear(&ui, "composer-anim", card))
    }
}

fn field_block(ui: &Ui, title: &str, input: Entity<SearchInput>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(widgets::label(ui, title.to_string()))
        .child(input)
}

pub fn render_composer_overlay(composer: &Entity<RequestComposer>, _cx: &App) -> AnyElement {
    composer.clone().into_any_element()
}
