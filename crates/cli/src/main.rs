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

mod capture;
mod proxy;
mod trace;

pub(crate) use lenslet_core::{access, findings, iac, lock, problem, runs, summary, wire};

use std::{
    collections::BTreeMap,
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
  lenslet init [--upstream ADDRESS] -- COMMAND
      First run: check the emulator, write lenslet.lock from COMMAND, and add
      .github/workflows/lenslet.yml so pull requests show their AWS access.
  lenslet last [--json]
      The report of the newest run.
  lenslet runs [--json] [--limit N]
      List recorded runs in ~/.lenslet/runs (newest first).
  lenslet diff [BASE] [TARGET] [--json] [--markdown PATH]
      Compare two runs (defaults to the previous finished run vs the latest).
  lenslet check [--policy PATH | --lock PATH] [--context KEY=VAL] [RUN] [--json] [--markdown PATH]
      Re-check a recorded run (default: the newest finished one) against a
      policy or lock without re-running it: each denied permission and the
      statement that denied it. Exit {changed} if anything is denied.
  lenslet policy [--lock PATH | --run RUN] [--format iam|terraform|cfn|sam|cdk] [--role ROLE]
      Compile least-privilege IAM JSON, Terraform HCL, CloudFormation, SAM, or CDK
      TypeScript from lenslet.lock or a recorded run.
  lenslet env [--shell sh|fish|pwsh|dotenv|docker] [--listen PORT]
      Print shell environment exports pointing SDKs at a running Lenslet proxy.
  lenslet ui
      Open the window (same as `lenslet` with no arguments).

Run options:
  --enforce            block calls outside lenslet.lock (or --policy) with AWS AccessDenied
  --check              exit {changed} if the run used access not in the lock (or --policy)
  --strict             with --check: also exit {changed} for locked access the run did not use
  --update             write the lock from this run (keeps `# test` marks)
  --lock PATH          the lock file (default ./lenslet.lock)
  --policy PATH        check or enforce against an IAM policy JSON document instead of lock
  --max-body SIZE      body bytes captured per message, e.g. 256K, 4M, unlimited (default {mib} MiB)
  --json PATH          also write the report as JSON (`-` for stdout)
  --markdown PATH      also write the report as Markdown (PR comments, job summaries)
  --quiet              no banner or report on stderr
  --ui                 open the window first
  --no-env             set only AWS_ENDPOINT_URL, leave credentials/config alone
  --listen PORT        loopback port (default: a free port; 9297 without COMMAND)
  --upstream ADDRESS   AWS Emulator port or http(s) URL (default 9298)
  --route SVC=ADDRESS  route specific service(s) to another local emulator (e.g. s3=http://127.0.0.1:9000)
  --allow-docker       also accept connections from local Docker container bridge interfaces
  --emulator-ca PATH   emulator CA (default ~/.aws/emulator/PORT/ca.pem)
  --trace PATH         run file (default: a new file in ~/.lenslet/runs)

Exit codes: COMMAND's own code when it fails; {changed} when it passed but --check
found a change in access; 2 for Lenslet's own errors; otherwise 0.
Bodies are forwarded unchanged with no redaction; traces keep up to {mib} MiB per body by default.",
        version = env!("CARGO_PKG_VERSION"),
        changed = EXIT_ACCESS_CHANGED,
        mib = MAX_CAPTURE_BYTES / (1024 * 1024),
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
        "init" => runtime().and_then(|runtime| runtime.block_on(init(args))),
        "last" => last(args),
        "runs" => runs_cmd(args),
        "diff" => diff_cmd(args),
        "check" => check_cmd(args),
        "policy" => policy(args),
        "env" => env_cmd(args),
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
    markdown: Option<String>,
    log: Option<PathBuf>,
    quiet: bool,
    ui: bool,
    no_env: bool,
    check: bool,
    strict: bool,
    update: bool,
    lock: Option<PathBuf>,
    policy: Option<PathBuf>,
}

fn parse_run(args: Vec<String>, home: &Path) -> Result<RunArgs> {
    let mut parsed = RunArgs {
        options: Options::new(home, 0),
        command: Vec::new(),
        json: None,
        markdown: None,
        log: None,
        quiet: false,
        ui: false,
        no_env: false,
        check: false,
        strict: false,
        update: false,
        lock: None,
        policy: None,
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
            "--markdown" => parsed.markdown = Some(value()?),
            "--log" => parsed.log = Some(PathBuf::from(value()?)),
            "--lock" => parsed.lock = Some(PathBuf::from(value()?)),
            "--policy" => parsed.policy = Some(PathBuf::from(value()?)),
            "--strict" => parsed.strict = true,
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
    if parsed.options.enforce && parsed.update {
        bail!("use --enforce or --update, not both");
    }
    if parsed.policy.is_some() && parsed.update {
        bail!("--update writes lenslet.lock; remove --policy when updating the lock");
    }
    if parsed.strict && !parsed.check {
        bail!("--strict goes with --check");
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

/// Placeholder keys for emulators that accept any credentials (moto,
/// LocalStack) when AWS Emulator's own credentials file is not there.
const LOCAL_KEYS: (&str, &str) = ("lenslet-local", "lenslet-local");

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
    // Real credentials never reach the emulator or the trace: keys, profiles,
    // OIDC web-identity tokens, container credential URIs and bearer tokens go.
    for name in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_DEFAULT_PROFILE",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_ROLE_ARN",
        "AWS_ROLE_SESSION_NAME",
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN_FILE",
        "AWS_BEARER_TOKEN_BEDROCK",
    ] {
        if current(name).is_some() {
            env.remove.push(name.to_string());
        }
    }
    let credentials = emulator_dir.join("emulator_credentials");
    let config = emulator_dir.join("config");
    if current("AWS_SHARED_CREDENTIALS_FILE").is_none() {
        if credentials.is_file() {
            env.set.push((
                "AWS_SHARED_CREDENTIALS_FILE".to_string(),
                credentials.display().to_string(),
            ));
        } else {
            // Without these, the SDK would fall back to ~/.aws/credentials.
            env.set
                .push(("AWS_ACCESS_KEY_ID".to_string(), LOCAL_KEYS.0.to_string()));
            env.set.push((
                "AWS_SECRET_ACCESS_KEY".to_string(),
                LOCAL_KEYS.1.to_string(),
            ));
        }
    }
    if current("AWS_CONFIG_FILE").is_none() && config.is_file() {
        env.set
            .push(("AWS_CONFIG_FILE".to_string(), config.display().to_string()));
    }
    env
}

async fn run(args: Vec<String>) -> Result<i32> {
    let home = home();
    let RunArgs {
        mut options,
        command,
        json,
        markdown,
        log,
        quiet,
        ui,
        no_env,
        check,
        strict,
        update,
        lock,
        policy,
    } = parse_run(args, &home)?;
    if !options.upstream_given {
        let env_ep = std::env::var("AWS_ENDPOINT_URL").ok();
        if let Some((discovered, source_label)) =
            proxy::Upstream::auto_discover(env_ep.as_deref(), options.listen).await
        {
            if !quiet {
                eprintln!(
                    "lenslet: auto-selected upstream {} ({source_label})",
                    discovered.base
                );
            }
            options.upstream = discovered;
            if options.routes.is_empty() && matches!(options.upstream.port, 9000 | 8000 | 9324) {
                let extra = proxy::Upstream::auto_discover_service_routes(
                    options.listen,
                    options.upstream.port,
                )
                .await;
                if !extra.is_empty() && !quiet {
                    let list: Vec<String> = extra
                        .iter()
                        .map(|(s, u)| format!("{s}={}", u.base))
                        .collect();
                    eprintln!("lenslet: auto-routed services {}", list.join(", "));
                }
                options.routes.extend(extra);
            }
            options.validate()?;
        }
    }
    let cwd = std::env::current_dir().context("read the current directory")?;
    let lock_path = lock.unwrap_or_else(|| lock::path_in(&cwd));
    // How the lock is named in reports: relative to the run's directory.
    let lock_label = relative_to(&lock_path, &cwd);
    let locked = lock::read(&lock_path)?;
    let (policy_label, gate) = if let Some(policy_path) = policy.as_ref() {
        let label = relative_to(policy_path, &cwd);
        let gate = lock::Gate::from_policy_file(policy_path, label.clone())?;
        (Some(label), Some(gate))
    } else {
        if (check || options.enforce) && locked.is_none() {
            let flag = if options.enforce {
                "--enforce"
            } else {
                "--check"
            };
            bail!(
                "{flag} needs {lock_label} (or --policy PATH); create it with `lenslet run --update -- <tests>`"
            );
        }
        let gate = locked
            .as_ref()
            .map(|lock| lock::Gate::from_lock(lock_label.clone(), lock.access.clone()));
        (None, gate)
    };
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
    let header_routes = options
        .routes
        .iter()
        .map(|(svc, up)| (svc.clone(), up.base.clone()))
        .collect();
    let header = RunHeader {
        version: trace::VERSION,
        run: runs::run_id(&trace_path),
        command: command.clone(),
        cwd: cwd.display().to_string(),
        pid: std::process::id(),
        started: runs::now(),
        lock: (policy_label.is_none() && locked.is_some()).then(|| lock_label.clone()),
        policy: policy_label.clone(),
        enforce: options.enforce,
        routes: header_routes,
        allow_docker: options.allow_docker,
        max_body: Some(options.max_body as u64),
        log: log.as_ref().map(|p| p.display().to_string()),
        ..RunHeader::default()
    };
    let server = Server::start(&options, trace_path.clone(), header, gate.clone()).await?;
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
            let enforce_note = match (options.enforce, gate.as_ref()) {
                (true, Some(g)) => format!(" · enforcing {}", g.label),
                _ => String::new(),
            };
            eprintln!(
                "lenslet: {} → {} · trace {}{enforce_note}",
                endpoint,
                options.upstream.base,
                tilde(&trace_path, &home)
            );
            for (name, value) in &env.set {
                eprintln!("lenslet:   {name}={}", tilde(Path::new(value), &home));
            }
            if !env.remove.is_empty() {
                eprintln!(
                    "lenslet:   unset {} (real credentials never reach the emulator)",
                    env.remove.join(", ")
                );
            }
            if env.set.iter().any(|(name, _)| name == "AWS_ACCESS_KEY_ID") {
                eprintln!(
                    "lenslet: note: no emulator credentials in {}; using placeholder keys",
                    tilde(&options.emulator_dir(), &home)
                );
            }
        }
        if ui && let Err(error) = open_ui(Some(&trace_path)) {
            eprintln!("lenslet: {error:#}");
        }
        Some(run_child(&command, &env, log.as_deref(), server.shutdown_requested()).await?)
    };

    let state = server.state.clone();
    server.stop(Duration::from_secs(2)).await;
    // From here on, a call that finishes late is not recorded: it stays
    // counted as unfinished, and nothing is written after the `end` line.
    let (summary, calls) = state.trace.seal();
    let gate_label = gate.as_ref().map(|gate| gate.label.clone());
    let mut report = Report {
        version: trace::VERSION,
        run: runs::run_id(&trace_path),
        command: command.clone(),
        exit_code: code,
        ended: runs::now(),
        duration_ms: started.elapsed().as_millis() as u64,
        calls: summary.calls,
        retries: summary.retries,
        errors: summary.errors,
        blocked: summary.blocked,
        unfinished: summary.unfinished,
        services: summary.services.clone(),
        unmapped: summary.unmapped.clone(),
        failures: summary.failures.clone(),
        lock: (policy_label.is_none() && locked.is_some()).then(|| lock_label.clone()),
        policy: policy_label,
        trace: trace_path.display().to_string(),
        endpoint: endpoint.clone(),
        upstream: options.upstream.base.clone(),
        findings: findings::analyze_against(
            &calls,
            gate_label.as_deref().unwrap_or(lock::FILE_NAME),
        ),
        ..Report::default()
    };
    drop(calls);
    report.set_access_with_gate(&summary, gate.as_ref());
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
    // Live event streams end with the `end` line; let them deliver it.
    server.join(Duration::from_millis(500)).await;

    if !quiet {
        eprint!("{}", report.render(&tilde(&trace_path, &home)));
    }
    if update {
        // A failed run stops early, so its access is incomplete: keep the old lock.
        if code == Some(0) {
            let test = locked
                .as_ref()
                .map(|lock| lock.test.clone())
                .unwrap_or_default();
            lock::write(&lock_path, &summary.access, &test)?;
            if !quiet {
                eprintln!(
                    "lenslet: wrote {lock_label} ({} permissions)",
                    summary.access.len()
                );
            }
        } else {
            eprintln!(
                "lenslet: {lock_label} not written: the command failed, so its access may be incomplete"
            );
        }
    }
    if let Some(target) = json {
        write_json(&report, &target)?;
    }
    if let Some(target) = markdown {
        write_text(&report.markdown(), &target)?;
    }
    let changed = !report.access_added.is_empty() || (strict && !report.access_unused.is_empty());
    Ok(match code {
        Some(code) if code != 0 => code,
        _ if check && changed => EXIT_ACCESS_CHANGED,
        _ => 0,
    })
}

