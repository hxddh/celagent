//! `lenslet`: run a program against local AWS and see every call it made.
//!
//! ```text
//! lenslet run -- npm test     proxy + env for one command, summary at the end
//! lenslet proxy               long-running proxy on 127.0.0.1:9297
//! lenslet ui                  open the window (also: bare `lenslet`)
//! ```

mod capture;
mod problem;
mod proxy;
mod runs;
mod summary;
mod trace;
mod wire;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{ExitCode, Stdio},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

use crate::{
    capture::MAX_CAPTURE_BYTES,
    proxy::{Options, Server},
    runs::RunInfo,
};

fn usage() -> String {
    format!(
        "lenslet {version} — see what your code sends to local AWS

Usage:
  lenslet run [options] [--] COMMAND [ARGS...]
      Run COMMAND with AWS_ENDPOINT_URL pointing at Lenslet, then print what it
      called and why calls failed. Exits with COMMAND's exit code.
  lenslet proxy [options]
      Keep a proxy running on 127.0.0.1:9297 (point SDKs at http://127.0.0.1:9297).
  lenslet ui
      Open the Lenslet window (same as running `lenslet` with no arguments).

Run options:
  --json PATH          also write the summary as JSON (`-` for stdout)
  --quiet              no banner or summary on stderr
  --ui                 open the window before running COMMAND
  --no-env             set only AWS_ENDPOINT_URL, leave credentials/config alone

Proxy options (run and proxy):
  --listen PORT        loopback port (run: a free port; proxy: 9297)
  --upstream ADDRESS   AWS Emulator port or http(s) URL (default 9298)
  --emulator-ca PATH   emulator CA (default ~/.aws/emulator/PORT/ca.pem)
  --trace PATH         trace file (default: a new file in ~/.lenslet/runs)
  --cert PATH / --key PATH
                       TLS identity for https:// clients (generated)

The listening port accepts plain HTTP and TLS. Bodies are forwarded unchanged;
the trace keeps up to {kib} KiB of each text body and is owner-only (0600).",
        version = env!("CARGO_PKG_VERSION"),
        kib = MAX_CAPTURE_BYTES / 1024,
    )
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `~/.lenslet/runs/x.jsonl` for display.
fn tilde(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let command = if args.is_empty() {
        "ui".to_string()
    } else {
        args.remove(0)
    };
    let result = match command.as_str() {
        "run" => runtime().and_then(|runtime| runtime.block_on(run(args))),
        "proxy" => runtime().and_then(|runtime| runtime.block_on(serve_proxy(args))),
        "ui" => open_ui(None).map(|()| 0),
        "-V" | "--version" | "version" => {
            println!("lenslet {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        "-h" | "--help" | "help" => {
            println!("{}", usage());
            Ok(0)
        }
        other => Err(anyhow::anyhow!(
            "unknown command `{other}`; try `lenslet --help`"
        )),
    };
    match result {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(error) => {
            eprintln!("lenslet: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("start the async runtime")
}

struct RunArgs {
    options: Options,
    command: Vec<String>,
    json: Option<String>,
    quiet: bool,
    ui: bool,
    no_env: bool,
}

fn parse_run(args: Vec<String>, home: &Path) -> Result<RunArgs> {
    let mut parsed = RunArgs {
        options: Options::new(home, 0),
        command: Vec::new(),
        json: None,
        quiet: false,
        ui: false,
        no_env: false,
    };
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} requires a value"));
        match arg.as_str() {
            "--" => {
                parsed.command.extend(it.by_ref());
                break;
            }
            "--json" => parsed.json = Some(value()?),
            "--quiet" | "-q" => parsed.quiet = true,
            "--ui" => parsed.ui = true,
            "--no-env" => parsed.no_env = true,
            "-h" | "--help" => bail!("{}", usage()),
            flag if flag.starts_with('-') => {
                if !parsed.options.accept(flag, &mut value)? {
                    bail!("unknown option `{flag}`; try `lenslet --help`");
                }
            }
            _ => {
                parsed.command.push(arg);
                parsed.command.extend(it.by_ref());
                break;
            }
        }
    }
    if parsed.command.is_empty() {
        bail!("`lenslet run` needs a command, e.g. `lenslet run -- npm test`");
    }
    Ok(parsed)
}

/// What the child process gets: variables to set, variables to remove, and a
/// note per change, so the banner can say exactly what Lenslet did.
#[derive(Debug, Default, PartialEq)]
struct ChildEnv {
    set: Vec<(String, String)>,
    remove: Vec<String>,
}

fn child_env(
    endpoint: &str,
    emulator_dir: &Path,
    no_env: bool,
    current: impl Fn(&str) -> Option<OsString>,
) -> ChildEnv {
    let mut env = ChildEnv::default();
    env.set
        .push(("AWS_ENDPOINT_URL".to_string(), endpoint.to_string()));
    if no_env {
        return env;
    }
    let mut injected = false;
    for (name, file) in [
        ("AWS_SHARED_CREDENTIALS_FILE", "emulator_credentials"),
        ("AWS_CONFIG_FILE", "config"),
    ] {
        let path = emulator_dir.join(file);
        if current(name).is_none() && path.is_file() {
            env.set.push((name.to_string(), path.display().to_string()));
            injected = true;
        }
    }
    // A named profile from the real config does not exist in the emulator's.
    if injected {
        for name in ["AWS_PROFILE", "AWS_DEFAULT_PROFILE"] {
            if current(name).is_some() {
                env.remove.push(name.to_string());
            }
        }
    }
    env
}

async fn run(args: Vec<String>) -> Result<i32> {
    let home = home();
    let RunArgs {
        options,
        command,
        json,
        quiet,
        ui,
        no_env,
    } = parse_run(args, &home)?;
    let trace_path = match options.trace.clone() {
        Some(path) => path,
        None => runs::create(&runs::dir(&home))?,
    };
    let emulator_up = options.upstream.reachable().await;
    if !emulator_up {
        eprintln!(
            "lenslet: warning: nothing is listening on {} — start AWS Emulator first:\n  \
             EMULATOR_TELEMETRY=off aws-emulator --telemetry off --port {}",
            options.upstream.base, options.upstream.port
        );
    }
    let server = Server::start(&options, trace_path.clone()).await?;
    let endpoint = server.endpoint();
    let mut info = RunInfo {
        kind: "run".to_string(),
        command: command.clone(),
        pid: std::process::id(),
        endpoint: endpoint.clone(),
        upstream: options.upstream.base.clone(),
        started: runs::now(),
        ..RunInfo::default()
    };
    runs::write_info(&trace_path, &info)?;

    let env = child_env(&endpoint, &options.emulator_dir(), no_env, |name| {
        std::env::var_os(name)
    });
    if !quiet {
        eprintln!(
            "lenslet: {} → {} · trace {}",
            endpoint,
            options.upstream.base,
            tilde(&trace_path, &home)
        );
        for (name, value) in &env.set {
            eprintln!("lenslet:   {name}={}", tilde(Path::new(value), &home));
        }
        for name in &env.remove {
            eprintln!("lenslet:   unset {name} (the emulator config has no such profile)");
        }
        let injected = env
            .set
            .iter()
            .any(|(name, _)| name == "AWS_SHARED_CREDENTIALS_FILE");
        if !no_env && !injected && std::env::var_os("AWS_SHARED_CREDENTIALS_FILE").is_none() {
            eprintln!(
                "lenslet: warning: no emulator credentials in {}; the program uses its own AWS config",
                tilde(&options.emulator_dir(), &home)
            );
        }
    }
    if ui && let Err(error) = open_ui(options.trace.as_deref()) {
        eprintln!("lenslet: {error:#}");
    }

    let mut child = tokio::process::Command::new(&command[0]);
    child.args(&command[1..]).stdin(Stdio::inherit());
    for (name, value) in &env.set {
        child.env(name, value);
    }
    for name in &env.remove {
        child.env_remove(name);
    }
    let started = Instant::now();
    let mut child = match child.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("lenslet: cannot start `{}`: {error}", command[0]);
            info.ended = Some(runs::now());
            info.exit_code = Some(127);
            runs::write_info(&trace_path, &info).ok();
            return Ok(127);
        }
    };
    // Ctrl-C reaches the child through the terminal; Lenslet waits for it to
    // exit so the summary still covers every call.
    let interrupts = tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
    let status = child.wait().await.context("wait for the command")?;
    interrupts.abort();
    let code = exit_code(status);
    let elapsed = started.elapsed().as_millis();

    let state = server.state.clone();
    server.stop(Duration::from_secs(2)).await;
    let summary = state.trace.summary();
    info.ended = Some(runs::now());
    info.exit_code = Some(code);
    runs::write_info(&trace_path, &info)?;

    if !quiet {
        eprint!("{}", summary.render(&tilde(&trace_path, &home), elapsed));
    }
    if let Some(target) = json {
        let mut value = serde_json::to_value(&summary).context("encode summary")?;
        if let Some(object) = value.as_object_mut() {
            object.insert("version".into(), 2.into());
            object.insert("command".into(), command.clone().into());
            object.insert("exit_code".into(), code.into());
            object.insert("duration_ms".into(), (elapsed as u64).into());
            object.insert("endpoint".into(), endpoint.into());
            object.insert("upstream".into(), options.upstream.base.clone().into());
            object.insert("trace".into(), trace_path.display().to_string().into());
        }
        let text = serde_json::to_string_pretty(&value).context("encode summary")?;
        if target == "-" {
            println!("{text}");
        } else {
            std::fs::write(&target, format!("{text}\n"))
                .with_context(|| format!("write {target}"))?;
        }
    }
    Ok(code)
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

async fn serve_proxy(args: Vec<String>) -> Result<i32> {
    let home = home();
    let mut options = Options::new(&home, 9297);
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} requires a value"));
        if arg == "-h" || arg == "--help" {
            println!("{}", usage());
            return Ok(0);
        }
        if !options.accept(&arg, &mut value)? {
            bail!("unknown option `{arg}`; try `lenslet --help`");
        }
    }
    let trace_path = match options.trace.clone() {
        Some(path) => path,
        None => runs::create(&runs::dir(&home))?,
    };
    let server = Server::start(&options, trace_path.clone()).await?;
    let mut info = RunInfo {
        kind: "proxy".to_string(),
        pid: std::process::id(),
        endpoint: server.endpoint(),
        upstream: options.upstream.base.clone(),
        started: runs::now(),
        ..RunInfo::default()
    };
    runs::write_info(&trace_path, &info)?;

    println!("lenslet {} proxy", env!("CARGO_PKG_VERSION"));
    println!("  endpoint  {}  (https:// works too)", server.endpoint());
    println!("  upstream  {}", options.upstream.base);
    println!("  trace     {}", tilde(&trace_path, &home));
    println!(
        "  sdk env   AWS_ENDPOINT_URL={} AWS_SHARED_CREDENTIALS_FILE={} AWS_CONFIG_FILE={}",
        server.endpoint(),
        tilde(&options.emulator_dir().join("emulator_credentials"), &home),
        tilde(&options.emulator_dir().join("config"), &home),
    );
    if !options.upstream.reachable().await {
        println!(
            "  warning   nothing is listening on {} yet",
            options.upstream.base
        );
    }

    tokio::signal::ctrl_c().await.ok();
    let state = server.state.clone();
    server.stop(Duration::from_secs(1)).await;
    info.ended = Some(runs::now());
    runs::write_info(&trace_path, &info)?;
    let summary = state.trace.summary();
    eprintln!(
        "\nlenslet: {} calls · {} errors · trace {}",
        summary.calls,
        summary.errors,
        tilde(&trace_path, &home)
    );
    Ok(0)
}

/// Start the window next to this binary: `lenslet-ui`, or `Lenslet.app` on
/// macOS. The window is a separate program so `lenslet run` never needs
/// graphics libraries (CI, containers, SSH).
fn open_ui(trace: Option<&Path>) -> Result<()> {
    let exe = std::env::current_exe().context("locate the lenslet binary")?;
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = if cfg!(windows) {
        "lenslet-ui.exe"
    } else {
        "lenslet-ui"
    };
    let mut command = if dir.join(name).is_file() {
        std::process::Command::new(dir.join(name))
    } else if cfg!(target_os = "macos") && dir.join("Lenslet.app").is_dir() {
        let mut open = std::process::Command::new("open");
        open.arg(dir.join("Lenslet.app"));
        if let Some(trace) = trace {
            open.arg("--env")
                .arg(format!("LENSLET_TRACE={}", trace.display()));
        }
        open
    } else {
        bail!(
            "the Lenslet window is not installed next to {} (expected {name}{}). \
             `lenslet run` works without it.",
            exe.display(),
            if cfg!(target_os = "macos") {
                " or Lenslet.app"
            } else {
                ""
            }
        );
    };
    if let Some(trace) = trace {
        command.env("LENSLET_TRACE", trace);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("open the Lenslet window")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn run_takes_options_then_the_command() {
        let home = Path::new("/home/tester");
        let parsed = parse_run(
            strings(&[
                "--upstream",
                "9400",
                "--json",
                "-",
                "--",
                "npm",
                "test",
                "--x",
            ]),
            home,
        )
        .unwrap();
        assert_eq!(parsed.command, ["npm", "test", "--x"]);
        assert_eq!(parsed.json.as_deref(), Some("-"));
        assert_eq!(parsed.options.listen, 0, "run picks a free port");
        assert_eq!(parsed.options.upstream.port, 9400);

        // Without `--`, the first non-option starts the command.
        let parsed = parse_run(strings(&["--quiet", "pytest", "-k", "s3"]), home).unwrap();
        assert!(parsed.quiet);
        assert_eq!(parsed.command, ["pytest", "-k", "s3"]);

        assert!(parse_run(strings(&[]), home).is_err());
        assert!(parse_run(strings(&["--nope", "x"]), home).is_err());
    }

    #[test]
    fn child_env_points_sdks_at_lenslet_and_the_emulator() {
        let dir = std::env::temp_dir().join(format!("lenslet-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("emulator_credentials"), "[default]").unwrap();
        std::fs::write(dir.join("config"), "[default]").unwrap();

        let env = child_env("http://127.0.0.1:5000", &dir, false, |name| {
            (name == "AWS_PROFILE").then(|| OsString::from("prod"))
        });
        let names: Vec<&str> = env.set.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "AWS_ENDPOINT_URL",
                "AWS_SHARED_CREDENTIALS_FILE",
                "AWS_CONFIG_FILE"
            ]
        );
        assert_eq!(env.remove, ["AWS_PROFILE"]);

        // The user's own settings win; only the endpoint is forced.
        let env = child_env("http://x", &dir, false, |name| {
            matches!(
                name,
                "AWS_SHARED_CREDENTIALS_FILE" | "AWS_CONFIG_FILE" | "AWS_PROFILE"
            )
            .then(|| OsString::from("mine"))
        });
        assert_eq!(env.set.len(), 1, "{env:?}");
        assert!(env.remove.is_empty());

        let env = child_env("http://x", &dir, true, |_| None);
        assert_eq!(env.set.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn exit_codes_pass_through() {
        let status = std::process::Command::new("sh")
            .args(["-c", "exit 7"])
            .status()
            .unwrap();
        assert_eq!(exit_code(status), 7);
    }

    #[test]
    fn tilde_shortens_home() {
        assert_eq!(
            tilde(Path::new("/h/u/.lenslet/runs/a.jsonl"), Path::new("/h/u")),
            "~/.lenslet/runs/a.jsonl"
        );
        assert_eq!(tilde(Path::new("/tmp/a"), Path::new("/h/u")), "/tmp/a");
    }
}
