# Lenslet run files (format 3)

Each `lenslet run` writes one JSONL file: `~/.lenslet/runs/YYYYMMDD-HHMMSS-PID.jsonl`,
or `--trace PATH` (an existing file there is replaced). Every line is an
object with a `type`:

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
| `cwd` | where `lenslet run` was started (the default lock is `cwd/lenslet.lock`) |
| `pid` | Lenslet's process id (to tell a live run from a crashed one) |
| `endpoint` | what SDKs point at, e.g. `http://127.0.0.1:37505` |
| `upstream` | the emulator, e.g. `https://127.0.0.1:9298` |
| `started` | RFC3339 |
| `lock` | the lock path (relative to `cwd` when inside it) when the run is compared with one |
| `policy` | the IAM policy JSON path when `--policy` is used |
| `enforce` | `true` when `--enforce` actively blocks unlocked calls |

## `call`

| field | meaning |
| --- | --- |
| `id` | unique call id; a later line with the same id replaces an earlier one |
| `at` | RFC3339 request-arrival time |
| `method`, `path`, `host` | as sent; `path` includes the query |
| `service` | SigV4 scope service (header or presigned URL); `s3` for SigV2; else `aws` |
| `region` | SigV4 scope region |
| `invocation_id` | SDK `amz-sdk-invocation-id`, shared across retries of one call |
| `attempt`, `max_attempts` | retry attempt (`> 1`) and configured max from `amz-sdk-request` |
| `operation` | from `x-amz-target`, `x-id`, the Query `Action=`, the S3/Lambda/Bedrock/API Gateway REST shape, or the HTTP method |
| `status`, `ms` | HTTP status (`0` while pending) and full exchange time |
| `authorization` | an Authorization header was present |
| `pending` | `true` on a line for a call still running after 1 s (carries full request headers and captured body) |
| `error` | proxy-side failure, e.g. the emulator was unreachable |
| `problem` | `{code, message?, hint?}` for status ≥ 400 or a proxy failure |
| `access` | `[{action, resource, new?, unverified?}]` this call needs: usually one; one per table/parameter for batches; each item's action for a DynamoDB transaction; the source read for an S3 copy |
| `implied` | `[{action, resource, new?, unverified?}]` needed elsewhere (SSE-KMS key, passed role) |
| `new` | at least one permission on this call is not in the lock/policy |
| `blocked` | `true` when `--enforce` denied the call at the proxy with an AWS `AccessDenied` reply |
| `blocked_phase` | `request` (the call never reached the emulator) or `response` (the emulator had already run it; only its response revealed the denied permission, e.g. the SSE-KMS key of an S3 object) |
| `replay_of` | the id of the recorded call this one re-sends (`x-lenslet-replay-of`) |
| `request`, `response` | `{headers, content_type?, body?}` |

`body` is `{text?, hex?, encoding?, bytes, truncated, binary, sha256}`. Headers,
query strings and bodies are preserved verbatim with **zero redaction**. `text`
holds up to 1 MiB by default (`--max-body` to change) of decoded text (`identity`,
`aws-chunked`, `application/vnd.amazon.eventstream`, `gzip` or `deflate`
unframed/decompressed for reading, including readable UTF-8 payloads without an
explicit text `content-type`; wire bytes are forwarded untouched). Binary payloads
record `hex` (first 512 bytes) alongside `bytes` and `sha256` over the whole wire
body.

### Access derivation

- **Action**: `{iam prefix}:{operation}`, plus exceptions. The SigV4 names
  `monitoring`, `email` and `bedrock-runtime` become `cloudwatch`, `ses` and
  `bedrock`. S3 `ListObjectsV2` becomes `s3:ListBucket`, `HeadObject` becomes
  `s3:GetObject`, multipart calls become `s3:PutObject`, Lambda `Invoke` becomes
  `lambda:InvokeFunction`, and Bedrock `Converse` becomes `bedrock:InvokeModel`.
  `sts:GetCallerIdentity` needs nothing. `TransactWriteItems` and
  `TransactGetItems` are not IAM actions: each item contributes its own
  (`PutItem`, `UpdateItem`, `DeleteItem`, `ConditionCheckItem`, `GetItem`).
  `CopyObject` and `UploadPartCopy` also need `s3:GetObject` on the source from
  `x-amz-copy-source`.
- **Resource**: read from the request for S3, DynamoDB (tables, indexes, and
  Streams `StreamArn`; one entry per table, since an IAM resource names one
  table), SQS, SNS, Lambda, SSM Parameter Store, CloudWatch Logs, Kinesis,
  Bedrock, Secrets Manager, KMS, STS, IAM, EventBridge (including per-entry
  `PutEvents` `EventBusName`, defaulting omitted entries to `default`), and Step
  Functions (`stateMachineArn`, `CreateStateMachine` `name`, and `executionArn`);
  otherwise `*`. Account and region are `*`, while `/` paths in secrets,
  parameters and IAM roles are preserved.
- **Normalization**: UUIDs, runs of six or more digits, and hex segments of six
  or more characters become `*`, so generated test names do not churn the lock.
- **Unreadable bodies**: when the body that names the resource cannot be read
  (binary such as CBOR, or longer than the 64 MiB inspection limit), the
  action stands on resource `*` with `unverified: true`. That line is stable
  and can be accepted into the lock; a policy grants it only with
  `Resource: "*"`, and any `Deny` on the action applies to it.