async fn run_child(
    command: &[String],
    env: &ChildEnv,
    log_path: Option<&Path>,
    stop_notify: std::sync::Arc<tokio::sync::Notify>,
) -> Result<i32> {
    use tokio::io::AsyncReadExt as _;
    let log_sink = if let Some(path) = log_path {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create log directory {}", parent.display()))?;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts
            .open(path)
            .with_context(|| format!("open log file {}", path.display()))?;
        Some(std::sync::Arc::new(std::sync::Mutex::new(file)))
    } else {
        None
    };

    let mut child = tokio::process::Command::new(&command[0]);
    child.args(&command[1..]).stdin(Stdio::inherit());
    if log_sink.is_some() {
        child.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    // Remove first: a placeholder key replaces a real one of the same name.
    for name in &env.remove {
        child.env_remove(name);
    }
    for (name, value) in &env.set {
        child.env(name, value);
    }
    let mut child = match child.spawn() {
        Ok(child) => child,
        Err(error) => {
            let msg = format!("lenslet: cannot start `{}`: {error}\n", command[0]);
            eprint!("{msg}");
            if let Some(sink) = log_sink.as_ref()
                && let Ok(mut f) = sink.lock()
            {
                use std::io::Write as _;
                let _ = f.write_all(msg.as_bytes());
            }
            return Ok(127);
        }
    };

    let mut tee_tasks = Vec::new();
    if let Some(sink) = log_sink {
        if let Some(mut out) = child.stdout.take() {
            let sink = sink.clone();
            tee_tasks.push(tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = out.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    {
                        use std::io::Write as _;
                        let _ = std::io::stdout().write_all(&buf[..n]);
                        let _ = std::io::stdout().flush();
                        if let Ok(mut f) = sink.lock() {
                            let _ = f.write_all(&buf[..n]);
                            let _ = f.flush();
                        }
                    }
                }
            }));
        }
        if let Some(mut err) = child.stderr.take() {
            let sink = sink.clone();
            tee_tasks.push(tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                while let Ok(n) = err.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    {
                        use std::io::Write as _;
                        let _ = std::io::stderr().write_all(&buf[..n]);
                        let _ = std::io::stderr().flush();
                        if let Ok(mut f) = sink.lock() {
                            let _ = f.write_all(&buf[..n]);
                            let _ = f.flush();
                        }
                    }
                }
            }));
        }
    }

    // Ctrl-C reaches the child through the terminal; `SIGTERM` or `/_lenslet/shutdown`
    // terminates the child cleanly so the `end` report is still written.
    let interrupts = tokio::spawn(async { while tokio::signal::ctrl_c().await.is_ok() {} });
    let status = tokio::select! {
        res = child.wait() => res.context("wait for the command")?,
        () = stop_notify.notified() => {
            let _ = child.start_kill();
            child.wait().await.context("wait for stopped command")?
        }
        () = wait_for_sigterm() => {
            let _ = child.start_kill();
            child.wait().await.context("wait for terminated command")?
        }
    };
    interrupts.abort();
    for task in tee_tasks {
        let _ = tokio::time::timeout(Duration::from_millis(250), task).await;
    }
    Ok(exit_code(status))
}

