# Lenslet 8.0.0

Lenslet 8.0 is a rebuild around one idea: every fact is computed once, in one
place, and every surface shows the same thing. The CLI, the PR comment, the
JSON report and the window now share one core. The window was rewritten to
be instant and to read in a straight line.

```bash
curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh
lenslet init -- npm test
```

## New

- **Findings.** Each run ends with one ranked list of what is worth a look:
  - failures, with their cause;
  - permissions not in the lock;
  - serial bursts a batch API would collapse (eight `GetItem` in a row →
    `BatchGetItem`);
  - SDK retries;
  - resources the run created and left behind.

  The terminal, the PR comment, `report.findings` and the window all lead
  with this list. Leftover test fixtures take a single line, not one line each.
- **`lenslet check --policy role.json [RUN]`.** It re-evaluates a recorded run
  against a real IAM policy, or a lock, without re-running your tests. For
  each denied permission it names the statement that denies it, or says that
  nothing allows it. Repeats of the same permission are grouped. Exit code 3
  means something was denied; `--json` and `--markdown` are available for CI.
- **Live events.** A running proxy streams its run file at
  `GET <endpoint>/_lenslet/events` (server-sent events: the backlog first,
  then each new line, ending after the report). `GET /_lenslet/health` says
  which run it is. The window uses this stream, and so can agents.
- **Replay through the proxy.** Replaying a call from the window now sends it
  through the live proxy. The replay is recorded with `replay_of`, checked
  against the lock or policy, and kept out of retries and findings.

## The window

- **Two views, Calls and Access**, plus **Compared with ▾**: `lenslet.lock`,
  the previous run, or any recorded run. The separate Diff page is gone;
  every view shows what changed against the chosen baseline.
- **Findings at the top of Calls.** Click one to jump to its calls.
- **One body view**: Decoded or Raw. Bodies of any size are virtualized line
  by line, so the old Clip/Full toggle is gone. Lines are selectable and
  copyable.
- **Instant.** Nothing reads the disk while drawing. Loading, search,
  decoding and the lock and baseline comparisons run in the background, and
  a live run updates by push instead of polling. Measured on a 5,000-call run:
  - rows ready in 44 ms;
  - a 10-call append in 0.3 ms;
  - search in 0.3 ms;
  - median 1 ms to build a frame.
- **Native.**
  - Right-click menus on calls and permissions, tooltips with shortcuts,
    anchored popovers, a splitter that cannot stick.
  - Context-aware shortcuts: `⌘⏎` does not replay while you type, and every
    `⌘` is `Ctrl` on Linux.
  - `j`/`k`, PageUp/PageDown/Home/End, `Esc` to back out.
  - Normal window decorations on Linux; the unified title bar stays on macOS.
- **Leaner Settings**: appearance, accent, density, language and run storage.
- **Lock edits from the window** use the same atomic writer as
  `lenslet run --update`.

## Fixed

- **Policy evaluation is fail-closed.** A `Deny` now applies to a call whose
  resource could not be derived. An `Allow` for one resource no longer grants
  an unknown one.
- **The lock is exact.** A lock line with a `*` (from a generated name) no
  longer matches other names.
- **IAM action names**:
  - SQS/SNS batches need the single action (`sqs:SendMessage`, not
    `sqs:SendMessageBatch`; `sns:Publish`);
  - S3 calls with `versionId` need the `*Version` actions;
  - `GetObjectAttributes`, S3 bucket configuration deletes, and the
    analytics, inventory, metrics and tiering configurations are mapped;
  - DynamoDB PartiQL maps to `dynamodb:PartiQL*` on the parsed table.
- **Implied permissions** now read nested request fields:
  - KMS for SQS/SNS `Attributes.KmsMasterKeyId` and DynamoDB
    `SSESpecification`, plus SSM `PutParameter` keys;
  - `iam:PassRole` for EventBridge `Targets[].RoleArn`, Scheduler
    `Target.RoleArn`, every Firehose `RoleARN` and Glue `JobUpdate.Role`.

  Claims Lenslet could not back are gone. The key of an existing encrypted
  queue, topic or secret is not in the request that uses it, so it is not
  derived.
- **Unreadable request bodies** (binary such as CBOR, or over the inspection
  limit) give the action on `*`, marked `unverified`. Before, they gave a
  permission that could never be accepted, so `--check` failed forever.
- **`--enforce`** checks request-visible KMS keys before the call is
  forwarded. Only an S3 key revealed by the response is checked after the
  call; the trace marks it `blocked_phase: "response"`.
- **Shutdown and files.**
  - A call that finishes after the run ends is no longer written after the
    report.
  - `--trace PATH` replaces the file instead of appending to it.
  - Lock writes are atomic, keep file permissions and keep the comment block
    at the top.
  - `--ui` opens this run, not whichever is newest.
- **Proxy.**
  - Hop-by-hop headers are recorded but not forwarded.
  - A blocked upload's body is drained, so the SDK sees the `AccessDenied`
    instead of a reset.
  - Trailers are kept, and reading request headers times out.
  - Each call's access is derived once. The trace lock now covers only the
    write.
  - Bodies are buffered for inspection only when their access depends on the
    body.

## Upgrading

- The fixed IAM action names and the exact lock matching can change what
  `--check` reports against an existing lock. Run
  `lenslet run --update -- <tests>` once, review the diff, and commit it.
- Hand-written wildcard lines in `lenslet.lock` now grant only that literal
  resource. Use `--policy` with an IAM policy document for real patterns.
- Code that reads run files gets new optional fields: `findings` in the
  report; `replay_of`, `blocked_phase` and `unverified` on calls. The format
  version stays 3, and readers ignore unknown fields.
