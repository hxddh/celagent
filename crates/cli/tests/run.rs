//! `lenslet` end to end: the real binary, a child process that calls AWS
//! through `AWS_ENDPOINT_URL`, a stand-in emulator, and the lock workflow.

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// A one-response-per-connection "emulator": GetQueueUrl fails the way SQS
/// does for a missing queue; everything else succeeds.
fn fake_emulator() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => request.extend_from_slice(&buffer[..read]),
                }
            }
            let missing = String::from_utf8_lossy(&request).contains("GetQueueUrl");
            let (status, body) = if missing {
                (
                    "400 Bad Request",
                    r#"{"__type":"com.amazonaws.sqs#QueueDoesNotExist","message":"The specified queue does not exist."}"#,
                )
            } else {
                ("200 OK", "{}")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/x-amz-json-1.0\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    port
}

fn temp_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lenslet-run-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Env {
    home: PathBuf,
    work: PathBuf,
    port: u16,
}

impl Env {
    fn new() -> Env {
        let port = fake_emulator();
        let home = temp_dir();
        let emulator = home.join(format!(".aws/emulator/{port}"));
        std::fs::create_dir_all(&emulator).unwrap();
        std::fs::write(emulator.join("emulator_credentials"), "[default]\n").unwrap();
        std::fs::write(emulator.join("config"), "[default]\nregion = us-east-1\n").unwrap();
        let work = home.join("project");
        std::fs::create_dir_all(&work).unwrap();
        Env { home, work, port }
    }

    fn lenslet(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_lenslet"))
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env_remove("AWS_SHARED_CREDENTIALS_FILE")
            .env_remove("AWS_CONFIG_FILE")
            .output()
            .unwrap()
    }