#[cfg(unix)]
async fn wait_for_sigterm() {
    if let Ok(mut sig) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        sig.recv().await;
    } else {
        std::future::pending::<()>().await;
    }
}

#[cfg(not(unix))]
async fn wait_for_sigterm() {
    std::future::pending::<()>().await;
}

async fn serve_until_interrupted(server: &Server, options: &Options, home: &Path) {
    let stop_notify = server.shutdown_requested();
    println!("lenslet {} · proxy until Ctrl-C", env!("CARGO_PKG_VERSION"));
    println!("  endpoint  {}", server.endpoint());
    println!("  upstream  {}", options.upstream.base);
    println!(
        "  sdk env   AWS_ENDPOINT_URL={} AWS_SHARED_CREDENTIALS_FILE={} AWS_CONFIG_FILE={}",
        server.endpoint(),
        tilde(&options.emulator_dir().join("emulator_credentials"), home),
        tilde(&options.emulator_dir().join("config"), home),
    );
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        () = stop_notify.notified() => {}
        () = wait_for_sigterm() => {}
    }
    eprintln!();
}

fn write_json(report: &Report, target: &str) -> Result<()> {
    let text = serde_json::to_string_pretty(report).context("encode report")?;
    write_text(&format!("{text}\n"), target)
}

/// Write to a file, or to stdout for `-`.
fn write_text(text: &str, target: &str) -> Result<()> {
    if target == "-" {
        print!("{text}");
        Ok(())
    } else {
        std::fs::write(target, text).with_context(|| format!("write {target}"))
    }
}

/// The workflow `lenslet init` writes. `{version}`, `{command}` and
/// `{upstream}` are filled in; everything else is plain YAML a user can edit.
const WORKFLOW: &str = r#"# Lenslet: show the AWS access each pull request needs, and fail when it grows.
# Docs: https://github.com/hxddh/lenslet#ci
name: Lenslet
on:
  pull_request:
  push:
    branches: [main]
permissions:
  contents: read
  pull-requests: write # the access summary is posted as one PR comment
jobs:
  aws-access:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      # Install what your tests need here (language runtime, dependencies).
      - uses: hxddh/lenslet@lenslet-v{version}
        with:
          command: {command}
          upstream: {upstream}
          # A command that starts your local AWS emulator inside this job, for
          # example `aws-emulator --telemetry off --port 9298` once AWS Emulator
          # is installed (see its docs), or `moto_server -p 9298`.
          emulator: {emulator}
