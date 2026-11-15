# Lenslet

**Run your code against local AWS. See what it did, why calls failed, and what
AWS access it needs.**

```console
$ lenslet run --check -- npm test
...your tests...
lenslet: 15 calls · 2 errors · 6 services (dynamodb, iam, kms, lambda, s3, sqs) · 476 ms
  ✗ S3 GetObject  404  NoSuchKey  The specified key does not exist.
      → The resource does not exist in the emulator. Was it created in this run, in the same region?
  access: 1 permission not in lenslet.lock
    + dynamodb:DeleteItem              arn:aws:dynamodb:*:*:table/orders
$ echo $?
3
```

Lenslet sits between your program and
[AWS Emulator](https://github.com/aws/aws-emulator). AWS Emulator does not
evaluate IAM, so code that works locally can still be denied on AWS. Lenslet
records every call your program makes and works out the IAM action and
resource each one needs. It keeps that list in `lenslet.lock`, a reviewable
file you commit. When a change widens your AWS access, the diff shows up in
the PR and `--check` fails CI.

![A run in the Lenslet window](docs/lenslet.png)

## Quick start

```bash
EMULATOR_TELEMETRY=off aws-emulator --telemetry off --port 9298   # once

lenslet run -- npm test             # see every call, and why failures failed
lenslet run --update -- npm test    # write lenslet.lock; commit it
lenslet run --check -- npm test     # in CI: exit 3 if access grew
lenslet ui                          # the window
```

`lenslet run` sets `AWS_ENDPOINT_URL` for the child process, plus the
emulator's credentials and config when you have not set your own. It unsets
`AWS_PROFILE` and any `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and
`AWS_SESSION_TOKEN` from your environment, so real keys never reach the trace.
The exit code is your program's. Use `127.0.0.1`, not `localhost`, if you set
the endpoint yourself.

## The three answers

**What did it do?** Every call, live, with the service, operation, status,
latency, raw headers and bodies (JSON and XML formatted). Calls slower than a
second show as pending.

**Why did it fail?** The AWS error code and message, plus a one-line likely
cause for common local failures: missing resource, unknown credentials,
signature, region, not implemented, emulator down. A run also compares itself
with the previous run of the same command: "1 new failure, 0 fixed".

**What access does it need?** Each call maps to an IAM action and a resource
ARN, with account and region set to `*`. Implied permissions are included:
`kms:Decrypt` behind an SSE-KMS object, and `iam:PassRole` for a role handed to
Lambda. Generated names (UUIDs, timestamps) become `*`, so the lock stays
stable between runs.

```text
# lenslet.lock
dynamodb:GetItem       arn:aws:dynamodb:*:*:table/orders
iam:PassRole           arn:aws:iam::*:role/shop-lambda  # implied
kms:Decrypt            arn:aws:kms:*:*:key/*  # implied
s3:GetObject           arn:aws:s3:::shop-assets/*
```

`lenslet policy` turns the lock into an IAM policy document. This is
**observed** access: what your tests exercised, mapped by rules. Review it
before you attach it to a role.

## The window

`lenslet ui` follows the newest run. `[` and `]` step through earlier runs.

- **Calls** (default): the stream and inspector. A failed call shows its error
  in the row and a "Why it failed" panel. A call that needed access outside the
  lock is marked `NEW ACCESS`.
- **Access** (`a`): one row per permission, with call counts, `NEW` and
  `implied` markers. Click a row to see the calls behind it.

The header shows the run at a glance: command, exit code, `+N access`, and new
failures. The footer shows whether the proxy and the emulator are actually up.

![The access table](docs/access.png)

## For agents and CI

Every run ends with one JSON report. It is the same object in three places:
the last line of the run file, `lenslet run --json PATH` (`-` for stdout), and
`lenslet last --json`, which reads the newest run without re-running it.

```json
{
  "exit_code": 0, "calls": 15, "errors": 2, "unfinished": 0,
  "failures": [{"service": "s3", "operation": "GetObject", "status": 404,
                "code": "NoSuchKey", "message": "…", "hint": "…"}],
  "access": [{"action": "dynamodb:DeleteItem", "resource": "arn:aws:dynamodb:*:*:table/orders",
              "calls": 1, "errors": 0, "new": true}],
  "lock": "lenslet.lock",
  "access_added": [{"action": "dynamodb:DeleteItem", "resource": "…"}],
  "access_unused": [],
  "regressions": {"previous_run": "20261115-095353-9629", "new_failures": [], "fixed": []},
  "trace": "/home/me/.lenslet/runs/20261115-095354-9655.jsonl"
}
```

Exit codes:

| code | meaning |
| --- | --- |
| the command's | the command failed |
| `3` | the command passed, but `--check` found access not in `lenslet.lock` |
| `2` | Lenslet could not run (usage, emulator CA, …) |
| `0` | otherwise |

The full format is in [docs/trace.md](docs/trace.md).

## Commands

```text
lenslet run [options] [--] COMMAND   run under Lenslet; report at the end
lenslet run [options]                no command: a proxy on 127.0.0.1:9297 until Ctrl-C
lenslet last [--json]                the newest run's report
lenslet policy [--lock PATH]         an IAM policy from lenslet.lock
lenslet ui                           the window (also: bare `lenslet`)

run options: --check  --update  --json PATH  --quiet  --ui  --no-env
             --listen PORT  --upstream ADDRESS  --emulator-ca PATH  --trace PATH
```

## Install

Release archives: Linux x86_64 (`lenslet`, `lenslet-ui`) and macOS arm64
(`lenslet`, `Lenslet.app`, ad-hoc signed and not notarized). Put `lenslet` on
your `PATH` and keep the window next to it. `lenslet run` needs no display or
graphics libraries, so it works in CI, containers and over SSH.

```bash
curl -fL -o lenslet.tar.gz \
  https://github.com/hxddh/celagent/releases/download/lenslet-v3.0.0/lenslet-3.0.0-linux-x86_64.tar.gz
tar xzf lenslet.tar.gz && cd lenslet-3.0.0-linux-x86_64
./lenslet run -- aws s3 ls
```

## Files

| path | what |
| --- | --- |
| `./lenslet.lock` | the access your project's runs need; commit it |
| `~/.lenslet/runs/*.jsonl` | one file per run: header, calls, report; the last 20 finished runs are kept |

Run files are a **local development artifact**. They hold raw headers and text
bodies, including `Authorization`, and are written owner-only (`0600`). Do not
commit them.

## Build from source

Rust 1.88+. For the window you also need the GPUI Linux libraries
(`libxkbcommon-dev`, `libxkbcommon-x11-dev`, `libwayland-dev`,
`libx11-xcb-dev`, `libxcb1-dev`, `libvulkan-dev`, `libfontconfig1-dev`,
`libfreetype-dev`, `pkg-config`, `cmake`, `clang`) and a Vulkan driver.

```bash
cargo build --release -p lenslet          # CLI only: no graphics libraries
cargo build --release -p lenslet-ui       # the window
cargo test --workspace
LENSLET_DEMO=1 ./target/release/lenslet-ui
```

```text
crates/cli   lenslet: proxy, error and access derivation, lock, reports
crates/ui    lenslet-ui: the GPUI window (reads run files; no IAM logic)
vendor/xattr Linux build fix for gpui 0.2.2's xattr dependency
```

## Limits

- HTTP/1.1 between SDK and proxy; loopback only; plain HTTP only.
- Text capture is the first 64 KiB per body; binary bodies keep size and
  SHA-256.
- Resources are read for S3, DynamoDB, SQS, SNS, Lambda, Secrets Manager, KMS,
  STS, IAM, EventBridge and Step Functions; other services get `*`. Operations
  that are only an HTTP method get no action.
- Access is what your tests exercised. Code paths they never reach are not in
  the lock.
- Windows is not shipped.

## History

Lenslet 2.0 is at
[lenslet-v2.0.0](https://github.com/hxddh/celagent/tree/lenslet-v2.0.0).
celagent, the agent runtime this repository used to hold, is at
[v4.1.0](https://github.com/hxddh/celagent/tree/v4.1.0).

## License

Apache-2.0. Bundled fonts are under the SIL Open Font License; see
`assets/fonts/`.
