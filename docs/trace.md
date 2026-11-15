# Lenslet run files (format 3)

Each `lenslet run` writes one JSONL file: `~/.lenslet/runs/YYYYMMDD-HHMMSS-PID.jsonl`,
or `--trace PATH`. Every line is an object with a `type`:

```text
{"type":"run", ...}      first line: what the run is
{"type":"call", ...}     one per proxied call (a slow call may also get an earlier pending line)
{"type":"end", ...}      last line, once the run ends: the report
```

Lenslet computes every fact once, when it writes the line: the error, the
IAM action, whether the access is new. The window and `--json` only read
those facts, so they always agree. Readers ignore unknown fields. Format 3 is
not compatible with earlier files.

## `run`

| field | meaning |
| --- | --- |
| `version` | `3` |
| `run` | the file stem, e.g. `20261115-095354-9655` |
| `command` | argv of the program; empty for a bare proxy session |
| `cwd` | where `lenslet run` was started (the lock is `cwd/lenslet.lock`) |
| `pid` | Lenslet's process id (to tell a live run from a crashed one) |
| `endpoint` | what SDKs point at, e.g. `http://127.0.0.1:37505` |
| `upstream` | the emulator, e.g. `https://127.0.0.1:9298` |
| `started` | RFC3339 |
| `lock` | `lenslet.lock` when the run is compared with one |

## `call`

| field | meaning |
| --- | --- |
| `id` | unique call id; a later line with the same id replaces an earlier one |
| `at` | RFC3339 request-arrival time |
| `method`, `path`, `host` | as sent; `path` includes the query |
| `service` | SigV4 scope service (header or presigned URL); `s3` for SigV2; else `aws` |
| `region` | SigV4 scope region |
| `operation` | from `x-amz-target`, `x-id`, the Query `Action=`, the S3 or Lambda REST shape, or the HTTP method |
| `status`, `ms` | HTTP status (`0` while pending) and full exchange time |
| `authorization` | an Authorization header was present |
| `pending` | `true` on a line for a call still running after 1 s |
| `error` | proxy-side failure, e.g. the emulator was unreachable |
| `problem` | `{code, message?, hint?}` for status ≥ 400 or a proxy failure |
| `action`, `resource` | the IAM action and resource ARN pattern this call needs |
| `implied` | `[{action, resource}]` needed elsewhere (SSE-KMS key, passed role) |
| `new` | the call's access is not in the lock |
| `request`, `response` | `{headers, content_type?, body?}` |

`body` is `{text?, bytes, truncated, binary, sha256}`. Headers are raw and
duplicate-preserving, with no redaction. `text` holds the first 64 KiB of a text
body and is absent for binary or content-encoded bodies. `bytes` and `sha256`
cover the whole body.

### Access derivation

- **Action**: `{iam prefix}:{operation}`, plus exceptions. The SigV4 names
  `monitoring` and `email` become `cloudwatch` and `ses`. S3 `ListObjectsV2`
  becomes `s3:ListBucket`, `HeadObject` becomes `s3:GetObject`, multipart calls
  become `s3:PutObject`, and Lambda `Invoke` becomes `lambda:InvokeFunction`.
  `sts:GetCallerIdentity` needs nothing.
- **Resource**: read from the request for S3, DynamoDB (tables and indexes,
  batch and transact), SQS, SNS, Lambda, Secrets Manager, KMS, STS, IAM,
  EventBridge and Step Functions; otherwise `*`. Account and region are `*`.
- **Normalization**: UUIDs, runs of six or more digits, and long hex segments
  become `*`, so generated test names do not churn the lock.
- A call whose operation is only an HTTP method gets no action. Lenslet does
  not guess.

## `end`

The report: the same object `lenslet run --json` and `lenslet last --json`
print.

| field | meaning |
| --- | --- |
| `run`, `command`, `exit_code`, `ended`, `duration_ms` | the run; `exit_code` is the program's (`null` for a proxy session) |
| `calls`, `errors`, `unfinished`, `services` | counts |
| `failures` | `[{id, service, operation, status, code, message?, hint?}]` |
| `access` | `[{action, resource, calls, errors, implied?, new?}]`, sorted |
| `lock` | the lock file compared with, or `null` |
| `access_added` | used by this run, not in the lock |
| `access_unused` | in the lock, not used by this run |
| `regressions` | `{previous_run, previous_exit_code, new_failures, fixed}` compared with the previous run of the same command in the same directory, or `null` |
| `trace`, `endpoint`, `upstream` | where to look |

## `lenslet.lock`

```text
# AWS access observed by `lenslet run`: action, resource.
dynamodb:GetItem   arn:aws:dynamodb:*:*:table/orders
kms:Decrypt        arn:aws:kms:*:*:key/*  # implied
```

The file has one `action  resource` pair per line, sorted. Text after `#` is a
comment. Only `lenslet run --update` writes it, and only when the command
succeeded.

## Retention

Lenslet keeps the newest 20 finished runs. A run without an `end` line whose
process is still alive is never deleted.