"#;

/// One shell word: left as is when it is plain, single-quoted otherwise, so
/// `bash -c` in the action runs exactly the argv `init` ran.
fn shell_quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b));
    if plain {
        word.to_string()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A YAML single-quoted scalar.
fn yaml_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn render_workflow(
    version: &str,
    command: &[String],
    upstream: &str,
    emulator: Option<&str>,
) -> String {
    let emulator_yaml = emulator
        .map(yaml_quote)
        .unwrap_or_else(|| "\"\"".to_string());
    WORKFLOW
        .replace("{version}", version)
        .replace("{command}", &yaml_quote(&shell_join(command)))
        .replace("{upstream}", &yaml_quote(upstream))
        .replace("{emulator}", &emulator_yaml)
}

/// `lenslet init [--upstream ADDRESS] [--force] -- COMMAND`: from nothing to
/// an enforced lock in one step.
async fn init(args: Vec<String>) -> Result<i32> {
    let mut upstream = "9298".to_string();
    let mut upstream_given = false;
    let mut force = false;
    let mut command = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--upstream" => {
                upstream = it.next().context("--upstream requires a value")?;
                upstream_given = true;
            }
            "--force" => force = true,
            "--" => {
                command.extend(it.by_ref());
                break;
            }
            flag if flag.starts_with('-') => {
                bail!("unknown option `{flag}`; try `lenslet --help`")
            }
            _ => {
                command.push(arg);
                command.extend(it.by_ref());
                break;
            }
        }
    }
    if command.is_empty() {
        bail!(
            "usage: lenslet init [--upstream ADDRESS] -- <test command>, e.g. `lenslet init -- npm test`"
        );
    }
    let mut target = proxy::Upstream::parse(&upstream)?;
    if !upstream_given {
        let env_ep = std::env::var("AWS_ENDPOINT_URL").ok();
        if let Some((discovered, _)) = proxy::Upstream::auto_discover(env_ep.as_deref(), 0).await {
            upstream = discovered.base.clone();
            target = discovered;
        }
    }
    eprintln!("lenslet: 1/3 emulator at {}", target.base);
    if !target.reachable().await {
        bail!(
            "nothing is listening on {}. Start your emulator first, e.g.\n  \
             EMULATOR_TELEMETRY=off aws-emulator --telemetry off --port {}\n\
             or point at another one with --upstream (moto: --upstream http://127.0.0.1:5000)",
            target.base,
            target.port
        );
    }

    let detected_emulator = target.detect_emulator().await;
    if let Some(emulator) = detected_emulator.as_deref() {
        eprintln!("lenslet:   detected `{emulator}`");
    }

    let cwd = std::env::current_dir().context("read the current directory")?;
    let workflow = cwd.join(".github/workflows/lenslet.yml");
    eprintln!("lenslet: 2/3 workflow {}", workflow.display());
    if workflow.exists() && !force {
        eprintln!("lenslet:   exists, left alone (use --force to replace it)");
    } else {
        let text = render_workflow(
            env!("CARGO_PKG_VERSION"),
            &command,
            &target.base,
            detected_emulator.as_deref(),
        );
        std::fs::create_dir_all(workflow.parent().unwrap_or(&cwd))
            .with_context(|| format!("create {}", workflow.display()))?;
        std::fs::write(&workflow, text).with_context(|| format!("write {}", workflow.display()))?;
    }

    eprintln!("lenslet: 3/3 lenslet.lock from `{}`", shell_join(&command));
    let mut run_args = vec!["--update".to_string(), "--upstream".to_string(), upstream];
    run_args.push("--".to_string());
    run_args.extend(command);
    let code = Box::pin(run(run_args)).await?;
    if code == 0 {
        let emulator_note = if detected_emulator.is_some() {
            "and make sure the workflow installs your emulator and test dependencies."
        } else {
            "and set `emulator:` in the workflow to start your emulator in CI."
        };
        eprintln!(
            "\nlenslet: done. Review lenslet.lock (mark fixture-only lines `# test`), then\n  \
             git add lenslet.lock .github/workflows/lenslet.yml\n\
             {emulator_note}"
        );
    }
    Ok(code)
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

/// `lenslet env [--shell sh|fish|pwsh|dotenv|docker] [--listen PORT]`:
/// print environment configuration pointing SDKs at a running Lenslet proxy.
fn env_cmd(args: Vec<String>) -> Result<i32> {
    let mut shell: Option<String> = None;
    let mut port: u16 = 9297;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--shell" | "-s" => {
                shell = Some(
                    it.next()
                        .context("--shell requires sh, fish, pwsh, dotenv, or docker")?,
                );
            }
            "--listen" | "-p" => {
                let raw = it.next().context("--listen requires a port")?;
                port = raw
                    .parse::<u16>()
                    .with_context(|| format!("--listen expects a port number, got `{raw}`"))?;
            }
            _ => bail!("usage: lenslet env [--shell sh|fish|pwsh|dotenv|docker] [--listen PORT]"),
        }
    }
    let detected = shell.unwrap_or_else(|| {
        let env_shell = std::env::var("SHELL").unwrap_or_default();
        if env_shell.ends_with("/fish") {
            "fish".to_string()
        } else {
            "sh".to_string()
        }
    });
    let endpoint = format!("http://127.0.0.1:{port}");
    let unset_keys = [
        "AWS_PROFILE",
        "AWS_DEFAULT_PROFILE",
        "AWS_SESSION_TOKEN",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_ROLE_ARN",
    ];
    match detected.to_ascii_lowercase().as_str() {
        "sh" | "bash" | "zsh" => {
            println!("unset {}", unset_keys.join(" "));
            println!("export AWS_ENDPOINT_URL={endpoint}");
            println!("export AWS_ACCESS_KEY_ID={}", LOCAL_KEYS.0);
            println!("export AWS_SECRET_ACCESS_KEY={}", LOCAL_KEYS.1);
            println!("export AWS_DEFAULT_REGION=us-east-1");
        }
        "fish" => {
            for k in unset_keys {
                println!("set -e {k}");
            }
            println!("set -gx AWS_ENDPOINT_URL {endpoint}");
            println!("set -gx AWS_ACCESS_KEY_ID {}", LOCAL_KEYS.0);
            println!("set -gx AWS_SECRET_ACCESS_KEY {}", LOCAL_KEYS.1);
            println!("set -gx AWS_DEFAULT_REGION us-east-1");
        }
        "pwsh" | "powershell" => {
            for k in unset_keys {
                println!("Remove-Item Env:{k} -ErrorAction SilentlyContinue");
            }
            println!("$env:AWS_ENDPOINT_URL = \"{endpoint}\"");
            println!("$env:AWS_ACCESS_KEY_ID = \"{}\"", LOCAL_KEYS.0);
            println!("$env:AWS_SECRET_ACCESS_KEY = \"{}\"", LOCAL_KEYS.1);
            println!("$env:AWS_DEFAULT_REGION = \"us-east-1\"");
        }
        "dotenv" | "env" => {
            println!("AWS_ENDPOINT_URL={endpoint}");
            println!("AWS_ACCESS_KEY_ID={}", LOCAL_KEYS.0);
            println!("AWS_SECRET_ACCESS_KEY={}", LOCAL_KEYS.1);
            println!("AWS_DEFAULT_REGION=us-east-1");
        }
        "docker" => {
            println!(
                "-e AWS_ENDPOINT_URL=http://host.docker.internal:{port} -e AWS_ACCESS_KEY_ID={} -e AWS_SECRET_ACCESS_KEY={} -e AWS_DEFAULT_REGION=us-east-1",
                LOCAL_KEYS.0, LOCAL_KEYS.1
            );
        }
        other => bail!("unknown --shell `{other}`; expected sh, fish, pwsh, dotenv, or docker"),
    }
    Ok(0)
}

