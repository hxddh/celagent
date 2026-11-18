# Lenslet 4.1.0

4.0 put the AWS access diff in every pull request. 4.1 sharpens observability
across retries, compressed payloads, timelines and unmapped calls, aligns the
window with `--strict` and `# test`, and removes first-run friction — with zero
new commands or configuration files.

```bash
curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh
lenslet init -- npm test
```

## Observability

- **SDK retries in plain sight**: Lenslet reads `amz-sdk-invocation-id` and
  `amz-sdk-request` (`attempt=N; max=M`). Retried calls show `(N retries)` in
  summaries, `(attempt 2/3)` on failures, a `RETRY 2/3` badge in the stream,
  and combined invocation wall time in the inspector. Search `retry` to filter
  to retried calls.
- **Relative timeline & idle-gap detection**: each row in the window shows its
  offset from the start of the run (`+142 ms`) and highlights idle gaps ≥ 500 ms
  (`Δ 1.2s`) in amber so stalls and backoff waits stand out immediately.
- **Compressed bodies decoded for inspection**: `gzip` and `deflate` JSON, XML
  and form bodies are decompressed up to the 64 KiB capture bound while raw
  compressed bytes still pass through untouched on the wire. Error codes,
  Query `Action=` fields, IAM resources and inspector previews now work on
  compressed traffic.
- **Unmapped calls are explicit**: calls whose operation could not be mapped to
  an IAM action are never guessed, and are now tallied in `unmapped` and noted
  in CLI and PR reports so blind spots never hide silently.
- **Window parity with `--strict` and `# test`**: the `Access` tab now displays
  `UNUSED` lock entries, `# test` marks from `lenslet.lock`, and true total
  call counts on runs larger than the 500-call live tail.

## Broader AWS coverage & accuracy

- **Per-permission `new` attribution**: each entry in `call.access` and
  `call.implied` carries its own `new` flag. Multi-table batches, S3 copies and
  SSE-KMS implied permissions now badge only the specific permission missing
  from `lenslet.lock` — both live and in the inspector.
- **More services and REST shapes**: resource and operation derivation now
  covers **SSM Parameter Store** (including multi-parameter `Names` batches and
  `Path` hierarchies), **CloudWatch Logs**, **Kinesis**, **Bedrock / Bedrock
  Runtime** (`InvokeModel`, `Converse`, streaming), **API Gateway**
  (`execute-api:Invoke`) and **AppConfig Data**, plus implied KMS keys across
  Secrets Manager, SSM, SQS and SNS.
- **Hierarchical ARN paths preserved**: Secrets Manager ARNs (`db/password-*`)
  and IAM role/user paths (`service-role/worker`) keep their `/` prefixes, and
  4–5 character hex-like segments (`app-f100`, `ab12`) are no longer mistaken
  for generated IDs.

## Zero-friction setup & CI

- **Emulator auto-detection in `lenslet init`**: `init` fingerprints AWS
  Emulator, Moto and LocalStack on `--upstream` and pre-fills `emulator:` in
  `.github/workflows/lenslet.yml`.
- **Dual-stack loopback**: the proxy binds IPv4 and best-effort IPv6 loopback,
  and `reachable()` honors IPv6 `localhost`.
- **Inline PR file annotations & bounded comments**: the GitHub Action emits
  file/line annotations on `lenslet.lock` for added and (under `strict`) unused
  permissions — visible even on read-only fork PRs — and folds large permission
  diffs inside `<details>` so PR comments stay well within GitHub's size limit.
- **Single-descriptor trace writer**: `TraceSink` keeps one open append handle
  per run rather than reopening the JSONL file on every call.
