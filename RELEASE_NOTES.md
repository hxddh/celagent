# Lenslet 6.0.0

Lenslet 6.0 redesigns the desktop application into a focused **3-Zone Workbench**
with a dedicated **Settings (`⌘,`)** center, upgrades the local IAM policy gate
with wire-level **`Condition`** evaluation and deeper AWS resource derivation,
and adds first-class **`lenslet runs`** and **`lenslet diff`** commands for
inspecting and comparing runs from the terminal or CI.

```bash
curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh
lenslet init -- npm test
```

## Redesigned 3-Zone Desktop Workbench (`lenslet ui` / `Lenslet.app`)

- **Clean two-bar top chrome (`82px` total)**: replaces seven stacked horizontal
  bars and duplicated counters with a `44px` Global Header (Run Switcher
  popover, `Calls | Access | Diff` segmented mode switch, `⌘,` Settings button,
  and `⌘B` Inspector toggle) plus a `38px` mode-linked contextual sub-toolbar
  and non-blocking floating toast notifications.
- **Payload-first `Exchange` inspector**: selecting a call opens **`Exchange`**
  by default—showing the root-cause failure or `BLOCKED` card alongside both
  **Request Body** and **Response Body** in one scrollable view with zero extra
  tab clicks, followed by structured key-value **`Headers`** (click any row to
  copy) and interactive **`IAM & Meta`** tabs.
- **Combinable filters & searchable `Access` workbench**: combine status chips
  (`All`, `Errors`, `Blocked`, `New IAM`, `Pending`) with service facets (`S3`,
  `DynamoDB`, `KMS`, …) and IAM permission filters (`ESC` clears one layer at a
  time). In **`Access`** mode, search and filter permissions (`All`, `New`,
  `Implied`, `Unused`, `# test`) while the right-hand **Permission Inspector**
  shows the exact IAM Statement JSON, `lenslet.lock` line, one-click
  `Export Policy`, and every triggering call.
- **Visual Run-to-Run `Diff` mode (`3` / `d`)**: compare the current run against
  any recorded base run directly in the window—surfacing new and fixed failures,
  added and removed IAM permissions, and per-service call deltas.
- **10× stream scale (`5,000` calls) with disk offloading & resizable layout**:
  stream capacity rises from `500` to `5,000` calls per run while request and
  response bodies over `8 KiB` are offloaded via file byte offsets and hydrated
  on demand when inspected. Drag the splitter (`320px–780px`), collapse the
  inspector with `⌘B`, and keep window geometry across launches.

## Dedicated Settings Modal (`⌘,` / `⚙`) & Theme Accents

- **Centralized preferences (`~/.lenslet/ui.json`)**: appearance, language,
  formatting, storage, and shortcut reference now live in a dedicated Settings
  sheet instead of crowding the main workspace or firing on bare single-letter
  `t` / `l` keypresses.
- **6 contrast-calibrated accent colors**: choose `Sky`, `Indigo`, `Violet`,
  `Emerald`, `Amber`, or `Rose`—each separately calibrated for Dark and Light
  themes to preserve WCAG AA contrast across text, selection, and badges.
- **Display & storage controls**: switch between **System / Light / Dark**
  appearance, **Comfortable / Compact** row density, **English / 简体中文**
  interface labels (keeping AWS, IAM, and HTTP technical terms canonical),
  **Relative (`+ms`) / Wall Clock** timestamps, **Formatted / Raw** default body
  rendering, **Word Wrap**, and one-click **Clear Finished Runs** storage
  cleanup.

## Wire-Level IAM `Condition` Evaluation & Expanded Resource Mapping

- **IAM `Condition` blocks in `--policy` & `--enforce`**: evaluates
  `StringEquals` / `StringNotEquals` / `StringEqualsIgnoreCase` /
  `StringNotEqualsIgnoreCase`, `StringLike` / `StringNotLike`, `ArnEquals` /
  `ArnNotEquals` / `ArnLike` / `ArnNotLike`, `Numeric*`, `Bool`, `Null`, set
  prefixes (`ForAnyValue:`, `ForAllValues:`), and `...IfExists` against
  unambiguous wire context keys (`aws:RequestedRegion`, `s3:prefix`,
  `s3:delimiter`, `s3:max-keys`, `s3:versionid`, `s3:x-amz-*`). IAM policy
  variables (`${...}`), unsupported operators, and cloud-only runtime keys (such
  as `aws:PrincipalOrgID`) fail closed at policy load time.
- **Finer AWS resource ARNs**: derives per-entry bus ARNs on EventBridge
  `PutEvents` (`Entries[].EventBusName`, defaulting omitted entries to
  `arn:aws:events:*:*:event-bus/default`), Step Functions `executionArn` and
  `CreateStateMachine` `name` ARNs, and DynamoDB Streams `StreamArn` table/stream
  ARNs.

## `lenslet runs` & `lenslet diff` CLI Subcommands

- **`lenslet runs [--json] [--limit N]`**: list recorded runs newest-first with
  live/exit status, call/error/blocked counts, duration, and trace path.
- **`lenslet diff [BASE] [TARGET] [--json] [--markdown PATH]`**: compare any two
  runs by run ID, prefix, `latest` / `prev`, or file path—emitting terminal
  summaries, machine-readable JSON, or PR-ready Markdown.
