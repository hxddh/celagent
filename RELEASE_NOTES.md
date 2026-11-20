# Lenslet 10.0.0

Lenslet 10.0 transforms Lenslet into a self-contained, end-to-end native desktop
cloud workbench with zero-loss wire observability and full Apple HIG / Linux /
Windows platform conformance — while maintaining Lenslet's strict local-only
boundary (`127.0.0.1` / `localhost` / local container bridge only, zero cloud
forwarding, zero telemetry).

```bash
curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh
lenslet init -- npm test
```

## Highlights

- **End-to-End In-App Workflow (Zero Terminal Required).**
  - **Run Launcher (`⌘N` / `Ctrl+N`)**: Pick any workspace folder (with recent
    workspace switcher), auto-detect project test presets (`npm`/`pnpm`/`yarn`/`bun`,
    `pytest`/`uv`, `cargo test`, `go test`, `make test`), configure local
    emulator upstreams, per-service `--route` mappings, `--allow-docker`, and
    choose between **Record**, **Update Lock (`--update`)**, **Check Lock
    (`--check --strict`)**, and **Enforce Live (`--enforce`)**.
  - **Live Output & Run Telemetry Console (`⌘3`)**: `lenslet run --log <PATH>`
    tees child `stdout` and `stderr` into an owner-only (`0600`) companion log
    streamed live inside the desktop workbench alongside 100% of `RunHeader`
    and `Report` telemetry.
  - **Local Emulator Lifecycle & Multi-Shell Snippets**: Automatically probes
    installed local emulators (`aws-emulator`, `moto_server`, `localstack`,
    `minio`) for 1-click startup from the Sidebar or Run Launcher, and copies
    `AWS_ENDPOINT_URL` snippets in 5 formats (`sh/zsh`, `fish`, `PowerShell`,
    `.env`, `docker`).
  - **Scratch AWS Request Composer (`⇧⌘N`)**: Craft and locally SigV4-sign new
    AWS requests from scratch using 1-click templates (S3, DynamoDB, SQS, STS),
    multi-header `;` overrides, and `-Header` removals without needing a prior
    recorded call.
  - **Full `lenslet.lock` Lifecycle & 1-Click CI Init**: Remove individual
    permissions or bulk-prune unused permissions directly from `lenslet.lock`,
    bind a workspace folder to create `lenslet.lock` from proxy sessions, and
    generate `.github/workflows/lenslet.yml` in one click.

- **Zero-Loss Wire & Protocol Observability.**
  - **Lossless Binary & Encoded Payloads**: Preserves exact wire frames inline
    via `raw_b64` (`≤ 32 KiB`) and streams larger payloads (`> 32 KiB`, up to
    64 MiB+ by default with bounded RAM) into content-addressed owner-only
    sidecar blobs (`~/.lenslet/runs/blobs/<sha256>.bin`), enabling lossless
    `Raw` wire inspection, binary/CBOR/`gzip`/`zstd`/`aws-chunked` replay, and
    1-click **Save Payload… (`⌥⌘S`)** export.
  - **HTTP/2 Trailers, `aws-chunked` Checksums & Complete EventStream Headers**:
    Captures HTTP/2 `trailers` and `aws-chunked` trailing checksum headers
    (e.g. `x-amz-checksum-crc32`), preserves full `Content-Type` parameters,
    and decodes all 10 `aws-eventstream` frame header types plus binary
    `payload_base64`.
  - **Transport, Routing & Latency Breakdown**: Records and displays client and
    upstream HTTP protocol versions (`HTTP/1.1 → HTTP/2`), peer socket address,
    resolved upstream URL, `TTFB` vs body-transfer timing, full SigV4 scope and
    signature headers, replay response headers, and explicit `+N earlier · Load
    all` / corrupt-line indicators so zero calls or bytes are silently hidden.

- **Native Platform Conformance (macOS Apple HIG, Linux & Windows).**
  - **Apple HIG Menu Bar & Native Selectors**: Standard 7-menu layout (`Lenslet`,
    `File`, `Edit`, `View`, `Run`, `Window`, `Help`) with `Services`, `Hide` /
    `Hide Others` / `Show All`, `OsAction::{Undo, Redo, Cut, Copy, Paste,
    SelectAll}` (enabling `⌘C`/`⌘V`/`⌘Z`/`⌘A` inside both GPUI fields and
    native `NSOpenPanel`/`NSSavePanel` dialogs), `NSApp.windowsMenu` management,
    Spotlight Menu Search, Dock context menu, and live `En`/`中文` localization.
  - **Desktop OS Integration**: Supports Dock click reopen after `⌘W`
    (`on_reopen`), Finder `.jsonl` file association & `on_open_urls`, external
    file/folder/policy drag-and-drop, titlebar drag & double-click zoom, login
    shell `PATH` hydration on macOS, `AppleLanguages` locale detection, and
    bundled `dev.lenslet.app.desktop` + icon on Linux.

## Fixed

- Cleanly terminates managed proxies, active command runs, and local emulators
  via `on_app_quit` and `SIGTERM` forwarding so `⌘Q` never leaves orphan
  processes and stopped runs still seal their `end` report.
- Fixed an index-shift bug in `CallStore::ingest` when out-of-order calls and
  in-place `pending → finished` updates arrived in the same batch, and tracked
  `Delta::replaced_ids` across `reindexed` batches so Inspector never freezes on
  `pending`.
- Preserved authoritative empty `report.findings` on finished runs, tracked
  exact `max_end_ms` across >256-call runs, updated Call row `NEW ACCESS` badges
  live on `lenslet.lock` edits, fixed the Sidebar folder SVG asset path, added
  full `Undo`/`Redo` + global shortcut passthrough in text inputs, and pruned
  companion `.log` and orphan `.bin` blobs during storage cleanup.
