//! `lenslet run` end to end: the real binary, a child process that calls AWS
//! through `AWS_ENDPOINT_URL`, and a stand-in emulator.

use std::{
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
};

/// A one-response-per-connection "emulator" that rejects every call the way
/// SQS does for a missing queue.
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
            let body = r#"{"__type":"com.amazonaws.sqs#QueueDoesNotExist","message":"The specified queue does not exist."}"#;
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\ncontent-type: application/x-amz-json-1.0\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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

#[test]
fn run_routes_the_child_explains_failures_and_keeps_its_exit_code() {
    if Command::new("curl").arg("--version").output().is_err() {
        eprintln!("skipping: curl is not installed");
        return;
    }
    let port = fake_emulator();
    let home = temp_dir();
    let emulator = home.join(format!(".aws/emulator/{port}"));
    std::fs::create_dir_all(&emulator).unwrap();
    std::fs::write(emulator.join("emulator_credentials"), "[default]\n").unwrap();
    std::fs::write(emulator.join("config"), "[default]\nregion = us-east-1\n").unwrap();
    let summary = home.join("summary.json");

    let script = r#"
        test -n "$AWS_ENDPOINT_URL" || exit 90
        test -n "$AWS_SHARED_CREDENTIALS_FILE" || exit 91
        curl -s -o /dev/null -X POST "$AWS_ENDPOINT_URL/" \
          -H 'x-amz-target: AmazonSQS.GetQueueUrl' \
          -H 'content-type: application/x-amz-json-1.0' \
          -H 'authorization: AWS4-HMAC-SHA256 Credential=AKID/20261115/us-east-1/sqs/aws4_request, SignedHeaders=host, Signature=x' \
          -d '{"QueueName":"missing"}'
        exit 4
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_lenslet"))
        .args(["run", "--upstream", &format!("http://127.0.0.1:{port}")])
        .arg("--json")
        .arg(&summary)
        .args(["--", "sh", "-c", script])
        .env("HOME", &home)
        .env_remove("AWS_SHARED_CREDENTIALS_FILE")
        .env_remove("AWS_CONFIG_FILE")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(4),
        "exit code passes through:\n{stderr}"
    );
    assert!(stderr.contains("1 call · 1 error"), "{stderr}");
    assert!(
        stderr.contains(
            "✗ SQS GetQueueUrl  400  QueueDoesNotExist  The specified queue does not exist."
        ),
        "{stderr}"
    );
    assert!(stderr.contains("→ The resource does not exist"), "{stderr}");

    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&summary).unwrap()).unwrap();
    assert_eq!(json["exit_code"], 4);
    assert_eq!(json["calls"], 1);
    assert_eq!(json["failures"][0]["code"], "QueueDoesNotExist");

    // The run left a trace and a sidecar in ~/.lenslet/runs for the window.
    let trace = PathBuf::from(json["trace"].as_str().unwrap());
    assert!(trace.starts_with(home.join(".lenslet/runs")));
    let line = std::fs::read_to_string(&trace).unwrap();
    let record: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["operation"], "GetQueueUrl");
    assert_eq!(record["problem"]["code"], "QueueDoesNotExist");
    let info: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(trace.with_extension("run.json")).unwrap())
            .unwrap();
    assert_eq!(info["kind"], "run");
    assert_eq!(info["exit_code"], 4);
    assert!(info["ended"].is_string());

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn run_without_a_command_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_lenslet"))
        .arg("run")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("needs a command"));
}