- **Implied permissions** are derived only from what is on the wire: SSE-KMS
  headers of an S3 request or response; `KmsKeyId` of Secrets Manager
  `CreateSecret`/`UpdateSecret`; `KeyId` of SSM `PutParameter`
  (`kms:Encrypt`, or `kms:GenerateDataKey` for the Advanced tier); the
  `KmsMasterKeyId` attribute of SQS/SNS queue and topic creation or update
  (`kms:GenerateDataKey`, what producers will need); DynamoDB
  `SSESpecification.KMSMasterKeyId`; Kinesis `StartStreamEncryption`. The
  key of an existing encrypted queue, topic or secret is not in the request
  that uses it and is not derived. Roles passed to Lambda, Step Functions,
  ECS, EventBridge (`PutRule`, `PutTargets` `Targets[].RoleArn`), Pipes,
  Scheduler (`Target.RoleArn`), Firehose (every `RoleARN`) and Glue
  (`Role`, `JobUpdate.Role`) need `iam:PassRole`.
- A call whose operation is only an HTTP method gets no action. Lenslet does
  not guess, and reports unmapped call counts in `unmapped`.

## `end`

The report: the same object `lenslet run --json` and `lenslet last --json`
print. `lenslet run --markdown PATH` renders it for PR comments and job
summaries.

| field | meaning |
| --- | --- |
| `run`, `command`, `exit_code`, `ended`, `duration_ms` | the run; `exit_code` is the program's (`null` for a proxy session) |
| `calls`, `retries`, `errors`, `blocked`, `unfinished`, `services`, `unmapped` | counts (`retries`, `blocked` and `unmapped` omitted when empty) |
| `failures` | `[{id, service, operation, status, code, message?, hint?, attempt?, max_attempts?, blocked?}]` |
| `access` | `[{action, resource, calls, errors, implied?, new?, via?}]`, sorted; `via` names the first calls that needed it, e.g. `DYNAMODB DeleteItem` |
| `lock`, `policy` | the lock file or IAM policy JSON compared/enforced with, or `null` |
| `access_added` | used by this run, not in the lock/policy |
| `access_unused` | in the lock, not used by this run |
| `regressions` | `{previous_run, previous_exit_code, previous_calls, previous_duration_ms, new_failures, fixed, new_access?}` compared with the previous run of the same command in the same directory, or `null` |
| `findings` | `[{kind, severity, title, detail?, calls}]`, most severe first, then in call order (omitted when empty); see below |
| `trace`, `endpoint`, `upstream` | where to look |

### Findings

| kind | severity | one finding per |
| --- | --- | --- |
| `failure` | `error` | distinct (service, operation, error code) that failed, with every call; an attempt a later retry recovered from is a `retries` finding instead |
| `access` | `warning` | permission not in the lock/policy |
| `n_plus_one` | `warning` | 5+ calls of one operation on one resource, each starting ≤100 ms after the previous ended, with different requests; names the batch API where one exists |
| `leaked` | `info` | resource a successful call created that no later call deleted (S3 objects are not tracked) |
| `retries` | `warning` if the invocation finally failed, else `info` | SDK invocation (`invocation_id`) with more than one attempt |

## Live events

While a run is going, the proxy also answers two requests of its own on the
run's `endpoint`. They never reach the emulator and are never recorded.

- `GET /_lenslet/health` → `200 {"run": "<run id>", "pid": <lenslet's pid>}`.
- `GET /_lenslet/events` → `200`, `content-type: text/event-stream`: every
  line already in the run file, in order (the `run` header first), then each
  new line as it is written, each as one event `data: <the exact JSONL
  line>\n\n`. No line is skipped or repeated. An idle stream gets a
  `: keepalive\n\n` comment every 15 s. The stream ends after the `end` line.

A request with `x-lenslet-replay-of: <call id>` is proxied and recorded like
any call, with `replay_of` set; the header is removed before forwarding and
is not recorded among the request headers.

## `lenslet check`

`lenslet check [--policy PATH | --lock PATH] [RUN] [--json] [--markdown PATH]`
derives each recorded call's access again from its recorded request and
evaluates it against the policy or lock (default `./lenslet.lock`; `RUN` as
for `lenslet diff`, default the newest finished run). A request body the run
file kept only in part (`--max-body`) is read as truncated. `--json` prints
`{run, gate, policy, calls, denied: [{id, service, operation, action,
resource, implied?, unverified?, allowed, by?, reason}]}`; `reason` is
`explicit_deny` (with `by`, the statement's `Sid` or `Statement[i]`) or
`no_allow`. Exit 3 when anything is denied, 2 on usage errors, else 0.

## `lenslet.lock`

```text
# AWS access observed by `lenslet run`: action, resource.
dynamodb:GetItem   arn:aws:dynamodb:*:*:table/orders
kms:Decrypt        arn:aws:kms:*:*:key/*  # implied
```

The file has one `action  resource` pair per line, sorted. Text after `#` is a
comment. A comment containing the word `test` marks a line that only the tests
need: `--check` still counts it, but `lenslet policy` leaves it out. A lock
grants exactly its lines: a `*` in a line (a generated name) is not a pattern
over other names. `lenslet run --update` writes the file, only when the
command succeeded; the window can add lines and toggle `test` marks. Every
write is atomic (a temporary file renamed over the lock, permissions kept)
and canonical: it keeps the `test` marks and the full-line comments at the
top of the file; other comments are not kept. `--lock PATH` uses another file.

## Retention

Lenslet keeps the newest 20 finished runs. A run without an `end` line whose
process is still alive is never deleted.