const POLICY_USAGE: &str = "usage: lenslet policy [--lock PATH | --run RUN] [--format iam|terraform|cfn|sam|cdk] [--role ROLE] [--region REG] [--account ACC]";

/// `lenslet policy`: compile least-privilege IAM JSON or IaC (Terraform, CloudFormation,
/// SAM, CDK) from `lenslet.lock` or a recorded run.
fn policy(args: Vec<String>) -> Result<i32> {
    let mut lock_path: Option<PathBuf> = None;
    let mut run_spec: Option<String> = None;
    let mut format = iac::PolicyFormat::IamJson;
    let mut role_filter: Option<String> = None;
    let mut region: Option<String> = None;
    let mut account: Option<String> = None;

    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--lock" => lock_path = Some(PathBuf::from(it.next().context(POLICY_USAGE)?)),
            "--run" => run_spec = Some(it.next().context(POLICY_USAGE)?),
            "--format" | "-f" => {
                let raw = it.next().context(POLICY_USAGE)?;
                format = iac::PolicyFormat::parse(&raw).ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown --format `{raw}`; expected iam, terraform, cfn, sam, or cdk"
                    )
                })?;
            }
            "--role" => role_filter = Some(it.next().context(POLICY_USAGE)?),
            "--region" => region = Some(it.next().context(POLICY_USAGE)?),
            "--account" => account = Some(it.next().context(POLICY_USAGE)?),
            _ => bail!("{POLICY_USAGE}"),
        }
    }
    if lock_path.is_some() && run_spec.is_some() {
        bail!("use --lock or --run, not both\n{POLICY_USAGE}");
    }

    let opts = iac::CompileOptions {
        format,
        region,
        account,
    };

    if let Some(spec) = run_spec.as_deref() {
        let dir = runs::dir(&home());
        let run_path = runs::resolve(&dir, spec)?;
        let report = runs::report(&run_path)
            .with_context(|| format!("run {} has not finished yet", runs::run_id(&run_path)))?;
        let entries: &[summary::AccessEntry] = if let Some(filter) = role_filter.as_deref() {
            let matched = report
                .roles
                .iter()
                .find(|(role_arn, _)| {
                    *role_arn == filter
                        || role_arn.ends_with(&format!("/{filter}"))
                        || role_arn.ends_with(&format!(":{filter}"))
                })
                .map(|(_, list)| list.as_slice());
            let Some(list) = matched else {
                let available: Vec<&str> = report.roles.keys().map(String::as_str).collect();
                if available.is_empty() {
                    bail!(
                        "run {} did not record any `sts:AssumeRole` sessions",
                        report.run
                    );
                }
                bail!(
                    "role `{filter}` not found in run {} (observed roles: {})",
                    report.run,
                    available.join(", ")
                );
            };
            list
        } else {
            &report.access
        };
        let inputs: Vec<iac::PolicyInput> = entries
            .iter()
            .map(|e| iac::PolicyInput {
                access: access::Access::new(e.action.clone(), e.resource.clone()),
                implied: e.used.implied,
                via: e.used.via.clone(),
            })
            .collect();
        println!("{}", iac::compile(&inputs, &opts));
        eprintln!(
            "lenslet: compiled {} ({} permissions from run {}). Review before attaching it to a role.",
            format.label(),
            inputs.len(),
            report.run
        );
        return Ok(0);
    }

    if role_filter.is_some() {
        bail!("--role requires `--run RUN` (e.g. `lenslet policy --run latest --role worker`)");
    }

    let path = match lock_path {
        Some(p) => p,
        None => lock::path_in(&std::env::current_dir().context("read the current directory")?),
    };
    let Some(locked) = lock::read(&path)? else {
        bail!(
            "no {}; create it with `lenslet run --update -- <tests>`",
            path.display()
        );
    };
    let inputs: Vec<iac::PolicyInput> = locked
        .access
        .difference(&locked.test)
        .map(|acc| iac::PolicyInput {
            access: acc.clone(),
            implied: locked.implied.contains(acc),
            via: Vec::new(),
        })
        .collect();
    println!("{}", iac::compile(&inputs, &opts));
    eprintln!(
        "lenslet: observed access from {} ({} permissions, {} marked `# test` left out). \
         Review before attaching it to a role.",
        path.display(),
        locked.access.len() - locked.test.len(),
        locked.test.len()
    );
    Ok(0)
}

#[derive(serde::Serialize)]
struct RunListEntry {
    run: String,
    started: String,
    command: Vec<String>,
    running: bool,
    exit_code: Option<i32>,
    calls: Option<usize>,
    errors: Option<usize>,
    blocked: Option<usize>,
    duration_ms: Option<u64>,
    trace: String,
}

