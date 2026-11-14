// tests/contract.test.mjs — 发布合同（只锚定跨文件必须一致的事实）
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";

const root = new URL("..", import.meta.url).pathname;
const read = (p) => readFileSync(join(root, p), "utf8");

test("v4.0.0 版本三处一致", () => {
  const pkg = JSON.parse(read("package.json"));
  assert.equal(pkg.version, "4.0.0");
  assert.match(read("bin/celagent-tui.mjs"), /CELAGENT_VERSION = "4\.0\.0"/);
  assert.match(read("install.sh"), /VERSION="4\.0\.0"/);
});

test("Pi 精确钉 1.5.2（不吃 caret）", () => {
  const pkg = JSON.parse(read("package.json"));
  for (const p of ["pi-coding-agent", "pi-agent-core", "pi-ai"]) {
    assert.equal(pkg.dependencies[`@earendil-works/${p}`], "1.5.2");
  }
});

test("celld 随包 v0.6.3，且不再传 CELLD_STORAGE_PROBE", () => {
  assert.match(read("scripts/prepare-release-assets.sh"), /CELLD_UPSTREAM_TAG:-v0\.6\.3/);
  assert.match(read("install.sh"), /CELLD_BUNDLED_VERSION="0\.6\.3"/);
  for (const f of ["bin/celagent-tui.mjs", "setup.sh", "install.sh", "scripts/node_mgr.sh", "scripts/cluster_mgr.sh"]) {
    assert.doesNotMatch(read(f), /CELLD_STORAGE_PROBE/, `${f} 不得再传已移除的 CELLD_STORAGE_PROBE`);
  }
});

test("health 路径使用 celld 0.6 的新地址；不再自动清 own.json", () => {
  for (const f of ["bin/celagent-tui.mjs", "setup.sh", "install.sh", "scripts/node_mgr.sh", "scripts/cluster_mgr.sh"]) {
    assert.doesNotMatch(read(f), /\/__celld\/health/, `${f} 仍用旧 health 路径`);
  }
  assert.match(read("bin/celagent-tui.mjs"), /\.well-known\/celld\/health/);
  assert.doesNotMatch(read("bin/celagent-tui.mjs"), /delete-object.*own\.json/s);
  assert.doesNotMatch(read("setup.sh"), /delete-object.*own\.json/s);
});

test("v2 模块与 CLI 命令存在", () => {
  for (const f of ["src/v2/session.js", "src/v2/manifest.js", "src/v2/lease.js", "src/v2/outbox.js", "src/v2/journal.js", "src/v2/workspace.js", "src/v2/workspace-cas.js", "src/v2/supervisor.js"]) {
    assert.equal(existsSync(join(root, f)), true, f);
  }
  const help = read("bin/celagent-tui.mjs");
  for (const cmd of ["run", "status", "import-v0", "workspace", "up", "attach", "down", "ps"]) {
    assert.match(help, new RegExp(`celagent ${cmd}`), cmd);
  }
});

test("v3 委派模块与命令存在", () => {
  for (const f of ["src/v3/index.js", "src/v3/child.js", "src/v3/worktree.js", "src/v3/delegate.js"]) {
    assert.equal(existsSync(join(root, f)), true, f);
  }
  const bin = read("bin/celagent-tui.mjs");
  for (const cmd of ["celagent delegate spawn", "celagent delegate list", "celagent delegate wait", "celagent delegate send", "celagent ps"]) {
    assert.match(bin, new RegExp(cmd.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")), cmd);
  }
  assert.match(bin, /__child/);
  assert.match(bin, /ps --tree|--tree/);
});

test("worker 不再暴露模拟任务 CLI", () => {
  assert.doesNotMatch(read("bin/celagent-tui.mjs"), /taskCommand|task submit/);
});

test("v4 goal 模块与命令存在", () => {
  for (const f of ["src/v4/index.js", "src/v4/goal.js", "src/v4/extension.js"]) {
    assert.equal(existsSync(join(root, f)), true, f);
  }
  const bin = read("bin/celagent-tui.mjs");
  for (const cmd of ["celagent goal start", "celagent goal status", "celagent goal answer", "celagent goal stop"]) {
    assert.match(bin, new RegExp(cmd.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")), cmd);
  }
  assert.match(bin, /--goal/);
});
