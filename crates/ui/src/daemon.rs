//! Active desktop control plane: local proxy lifecycle, emulator discovery,
//! shell SDK environment snippets, and 1-click CLI installation to `PATH`.
//!
//! All network probes and process spawns check [`crate::fsguard::assert_io_allowed`]
//! and run strictly on GPUI's background executor.

use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use crate::fsguard;

pub const DEFAULT_PROXY_PORT: u16 = 9297;

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
    pub emulators: Vec<DetectedEmulator>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetectedEmulator {
    pub name: &'static str,
    pub port: u16,
}

/// Probe `127.0.0.1:port/_lenslet/health` and common local AWS emulator ports.
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
    }

    for (name, emu_port) in [
        ("AWS Emulator", 9298u16),
        ("LocalStack", 4566),
        ("Moto", 5000),
        ("MinIO", 9000),
        ("DynamoDB Local", 8000),
        ("ElasticMQ", 9324),
    ] {
        if tcp_open(emu_port) {
            health.emulators.push(DetectedEmulator {
                name,
                port: emu_port,
            });
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

/// Locate the sibling `lenslet` CLI binary next to `lenslet-ui`.
pub fn sibling_cli_binary() -> Result<PathBuf, String> {
    fsguard::assert_io_allowed();
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("cannot locate current executable: {e}"))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "cannot determine executable folder".to_string())?;
    let name = if cfg!(windows) {
        "lenslet.exe"
    } else {
        "lenslet"
    };
    let candidate = dir.join(name);
    if candidate.is_file() {
        return Ok(candidate);
    }
    Err(format!(
        "CLI binary `{}` not found next to UI",
        candidate.display()
    ))
}

/// Spawn `lenslet run --listen <port>` in the background as a managed child process.
pub fn spawn_proxy(port: u16, enforce: bool, cwd: Option<&Path>) -> Result<Child, String> {
    fsguard::assert_io_allowed();
    let bin = sibling_cli_binary()?;
    let mut cmd = Command::new(&bin);
    cmd.arg("run")
        .arg("--listen")
        .arg(port.to_string())
        .arg("--quiet")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if enforce {
        cmd.arg("--enforce");
    }
    if let Some(dir) = cwd.filter(|d| d.is_dir()) {
        cmd.current_dir(dir);
    }
    cmd.spawn()
        .map_err(|e| format!("failed to start `lenslet run`: {e}"))
}

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

pub fn shell_env_snippet(port: u16) -> String {
    format!(
        "unset AWS_PROFILE AWS_DEFAULT_PROFILE AWS_SESSION_TOKEN\nexport AWS_ENDPOINT_URL=http://127.0.0.1:{port}\nexport AWS_ACCESS_KEY_ID=lenslet-local\nexport AWS_SECRET_ACCESS_KEY=lenslet-local\nexport AWS_DEFAULT_REGION=us-east-1"
    )
}
