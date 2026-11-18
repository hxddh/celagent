# Lenslet 5.0.0

Local AWS emulators do not enforce IAM. Until 4.x, Lenslet observed what your
code did and told you after the run finished. **5.0 closes the loop: Lenslet can
now enforce `lenslet.lock` or a real IAM policy live on loopback, while
eliminating payload blind spots and bringing a precision Light theme and
selective Chinese UI to the desktop app — with zero new subcommands or config
files.**

```bash
curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh
lenslet init -- npm test
```

## Live local IAM gate (`--enforce` & `--policy`)

- **Real-time `AccessDenied` at the proxy (`lenslet run --enforce`)**: calls
  outside `lenslet.lock` (including implied SSE-KMS `kms:GenerateDataKey` /
  `kms:Decrypt` and `iam:PassRole` permissions) are intercepted before an
  unauthorized write reaches the emulator, and answered with protocol-accurate
  AWS wire errors (`403 <Error><Code>AccessDenied</Code>` on S3/Query XML,
  `400`/`403 AccessDeniedException` on AWS JSON and REST-JSON). Your tests can
  now exercise least-privilege boundaries and `AccessDenied` fallback paths
  locally against Moto, AWS Emulator or LocalStack.
- **IAM policy JSON verification (`--policy PATH`)**: check (`--check`) or
  actively enforce (`--enforce`) runs directly against a standard AWS IAM policy
  document (`Allow`, explicit `Deny`, `NotAction`, `NotResource`, `*`/`?`
  wildcards, and concrete account/region ARNs).
- **Run-to-run access & failure diff**: run comparisons now track newly
  exercised permissions alongside new and fixed failures across consecutive runs,
  even before a lock file is created.

## Zero-redaction, zero-blind-spot observability

- **No redaction, full wire fidelity**: headers (`Authorization`,
  `x-amz-security-token`, cookies), presigned query strings and bodies are kept
  100% unredacted, while `child_env` now also strips ambient OIDC, ECS/EKS
  container credential URIs and Bedrock bearer tokens (`AWS_WEB_IDENTITY_TOKEN_FILE`,
  `AWS_ROLE_ARN`, `AWS_CONTAINER_CREDENTIALS_*`, `AWS_BEARER_TOKEN_BEDROCK`) so
  real cloud credentials never enter local runs.
- **`aws-chunked` streaming & untyped UTF-8 payloads**: modern AWS SDK v3 /
  `boto3` streaming S3 uploads (`Content-Encoding: aws-chunked` /
  `STREAMING-*`) are unframed for inspection while raw wire bytes still stream
  untouched; readable UTF-8 payloads sent without a text `Content-Type`
  (`binary/octet-stream` or omitted) now appear as text instead of opaque binary.
- **1 MiB default capture (`--max-body`), raw/formatted toggle & Hex dump**:
  default body capture grows 16× from 64 KiB to **1 MiB** (`--max-body SIZE` or
  `unlimited` to customize), truncated DynamoDB batches/transactions salvage
  table permissions instead of dropping them, binary payloads record an
  unredacted 512-byte **Hex + ASCII** preview, and `pending` hung calls now
  expose their full request headers and captured body live while still in flight.
- **One-key `Copy curl` (`c`)**: copy any call from the window as a ready-to-run,
  unredacted `curl` command with exact headers and body.

## Desktop window: Light theme, Chinese UI & fixes

- **System / Light / Dark themes (`t`)**: a contrast-calibrated Light theme
  (WCAG AA ≥ 4.5:1 across text and service palettes) alongside Dark mode and
  live OS appearance following (`~/.lenslet/ui.json`).
- **Selective Chinese UI (`l`) + CJK font fallbacks & IME fix**: toggle desktop
  UI chrome, filter chips and guidance between English and Chinese while keeping
  AWS, IAM and HTTP parameters (`Service`, `Operation`, `Method`, `Headers`,
  `Action`, `Resource`, `SigV4`, `lenslet.lock`, CLI output) in canonical
  English. Explicit cross-platform CJK font fallbacks (`PingFang SC`, `Noto Sans
  CJK SC`, `Microsoft YaHei`) and a fixed UTF-16 preedit cursor calculation make
  Chinese search input and CJK payload inspection seamless.
- **Loopback, IPv6 & container run-file fixes**: bracketed IPv6 `Host: [::1]`
  headers no longer split on colons, `Authority` URIs fall back cleanly when
  `Host` is absent, Linux run pruning uses `/proc` instead of spawning `kill`
  subprocesses, and same-second `PID=1` container runs never collide or sort out
  of order.
