//! `lenslet`: run a program against local AWS and see what it did, what failed,
//! and what AWS access it needs.
//!
//! ```text
//! lenslet run -- npm test     proxy + env for one command; report at the end
//! lenslet run                 a long-running proxy on 127.0.0.1:9297
//! lenslet last                the report of the newest run
//! lenslet policy              an IAM policy from lenslet.lock
//! lenslet ui                  the window (also: bare `lenslet`)
//! ```

mod access;
mod capture;
mod lock;
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
    summary::Report,
    trace::RunHeader,
};

/// Exit code when the command passed but `--check` found access outside the lock.
const EXIT_ACCESS_CHANGED: i32 = 3;

fn usage() -> String {
    format!(
        "lenslet {version} — what your code does against local AWS, and what it needs

Usage:
  lenslet run [options] [--] COMMAND [ARGS...]
      Run COMMAND with AWS_ENDPOINT_URL pointing at Lenslet. Prints every
      failed call with its cause, the AWS access the run used, and what changed
      since the last run and since lenslet.lock.
  lenslet run [options]
      No command: keep a proxy running on 127.0.0.1:9297 until Ctrl-C.
  lenslet last [--json]
      The report of the newest run.
  lenslet policy [--lock PATH]
      An IAM policy document granting exactly what lenslet.lock lists.
  lenslet ui
      Open the window (same as `lenslet` with no arguments).

Run options:
  --check              exit {changed} if the run used access not in ./lenslet.lock
  --update             write ./lenslet.lock from this run
  --json PATH          also write the report as JSON (`-` for stdout)
  --quiet              no banner or report on stderr
  --ui                 open the window first
  --no-env             set only AWS_ENDPOINT_URL, leave credentials/config alone
  --listen PORT        loopback port (default: a free port; 9297 without COMMAND)
  --upstream ADDRESS   AWS Emulator port or http(s) URL (default 9298)
  --emulator-ca PATH   emulator CA (default ~/.aws/emulator/PORT/ca.pem)
  --trace PATH         run file (default: a new file in ~/.lenslet/runs)

Exit codes: COMMAND's own code when it fails; {changed} when it passed but --check
found new access; 2 for Lenslet's own errors; otherwise 0.
Bodies are forwarded unchanged; traces keep up to {kib} KiB of each text body.",
        version = env!("CARGO_PKG_VERSION"),
        changed = EXIT_ACCESS_CHANGED,
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
        "last" => last(args),
        "policy" => policy(args),
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
    check: bool,
    update: bool,
}

fn parse_run(args: Vec<String>, home: &Path) -> Result<RunArgs> {
    let mut parsed = RunArgs {
        options: Options::new(home, 0),
        command: Vec::new(),
        json: None,
        quiet: false,
        ui: false,
        no_env: false,
        check: false,
        update: false,
    };
    let mut listen_given = false;
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
            "--check" => parsed.check = true,
            "--update" => parsed.update = true,
            "-h" | "--help" => bail!("{}", usage()),
            flag if flag.starts_with('-') => {
                listen_given |= flag == "--listen";
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
        if parsed.check || parsed.update {
            bail!("--check and --update need a command, e.g. `lenslet run --update -- npm test`");
        }
        if !listen_given {
            parsed.options.listen = 9297;
        }
    }
    if parsed.check && parsed.update {
        bail!("use --check or --update, not both");
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
    // A named profile from the real config does not exist in the emulator's,
    // and keys in the environment would override the emulator's credentials
    // (and put a real access key id into the local trace).
    if injected {
        for name in [
            "AWS_PROFILE",
            "AWS_DEFAULT_PROFILE",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
        ] {
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
        check,
        update,
    } = parse_run(args, &home)?;
    let cwd = std::env::current_dir().context("read the current directory")?;
    let lock_path = lock::path_in(&cwd);
    let locked = lock::read(&lock_path)?;
    if check && locked.is_none() {
        bail!(
            "--check needs {}; create it with `lenslet run --update -- <tests>`",
            lock::FILE_NAME
        );
    }
    let runs_dir = runs::dir(&home);
    let trace_path = match options.trace.clone() {
        Some(path) => path,
        None => runs::create(&runs_dir)?,
    };
    if !options.upstream.reachable().await {
        eprintln!(
            "lenslet: warning: nothing is listening on {} — start AWS Emulator first:\n  \
             EMULATOR_TELEMETRY=off aws-emulator --telemetry off --port {}",
            options.upstream.base, options.upstream.port
        );
    }
    let header = RunHeader {
        version: trace::VERSION,
        run: runs::run_id(&trace_path),
        command: command.clone(),
        cwd: cwd.display().to_string(),
        pid: std::process::id(),
        started: runs::now(),
        lock: locked.is_some().then(|| lock::FILE_NAME.to_string()),
        ..RunHeader::default()
    };
    let server = Server::start(&options, trace_path.clone(), header, locked.clone()).await?;
    let endpoint = server.endpoint();
    let started = Instant::now();

    let code = if command.is_empty() {
        serve_until_interrupted(&server, &options, &home).await;
        None
    } else {
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
                eprintln!("lenslet:   unset {name} (the emulator's credentials are used instead)");
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
        Some(run_child(&command, &env).await?)
    };

    let state = server.state.clone();
    server.stop(Duration::from_secs(2)).await;
    let summary = state.trace.summary();
    let mut report = Report {
        version: trace::VERSION,
        run: runs::run_id(&trace_path),
        command: command.clone(),
        exit_code: code,
        ended: runs::now(),
        duration_ms: started.elapsed().as_millis() as u64,
        calls: summary.calls,
        errors: summary.errors,
        unfinished: summary.unfinished,
        services: summary.services.clone(),
        failures: summary.failures.clone(),
        lock: locked.is_some().then(|| lock::FILE_NAME.to_string()),
        trace: trace_path.display().to_string(),
        endpoint: endpoint.clone(),
        upstream: options.upstream.base.clone(),
        ..Report::default()
    };
    report.set_access(&summary, locked.as_ref());
    let runs_dir = trace_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or(runs_dir);
    if let Some(previous) =
        runs::previous(&runs_dir, &trace_path, &command, &cwd.display().to_string())
    {
        report.compare_with(&previous);
    }
    state.trace.end(&report);

    if !quiet {
        eprint!("{}", report.render(&tilde(&trace_path, &home)));
    }
    if update {
        // A failed run stops early, so its access is incomplete: keep the old lock.
        if code == Some(0) {
            lock::write(&lock_path, &summary.access)?;
            if !quiet {
                eprintln!(
                    "lenslet: wrote {} ({} permissions)",
                    lock::FILE_NAME,
                    summary.access.len()
                );
            }
        } else {
            eprintln!(
                "lenslet: {} not written: the command failed, so its access may be incomplete",
                lock::FILE_NAME
            );
        }
    }
    if let Some(target) = json {
        write_json(&report, &target)?;
    }
    Ok(match code {
        Some(code) if code != 0 => code,
        _ if check && !report.access_added.is_empty() => EXIT_ACCESS_CHANGED,
        _ => 0,
    })
}

async fn run_child(command: &[String], env: &ChildEnv) -> Result<i32> {
    let mut child = tokio::process::Command::new(&command[0]);
    child.args(&command[1..]).stdin(Stdio::inherit());
    for (name, value) in &env.set {
        child.env(name, value);
    }
    for name in &env.remove {
        child.env_remove(name);
    }
    let mut child = match child.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!("lenslet: cannot start `{}`: {error}", command[0]);
            return Ok(127);
        }
    };
    // Ctrl-C reaches the child through the terminal; Lenslet waits for it to
    // exit so the report still covers every call.
    let interrupts = tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
    let status = child.wait().await.context("wait for the command")?;
    interrupts.abort();
    Ok(exit_code(status))
}

async fn serve_until_interrupted(server: &Server, options: &Options, home: &Path) {
    println!("lenslet {} · proxy until Ctrl-C", env!("CARGO_PKG_VERSION"));
    println!("  endpoint  {}", server.endpoint());
    println!("  upstream  {}", options.upstream.base);
    println!(
        "  sdk env   AWS_ENDPOINT_URL={} AWS_SHARED_CREDENTIALS_FILE={} AWS_CONFIG_FILE={}",
        server.endpoint(),
        tilde(&options.emulator_dir().join("emulator_credentials"), home),
        tilde(&options.emulator_dir().join("config"), home),
    );
    tokio::signal::ctrl_c().await.ok();
    eprintln!();
}

fn write_json(report: &Report, target: &str) -> Result<()> {
    let text = serde_json::to_string_pretty(report).context("encode report")?;
    if target == "-" {
        println!("{text}");
        Ok(())
    } else {
        std::fs::write(target, format!("{text}\n")).with_context(|| format!("write {target}"))
    }
}

/// `lenslet last [--json]`: the newest run's report, without running anything.
fn last(args: Vec<String>) -> Result<i32> {
    let json = match args.as_slice() {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => bail!("usage: lenslet last [--json]"),
    };
    let home = home();
    let Some(newest) = runs::list(&runs::dir(&home)).pop() else {
        bail!("no runs yet; try `lenslet run -- <command>`");
    };
    match runs::report(&newest) {
        Some(report) if json => write_json(&report, "-").map(|()| 0),
        Some(report) => {
            eprint!("{}", report.render(&tilde(&newest, &home)));
            Ok(0)
        }
        None => {
            let command = runs::header(&newest)
                .map(|header| header.command.join(" "))
                .unwrap_or_default();
            if json {
                println!(
                    "{}",
                    serde_json::json!({"run": runs::run_id(&newest), "running": true, "command": command})
                );
            } else {
                eprintln!(
                    "lenslet: run {} is still running: {command}",
                    runs::run_id(&newest)
                );
            }
            Ok(0)
        }
    }
}

/// `lenslet policy [--lock PATH]`: the locked access as an IAM policy.
fn policy(args: Vec<String>) -> Result<i32> {
    let path = match args.as_slice() {
        [] => lock::path_in(&std::env::current_dir().context("read the current directory")?),
        [flag, path] if flag == "--lock" => PathBuf::from(path),
        _ => bail!("usage: lenslet policy [--lock PATH]"),
    };
    let Some(locked) = lock::read(&path)? else {
        bail!(
            "no {}; create it with `lenslet run --update -- <tests>`",
            path.display()
        );
    };
    println!("{}", serde_json::to_string_pretty(&lock::policy(&locked))?);
    eprintln!(
        "lenslet: observed access from {} ({} permissions). Review before attaching it to a role.",
        path.display(),
        locked.len()
    );
    Ok(0)
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

        // No command: a long-running proxy on the well-known port.
        let parsed = parse_run(strings(&[]), home).unwrap();
        assert!(parsed.command.is_empty());
        assert_eq!(parsed.options.listen, 9297);
        assert!(parse_run(strings(&["--check"]), home).is_err());
        assert!(parse_run(strings(&["--check", "--update", "x"]), home).is_err());
        let parsed = parse_run(strings(&["--check", "npm", "test"]), home).unwrap();
        assert!(parsed.check);
        assert!(parse_run(strings(&["--nope", "x"]), home).is_err());
    }

    #[test]
    fn child_env_points_sdks_at_lenslet_and_the_emulator() {
        let dir = std::env::temp_dir().join(format!("lenslet-env-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("emulator_credentials"), "[default]").unwrap();
        std::fs::write(dir.join("config"), "[default]").unwrap();

        let env = child_env("http://127.0.0.1:5000", &dir, false, |name| {
            matches!(name, "AWS_PROFILE" | "AWS_ACCESS_KEY_ID").then(|| OsString::from("prod"))
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
        assert_eq!(env.remove, ["AWS_PROFILE", "AWS_ACCESS_KEY_ID"]);

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