/// `lenslet runs [--json] [--limit N]`: list recorded runs newest-first.
fn runs_cmd(args: Vec<String>) -> Result<i32> {
    let mut json = false;
    let mut limit: usize = 20;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--limit" => {
                let raw = it.next().context("--limit requires a number")?;
                limit = raw
                    .parse::<usize>()
                    .with_context(|| format!("--limit expects a positive integer, got `{raw}`"))?;
            }
            _ => bail!("usage: lenslet runs [--json] [--limit N]"),
        }
    }
    let home = home();
    let all = runs::list(&runs::dir(&home));
    if all.is_empty() {
        if json {
            println!("[]");
            return Ok(0);
        }
        bail!("no runs yet; try `lenslet run -- <command>`");
    }
    let mut entries = Vec::new();
    for path in all.into_iter().rev().take(limit) {
        let id = runs::run_id(&path);
        let hdr = runs::header(&path);
        let rep = runs::report(&path);
        let running = rep.is_none() && runs::is_active(&path);
        entries.push(RunListEntry {
            run: id,
            started: hdr.as_ref().map(|h| h.started.clone()).unwrap_or_default(),
            command: hdr.map(|h| h.command).unwrap_or_default(),
            running,
            exit_code: rep.as_ref().and_then(|r| r.exit_code),
            calls: rep.as_ref().map(|r| r.calls),
            errors: rep.as_ref().map(|r| r.errors),
            blocked: rep.as_ref().map(|r| r.blocked),
            duration_ms: rep.as_ref().map(|r| r.duration_ms),
            trace: tilde(&path, &home),
        });
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
        return Ok(0);
    }
    for e in &entries {
        let status = if e.running {
            "running".to_string()
        } else {
            match e.exit_code {
                Some(code) => format!("exit {code}"),
                None => "stopped".to_string(),
            }
        };
        let stats = match (e.calls, e.errors, e.duration_ms) {
            (Some(c), Some(err), Some(ms)) => format!("{c} calls · {err} errors · {ms} ms"),
            _ => "in progress".to_string(),
        };
        let cmd = if e.command.is_empty() {
            "(proxy)".to_string()
        } else {
            format!("$ {}", e.command.join(" "))
        };
        println!("{:<24}  {:<9}  {:<32}  {}", e.run, status, stats, cmd);
    }
    Ok(0)
}

/// `lenslet diff [BASE] [TARGET] [--json] [--markdown PATH]`: compare two runs.
fn diff_cmd(args: Vec<String>) -> Result<i32> {
    let mut json = false;
    let mut markdown: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--markdown" => markdown = Some(it.next().context("--markdown requires a path")?),
            flag if flag.starts_with('-') => {
                bail!("usage: lenslet diff [BASE] [TARGET] [--json] [--markdown PATH]")
            }
            other => positional.push(other.to_string()),
        }
    }
    if positional.len() > 2 {
        bail!("usage: lenslet diff [BASE] [TARGET] [--json] [--markdown PATH]");
    }
    let home = home();
    let dir = runs::dir(&home);
    let (base_path, target_path) = match positional.as_slice() {
        [base, target] => (runs::resolve(&dir, base)?, runs::resolve(&dir, target)?),
        [base] => {
            let latest = runs::list(&dir)
                .into_iter()
                .rev()
                .find(|p| runs::report(p).is_some())
                .context("no finished runs yet")?;
            (runs::resolve(&dir, base)?, latest)
        }
        [] => {
            let finished: Vec<PathBuf> = runs::list(&dir)
                .into_iter()
                .filter(|p| runs::report(p).is_some())
                .collect();
            if finished.len() < 2 {
                bail!("need at least 2 finished runs to diff; try `lenslet runs`");
            }
            (
                finished[finished.len() - 2].clone(),
                finished[finished.len() - 1].clone(),
            )
        }
        _ => unreachable!(),
    };
    let base_report = runs::report(&base_path)
        .with_context(|| format!("run {} has not finished yet", runs::run_id(&base_path)))?;
    let target_report = runs::report(&target_path)
        .with_context(|| format!("run {} has not finished yet", runs::run_id(&target_path)))?;

    let diff = summary::RunDiff::compute(&base_report, &target_report);
    if let Some(md_path) = markdown.as_deref() {
        write_text(&diff.markdown(), md_path)?;
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&diff)?);
    } else {
        print!("{}", diff.render());
    }
    Ok(0)
}

const CHECK_USAGE: &str = "usage: lenslet check [--policy PATH | --lock PATH] [--context KEY=VAL] [RUN] [--json] [--markdown PATH]";

/// One denied permission of one recorded call.
#[derive(serde::Serialize)]
struct CheckDenial {
    id: String,
    service: String,
    operation: String,
    #[serde(flatten)]
    denial: lock::Denial,
}

/// What `lenslet check` found.
#[derive(serde::Serialize)]
struct CheckReport {
    run: String,
    /// The policy or lock checked against.
    gate: String,
    policy: bool,
    calls: usize,
    denied: Vec<CheckDenial>,
}

impl CheckReport {
    /// Why a permission was denied: the denying statement, or that nothing
    /// grants it.
    fn reason(&self, denial: &lock::Denial) -> String {
        let mut text = match (denial.decision.reason, denial.decision.by.as_deref()) {
            (lock::Reason::ExplicitDeny, Some(statement)) => {
                format!("denied by statement `{statement}`")
            }
            (lock::Reason::NeedsContext, Some(statement)) => {
                let keys = denial.decision.missing_context.join(", ");
                let example = denial
                    .decision
                    .missing_context
                    .first()
                    .map(|k| format!("; pass `--context {k}=...`"))
                    .unwrap_or_default();
                format!("statement `{statement}` needs cloud context ({keys}{example})")
            }
            _ if self.policy => "no statement allows it".to_string(),
            _ => format!("not in {}", self.gate),
        };
        if denial.implied {
            text.push_str(" (implied)");
        }
        if denial.unverified {
            text.push_str(" (resource unknown: request body not readable)");
        }
        text
    }

