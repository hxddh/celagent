# Lenslet 2.0.0

Lenslet 1.0 showed the AWS calls your code made, after you had started three
processes and exported four variables. 2.0 is one command, and it tells you why
a call failed.

```bash
lenslet run -- npm test
```

## What changed

**One command, no setup.**
- `lenslet run -- <command>` starts a proxy on a free loopback port, sets
  `AWS_ENDPOINT_URL` and the emulator's credentials/config for the child, runs
  it, and prints a summary. The exit code is the child's.
- SDKs talk plain HTTP to the proxy, so no certificate needs to be trusted. TLS
  still works on the same port for 1.x setups.
- `lenslet proxy` keeps a proxy running on `127.0.0.1:9297`.
- `lenslet ui` (or bare `lenslet`) opens the window.

**Why it failed.**
- Every failed call carries its AWS error code and message (JSON, XML, Query or
  header), plus a one-line likely cause for common local failures.
- The stream shows the error instead of the path, and the inspector opens with
  a "Why it failed" panel.

**Works without a window.**
- The summary goes to stderr; `--json PATH` (or `-`) writes it as JSON for CI
  and agents.
- The `lenslet` binary has no graphics dependencies.

**Better data.**
- Calls running for more than 1 s appear as pending, then resolve in place.
- S3 operations are named from the REST shape, so boto3 and the AWS CLI show
  `GetObject` instead of `GET`.
- Presigned URLs (SigV4 and SigV2) are attributed to their service.
- The SigV4 region is recorded.

**Window.**
- Status is real: the window probes the run's proxy and emulator, and shows
  when a run ended and with which exit code.
- It follows the newest run automatically.
- JSON and XML bodies are formatted (Copy stays raw).
- Text is larger, and the window opens at 1280×800.

## Fixes from the 1.0 review

- The footer said "proxy :9297 watching" even when nothing was running. Ports
  and status are no longer hard-coded.
- Traces with large bodies showed as few as 10 calls on load. Reload now reads
  backwards until it has the last 500.
- Multi-byte characters split across two reads were corrupted. Lines are now
  decoded only once complete.
- Any `PUT` ending in `/` was named `CreateBucket`, in every service.
- `x-id` matched as a substring of other query parameters.
- The truncation note reported the full body size as "captured".
- The proxy certificate now also covers `localhost` and `::1`.
- The 502 trace keeps the cause of the failure ("connection refused"), not just
  "client error (Connect)".
- Traces no longer grow forever: one file per session, newest 20 kept.
- Accent borders were drawn in the wrong color (an `rgba`/`rgb` mix-up).
- The 1.0 Linux archive was built on a glibc 2.43 runner and did not start on
  Ubuntu 24.04 or older. 2.0 builds Linux on Ubuntu 22.04.

## Repository

The celagent 4.x agent runtime is removed from `main`. It remains at tag
[v4.1.0](https://github.com/hxddh/celagent/tree/v4.1.0). Lenslet now lives at
the repository root (`crates/cli`, `crates/ui`).

## Compatibility

- The trace format is additive: new fields are `region`, `pending` and
  `problem`. Traces from 1.0 still load.
- `lenslet-proxy` is now `lenslet proxy`, with the same options. The default
  trace moves from `~/.lenslet/trace.jsonl` to `~/.lenslet/runs/`. The window
  shows the old file until the first run exists.
- The app binary is now `lenslet-ui`.

## Verification

- `cargo test --workspace`: 49 CLI tests (including an end-to-end `lenslet run`)
  and 30 window/model tests.
- Real SDKs through `lenslet run` over plain HTTP: boto3 (S3, SQS, DynamoDB,
  IAM, CloudWatch, a presigned GET), AWS SDK for JavaScript v3 (S3, SQS,
  DynamoDB), and AWS CLI v1. Also checked: the HTTPS path with
  `NODE_EXTRA_CA_CERTS`, `lenslet proxy`, an unreachable emulator, and
  exit-code pass-through. The upstream was a moto server standing in for AWS
  Emulator.
- The window was rendered headlessly (Xvfb + lavapipe) with demo data and with
  live runs.
