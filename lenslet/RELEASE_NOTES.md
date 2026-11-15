# Lenslet 1.0.0

Lenslet is a small native lens over your local AWS. It sits between an AWS SDK
and AWS Emulator and shows the actual wire calls in one GPUI window.

## First stable release

- Transparent TLS proxy from `127.0.0.1:9297` to AWS Emulator `127.0.0.1:9298`,
  preserving Host and SigV4 so signatures remain valid.
- Live request stream with search, error/service filters, keyboard selection,
  pause/resume and newest-call following.
- Inspector with Overview / Request / Response tabs, raw headers and text
  bodies, copy actions, binary size/SHA-256 metadata and truncation markers.
- Streaming, byte-exact body forwarding; bounded 64 KiB text capture per side;
  trace written owner-only (`0600`), with no redaction by design.
- Generic service awareness from the SigV4 scope; dynamic service chips for any
  service the emulator routes. Verified through the real proxy with S3, SQS, IAM
  and CloudWatch.
- Operation naming from `x-amz-target`, S3 `x-id`, AWS Query `Action=...`, with
  a method/path fallback for other protocols.
- Polished single-window GPUI UI with bundled Inter / JetBrains Mono fonts,
  responsive compact layout, empty/error states and a seeded demo mode
  (`LENSLET_DEMO=1`).
- Linux x86_64 release binaries: `lenslet` and `lenslet-proxy`.

## Install

```bash
curl -fL -o lenslet.tar.gz \
  https://github.com/hxddh/celagent/releases/download/lenslet-v1.0.0/lenslet-1.0.0-linux-x86_64.tar.gz
tar xzf lenslet.tar.gz
cd lenslet-1.0.0-linux-x86_64
```

Run AWS Emulator on `9298`, `lenslet-proxy` on `9297`, then `lenslet`. See
[README.md](README.md) for SDK and CLI setup.

## Limits

- Linux x86_64 is the verified build; macOS/Windows are not shipped in 1.0.
- AWS Emulator fidelity varies by service; Lenslet reports wire behavior, not
  AWS equivalence.
- Text capture is a 64 KiB prefix; binary bodies keep size and SHA-256 only.
- The trace contains raw credentials when the SDK sends them. Keep it local.

## Verification

- `cargo test --workspace`: 23 UI/model tests + 28 proxy tests.
- Real AWS Emulator 1.0.385 + Node AWS SDK through the TLS proxy:
  S3 CreateBucket/PutObject/GetObject, SQS CreateQueue/SendMessage, IAM
  ListUsers, CloudWatch PutMetricData — all 200.
- Release build on Linux x86_64; headless GPUI render inspected.