    /// Denials grouped by permission and reason, in first-call order, with how
    /// many calls hit each: a loop of the same call is one line, not fifty.
    fn grouped(&self) -> Vec<(&CheckDenial, usize)> {
        let mut groups: Vec<(&CheckDenial, usize)> = Vec::new();
        for item in &self.denied {
            let key = |d: &CheckDenial| {
                (
                    d.denial.action.clone(),
                    d.denial.resource.clone(),
                    self.reason(&d.denial),
                )
            };
            match groups.iter_mut().find(|(first, _)| key(first) == key(item)) {
                Some((_, calls)) => *calls += 1,
                None => groups.push((item, 1)),
            }
        }
        groups
    }

    fn render(&self) -> String {
        let mut out = format!(
            "lenslet check: run {} against {} · {} · ",
            self.run,
            self.gate,
            count(self.calls, "call")
        );
        if self.denied.is_empty() {
            out.push_str("all allowed\n");
            return out;
        }
        out.push_str(&format!(
            "{} denied\n",
            count(self.grouped().len(), "permission")
        ));
        for (item, calls) in self.grouped() {
            let times = if calls > 1 {
                format!(" ×{calls}")
            } else {
                String::new()
            };
            out.push_str(&format!(
                "  ✗ {}  {}  ({} {}{times})\n      → {}\n",
                item.denial.action,
                item.denial.resource,
                item.service.to_ascii_uppercase(),
                item.operation,
                self.reason(&item.denial)
            ));
        }
        out
    }

    fn markdown(&self) -> String {
        let mut out = format!(
            "### Lenslet check · run `{}` against `{}`\n\n",
            self.run, self.gate
        );
        if self.denied.is_empty() {
            out.push_str(&format!("All {} allowed.\n", count(self.calls, "call")));
            return out;
        }
        out.push_str(&format!(
            "**{} denied** across {}.\n\n| Call | Permission | Resource | Why |\n|---|---|---|---|\n",
            count(self.grouped().len(), "permission"),
            count(self.calls, "call")
        ));
        for (item, calls) in self.grouped() {
            let times = if calls > 1 {
                format!(" ×{calls}")
            } else {
                String::new()
            };
            out.push_str(&format!(
                "| {} {}{times} | `{}` | `{}` | {} |\n",
                item.service.to_ascii_uppercase(),
                item.operation,
                item.denial.action,
                item.denial.resource,
                self.reason(&item.denial).replace('|', "\\|")
            ));
        }
        out
    }
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// `lenslet check [--policy PATH | --lock PATH] [RUN] [--json] [--markdown PATH]`:
/// re-evaluate a recorded run's calls against a policy or lock, deriving each
/// call's access again from its recorded request.
fn check_cmd(args: Vec<String>) -> Result<i32> {
    let mut json = false;
    let mut markdown: Option<String> = None;
    let mut policy_path: Option<PathBuf> = None;
    let mut lock_path: Option<PathBuf> = None;
    let mut context_overrides: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut run: Option<String> = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--json" => json = true,
            "--markdown" => markdown = Some(it.next().context(CHECK_USAGE)?),
            "--policy" => policy_path = Some(PathBuf::from(it.next().context(CHECK_USAGE)?)),
            "--lock" => lock_path = Some(PathBuf::from(it.next().context(CHECK_USAGE)?)),
            "--context" | "-c" => {
                let raw = it.next().context(CHECK_USAGE)?;
                let Some((k, v)) = raw.split_once('=') else {
                    bail!(
                        "--context expects KEY=VALUE (e.g. `--context aws:PrincipalOrgID=o-123`), got `{raw}`"
                    );
                };
                let key = k.trim();
                if key.is_empty() {
                    bail!("--context `{raw}` must name a non-empty IAM condition key");
                }
                context_overrides
                    .entry(key.to_string())
                    .or_default()
                    .push(v.to_string());
            }
            flag if flag.starts_with('-') => bail!("{CHECK_USAGE}"),
            _ if run.is_none() => run = Some(arg),
            _ => bail!("{CHECK_USAGE}"),
        }
    }
    if policy_path.is_some() && lock_path.is_some() {
        bail!("use --policy or --lock, not both\n{CHECK_USAGE}");
    }
    let cwd = std::env::current_dir().context("read the current directory")?;
    let (gate, policy) = match policy_path {
        Some(path) => {
            let label = relative_to(&path, &cwd);
            (lock::Gate::from_policy_file_simulating(&path, label)?, true)
        }
        None => {
            let path = lock_path.unwrap_or_else(|| lock::path_in(&cwd));
            let label = relative_to(&path, &cwd);
            let Some(locked) = lock::read(&path)? else {
                bail!("no {label}; pass --policy PATH or --lock PATH");
            };
            (lock::Gate::from_lock(label, locked.access), false)
        }
    };
    let dir = runs::dir(&home());
    let path = match run.as_deref() {
        Some(spec) => runs::resolve(&dir, spec)?,
        None => runs::list(&dir)
            .into_iter()
            .rev()
            .find(|path| runs::report(path).is_some())
            .context("no finished runs yet; try `lenslet run -- <command>`")?,
    };
    let calls = runs::calls(&path)?;
    let denied = calls
        .iter()
        .flat_map(|call| {
            lock::evaluate_with_context(call, Some(&gate), &context_overrides)
                .denied
                .into_iter()
                .map(|denial| CheckDenial {
                    id: call.id.clone(),
                    service: call.service.clone(),
                    operation: call.operation.clone(),
                    denial,
                })
        })
        .collect();
    let report = CheckReport {
        run: runs::run_id(&path),
        gate: gate.label.clone(),
        policy,
        calls: calls.len(),
        denied,
    };
    if let Some(target) = markdown.as_deref() {
        write_text(&report.markdown(), target)?;
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", report.render());
    }
    Ok(if report.denied.is_empty() {
        0
    } else {
        EXIT_ACCESS_CHANGED
    })
}