    /// `lenslet run [flags] -- sh -c SCRIPT`. The script is always the same
    /// (it reads its calls and exit code from the environment), so runs are
    /// runs of one command, as with a real `npm test`.
    fn run(&self, flags: &[&str], targets: &[&str], exit: i32) -> Output {
        let script = "test -n \"$AWS_ENDPOINT_URL\" || exit 90\n\
            test -n \"$AWS_SHARED_CREDENTIALS_FILE\" || exit 91\n\
            for target in $TARGETS; do\n\
              curl -s -o /dev/null -X POST \"$AWS_ENDPOINT_URL/\" \
                -H \"x-amz-target: AmazonSQS.$target\" \
                -H 'content-type: application/x-amz-json-1.0' \
                -H 'authorization: AWS4-HMAC-SHA256 Credential=AKID/20261115/us-east-1/sqs/aws4_request, SignedHeaders=host, Signature=x' \
                -d '{\"QueueUrl\":\"http://127.0.0.1/000000000000/jobs\"}'\n\
            done\n\
            exit $EXIT\n";
        let upstream = format!("http://127.0.0.1:{}", self.port);
        let mut args = vec!["run", "--upstream", &upstream];
        args.extend_from_slice(flags);
        args.extend_from_slice(&["--", "sh", "-c", script]);
        Command::new(env!("CARGO_BIN_EXE_lenslet"))
            .args(&args)
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("TARGETS", targets.join(" "))
            .env("EXIT", exit.to_string())
            .env_remove("AWS_SHARED_CREDENTIALS_FILE")
            .env_remove("AWS_CONFIG_FILE")
            .output()
            .unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn has_curl() -> bool {
    Command::new("curl").arg("--version").output().is_ok()
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn run_explains_failures_keeps_the_exit_code_and_writes_one_run_file() {
    if !has_curl() {
        eprintln!("skipping: curl is not installed");
        return;
    }
    let env = Env::new();
    let summary = env.home.join("summary.json");
    let output = env.run(
        &["--json", summary.to_str().unwrap()],
        &["SendMessage", "GetQueueUrl"],
        4,
    );
    let text = stderr(&output);
    assert_eq!(
        output.status.code(),
        Some(4),
        "exit code passes through:\n{text}"
    );
    assert!(text.contains("2 calls · 1 error"), "{text}");
    assert!(
        text.contains(
            "✗ SQS GetQueueUrl  400  QueueDoesNotExist  The specified queue does not exist."
        ),
        "{text}"
    );
    assert!(text.contains("access: 2 permissions observed"), "{text}");

    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&summary).unwrap()).unwrap();
    assert_eq!(report["exit_code"], 4);
    assert_eq!(report["failures"][0]["code"], "QueueDoesNotExist");
    assert_eq!(report["access"][0]["action"], "sqs:GetQueueUrl");
    assert_eq!(report["access"][0]["resource"], "arn:aws:sqs:*:*:jobs");

    // One run file: header, calls, and the same report as the last line.
    let trace = PathBuf::from(report["trace"].as_str().unwrap());
    assert!(trace.starts_with(env.home.join(".lenslet/runs")));
    let records = lines(&trace);
    assert_eq!(records[0]["type"], "run");
    assert_eq!(records[0]["command"][0], "sh");
    assert_eq!(records[1]["type"], "call");
    assert_eq!(records[1]["access"][0]["action"], "sqs:SendMessage");
    let end = records.last().unwrap();
    assert_eq!(end["type"], "end");
    assert_eq!(end["exit_code"], 4);

    // `lenslet last --json` is that report, for an agent that did not run it.
    let last = env.lenslet(&["last", "--json"]);
    let last: serde_json::Value = serde_json::from_slice(&last.stdout).unwrap();
    assert_eq!(last["run"], report["run"]);
    assert_eq!(last["failures"], report["failures"]);
}

#[test]
fn the_lock_workflow_catches_new_access_and_regressions() {
    if !has_curl() {
        eprintln!("skipping: curl is not installed");
        return;
    }
    let env = Env::new();

    // No lock yet: --check refuses to guess.
    let output = env.run(&["--check"], &["SendMessage"], 0);
    assert_eq!(output.status.code(), Some(2), "{}", stderr(&output));
    assert!(stderr(&output).contains("--check needs lenslet.lock"));

    // --update never locks a failed run's partial access.
    let output = env.run(&["--update"], &["SendMessage"], 1);
    assert_eq!(output.status.code(), Some(1));
    assert!(!env.work.join("lenslet.lock").exists());
    assert!(stderr(&output).contains("lenslet.lock not written"));

    // --update writes the observed access.
    let output = env.run(&["--update"], &["SendMessage"], 0);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let lock = std::fs::read_to_string(env.work.join("lenslet.lock")).unwrap();
    assert!(
        lock.contains("sqs:SendMessage  arn:aws:sqs:*:*:jobs"),
        "{lock}"
    );

    // Same access: --check passes.
    let output = env.run(&["--check"], &["SendMessage"], 0);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).contains("access: within lenslet.lock"));

    // New access and a new failure: --check fails with 3 and says what changed.
    let summary = env.home.join("check.json");
    let output = env.run(
        &["--check", "--json", summary.to_str().unwrap()],
        &["SendMessage", "DeleteMessage", "GetQueueUrl"],
        0,
    );
    let text = stderr(&output);
    assert_eq!(output.status.code(), Some(3), "{text}");
    assert!(
        text.contains("access: 2 permissions not in lenslet.lock"),
        "{text}"
    );
    assert!(text.contains("+ sqs:DeleteMessage"), "{text}");
    assert!(text.contains("1 new failure, 0 fixed"), "{text}");
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&summary).unwrap()).unwrap();
    assert_eq!(report["access_added"][0]["action"], "sqs:DeleteMessage");
    assert_eq!(
        report["regressions"]["new_failures"][0]["code"],
        "QueueDoesNotExist"
    );
    let records = lines(PathBuf::from(report["trace"].as_str().unwrap()).as_path());
    let delete = records
        .iter()
        .find(|record| record["access"][0]["action"] == "sqs:DeleteMessage")
        .unwrap();
    assert_eq!(
        delete["new"], true,
        "the window can mark the call that widened access"
    );

    // A failing command keeps its own exit code even when access grew.
    let output = env.run(&["--check"], &["DeleteMessage"], 7);
    assert_eq!(output.status.code(), Some(7));

    // The lock becomes a policy.
    let output = env.lenslet(&["policy"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let policy: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(policy["Statement"][0]["Action"][0], "sqs:SendMessage");
    assert_eq!(policy["Statement"][0]["Resource"], "arn:aws:sqs:*:*:jobs");
}

#[test]
fn usage_errors_exit_2() {
    let env = Env::new();
    for args in [
        &["run", "--check"][..],
        &["run", "--nope", "x"],
        &["frobnicate"],
    ] {
        let output = env.lenslet(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}
