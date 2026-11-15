# Lenslet

**See what your code sends to local AWS, and why a call failed.**

```console
$ lenslet run -- npm test
lenslet: http://127.0.0.1:43761 → https://127.0.0.1:9298 · trace ~/.lenslet/runs/20261115-054628-15569.jsonl
...your tests run...
lenslet: 8 calls · 1 error · 3 services (dynamodb, s3, sqs) · 341 ms
  ✗ DYNAMODB GetItem  400  ResourceNotFoundException  Requested resource not found
      → The resource does not exist in the emulator. Was it created in this run, in the same region?
lenslet: trace ~/.lenslet/runs/20261115-054628-15569.jsonl · open it with `lenslet ui`
```

Lenslet sits between an AWS SDK and
[AWS Emulator](https://github.com/aws/aws-emulator). `lenslet run` starts a
proxy, points your program at it, runs it, and prints every AWS call it made,
with the error code, message and likely cause of each failure. The exit code
is your program's. `lenslet ui` shows the same run, live, in a native window.

![Lenslet showing a live run](docs/lenslet.png)

## Quick start

```bash
# 1. AWS Emulator, once
EMULATOR_TELEMETRY=off aws-emulator --telemetry off --port 9298

# 2. Your program, tests or script, through Lenslet
lenslet run -- npm test          # or: pytest, go test ./..., aws s3 ls, ...

# 3. Optional: the window (follows the newest run)
lenslet ui
```

That's all the setup. `lenslet run` sets, for the child process only:

| variable | value |
| --- | --- |
| `AWS_ENDPOINT_URL` | `http://127.0.0.1:<free port>` (plain HTTP on loopback: no certificate to trust) |
| `AWS_SHARED_CREDENTIALS_FILE` | `~/.aws/emulator/9298/emulator_credentials`, if you have not set it |
| `AWS_CONFIG_FILE` | `~/.aws/emulator/9298/config`, if you have not set it |

When it points at the emulator's files it also unsets `AWS_PROFILE`, since your
real profile names do not exist there. `--no-env` sets only the endpoint. Current
AWS SDKs and the AWS CLI read `AWS_ENDPOINT_URL`. Use the IP
(`127.0.0.1`), not `localhost`: with `localhost`, S3 clients try
virtual-hosted buckets (`bucket.localhost`).

### In CI or for an agent

`lenslet run` needs no display. The summary goes to stderr. Add
`--json summary.json` (or `--json -` for stdout) for a machine-readable copy:

```json
{"calls": 8, "errors": 1, "unfinished": 0, "exit_code": 0,
 "failures": [{"service": "dynamodb", "operation": "GetItem", "status": 400,
               "code": "ResourceNotFoundException",
               "message": "Requested resource not found",
               "hint": "The resource does not exist in the emulator. ..."}],
 "trace": "/home/me/.lenslet/runs/20261115-054628-15569.jsonl", ...}
```

### A long-running proxy

For an app server or a debugger session, keep a proxy up instead:

```bash
lenslet proxy                    # http://127.0.0.1:9297, prints the env to export
```

The port also accepts TLS, so 1.x setups that use `https://127.0.0.1:9297` and
trust `~/.lenslet/proxy-cert.pem` keep working.

## What you see

- **Every call, as it completes.** The service comes from the SigV4 scope (header
  or presigned URL), so any emulator service shows up. Operations come from
  `x-amz-target`, `x-id`, the Query `Action=`, and S3's REST shape (`GET /b/k` →
  `GetObject`, `PUT /b?versioning` → `PutBucketVersioning`, …), so boto3 and the
  AWS CLI get real names too.
- **Why it failed.** The AWS error code and message from JSON, XML or Query
  bodies, or the `x-amzn-ErrorType` header, plus a one-line likely cause for the
  common local failures: missing resource, unknown credentials, signature,
  region, not implemented, emulator down.
- **Calls still running.** A call that takes longer than a second shows as
  pending (long polls, large transfers, a stuck emulator).
- **The raw exchange.** Headers and the first 64 KiB of text bodies, formatted
  JSON and XML, copy as raw. Binary bodies keep size and SHA-256.
- **Real status.** The window probes the run's proxy and emulator; it says when a
  run ended and with what exit code.

Lenslet records behavior; it does not claim the emulator matches AWS.

## Install

Release archives: Linux x86_64 (`lenslet`, `lenslet-ui`) and macOS
(`lenslet`, `Lenslet.app`, ad-hoc signed, not notarized).

```bash
curl -fL -o lenslet.tar.gz \
  https://github.com/hxddh/celagent/releases/download/lenslet-v2.0.0/lenslet-2.0.0-linux-x86_64.tar.gz
tar xzf lenslet.tar.gz && cd lenslet-2.0.0-linux-x86_64
./lenslet run -- aws s3 ls
```

Put `lenslet` on your `PATH` and keep `lenslet-ui` (or `Lenslet.app`) next to
it; `lenslet ui` looks there. `lenslet run` and `lenslet proxy` do not need the
window or any graphics libraries. On macOS, if Gatekeeper complains:
`xattr -dr com.apple.quarantine Lenslet.app lenslet`.

## Files

| path | what |
| --- | --- |
| `~/.lenslet/runs/*.jsonl` | one trace per `run` / `proxy` session; the last 20 are kept |
| `~/.lenslet/runs/*.run.json` | what the session was: command, endpoint, upstream, exit code |
| `~/.lenslet/proxy-cert.pem`, `proxy-key.pem` | TLS identity for `https://` clients (generated) |

Traces are a **local development artifact**: raw headers and text bodies,
including `Authorization`, written owner-only (`0600`). Do not commit them or
attach them to public issues. Format: [docs/trace.md](docs/trace.md).

## Build from source

Rust 1.88+ and, for the window, the GPUI Linux libraries (`libxkbcommon-dev`,
`libxkbcommon-x11-dev`, `libwayland-dev`, `libx11-xcb-dev`, `libxcb1-dev`,
`libvulkan-dev`, `libfontconfig1-dev`, `libfreetype-dev`, `pkg-config`,
`cmake`, `clang`) and a Vulkan driver.

```bash
cargo build --release -p lenslet          # CLI only: no graphics libraries needed
cargo build --release -p lenslet-ui       # the window
cargo test --workspace
LENSLET_DEMO=1 ./target/release/lenslet-ui
```

```text
crates/cli   lenslet: run / proxy / ui; proxy, error explanation, summaries
crates/ui    lenslet-ui: the GPUI window
vendor/xattr Linux build fix for gpui 0.2.2's xattr dependency
```

## Limits

- HTTP/1.1 between SDK and proxy; loopback only.
- Text capture is the first 64 KiB per body; binary bodies keep size and SHA-256.
- The window shows the newest 500 calls of the newest run (or `LENSLET_TRACE`).
- Windows is not shipped.

## History

This repository used to hold celagent, an agent runtime; its last release is
[v4.1.0](https://github.com/hxddh/celagent/tree/v4.1.0). Lenslet 1.0 is at
[lenslet-v1.0.0](https://github.com/hxddh/celagent/tree/lenslet-v1.0.0).

## License

Apache-2.0. Bundled fonts are under the SIL Open Font License; see
`assets/fonts/`.
