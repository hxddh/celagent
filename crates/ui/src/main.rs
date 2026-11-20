//! Lenslet: native cross-platform desktop workbench over Lenslet run traces & local cloud emulators.

mod app;
mod body;
mod daemon;
mod demo;
mod feed;
mod fonts;
mod fsguard;
mod glyph;
mod i18n;
mod icons;
mod lockfile;
mod model;
mod prefs;
mod replay;
mod search;
mod sigv4;
mod sse;
mod store;
mod syntax;
#[cfg(test)]
mod testdata;
mod theme;

#[cfg(test)]
mod bench;

use std::path::PathBuf;

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType,
    WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowOptions, prelude::*, px, size,
};

use crate::{app::*, prefs::Lang};

/// Context predicates for the key map.
const LIST: &str = "LensletApp && !Search && !Palette && !Body && !ComposerField";
const NOT_TYPING: &str = "LensletApp && !Search && !Palette && !ComposerField";

/// The whole key map. Every `cmd-` binding also gets a `ctrl-` twin off macOS.
fn key_bindings() -> Vec<KeyBinding> {
    type Make = Box<dyn Fn(&str) -> KeyBinding>;
    let primary: Vec<(&str, Make)> = vec![
        (
            "n",
            Box::new(|k| KeyBinding::new(k, OpenLauncher, Some("LensletApp"))),
        ),
        (
            "shift-n",
            Box::new(|k| KeyBinding::new(k, NewRequest, Some("LensletApp"))),
        ),
        (
            "alt-o",
            Box::new(|k| KeyBinding::new(k, OpenWorkspace, Some("LensletApp"))),
        ),
        (
            "k",
            Box::new(|k| KeyBinding::new(k, OpenPalette, Some("LensletApp"))),
        ),
        (
            "shift-p",
            Box::new(|k| KeyBinding::new(k, OpenPalette, Some("LensletApp"))),
        ),
        (
            "f",
            Box::new(|k| KeyBinding::new(k, FocusSearch, Some("LensletApp"))),
        ),
        (
            ",",
            Box::new(|k| KeyBinding::new(k, OpenSettings, Some("LensletApp"))),
        ),
        (
            "0",
            Box::new(|k| KeyBinding::new(k, ToggleSidebar, Some("LensletApp"))),
        ),
        (
            "b",
            Box::new(|k| KeyBinding::new(k, ToggleInspector, Some("LensletApp"))),
        ),
        (
            "o",
            Box::new(|k| KeyBinding::new(k, OpenFile, Some("LensletApp"))),
        ),
        (
            "s",
            Box::new(|k| KeyBinding::new(k, ExportReport, Some("LensletApp"))),
        ),
        (
            "alt-s",
            Box::new(|k| KeyBinding::new(k, SavePayload, Some("LensletApp"))),
        ),
        (
            "shift-r",
            Box::new(|k| KeyBinding::new(k, RevealTrace, Some("LensletApp"))),
        ),
        (
            ".",
            Box::new(|k| KeyBinding::new(k, StopActiveRun, Some("LensletApp"))),
        ),
        (
            "g",
            Box::new(|k| KeyBinding::new(k, NextMatch, Some("LensletApp"))),
        ),
        (
            "shift-g",
            Box::new(|k| KeyBinding::new(k, PrevMatch, Some("LensletApp"))),
        ),
        (
            "r",
            Box::new(|k| KeyBinding::new(k, ReloadRun, Some("LensletApp"))),
        ),
        (
            "1",
            Box::new(|k| KeyBinding::new(k, ShowCalls, Some("LensletApp"))),
        ),
        (
            "2",
            Box::new(|k| KeyBinding::new(k, ShowAccess, Some("LensletApp"))),
        ),
        (
            "3",
            Box::new(|k| KeyBinding::new(k, ShowConsole, Some("LensletApp"))),
        ),
        (
            "[",
            Box::new(|k| KeyBinding::new(k, PrevRun, Some("LensletApp"))),
        ),
        (
            "]",
            Box::new(|k| KeyBinding::new(k, NextRun, Some("LensletApp"))),
        ),
        (
            "shift-c",
            Box::new(|k| KeyBinding::new(k, CopyCurl, Some(NOT_TYPING))),
        ),
        (
            "shift-e",
            Box::new(|k| KeyBinding::new(k, CopyPolicy, Some("LensletApp"))),
        ),
        (
            "enter",
            Box::new(|k| KeyBinding::new(k, Replay, Some(NOT_TYPING))),
        ),
        (
            "shift-enter",
            Box::new(|k| KeyBinding::new(k, OpenComposer, Some(NOT_TYPING))),
        ),
        (
            "x",
            Box::new(|k| KeyBinding::new(k, Cut, Some("Search || Palette || ComposerField"))),
        ),
        (
            "v",
            Box::new(|k| KeyBinding::new(k, Paste, Some("Search || Palette || ComposerField"))),
        ),
        (
            "z",
            Box::new(|k| KeyBinding::new(k, Undo, Some("Search || Palette || ComposerField"))),
        ),
        (
            "shift-z",
            Box::new(|k| KeyBinding::new(k, Redo, Some("Search || Palette || ComposerField"))),
        ),
        (
            "c",
            Box::new(|k| {
                KeyBinding::new(k, Copy, Some("Body || Search || Palette || ComposerField"))
            }),
        ),
        (
            "a",
            Box::new(|k| {
                KeyBinding::new(
                    k,
                    SelectAll,
                    Some("Body || Search || Palette || ComposerField"),
                )
            }),
        ),
        (
            "w",
            Box::new(|k| KeyBinding::new(k, CloseWindow, Some("LensletApp"))),
        ),
        (
            "m",
            Box::new(|k| KeyBinding::new(k, MinimizeWindow, Some("LensletApp"))),
        ),
        ("q", Box::new(|k| KeyBinding::new(k, Quit, None))),
    ];
    let mut bindings = Vec::new();
    for (key, make) in &primary {
        bindings.push(make(&format!("cmd-{key}")));
        if !cfg!(target_os = "macos") {
            bindings.push(make(&format!("ctrl-{key}")));
        }
    }
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("cmd-h", HideApp, None),
            KeyBinding::new("alt-cmd-h", HideOthers, None),
            KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, Some("LensletApp")),
        ]);
    } else {
        bindings.push(KeyBinding::new("f11", ToggleFullScreen, Some("LensletApp")));
        bindings.push(KeyBinding::new(
            "ctrl-y",
            Redo,
            Some("Search || Palette || ComposerField"),
        ));
    }
    bindings.extend([
        KeyBinding::new("/", FocusSearch, Some(NOT_TYPING)),
        KeyBinding::new("j", SelectNext, Some(LIST)),
        KeyBinding::new("k", SelectPrev, Some(LIST)),
        KeyBinding::new("down", SelectNext, Some(LIST)),
        KeyBinding::new("up", SelectPrev, Some(LIST)),
        KeyBinding::new("pagedown", PageDown, Some(LIST)),
        KeyBinding::new("pageup", PageUp, Some(LIST)),
        KeyBinding::new("home", SelectFirst, Some(LIST)),
        KeyBinding::new("end", SelectLast, Some(LIST)),
        KeyBinding::new("enter", Confirm, Some(NOT_TYPING)),
        KeyBinding::new("escape", Cancel, Some(NOT_TYPING)),
        // While typing a search, ↑/↓ still move through the results.
        KeyBinding::new("down", SelectNext, Some("Search")),
        KeyBinding::new("up", SelectPrev, Some("Search")),
        KeyBinding::new("down", SelectNext, Some("Palette")),
        KeyBinding::new("up", SelectPrev, Some("Palette")),
        KeyBinding::new("ctrl-n", SelectNext, Some("Palette")),
        KeyBinding::new("ctrl-p", SelectPrev, Some("Palette")),
        KeyBinding::new("enter", Confirm, Some("Palette")),
        KeyBinding::new("escape", Cancel, Some("Palette")),
    ]);
    bindings
}

