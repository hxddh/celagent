//! Active desktop control plane: local proxy lifecycle, end-to-end command &
//! test-suite execution, local emulator discovery & launch, CI workflow
//! generation, multi-shell SDK snippets, and 1-click CLI installation.
//!
//! All network probes and process spawns check [`crate::fsguard::assert_io_allowed`]
//! and run strictly on GPUI's background executor.

use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};

use crate::fsguard;

pub const DEFAULT_PROXY_PORT: u16 = 9297;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShellFormat {
    #[default]
    Sh,
    Fish,
    Pwsh,
    Dotenv,
    Docker,
}

impl ShellFormat {
    pub const ALL: [ShellFormat; 5] = [
        ShellFormat::Sh,
        ShellFormat::Fish,
        ShellFormat::Pwsh,
        ShellFormat::Dotenv,
        ShellFormat::Docker,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ShellFormat::Sh => "sh / zsh",
            ShellFormat::Fish => "fish",
            ShellFormat::Pwsh => "PowerShell",
            ShellFormat::Dotenv => ".env",
            ShellFormat::Docker => "docker",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RunGateMode {
    #[default]
    Record,
    UpdateLock,
    CheckLock,
    Enforce,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmulatorKind {
    AwsEmulator,
    Moto,
    LocalStack,
    Minio,
}

impl EmulatorKind {
    pub fn label(self) -> &'static str {
        match self {
            EmulatorKind::AwsEmulator => "AWS Emulator (:9298)",
            EmulatorKind::Moto => "Moto (:5000)",
            EmulatorKind::LocalStack => "LocalStack (:4566)",
            EmulatorKind::Minio => "MinIO (:9000)",
        }
    }

    pub fn binary_name(self) -> &'static str {
        match self {
            EmulatorKind::AwsEmulator => "aws-emulator",
            EmulatorKind::Moto => "moto_server",
            EmulatorKind::LocalStack => "localstack",
            EmulatorKind::Minio => "minio",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            EmulatorKind::AwsEmulator => 9298,
            EmulatorKind::Moto => 5000,
            EmulatorKind::LocalStack => 4566,
            EmulatorKind::Minio => 9000,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProxyHealth {
    pub running: bool,
    pub managed: bool,
    pub port: u16,
    pub pid: Option<u32>,
    pub run: Option<String>,
    pub upstream: Option<String>,
    pub enforce: bool,
    pub calls: usize,
    pub errors: usize,
    pub blocked: usize,
    pub emulators: Vec<DetectedEmulator>,
    pub installed_emulators: Vec<EmulatorKind>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedEmulator {
    pub name: &'static str,
    pub port: u16,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct ProxyLaunchOpts {
    pub port: u16,
    pub upstream: Option<String>,
    pub routes: Vec<String>,
    pub enforce: bool,
    pub allow_docker: bool,
    pub cwd: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct CommandRunOpts {
    pub command_line: String,
    pub cwd: PathBuf,
    pub upstream: Option<String>,
    pub routes: Vec<String>,
    pub gate_mode: RunGateMode,
    pub allow_docker: bool,
    pub runs_dir: PathBuf,
}

pub struct ManagedRun {
    pub child: Child,
    pub trace_path: PathBuf,
    pub command_line: String,
}

/// Resolve a complete developer `PATH` even when `Lenslet.app` was launched
/// from macOS Finder/Dock/Spotlight (where `launchd` only sets `/usr/bin:/bin`).
pub fn augmented_path() -> OsString {
    static AUGMENTED: OnceLock<OsString> = OnceLock::new();
    AUGMENTED
        .get_or_init(|| {
            let mut dirs: Vec<PathBuf> = Vec::new();
            let mut push_unique = |p: PathBuf| {
                if !p.as_os_str().is_empty() && !dirs.contains(&p) {
                    dirs.push(p);
                }
            };
            if let Some(cur) = std::env::var_os("PATH") {
                for p in std::env::split_paths(&cur) {
                    push_unique(p);
                }
            }
            #[cfg(unix)]
            if let Some(shell_path) = probe_login_shell_path() {
                for p in std::env::split_paths(&shell_path) {
                    push_unique(p);
                }
            }
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                for rel in [
                    ".local/bin",
                    ".cargo/bin",
                    ".orbstack/bin",
                    ".rd/bin",
                    ".volta/bin",
                    ".bun/bin",
                    ".pyenv/shims",
                    ".asdf/shims",
                    "go/bin",
                ] {
                    let candidate = home.join(rel);
                    if candidate.is_dir() {
                        push_unique(candidate);
                    }
                }
            }
            for sys in [
                "/opt/homebrew/bin",
                "/opt/homebrew/sbin",
                "/usr/local/bin",
                "/usr/local/sbin",
                "/usr/bin",
                "/bin",
                "/usr/sbin",
                "/sbin",
            ] {
                let p = PathBuf::from(sys);
                if p.is_dir() {
                    push_unique(p);
                }
            }
            std::env::join_paths(dirs)
                .unwrap_or_else(|_| std::env::var_os("PATH").unwrap_or_default())
        })
        .clone()
}

#[cfg(unix)]
fn probe_login_shell_path() -> Option<OsString> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/zsh".to_string());
    let mut child = Command::new(&shell)
        .args(["-lc", "printf '%s' \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_millis(300);
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            if !status.success() {
                return None;
            }
            let mut out = String::new();
            child.stdout.take()?.read_to_string(&mut out).ok()?;
            let trimmed = out.trim();
            return (!trimmed.is_empty()).then(|| OsString::from(trimmed));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

pub fn find_in_path(bin_name: &str) -> Option<PathBuf> {
    let exe_name = if cfg!(windows) && !bin_name.ends_with(".exe") {
        format!("{bin_name}.exe")
    } else {
        bin_name.to_string()
    };
    for dir in std::env::split_paths(&augmented_path()) {
        let candidate = dir.join(&exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Probe `127.0.0.1:port/_lenslet/health`, active local emulator ports, and installed emulator CLIs.
pub fn probe_environment(port: u16, managed: bool) -> ProxyHealth {
    fsguard::assert_io_allowed();
    let mut health = ProxyHealth {
        port,
        managed,
        ..ProxyHealth::default()
    };

    if let Some(json) = http_get_json(port, "/_lenslet/health") {
        health.running = true;
        health.pid = json.get("pid").and_then(|v| v.as_u64()).map(|n| n as u32);
        health.run = json.get("run").and_then(|v| v.as_str()).map(str::to_string);
        health.upstream = json
            .get("upstream")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        health.enforce = json
            .get("enforce")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        health.calls = json.get("calls").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        health.errors = json.get("errors").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        health.blocked = json.get("blocked").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
    }

    for (name, emu_port, tls) in [
        ("AWS Emulator", 9298u16, true),
        ("LocalStack", 4566, false),
        ("Moto", 5000, false),
        ("MinIO", 9000, false),
        ("DynamoDB Local", 8000, false),
        ("ElasticMQ", 9324, false),
    ] {
        if tcp_open(emu_port) {
            let scheme = if tls { "https" } else { "http" };
            health.emulators.push(DetectedEmulator {
                name,
                port: emu_port,
                url: format!("{scheme}://127.0.0.1:{emu_port}"),
            });
        }
    }

    for kind in [
        EmulatorKind::AwsEmulator,
        EmulatorKind::Moto,
        EmulatorKind::LocalStack,
        EmulatorKind::Minio,
    ] {
        if find_in_path(kind.binary_name()).is_some() {
            health.installed_emulators.push(kind);
        }
    }

    health
}

fn tcp_open(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(80)).is_ok()
}

fn http_get_json(port: u16, path: &str) -> Option<serde_json::Value> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(120)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(180)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(180)));
    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::with_capacity(1024);
    stream.read_to_end(&mut buf).ok()?;
    let text = std::str::from_utf8(&buf).ok()?;
    if !text.starts_with("HTTP/1.1 200") && !text.starts_with("HTTP/1.0 200") {
        return None;
    }
    let (_, body) = text.split_once("\r\n\r\n")?;
    serde_json::from_str(body.trim()).ok()
}

/// Locate the bundled `lenslet` CLI binary next to `lenslet-ui` (inside `Lenslet.app`
/// or release directory), falling back to user `PATH`.
pub fn sibling_cli_binary() -> Result<PathBuf, String> {
    fsguard::assert_io_allowed();
    let name = if cfg!(windows) {
        "lenslet.exe"
    } else {
        "lenslet"
    };
    if let Ok(exe) = std::env::current_exe().and_then(|p| p.canonicalize())
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    if let Some(found) = find_in_path("lenslet") {
        return Ok(found);
    }
    Err("CLI binary `lenslet` not found next to Lenslet UI or on PATH".to_string())
}

/// Spawn `lenslet run` as a managed proxy and verify it actually binds & responds
/// on `/_lenslet/health`, surfacing exact `stderr` diagnostics on failure.
pub fn spawn_proxy_verified(opts: &ProxyLaunchOpts) -> Result<Child, String> {
    fsguard::assert_io_allowed();
    let bin = sibling_cli_binary()?;
    let err_path = std::env::temp_dir().join(format!(
        "lenslet-proxy-err-{}-{}.log",
        std::process::id(),
        opts.port
    ));
    let err_file =
        fs::File::create(&err_path).map_err(|e| format!("cannot open temporary proxy log: {e}"))?;

    let mut cmd = Command::new(&bin);
    cmd.env("PATH", augmented_path())
        .arg("run")
        .arg("--listen")
        .arg(opts.port.to_string())
        .arg("--quiet")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(err_file));
    if let Some(up) = opts
        .upstream
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        cmd.arg("--upstream").arg(up);
    }
    for route in &opts.routes {
        let r = route.trim();
        if !r.is_empty() {
            cmd.arg("--route").arg(r);
        }
    }
    if opts.enforce {
        cmd.arg("--enforce");
    }
    if opts.allow_docker {
        cmd.arg("--allow-docker");
    }
    if let Some(dir) = opts.cwd.as_deref().filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start `lenslet run`: {e}"))?;

    let deadline = Instant::now() + Duration::from_millis(600);
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            let detail = fs::read_to_string(&err_path).unwrap_or_default();
            let _ = fs::remove_file(&err_path);
            let msg = detail
                .lines()
                .find(|l| !l.trim().is_empty())
                .map(str::trim)
                .unwrap_or("proxy exited immediately");
            return Err(format!("{msg} (exit {})", status.code().unwrap_or(1)));
        }
        if http_get_json(opts.port, "/_lenslet/health").is_some() {
            let _ = fs::remove_file(&err_path);
            return Ok(child);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let _ = fs::remove_file(&err_path);
    Ok(child)
}

/// Launch an end-to-end command / test suite under `lenslet run`, streaming trace
/// events to `<trace_path>` and stdout/stderr to `<log_path>`.
pub fn spawn_command_run(opts: &CommandRunOpts) -> Result<ManagedRun, String> {
    fsguard::assert_io_allowed();
    let command_trimmed = opts.command_line.trim();
    if command_trimmed.is_empty() {
        return Err("Please enter a command to run (e.g. `pytest` or `npm test`)".into());
    }
    if !opts.cwd.is_dir() {
        return Err(format!(
            "Workspace folder `{}` does not exist",
            opts.cwd.display()
        ));
    }
    let bin = sibling_cli_binary()?;
    let trace_path = lenslet_core::runs::create(&opts.runs_dir)
        .map_err(|e| format!("cannot create run trace file: {e}"))?;
    let log_path = lenslet_core::runs::log_path_for(&trace_path);

    let mut cmd = Command::new(&bin);
    cmd.env("PATH", augmented_path())
        .current_dir(&opts.cwd)
        .arg("run")
        .arg("--trace")
        .arg(&trace_path)
        .arg("--log")
        .arg(&log_path);

    if let Some(up) = opts
        .upstream
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        cmd.arg("--upstream").arg(up);
    }
    for route in &opts.routes {
        let r = route.trim();
        if !r.is_empty() {
            cmd.arg("--route").arg(r);
        }
    }
    if opts.allow_docker {
        cmd.arg("--allow-docker");
    }
    match opts.gate_mode {
        RunGateMode::Record => {}
        RunGateMode::UpdateLock => {
            cmd.arg("--update");
        }
        RunGateMode::CheckLock => {
            cmd.arg("--check").arg("--strict");
        }
        RunGateMode::Enforce => {
            cmd.arg("--enforce");
        }
    }
    cmd.arg("--");
    if cfg!(windows) {
        cmd.args(["cmd", "/C", command_trimmed]);
    } else {
        cmd.args(["sh", "-c", command_trimmed]);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to launch `lenslet run`: {e}"))?;

    Ok(ManagedRun {
        child,
        trace_path,
        command_line: command_trimmed.to_string(),
    })
}

/// Start an installed local emulator directly from the desktop workbench.
pub fn spawn_local_emulator(kind: EmulatorKind, home: &Path) -> Result<(Child, u16), String> {
    fsguard::assert_io_allowed();
    let port = kind.default_port();
    if tcp_open(port) {
        return Err(format!("Port {port} is already listening"));
    }
    let bin = find_in_path(kind.binary_name())
        .ok_or_else(|| format!("`{}` is not installed on PATH", kind.binary_name()))?;
    let mut cmd = Command::new(&bin);
    cmd.env("PATH", augmented_path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match kind {
        EmulatorKind::AwsEmulator => {
            cmd.env("EMULATOR_TELEMETRY", "off")
                .args(["--telemetry", "off", "--port", "9298"]);
        }
        EmulatorKind::Moto => {
            cmd.args(["-H", "127.0.0.1", "-p", "5000"]);
        }
        EmulatorKind::LocalStack => {
            cmd.args(["start"]);
        }
        EmulatorKind::Minio => {
            let data_dir = home.join(".lenslet/minio-data");
            let _ = fs::create_dir_all(&data_dir);
            cmd.arg("server")
                .arg(&data_dir)
                .args(["--address", "127.0.0.1:9000"]);
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", kind.binary_name()))?;

    let deadline = Instant::now() + Duration::from_millis(1_800);
    while Instant::now() < deadline {
        if let Ok(Some(st)) = child.try_wait() {
            return Err(format!(
                "{} exited early (code {})",
                kind.binary_name(),
                st.code().unwrap_or(1)
            ));
        }
        if tcp_open(port) {
            return Ok((child, port));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok((child, port))
}

/// Discover common test/run commands from files in `cwd` (`package.json`, `Cargo.toml`, `pyproject.toml`, `go.mod`, `Makefile`).
pub fn detect_project_commands(cwd: &Path) -> Vec<(String, String)> {
    fsguard::assert_io_allowed();
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |label: &str, cmd: &str| {
        if !out.iter().any(|(_, c)| c == cmd) {
            out.push((label.to_string(), cmd.to_string()));
        }
    };

    let pkg_json = cwd.join("package.json");
    if let Ok(text) = fs::read_to_string(&pkg_json)
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(scripts) = val.get("scripts").and_then(|s| s.as_object())
    {
        let runner = if cwd.join("pnpm-lock.yaml").is_file() {
            "pnpm"
        } else if cwd.join("yarn.lock").is_file() {
            "yarn"
        } else if cwd.join("bun.lockb").is_file() || cwd.join("bun.lock").is_file() {
            "bun run"
        } else {
            "npm run"
        };
        for key in ["test", "test:integration", "test:e2e", "dev", "start"] {
            if scripts.contains_key(key) {
                let cmd = if key == "test" && runner == "npm run" {
                    "npm test".to_string()
                } else {
                    format!("{runner} {key}")
                };
                push(&cmd, &cmd);
            }
        }
    }
    if cwd.join("pyproject.toml").is_file()
        || cwd.join("pytest.ini").is_file()
        || cwd.join("requirements.txt").is_file()
    {
        if cwd.join("uv.lock").is_file() {
            push("uv run pytest", "uv run pytest");
        }
        push("pytest", "pytest");
    }
    if cwd.join("Cargo.toml").is_file() {
        push("cargo test", "cargo test");
    }
    if cwd.join("go.mod").is_file() {
        push("go test ./...", "go test ./...");
    }
    if cwd.join("Makefile").is_file() {
        push("make test", "make test");
    }
    if out.is_empty() {
        out.push(("pytest".into(), "pytest".into()));
        out.push(("npm test".into(), "npm test".into()));
        out.push(("cargo test".into(), "cargo test".into()));
    }
    out
}

/// Generate `.github/workflows/lenslet.yml` in `cwd` directly from the workbench.
pub fn generate_ci_workflow(
    cwd: &Path,
    command_line: &str,
    upstream: &str,
    emulator_cmd: Option<&str>,
) -> Result<PathBuf, String> {
    fsguard::assert_io_allowed();
    if !cwd.is_dir() {
        return Err(format!("Workspace `{}` is not a directory", cwd.display()));
    }
    let wf_path = cwd.join(".github/workflows/lenslet.yml");
    if let Some(parent) = wf_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let cmd_trimmed = if command_line.trim().is_empty() {
        "npm test"
    } else {
        command_line.trim()
    };
    let up_trimmed = if upstream.trim().is_empty() {
        "https://127.0.0.1:9298"
    } else {
        upstream.trim()
    };
    let yaml_quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let emu_val = emulator_cmd
        .filter(|s| !s.trim().is_empty())
        .map(yaml_quote)
        .unwrap_or_else(|| "'moto_server -p 5000'".to_string());
    let version = env!("CARGO_PKG_VERSION");
    let content = format!(
        "# Lenslet: show the AWS access each pull request needs, and fail when it grows.\n\
         # Docs: https://github.com/hxddh/lenslet#ci\n\
         name: Lenslet\n\
         on:\n  pull_request:\n  push:\n    branches: [main]\n\
         permissions:\n  contents: read\n  pull-requests: write\n\
         jobs:\n  aws-access:\n    runs-on: ubuntu-latest\n    steps:\n\
         \x20     - uses: actions/checkout@v5\n\
         \x20     - uses: hxddh/lenslet@lenslet-v{version}\n\
         \x20       with:\n\
         \x20         command: {}\n\
         \x20         upstream: {}\n\
         \x20         emulator: {emu_val}\n",
        yaml_quote(cmd_trimmed),
        yaml_quote(up_trimmed),
    );
    fs::write(&wf_path, content).map_err(|e| format!("cannot write {}: {e}", wf_path.display()))?;
    Ok(wf_path)
}

/// Read up to the last `max_bytes` of a run's companion stdout/stderr `.log` file.
pub fn read_run_log(log_path: &Path, max_bytes: usize) -> Option<String> {
    fsguard::assert_io_allowed();
    let mut file = fs::File::open(log_path).ok()?;
    let len = file.metadata().ok()?.len() as usize;
    if len == 0 {
        return Some(String::new());
    }
    let mut buf = Vec::new();
    if len > max_bytes {
        use std::io::{Seek, SeekFrom};
        let skip = (len - max_bytes) as u64;
        let _ = file.seek(SeekFrom::Start(skip));
    }
    file.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Request a clean shutdown via `POST /_lenslet/shutdown` (or `SIGTERM`) so
/// `lenslet run` seals the trace and writes its `end` report before exiting;
/// falls back to `kill()` only if `grace` expires.
pub fn stop_proxy_gracefully(child: Child, port: u16) {
    fsguard::assert_io_allowed();
    stop_proxy_inner(child, Some(port), Duration::from_millis(2_500));
}

/// Stop an active `ManagedRun` cleanly via its proxy endpoint + `SIGTERM`.
pub fn stop_managed_run(run: ManagedRun, endpoint_port: Option<u16>) {
    fsguard::assert_io_allowed();
    stop_proxy_inner(run.child, endpoint_port, Duration::from_millis(2_500));
}

/// Graceful child shutdown safe to call during window teardown (`Quit` / `Drop`)
/// without tripping [`fsguard`], bounded so quitting never stalls the UI.
pub fn stop_proxy_on_exit(child: Child, port: u16) {
    stop_proxy_inner(child, Some(port), Duration::from_millis(800));
}

pub fn stop_child_on_exit(mut child: Child) {
    send_sigterm(&child);
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn stop_proxy_inner(mut child: Child, port: Option<u16>, grace: Duration) {
    if child.try_wait().ok().flatten().is_some() {
        return;
    }
    if let Some(port) = port.filter(|p| *p > 0) {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(100)) {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
            let _ = stream.set_write_timeout(Some(Duration::from_millis(100)));
            let req = format!(
                "POST /_lenslet/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            if stream.write_all(req.as_bytes()).is_ok() {
                let _ = stream.flush();
                let mut drain = [0u8; 256];
                let _ = stream.read(&mut drain);
            }
        }
    }
    send_sigterm(&child);
    let deadline = Instant::now() + grace;
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn send_sigterm(child: &Child) {
    let pid = child.id();
    if pid > 0 {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(not(unix))]
fn send_sigterm(_child: &Child) {}

/// Symlink (or copy on Windows) the bundled `lenslet` CLI into `~/.local/bin/lenslet`
/// (or `/usr/local/bin/lenslet` when already writable).
pub fn install_cli_to_path(home: &Path) -> Result<PathBuf, String> {
    fsguard::assert_io_allowed();
    let src = sibling_cli_binary()?;
    let usr_local = Path::new("/usr/local/bin/lenslet");
    if cfg!(unix)
        && let Some(parent) = usr_local.parent()
        && parent.is_dir()
        && try_link_binary(&src, usr_local).is_ok()
    {
        return Ok(usr_local.to_path_buf());
    }
    let local_bin = home.join(".local/bin");
    fs::create_dir_all(&local_bin)
        .map_err(|e| format!("cannot create {}: {e}", local_bin.display()))?;
    let dest = local_bin.join(if cfg!(windows) {
        "lenslet.exe"
    } else {
        "lenslet"
    });
    try_link_binary(&src, &dest)?;
    Ok(dest)
}

fn try_link_binary(src: &Path, dest: &Path) -> Result<(), String> {
    let tmp = dest.with_extension(format!("tmp.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(src, &tmp)
            .map_err(|e| format!("cannot symlink {}: {e}", dest.display()))?;
    }
    #[cfg(not(unix))]
    {
        fs::copy(src, &tmp).map_err(|e| format!("cannot copy {}: {e}", dest.display()))?;
    }
    fs::rename(&tmp, dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot install {}: {e}", dest.display())
    })
}

pub fn shell_env_snippet_for(port: u16, fmt: ShellFormat) -> String {
    match fmt {
        ShellFormat::Sh => format!(
            "unset AWS_PROFILE AWS_DEFAULT_PROFILE AWS_SESSION_TOKEN\nexport AWS_ENDPOINT_URL=http://127.0.0.1:{port}\nexport AWS_ACCESS_KEY_ID=lenslet-local\nexport AWS_SECRET_ACCESS_KEY=lenslet-local\nexport AWS_DEFAULT_REGION=us-east-1"
        ),
        ShellFormat::Fish => format!(
            "set -e AWS_PROFILE AWS_DEFAULT_PROFILE AWS_SESSION_TOKEN\nset -gx AWS_ENDPOINT_URL http://127.0.0.1:{port}\nset -gx AWS_ACCESS_KEY_ID lenslet-local\nset -gx AWS_SECRET_ACCESS_KEY lenslet-local\nset -gx AWS_DEFAULT_REGION us-east-1"
        ),
        ShellFormat::Pwsh => format!(
            "Remove-Item Env:AWS_PROFILE, Env:AWS_DEFAULT_PROFILE, Env:AWS_SESSION_TOKEN -ErrorAction SilentlyContinue\n$env:AWS_ENDPOINT_URL = \"http://127.0.0.1:{port}\"\n$env:AWS_ACCESS_KEY_ID = \"lenslet-local\"\n$env:AWS_SECRET_ACCESS_KEY = \"lenslet-local\"\n$env:AWS_DEFAULT_REGION = \"us-east-1\""
        ),
        ShellFormat::Dotenv => format!(
            "AWS_ENDPOINT_URL=http://127.0.0.1:{port}\nAWS_ACCESS_KEY_ID=lenslet-local\nAWS_SECRET_ACCESS_KEY=lenslet-local\nAWS_DEFAULT_REGION=us-east-1"
        ),
        ShellFormat::Docker => format!(
            "-e AWS_ENDPOINT_URL=http://host.docker.internal:{port} -e AWS_ACCESS_KEY_ID=lenslet-local -e AWS_SECRET_ACCESS_KEY=lenslet-local -e AWS_DEFAULT_REGION=us-east-1"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_snippets_cover_all_supported_formats() {
        for fmt in ShellFormat::ALL {
            let text = shell_env_snippet_for(9297, fmt);
            assert!(text.contains("9297"), "missing port in {:?}", fmt);
            assert!(
                text.contains("AWS_ENDPOINT_URL"),
                "missing endpoint in {:?}",
                fmt
            );
        }
        assert!(shell_env_snippet_for(9297, ShellFormat::Docker).contains("host.docker.internal"));
        assert!(shell_env_snippet_for(9297, ShellFormat::Fish).contains("set -gx"));
    }

    #[test]
    fn detects_project_commands_and_generates_ci_workflow() {
        let dir = std::env::temp_dir().join(format!("lenslet-daemon-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.json"),
            r#"{"scripts":{"test":"vitest","test:e2e":"playwright test"}}"#,
        )
        .unwrap();
        fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();

        let cmds = detect_project_commands(&dir);
        assert!(cmds.iter().any(|(_, c)| c == "pnpm test"));
        assert!(cmds.iter().any(|(_, c)| c == "pnpm test:e2e"));

        let wf = generate_ci_workflow(&dir, "pnpm test", "http://127.0.0.1:4566", None).unwrap();
        let yaml = fs::read_to_string(&wf).unwrap();
        assert!(yaml.contains("command: 'pnpm test'"));
        assert!(yaml.contains("upstream: 'http://127.0.0.1:4566'"));

        let log_path = dir.join("sample.log");
        fs::write(&log_path, "line1\nline2\n").unwrap();
        assert_eq!(
            read_run_log(&log_path, 64).as_deref(),
            Some("line1\nline2\n")
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