/// `path` relative to `dir` when it is inside it, also through symlinks
/// (macOS temp dirs live under `/var`, which is `/private/var`).
fn relative_to(path: &Path, dir: &Path) -> String {
    if let Ok(relative) = path.strip_prefix(dir) {
        return relative.display().to_string();
    }
    let canonical = |path: &Path| -> Option<PathBuf> {
        // The file may not exist yet: resolve its directory instead.
        let parent = path.parent()?.canonicalize().ok()?;
        Some(parent.join(path.file_name()?))
    };
    match (canonical(path), dir.canonicalize().ok()) {
        (Some(path), Some(dir)) => match path.strip_prefix(&dir) {
            Ok(relative) => relative.display().to_string(),
            Err(_) => path.display().to_string(),
        },
        _ => path.display().to_string(),
    }
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
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .context("locate the lenslet binary")?;
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let name = if cfg!(windows) {
        "lenslet-ui.exe"
    } else {
        "lenslet-ui"
    };
    // The CLI ships inside Lenslet.app on macOS (Contents/MacOS/lenslet, next
    // to the window's `lenslet-ui`), next to `lenslet-ui` in the Linux
    // archive, or on its own via install.sh (then /Applications is tried).
    let bundle = dir
        .ancestors()
        .nth(2)
        .filter(|app| app.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf);
    let app = bundle
        .or_else(|| Some(dir.join("Lenslet.app")).filter(|app| app.is_dir()))
        .or_else(|| Some(PathBuf::from("/Applications/Lenslet.app")).filter(|app| app.is_dir()))
        .filter(|_| cfg!(target_os = "macos"));
    let mut command = if let Some(app) = app {
        let mut open = std::process::Command::new("open");
        open.arg("-a").arg(app);
        if let Some(trace) = trace {
            open.arg("--env")
                .arg(format!("LENSLET_TRACE={}", trace.display()));
            open.arg(trace);
        }
        open
    } else if dir.join(name).is_file() {
        let mut bin = std::process::Command::new(dir.join(name));
        if let Some(trace) = trace {
            bin.arg(trace);
        }
        bin
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
        let names = |env: &ChildEnv| -> Vec<String> {
            env.set.iter().map(|(name, _)| name.clone()).collect()
        };

        let real = |name: &str| {
            matches!(
                name,
                "AWS_PROFILE"
                    | "AWS_ACCESS_KEY_ID"
                    | "AWS_SESSION_TOKEN"
                    | "AWS_WEB_IDENTITY_TOKEN_FILE"
                    | "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI"
                    | "AWS_BEARER_TOKEN_BEDROCK"
            )
            .then(|| OsString::from("real"))
        };
        let env = child_env("http://127.0.0.1:5000", &dir, false, real);
        assert_eq!(
            names(&env),
            [
                "AWS_ENDPOINT_URL",
                "AWS_SHARED_CREDENTIALS_FILE",
                "AWS_CONFIG_FILE"
            ]
        );
        assert_eq!(
            env.remove,
            [
                "AWS_ACCESS_KEY_ID",
                "AWS_SESSION_TOKEN",
                "AWS_PROFILE",
                "AWS_WEB_IDENTITY_TOKEN_FILE",
                "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
                "AWS_BEARER_TOKEN_BEDROCK",
            ]
        );

        // No emulator credentials: real keys still go, placeholders come in,
        // so the SDK cannot fall back to ~/.aws/credentials.
        let empty = dir.join("nothing");
        let env = child_env("http://x", &empty, false, real);
        assert_eq!(
            names(&env),
            [
                "AWS_ENDPOINT_URL",
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY"
            ]
        );
        assert_eq!(env.set[1].1, LOCAL_KEYS.0);
        assert!(env.remove.contains(&"AWS_ACCESS_KEY_ID".to_string()));

        // A credentials file the user chose is respected; env keys still go.
        let env = child_env("http://x", &empty, false, |name| {
            matches!(name, "AWS_SHARED_CREDENTIALS_FILE" | "AWS_ACCESS_KEY_ID")
                .then(|| OsString::from("mine"))
        });
        assert_eq!(names(&env), ["AWS_ENDPOINT_URL"]);
        assert_eq!(env.remove, ["AWS_ACCESS_KEY_ID"]);

        let env = child_env("http://x", &dir, true, real);
        assert_eq!(names(&env), ["AWS_ENDPOINT_URL"]);
        assert!(env.remove.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn init_keeps_argument_boundaries_for_bash() {
        let argv = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        assert_eq!(shell_join(&argv(&["npm", "test"])), "npm test");
        assert_eq!(
            shell_join(&argv(&["pytest", "-k", "not slow"])),
            "pytest -k 'not slow'"
        );
        assert_eq!(
            shell_join(&argv(&["python", "-c", "print('hi')"])),
            r"python -c 'print('\''hi'\'')'"
        );
        assert_eq!(shell_join(&argv(&["echo", ""])), "echo ''");
        // What bash does with it is the original argv.
        let joined = shell_join(&argv(&["printf", "%s|", "a b", "it's", "$HOME"]));
        let out = std::process::Command::new("bash")
            .args(["-c", &joined])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "a b|it's|$HOME|");
    }

    #[test]
    fn the_init_workflow_quotes_the_command() {
        assert_eq!(yaml_quote("npm test"), "'npm test'");
        assert_eq!(
            yaml_quote("pytest -k 'not slow'"),
            "'pytest -k ''not slow'''"
        );
        let text = render_workflow(
            "4.1.0",
            &strings(&["npm", "test"]),
            "https://127.0.0.1:9298",
            Some("aws-emulator --telemetry off --port 9298"),
        );
        assert!(text.contains("uses: hxddh/lenslet@lenslet-v4.1.0"));
        assert!(text.contains("command: 'npm test'"));
        assert!(text.contains("emulator: 'aws-emulator --telemetry off --port 9298'"));
        assert!(!text.contains('{'), "every placeholder is filled");
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
    fn lock_labels_are_relative_even_through_symlinks() {
        let base = std::env::temp_dir().join(format!("lenslet-rel-{}", std::process::id()));
        let real = base.join("real");
        std::fs::create_dir_all(real.join("infra")).unwrap();
        assert_eq!(
            relative_to(&real.join("lenslet.lock"), &real),
            "lenslet.lock"
        );
        #[cfg(unix)]
        {
            let link = base.join("link");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&real, &link).unwrap();
            // The lock is named through the link, the directory is the real path.
            assert_eq!(
                relative_to(&link.join("infra/aws.lock"), &real.canonicalize().unwrap()),
                "infra/aws.lock"
            );
        }
        assert_eq!(
            relative_to(Path::new("/elsewhere/x.lock"), &real),
            "/elsewhere/x.lock"
        );
        std::fs::remove_dir_all(&base).unwrap();
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
