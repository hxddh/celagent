# Lenslet 11.0.0

Lenslet 11.0 delivers zero-friction productization and one-click usability across
both the native Desktop App (`Lenslet.app` / `lenslet-ui`) and CLI (`lenslet`) —
eliminating external emulator setup barriers while maintaining Lenslet's strict
local-only boundary (`127.0.0.1` / `localhost` / local container bridge only,
zero cloud forwarding, zero telemetry).

```bash
# Desktop App + CLI (macOS & Linux):
curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh -s -- --app

# Run immediately with zero external emulator required:
lenslet pytest
lenslet init
```

## Highlights

- **Built-in Zero-Dependency Stateful Local AWS Sandbox (`--sandbox` & Auto-Fallback).**
  - When no external local emulator (`AWS Emulator`, `LocalStack`, `Moto`, `MinIO`)
    is listening on localhost—or when `--sandbox` / `upstream: sandbox` is set—Lenslet
    automatically serves stateful, protocol-accurate AWS responses in memory with
    zero external processes or Docker containers required.
  - Covers S3 (`CreateBucket`, `DeleteBucket`, `ListBuckets`, `HeadBucket`,
    `PutObject` with `aws-chunked` unframing & RFC 1321 MD5 `ETag`, `GetObject`,
    `HeadObject`, `CopyObject`, `DeleteObject`, `DeleteObjects`, `ListObjectsV2`,
    multipart uploads), DynamoDB (`CreateTable`, `DescribeTable`, `DeleteTable`,
    `ListTables`, `PutItem`, `GetItem`, `UpdateItem`, `DeleteItem`, `Query`,
    `Scan`, `BatchGetItem`, `BatchWriteItem`, `TransactWriteItems`,
    `TransactGetItems` across both JSON 1.0 and Smithy RPCv2 CBOR), SQS (JSON,
    CBOR, and Query XML with `MD5OfMessageBody`), SNS, STS (`GetCallerIdentity`,
    `AssumeRole*` with full session role attribution), IAM, Secrets Manager, SSM
    Parameter Store, KMS (`CreateKey`, `Encrypt`/`Decrypt` round-trip,
    `GenerateDataKey`), Lambda, CloudWatch Logs, EventBridge, CloudWatch, Kinesis,
    and Step Functions.
  - Preserves explicit `--upstream` error reporting (`502 EmulatorUnreachable`)
    whenever `--upstream` is explicitly passed, while seamlessly preferring live
    external emulators when running.

- **One-Click, Self-Explanatory Desktop Experience (`Lenslet.app` / `lenslet-ui`).**
  - **First-Launch Welcome & 1-Click Interactive Demo**: Launch straight into
    **Open Project Folder…** or **Explore Interactive Demo** (instant toggle in
    and out of rich sample AWS traces, failure diagnostics, and IAM permissions
    with no environment variables needed).
  - **1-Click Header `▶ <command>`**: Auto-detects the workspace test command
    (`pytest`, `npm`/`pnpm`/`yarn`/`bun`, `cargo test`, `go test ./...`, `make test`)
    and runs it in a single click from the header or sidebar with zero mandatory
    modal dialog; advanced network routing (`--upstream`, `--route`,
    `--allow-docker`) is tucked cleanly under a collapsible **Advanced options**
    section in `⌘N`.
  - **1-Click `Open Lenslet Terminal`**: Automatically starts the local proxy
    (with Built-in Sandbox fallback) and opens the native OS terminal
    (`Terminal.app` on macOS, desktop terminal emulators on Linux) pre-configured
    in the project directory with `AWS_ENDPOINT_URL` and local placeholder keys.
  - **Guided Next-Step Action Banner & Clear Labels**: Surfaces a natural-language
    action bar whenever unlocked AWS permissions are detected (**Save to
    `lenslet.lock`** / **Review & Export Policy →**), replaces cryptic `403` and
    `# test` chips with clear **Enforce Lock** / **Test-only** controls, and
    provides a clickable multi-shell **Copy Env ▾** menu.

- **Zero-Ceremony CLI & Installer Ergonomics.**
  - **Direct Command Shorthand (`lenslet <cmd>`)**: Run `lenslet pytest`,
    `lenslet npm test`, or `lenslet cargo test` directly without ceremony.
  - **Zero-Argument `lenslet init`**: Auto-detects your project's test command,
    runs against a live emulator or the Built-in AWS Sandbox, writes
    `lenslet.lock`, and generates a zero-config `.github/workflows/lenslet.yml`.
  - **`lenslet shell` & `lenslet doctor`**: Spawn an interactive Lenslet-wired
    subshell that prints a full access report on `exit`, or run `lenslet doctor`
    (`--json`) to inspect CLI, Desktop UI, local emulators, lock, and CI status.
  - **`install.sh --app`**: One-command installer for `Lenslet.app` on macOS
    (verifying `SHA256SUMS`, mounting the universal DMG, installing to
    `/Applications` or `~/Applications`, clearing `com.apple.quarantine`, and
    linking `lenslet` to `~/.local/bin`) and `lenslet-ui` + `lenslet` on Linux.
