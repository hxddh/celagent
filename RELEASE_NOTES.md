# Lenslet 3.0.0

2.0 told you what your code did against local AWS and why calls failed. 3.0
adds what it **needs**: the IAM access your tests exercised, kept in a file
you review like code.

```bash
lenslet run --update -- npm test    # write lenslet.lock, commit it
lenslet run --check -- npm test     # CI: exit 3 when a change widens AWS access
```

AWS Emulator does not evaluate IAM, so a call AWS would deny still succeeds
locally. 3.0 is aimed at that gap.

## New

**Access**
- Every call is mapped to an IAM action and resource ARN pattern. Implied
  permissions are included: `kms:Decrypt` / `kms:GenerateDataKey` behind
  SSE-KMS objects, and `iam:PassRole` for roles passed to Lambda, Step
  Functions and ECS.
- Generated names (UUIDs, timestamps, long hex) become `*`, so the lock stays
  stable.
- `lenslet.lock` holds one `action  resource` line per permission, sorted for
  diffing.
  - `--update` writes it, and only when the command succeeded.
  - `--check` exits `3` when the run used access the lock does not list.
- `lenslet policy` prints the lock as an IAM policy document, labelled as
  observed access.

**Regressions**
- Each run is compared with the previous run of the same command in the same
  directory: new failures and fixed ones.

**Agents**
- One report object, in three places: the run file's last line,
  `lenslet run --json`, and the new `lenslet last --json`.
- Exit codes are documented: the command's own code when it fails, `3` for
  access growth under `--check`, `2` for Lenslet errors.

**Window**
- New **Access** view (`a`): one row per permission, with call counts and
  `NEW` / `implied` markers. Click a row to filter the stream to its calls.
- Run switcher: `[` and `]`, or `‹ 2/7 ›` in the header. On an older run the
  view stays put; step back to the newest to follow live runs again.
- The header shows the run at a glance: command, exit code, `+N access`, new
  failures.
- Calls that widened access are marked `NEW ACCESS`; the inspector shows each
  call's access.

**Naming**
- Lambda REST operations are named from the path (`CreateFunction`, `Invoke`,
  …), so boto3 calls get real names.

## Simpler

Earlier formats are not supported, which allowed these cuts:

- One file per run, written as an event stream (`run` header, `call` lines,
  `end` report). The `.run.json` sidecar is gone.
- Every fact (error, IAM action, resource, new-vs-lock) is computed by the CLI
  at write time. The window only displays and groups what it reads, so the
  window and `--json` cannot disagree.
- `lenslet proxy` is now `lenslet run` with no command.
- The proxy listens on plain HTTP only. The TLS listener, certificate
  generation and two dependencies (`rcgen`, `tokio-rustls`) are removed. The
  proxy still connects to the emulator over TLS.
- 1.x/2.x trace readers, field aliases and the legacy trace path are removed.
- `demo.rs`, 492 lines of hand-written JSON, is replaced by a recorded run file.

## Fixes

- `lenslet run` now unsets `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` and
  `AWS_SESSION_TOKEN` when it injects the emulator's credentials. Before, keys
  in the environment took precedence, so a real access key id could be signed
  into requests and written to the local trace.

## Breaking

- The run file format is 3. 2.x traces do not load.
- `lenslet proxy` → `lenslet run`. `--cert` and `--key` are gone, and
  `https://` endpoints are no longer accepted.

## Verification

- `cargo test --workspace`: 58 CLI unit tests, 3 end-to-end tests of the real
  binary, and 30 window/model tests. The end-to-end tests cover the lock
  workflow, exit codes, `last` and `policy`.
- Real boto3 through `lenslet run` against a moto server standing in for AWS
  Emulator: S3 (with SSE-KMS), DynamoDB, SQS, KMS, IAM and Lambda.
  `--update` → `--check` caught a new `dynamodb:DeleteItem` with exit 3, and
  the implied `kms:*` and `iam:PassRole` were derived.
- The window was rendered headlessly (Xvfb + lavapipe). The Calls and Access
  screenshots are of real runs.
