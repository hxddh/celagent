# Lenslet 4.0.0

3.0 could work out the AWS access your tests need. 4.0 puts that in the pull
request, where the decision is made. It also makes the path from "never heard
of it" to "enforced in CI" one install line and one command.

```bash
curl -fsSL https://github.com/hxddh/lenslet/releases/latest/download/install.sh | sh
lenslet init -- npm test     # lock + CI workflow, then commit both
```

## In the pull request

- **GitHub Action** (`uses: hxddh/lenslet@lenslet-v4.0.0`).
  - Installs the pinned Lenslet and checks its checksum.
  - Can start your emulator and waits until it answers.
  - Runs your tests with `--check` and writes the report to the job summary.
  - Keeps one PR comment up to date. Fork PRs get the job summary only.
  - Until a lock exists, it only reports.
- **Explained diffs**: each permission records the calls that needed it
  (`via`). The PR comment reads "+ `dynamodb:DeleteItem` on `table/orders`,
  needed by DYNAMODB DeleteItem", not a bare line in a lock file.
- **`--markdown PATH`** renders the report for comments and summaries. The
  Markdown comes from the CLI, so the action stays a thin wrapper.

## First run

- **`lenslet init -- COMMAND`**:
  - checks the emulator answers, and says how to start it when it does not;
  - writes `.github/workflows/lenslet.yml`, and leaves an existing one alone
    unless you pass `--force`;
  - runs your tests once with `--update`.
- **`install.sh`**: one line, any Linux (static binary) or macOS (universal),
  checked against `SHA256SUMS`, installed into `~/.local/bin`.
- **`examples/python`**: a small project. Lenslet's CI runs the action against
  it with moto twice: unchanged access must pass, and added access must fail
  with exit code 3.

## The lock as a contract

- Reports now list **unused** access: lines in the lock that the run did not
  exercise. `--check --strict` fails on them, so over-grants can't build up
  unnoticed.
- **`# test` marks** label lines only the tests need, such as fixtures that
  create tables or buckets. `--check` still counts them, `lenslet policy`
  leaves them out, and `--update` keeps them.
- **`--lock PATH`** points at another lock file, for monorepos.

## Distribution

- **macOS**: one universal (Apple silicon and Intel) `Lenslet-X.Y.Z.dmg`. The
  CLI is inside the app at `Contents/MacOS/lenslet`. A CLI-only universal
  tarball is also published.
- **Linux**: x86_64 and aarch64 builds, each available as a CLI + window
  archive (glibc ≥ 2.35) or as a **static musl CLI** for any distro, Alpine
  and CI image.
- One `SHA256SUMS` covers every asset. The release is marked latest, so
  `install.sh` and the `releases/latest` links resolve to it.
- CI runs the same packaging scripts as the release, on every pull request.

## Fixes (planned for 3.0.1, shipped here)

- **Real credentials could reach the trace.** When the emulator had no
  credentials file, 3.0 left `AWS_ACCESS_KEY_ID` and friends in the child's
  environment. A real access key id was then signed into requests and written
  to `~/.lenslet/runs`. Now `lenslet run` always removes `AWS_ACCESS_KEY_ID`,
  `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` and `AWS_PROFILE`. It uses the
  emulator's credentials file or, if there is none, placeholder keys, so the
  SDK can't fall back to `~/.aws/credentials`. `--no-env` still opts out.

## Breaking

- The repository is `hxddh/lenslet` (formerly `hxddh/celagent`; GitHub
  redirects the old URLs).
- macOS assets are now a `.dmg` and a CLI tarball, not a tarball holding the
  app. Inside the app, the window binary is `lenslet-ui`.
- Linux asset names include the architecture first:
  `lenslet-4.0.0-x86_64-linux.tar.gz`.

## Not yet

- **Notarization.** The app is ad-hoc signed. On first open, use System
  Settings → Privacy & Security → Open Anyway. Notarizing needs an Apple
  Developer ID.
- Homebrew, Windows, other CI systems.

## Verification

- `cargo test --workspace`: 62 CLI unit tests, 6 end-to-end tests of the real
  binary and 30 window/model tests. The end-to-end tests cover `init`,
  `--strict`, `--markdown`, `--lock`, `# test` marks, and real keys never
  reaching the trace.
- The action's steps were run locally against moto: unchanged access exits 0;
  a new `dynamodb:DeleteItem` exits 3 and is explained in the summary; the
  emulator wait succeeds when the emulator is up and fails after 60 s when it
  is not.
- `install.sh` was tested against a local mirror: a good download installs, a
  checksum mismatch is refused, and an unknown version fails.
- Packaging scripts: the Linux musl CLI was built and checked as a static
  binary. The universal macOS build, the dmg and the aarch64 Linux build run in
  CI.
