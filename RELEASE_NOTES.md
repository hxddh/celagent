# Lenslet 9.0.0

Lenslet 9.0 turns Lenslet from a passive HTTP/1.1 trace viewer into an active,
protocol-complete local AWS flight recorder, interactive SigV4 workbench, and
multi-format IaC policy compiler — while preserving strict local-only safety
boundaries (`127.0.0.1` / `localhost` / local Docker bridge only, zero cloud
forwarding, zero payload redaction).

```bash
curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh
lenslet init -- npm test
```

## Highlights

- **Universal AWS Wire Coverage (HTTP/2 `h2c`, Smithy RPCv2 CBOR & `zstd`).**
  - Dual-stack HTTP/1.1 and HTTP/2 (`h2c`) proxy listener on the same port, plus
    HTTP/2 upstream negotiation.
  - Native RFC 8949 CBOR decoder (`application/cbor`, `application/x-amz-cbor-1.1`,
    `smithy-protocol: rpc-v2-cbor`) and `zstd` decompression, so modern AWS SDK
    v2/v3 calls (CloudWatch, DynamoDB, Kinesis, Smithy RPCv2) decode into
    human-readable JSON and derive exact resource ARNs instead of falling back
    to `unverified: *`.
- **Multi-Upstream Local Routing & Container Support.**
  - Route individual AWS services to dedicated local emulators with repeatable
    `--route SERVICE=ADDRESS` (e.g. `--route s3=http://127.0.0.1:9000 --route dynamodb=http://127.0.0.1:8000`)
    and automatic standalone discovery for MinIO (`:9000`), DynamoDB Local (`:8000`),
    and ElasticMQ (`:9324`) alongside AWS Emulator (`:9298`), LocalStack (`:4566`),
    and Moto (`:5000`).
  - `--allow-docker` binds `0.0.0.0` while enforcing a strict local/container-bridge
    IP filter and refusing non-local upstreams (`*.amazonaws.com` or public IPs).
  - `lenslet env [--shell sh|fish|pwsh|dotenv|docker]` prints copy-ready SDK
    environment configuration for any shell or container.
- **Interactive Desktop Workbench (`lenslet ui` / `Lenslet.app`).**
  - **Workspace Sidebar (`⌘0`)**: live local proxy status (`/_lenslet/health`),
    detected emulators, 1-click **Start / Stop Proxy**, **Copy Env**, **Open
    Trace File (`⌘O`)**, and project-grouped run history with `★` pin protection.
  - **Semantic Call Stream**: extracts human-meaningful targets (`table/orders · id="ord-42"`,
    `s3://bucket/key`, `queue/jobs`, `fn/worker`) on every row (including Compact
    density), plus clickable column sorting, SDK retry-chain grouping (`↻ ×N`),
    dual-scale waterfall bars, and timeline brush-drag range selection.
  - **Request Composer & Local SigV4 Re-signer (`⇧⌘⏎`)**: edit HTTP method, path,
    headers, and JSON/XML/Form body, automatically re-sign `AWS4-HMAC-SHA256`
    locally, dispatch to your emulator, and inspect **Replay** alongside a
    syntax-highlighted **Unified Diff** against the recorded response.
  - **Structured Headers & Payload Viewer**: SigV4 credential-scope card, decoded
    query parameters, character-accurate text selection (double-click to copy an
    ARN or token), collapsible multi-line JSON folding (`▾`/`▸`), search match
    stepping (`⌘G` / `⇧⌘G`), crisp embedded SVG icons, and disabled contextual
    ligatures so S3 ARNs (`arn:aws:s3:::bucket`) never render with `:::` ligature
    artifacts.
- **Role-Aware Access, Policy Simulator & Multi-Format IaC Compiler.**
  - Tracks `sts:AssumeRole*` session credentials across a run to attribute calls
    and permissions per assumed role (`caller_role` on calls, `roles` in reports)
    and supports `--role ROLE` filtering in `lenslet policy`.
  - Expanded REST URI & ARN catalog covering 35+ AWS services (ECS, EKS,
    CloudFormation, Glue, Athena, RDS, API Gateway v1/v2, CloudFront, Route 53,
    SES/SESv2, SageMaker, OpenSearch, EventBridge Pipes, AWS Backup, and more).
  - Synthesizes least-privilege `iam:PassedToService` and `kms:ViaService`
    conditions automatically and compiles directly to **IAM JSON**, **Terraform
    (`aws_iam_policy_document`)**, **CloudFormation**, **AWS SAM**, and **AWS
    CDK TypeScript** from both `lenslet policy --format ...` and the Access UI.
  - Wire-level ABAC condition extraction (`aws:RequestTag/*`, `aws:TagKeys`,
    `dynamodb:LeadingKeys`, `kms:EncryptionContext:*`), explicit `needs_context`
    diagnostics with `--context KEY=VAL` overrides in `lenslet check`, and
    interactive **Check policy…** simulation directly inside the Access view.

## Fixed

- Streaming `BufReader` trace hydration when inspecting calls in large multi-MB
  run files (eliminating full-file `fs::read_to_string` memory spikes).
- Cumulative live-session permission retention in `CallStore` so runs exceeding
  5,000 live calls never drop early permissions from the Access workbench.
- Preserved `# implied` annotations when accepting permissions from the desktop
  UI into `lenslet.lock`.
- Eliminated search placeholder/cursor overlap against the shortcut badge and
  localized 100% of remaining UI strings in `中文` mode.
- Workspace-aware and pin-aware run retention (`~/.lenslet/runs/pinned.json`),
  plus resilient retry logic in `scripts/package-macos.sh` for macOS `.dmg`
  packaging.