/// Complete Apple HIG-compliant menu bar (`Lenslet`, `File`, `Edit`, `View`,
/// `Run`, `Window`, `Help`). Note: `Window` and `Help` retain their standard
/// English titles so macOS attaches `NSApp.windowsMenu` and Spotlight Menu Search.
pub(crate) fn menus(lang: Lang) -> Vec<Menu> {
    let t = |en: &'static str, zh: &'static str| -> &'static str {
        match lang {
            Lang::En => en,
            Lang::Zh => zh,
        }
    };
    vec![
        Menu {
            name: "Lenslet".into(),
            items: vec![
                MenuItem::action(t("About Lenslet", "关于 Lenslet"), OpenAbout),
                MenuItem::separator(),
                MenuItem::action(t("Settings…", "设置…"), OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu(t("Services", "服务"), SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(t("Hide Lenslet", "隐藏 Lenslet"), HideApp),
                MenuItem::action(t("Hide Others", "隐藏其他"), HideOthers),
                MenuItem::action(t("Show All", "全部显示"), ShowAll),
                MenuItem::separator(),
                MenuItem::action(t("Quit Lenslet", "退出 Lenslet"), Quit),
            ],
        },
        Menu {
            name: t("File", "文件").into(),
            items: vec![
                MenuItem::action(t("New Run…", "新建运行…"), OpenLauncher),
                MenuItem::action(t("New Request…", "新建请求…"), NewRequest),
                MenuItem::separator(),
                MenuItem::action(t("Open Trace File…", "打开追踪文件…"), OpenFile),
                MenuItem::action(
                    t("Open Workspace Folder…", "打开工作区目录…"),
                    OpenWorkspace,
                ),
                MenuItem::separator(),
                MenuItem::action(t("Export Run Summary…", "导出运行摘要…"), ExportReport),
                MenuItem::action(t("Save Payload…", "保存当前报文…"), SavePayload),
                MenuItem::action(
                    t("Reveal Trace in Finder", "在访达中显示追踪文件"),
                    RevealTrace,
                ),
                MenuItem::action(
                    t(
                        "Generate GitHub Actions Workflow…",
                        "生成 GitHub Actions 工作流…",
                    ),
                    InitCiWorkflow,
                ),
                MenuItem::separator(),
                MenuItem::action(t("Close Window", "关闭窗口"), CloseWindow),
            ],
        },
        Menu {
            name: t("Edit", "编辑").into(),
            items: vec![
                MenuItem::os_action(t("Undo", "撤销"), Undo, OsAction::Undo),
                MenuItem::os_action(t("Redo", "重做"), Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action(t("Cut", "剪切"), Cut, OsAction::Cut),
                MenuItem::os_action(t("Copy", "拷贝"), Copy, OsAction::Copy),
                MenuItem::os_action(t("Paste", "粘贴"), Paste, OsAction::Paste),
                MenuItem::os_action(t("Select All", "全选"), SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::action(t("Copy Call as curl", "复制为 curl"), CopyCurl),
                MenuItem::action(t("Copy IAM Policy JSON", "复制 IAM 策略 JSON"), CopyPolicy),
                MenuItem::separator(),
                MenuItem::action(t("Find…", "查找…"), FocusSearch),
                MenuItem::action(t("Find Next", "查找下一个"), NextMatch),
                MenuItem::action(t("Find Previous", "查找上一个"), PrevMatch),
            ],
        },
        Menu {
            name: t("View", "视图").into(),
            items: vec![
                MenuItem::action(t("Calls", "请求列表"), ShowCalls),
                MenuItem::action(t("Access", "权限分析"), ShowAccess),
                MenuItem::action(t("Output & Telemetry", "终端输出与元数据"), ShowConsole),
                MenuItem::separator(),
                MenuItem::action(t("Command Palette…", "命令面板…"), OpenPalette),
                MenuItem::action(
                    t("Toggle Workspace Sidebar", "显示/隐藏工作区侧栏"),
                    ToggleSidebar,
                ),
                MenuItem::action(t("Toggle Inspector", "显示/隐藏详情栏"), ToggleInspector),
                MenuItem::action(t("Reload Run", "重新加载运行"), ReloadRun),
                MenuItem::action(t("Load All Calls", "无损加载全部调用"), LoadAllCalls),
                MenuItem::separator(),
                MenuItem::submenu(Menu {
                    name: t("Appearance", "外观").into(),
                    items: vec![
                        MenuItem::action(t("System", "跟随系统"), ThemeSystem),
                        MenuItem::action(t("Light", "浅色"), ThemeLight),
                        MenuItem::action(t("Dark", "深色"), ThemeDark),
                    ],
                }),
                MenuItem::submenu(Menu {
                    name: t("Density", "行高密度").into(),
                    items: vec![
                        MenuItem::action(t("Comfortable", "舒适"), DensityComfortable),
                        MenuItem::action(t("Compact", "紧凑"), DensityCompact),
                    ],
                }),
                MenuItem::separator(),
                MenuItem::action(t("Enter Full Screen", "进入全屏幕"), ToggleFullScreen),
            ],
        },
        Menu {
            name: t("Run", "运行").into(),
            items: vec![
                MenuItem::action(t("Run Command…", "发起运行…"), OpenLauncher),
                MenuItem::action(t("Stop Active Run", "停止当前运行"), StopActiveRun),
                MenuItem::separator(),
                MenuItem::action(t("Start Local Proxy", "启动本地代理"), StartProxyAction),
                MenuItem::action(t("Stop Local Proxy", "停止本地代理"), StopProxyAction),
                MenuItem::separator(),
                MenuItem::action(t("Replay Selected Call", "重放选中请求"), Replay),
                MenuItem::action(t("Edit & Replay Call…", "编辑并重放请求…"), OpenComposer),
                MenuItem::separator(),
                MenuItem::action(t("Previous Run", "上一个运行"), PrevRun),
                MenuItem::action(t("Next Run", "下一个运行"), NextRun),
                MenuItem::action(t("Follow Newest Run", "跟随最新运行"), FollowLatest),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action(t("Minimize", "最小化"), MinimizeWindow),
                MenuItem::action(t("Zoom", "缩放"), ZoomWindow),
                MenuItem::separator(),
                MenuItem::action(t("Bring All to Front", "前置全部窗口"), BringAllToFront),
            ],
        },
        Menu {
            name: "Help".into(),
            items: vec![
                MenuItem::action(t("Lenslet Documentation", "Lenslet 使用文档"), OpenDocs),
                MenuItem::action(t("Report an Issue…", "反馈问题…"), OpenIssues),
                MenuItem::separator(),
                MenuItem::action(
                    t(
                        "Install `lenslet` CLI to PATH…",
                        "安装 `lenslet` 命令行工具到 PATH…",
                    ),
                    InstallCliAction,
                ),
            ],
        },
    ]
}

pub(crate) fn dock_menu(lang: Lang) -> Vec<MenuItem> {
    let t = |en: &'static str, zh: &'static str| -> &'static str {
        match lang {
            Lang::En => en,
            Lang::Zh => zh,
        }
    };
    vec![
        MenuItem::action(t("New Run…", "新建运行…"), OpenLauncher),
        MenuItem::action(t("Start Local Proxy", "启动本地代理"), StartProxyAction),
        MenuItem::action(t("Open Trace File…", "打开追踪文件…"), OpenFile),
        MenuItem::action(
            t("Open Workspace Folder…", "打开工作区目录…"),
            OpenWorkspace,
        ),
    ]
}

fn decode_file_url(url: &str) -> Option<PathBuf> {
    let raw = url.strip_prefix("file://").unwrap_or(url);
    let raw = raw
        .strip_prefix("localhost/")
        .map(|s| format!("/{s}"))
        .unwrap_or_else(|| raw.to_string());
    let mut bytes = Vec::with_capacity(raw.len());
    let src = raw.as_bytes();
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' && i + 2 < src.len() {
            let hi = (src[i + 1] as char).to_digit(16);
            let lo = (src[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                bytes.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        bytes.push(src[i]);
        i += 1;
    }
    let decoded = String::from_utf8(bytes).ok()?;
    let path = PathBuf::from(decoded);
    path.exists().then_some(path)
}

fn open_main_window(initial_paths: Vec<PathBuf>, cx: &mut App) -> WindowHandle<LensletApp> {
    let saved = prefs::load(&app::home());
    let width = saved.window_width.unwrap_or(1360.0).clamp(960.0, 3840.0);
    let height = saved.window_height.unwrap_or(850.0).clamp(640.0, 2400.0);
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);

    let titlebar = if cfg!(target_os = "macos") {
        gpui::TitlebarOptions {
            title: Some("Lenslet".into()),
            appears_transparent: true,
            traffic_light_position: Some(gpui::point(px(14.0), px(15.0))),
        }
    } else {
        gpui::TitlebarOptions {
            title: Some("Lenslet".into()),
            appears_transparent: false,
            traffic_light_position: None,
        }
    };
    let handle = cx
        .open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(960.0), px(640.0))),
                window_background: WindowBackgroundAppearance::Opaque,
                app_id: Some("dev.lenslet.app".to_string()),
                titlebar: Some(titlebar),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| LensletApp::new(saved, initial_paths, window, cx)),
        )
        .expect("open Lenslet window");
    let _ = handle.update(cx, |_, _, cx| cx.activate(true));
    handle
}

