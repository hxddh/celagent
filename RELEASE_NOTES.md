# Lenslet 7.0.0

Lenslet 7.0 delivers a full-stack leap across **zero-config ease of use**,
**temporal & causal AWS observability**, **production-exact IAM policy fidelity**,
and **native desktop craftsmanship**—without adding a single configuration file
or mandatory CLI flag.

```bash
curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh
lenslet init -- npm test
```

## Zero-Config Ease of Use & In-App `lenslet.lock` Curation

- **Automatic emulator discovery & `AWS_ENDPOINT_URL` interposition**: when
  `--upstream` is omitted, `lenslet run` and `lenslet init` automatically inherit
  an existing loopback `AWS_ENDPOINT_URL` from your shell (transparently placing
  Lenslet in front of `.env` or Docker Compose setups), or probe LocalStack
  (`:4566`) and Moto (`:5000`) with protocol signature verification when `:9298`
  is inactive.
- **One-click `lenslet.lock` curation inside the window**: accept individual new
  permissions (`Accept into Lock`), accept all newly observed permissions at once
  (`Accept +N into Lock`), or toggle `# test` marks directly from the **Access**
  view—atomically updating `lenslet.lock` on disk without re-running your test
  suite.
- **In-memory Live Replay (`Replay` / `⌘⏎`)**: re-send any captured call against
  the active loopback emulator directly from the Inspector and compare status,
  latency, and response payloads side-by-side without polluting recorded `.jsonl`
  run files.

## Temporal, Causal & Semantic AWS Observability

- **Inline Waterfall spans & interactive `24px` Run Timeline Mini-Map**: the
  stream's `WATERFALL / MS` column visualizes proportional `[start .. end]`
  execution spans to expose N+1 serial staircases vs concurrent bursts at a
  glance, paired with a `24px` Run Timeline strip above the table that plots
  concurrency, errors, blocked calls, and retries—click any bucket to slice by
  time (`ESC` resets).
- **Stateful cross-call root-cause correlation**: missing-resource errors are
  correlated against earlier calls in the same run—pinpointing **region
  mismatches** (*"`table/orders` was created in `us-west-2` at call #2, but this
  `GetItem` targeted `us-east-1`"* ) and **prior deletions** (*"`s3://receipts`
  was deleted earlier at call #3"* ) in both terminal output and the window.
- **Clickable SDK Retry Chains**: calls sharing `amz-sdk-invocation-id` render
  an interactive retry timeline (`#1 503 ── +112ms ──▶ #2 200`) in the Inspector
  to jump between attempts and inspect backoff delays.
- **AWS Semantic Decoder & `vnd.amazon.eventstream` unframing**:
  - Unframes binary `application/vnd.amazon.eventstream` streams (including
    Bedrock `ConverseStream` and base64-wrapped `InvokeModelWithResponseStream`
    chunks, S3 `SelectObjectContent`, and Lambda response streams) into readable
    JSON events while forwarding raw wire bytes untouched;
  - Unwraps DynamoDB typed `AttributeValue` maps (`{"S": "...", "N": "42"}` →
    clean native JSON), decodes `x-www-form-urlencoded` AWS Query requests with
    indented nested JSON, and unwraps embedded JSON string fields (`MessageBody`,
    `Detail`, `PolicyDocument`, `input`, `output`), with instant switching
    between `AWS Decoded`, `Wire JSON`, and `Raw`.

## Production-Exact AWS IAM Fidelity

- **100% canonical S3 subresource IAM action names**: maps `PostObject` →
  `s3:PutObject`, `SelectObjectContent` → `s3:GetObject`, `?publicAccessBlock` →
  `s3:Get/PutBucketPublicAccessBlock`, `?notification` →
  `s3:Get/PutBucketNotification`, `?replication` →
  `s3:Get/PutReplicationConfiguration`, `?object-lock` →
  `s3:Get/PutBucketObjectLockConfiguration`, `?accelerate` →
  `s3:Get/PutAccelerateConfiguration`, and S3 `DELETE` subresources to exact AWS
  IAM actions.
- **Enterprise implied permissions & `Condition` keys**: derives implied
  `iam:PassRole` across Lambda, Step Functions, ECS (`taskRoleArn` +
  `executionRoleArn`), EventBridge, Scheduler, Pipes, Firehose, and Glue, plus
  implied KMS permissions across S3, Secrets Manager, SSM, SQS, SNS, DynamoDB,
  and Kinesis; evaluates `iam:PassedToService` and `kms:ViaService` in
  `--policy` / `--enforce`, and derives resource ARNs for Cognito IDP, ECR,
  Firehose, EventBridge Scheduler, and RDS Data API.

## Native Desktop Craftsmanship

- **macOS Unified Transparent Titlebar**: fuses native window traffic lights
  directly into the `44px` Global Header with drag-to-move window regions,
  eliminating the extra system titlebar.
- **GPU-accelerated syntax & search-match highlighting**: JSON, XML, Query form,
  and Hex payloads render with Dark/Light-calibrated syntax colors, line-number
  gutters, and live search-match highlighting + match counters across bodies and
  headers.
- **`⌘K` Command Palette, multi-layer elevation shadows & full OS menus**: launch
  any view, service filter, historical run, lock action, replay, or appearance
  preference from the keyboard via `⌘K` (`⌘F` / `/` focuses search), backed by
  multi-layer `BoxShadow` depth and complete `Lenslet / Edit / View / Run`
  platform menus.