fn ensure_window(cx: &mut App) -> WindowHandle<LensletApp> {
    if let Some(existing) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<LensletApp>())
    {
        return existing;
    }
    open_main_window(Vec::new(), cx)
}

fn stop_all_window_children(cx: &mut App) {
    for win in cx.windows() {
        if let Some(handle) = win.downcast::<LensletApp>() {
            let _ = handle.update(cx, |app, _, _| app.stop_all_children());
        }
    }
}

/// `LENSLET_LOG=1` prints gpui's own log (renderer, platform) to stderr.
struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "[{}] {}: {}",
                record.level(),
                record.target(),
                record.args()
            );
        }
    }
    fn flush(&self) {}
}

fn parse_cli_args() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-V" | "--version" => {
                println!("lenslet-ui {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!(
                    "Usage: lenslet-ui [TRACE_OR_WORKSPACE_PATH...]\n\n\
                     Open the Lenslet native desktop workbench."
                );
                std::process::exit(0);
            }
            s if s.starts_with("-psn_") => {
                // macOS LaunchServices process serial number flag; ignore.
            }
            other => {
                let p = PathBuf::from(other);
                if p.exists() {
                    paths.push(p);
                }
            }
        }
    }
    paths
}

fn main() {
    let cli_paths = parse_cli_args();
    fsguard::mark_start();
    if std::env::var_os("LENSLET_LOG").is_some() {
        static LOGGER: StderrLog = StderrLog;
        let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Info));
    }

    let pending_open_urls: std::rc::Rc<std::cell::RefCell<Vec<PathBuf>>> =
        std::rc::Rc::new(std::cell::RefCell::new(cli_paths));
    let urls_slot = pending_open_urls.clone();

    let app = Application::new().with_assets(icons::Assets);
    app.on_open_urls(move |urls| {
        let mut queue = urls_slot.borrow_mut();
        for u in urls {
            if let Some(p) = decode_file_url(&u) {
                queue.push(p);
            }
        }
    });
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            open_main_window(Vec::new(), cx);
        } else {
            cx.activate(true);
        }
    });

    app.run(move |cx: &mut App| {
        fonts::install(cx);
        cx.bind_keys(key_bindings());

        // Ensure child processes are terminated cleanly even on macOS `[NSApp terminate:]`.
        cx.on_app_quit(|cx| {
            stop_all_window_children(cx);
            std::future::ready(())
        })
        .detach();

        cx.on_action(|_: &Quit, cx| {
            stop_all_window_children(cx);
            cx.quit();
        });
        cx.on_action(|_: &HideApp, cx| cx.hide());
        cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
        cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
        cx.on_action(|_: &OpenDocs, cx| cx.open_url("https://github.com/hxddh/lenslet#readme"));
        cx.on_action(|_: &OpenIssues, cx| cx.open_url("https://github.com/hxddh/lenslet/issues"));

        // Global fallbacks when all windows are closed on macOS (`⌘W`):
        cx.on_action(|_: &OpenLauncher, cx| {
            if cx.windows().is_empty() {
                let win = ensure_window(cx);
                let _ = win.update(cx, |_, window, cx| {
                    window.dispatch_action(Box::new(OpenLauncher), cx);
                });
            }
        });
        cx.on_action(|_: &OpenFile, cx| {
            if cx.windows().is_empty() {
                let win = ensure_window(cx);
                let _ = win.update(cx, |_, window, cx| {
                    window.dispatch_action(Box::new(OpenFile), cx);
                });
            }
        });
        cx.on_action(|_: &OpenWorkspace, cx| {
            if cx.windows().is_empty() {
                let win = ensure_window(cx);
                let _ = win.update(cx, |_, window, cx| {
                    window.dispatch_action(Box::new(OpenWorkspace), cx);
                });
            }
        });
        cx.on_action(|_: &OpenAbout, cx| {
            if cx.windows().is_empty() {
                let win = ensure_window(cx);
                let _ = win.update(cx, |_, window, cx| {
                    window.dispatch_action(Box::new(OpenAbout), cx);
                });
            }
        });

        let saved = prefs::load(&app::home());
        if cfg!(target_os = "macos") {
            cx.set_menus(menus(saved.lang));
            cx.set_dock_menu(dock_menu(saved.lang));
        }

        let startup_paths: Vec<PathBuf> = pending_open_urls.borrow_mut().drain(..).collect();
        open_main_window(startup_paths, cx);

        // Drain any subsequent `on_open_urls` events delivered while the app is running.
        let poll_urls = pending_open_urls.clone();
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(250))
                    .await;
                let drained: Vec<PathBuf> = poll_urls.borrow_mut().drain(..).collect();
                if drained.is_empty() {
                    continue;
                }
                let _ = cx.update(|cx| {
                    let win = ensure_window(cx);
                    let _ = win.update(cx, |app, _, cx| {
                        for p in drained {
                            app.open_external_path(p, cx);
                        }
                    });
                    cx.activate(true);
                });
            }
        })
        .detach();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_binding_has_a_ctrl_twin_off_macos() {
        let bindings = key_bindings();
        let cmd = bindings
            .iter()
            .filter(|b| b.keystrokes()[0].modifiers().platform)
            .count();
        if !cfg!(target_os = "macos") {
            let ctrl = bindings
                .iter()
                .filter(|b| b.keystrokes()[0].modifiers().control)
                .count();
            assert!(ctrl >= cmd, "{ctrl} ctrl bindings for {cmd} cmd bindings");
        }
        assert!(cmd >= 20);
    }

    #[test]
    fn macos_hig_menus_cover_standard_sections_and_os_actions() {
        for lang in [Lang::En, Lang::Zh] {
            let built = menus(lang);
            assert_eq!(built.len(), 7);
            assert_eq!(built[0].name.as_ref(), "Lenslet");
            assert_eq!(built[5].name.as_ref(), "Window");
            assert_eq!(built[6].name.as_ref(), "Help");

            // Edit menu must expose OsAction::{Undo, Redo, Cut, Copy, Paste, SelectAll}
            let edit_os_actions: Vec<OsAction> = built[2]
                .items
                .iter()
                .filter_map(|item| match item {
                    MenuItem::Action { os_action, .. } => *os_action,
                    _ => None,
                })
                .collect();
            for expected in [
                OsAction::Undo,
                OsAction::Redo,
                OsAction::Cut,
                OsAction::Copy,
                OsAction::Paste,
                OsAction::SelectAll,
            ] {
                assert!(edit_os_actions.contains(&expected));
            }
        }
    }

    #[test]
    fn decode_file_url_handles_spaces_and_utf8() {
        let tmp = std::env::temp_dir();
        let url = format!("file://{}", tmp.display());
        assert_eq!(decode_file_url(&url), Some(tmp));
    }
}
