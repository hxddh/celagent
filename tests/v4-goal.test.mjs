// tests/v4-goal.test.mjs — Goal Loop 的验收、守卫、保护路径
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, rmSync, readFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  DEFAULT_PROTECT, changedPaths, listGoals, nextAction, protectedChanges,
  readGoal, renderPrompt, runAccept, writeGoal, writeGoalDoc, goalDocPath,
} from "../src/v4/index.js";

function future(hours = 2) { return new Date(Date.now() + hours * 3600 * 1000).toISOString(); }
function goal(over = {}) {
  return {
    id: "goal-test", objective: "把测试修绿", accept: "npm test", attempts: 1, maxAttempts: 8,
    deadline: future(), budgetUsd: 5, protect: [...DEFAULT_PROTECT], failureStreak: 0,
    lastCheck: null, ...over,
  };
}
function check(over = {}) {
  return { exitCode: 1, output: "1 failing", hash: "h1", timedOut: false, durationMs: 10, ...over };
}

test("runAccept: exit code / 输出 / 超时", async () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-v4-check-"));
  try {
    const ok = await runAccept({ cwd: dir, command: "echo hello" });
    assert.equal(ok.exitCode, 0);
    assert.match(ok.output, /hello/);
    assert.equal(ok.timedOut, false);

    const bad = await runAccept({ cwd: dir, command: "echo boom >&2; exit 3" });
    assert.equal(bad.exitCode, 3);
    assert.match(bad.output, /boom/);

    const slow = await runAccept({ cwd: dir, command: "sleep 3", timeoutMs: 1000 });
    assert.equal(slow.timedOut, true);
    assert.notEqual(slow.exitCode, 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("nextAction: pass / attempts / deadline / budget", () => {
  assert.equal(nextAction({ goal: goal(), check: check({ exitCode: 0 }) }).action, "done");

  const maxed = nextAction({ goal: goal({ attempts: 8 }), check: check() });
  assert.equal(maxed.action, "stopped");
  assert.equal(maxed.reason, "max-attempts");

  const late = nextAction({ goal: goal({ deadline: new Date(Date.now() - 1000).toISOString() }), check: check() });
  assert.equal(late.action, "stopped");
  assert.equal(late.reason, "deadline");

  const broke = nextAction({ goal: goal(), check: check(), cost: 6 });
  assert.equal(broke.action, "stopped");
  assert.equal(broke.reason, "budget");
});

test("nextAction: 同一失败 ×3 才 blocked；不同失败重新计数", () => {
  const a = nextAction({ goal: goal({ lastCheck: { hash: "h1" }, failureStreak: 1 }), check: check({ hash: "h1" }) });
  assert.equal(a.action, "prompt");
  assert.equal(a.streak, 2);

  const b = nextAction({ goal: goal({ lastCheck: { hash: "h1" }, failureStreak: 2 }), check: check({ hash: "h1" }) });
  assert.equal(b.action, "blocked");
  assert.equal(b.reason, "same-failure");

  const c = nextAction({ goal: goal({ lastCheck: { hash: "h1" }, failureStreak: 2 }), check: check({ hash: "h2" }) });
  assert.equal(c.action, "prompt");
  assert.equal(c.streak, 1);
});

test("nextAction: 验收通过但改了保护路径 → blocked", () => {
  const r = nextAction({ goal: goal(), check: check({ exitCode: 0 }), protectedChanged: ["tests/a.test.js"] });
  assert.equal(r.action, "blocked");
  assert.equal(r.reason, "protected");
  assert.deepEqual(r.paths, ["tests/a.test.js"]);
});

test("protectedChanges: 只命中 tests/test/*.test.*", () => {
  const root = mkdtempSync(join(tmpdir(), "celagent-v4-git-"));
  try {
    const git = (...a) => execFileSync("git", ["-C", root, ...a], { encoding: "utf8" }).trim();
    git("init", "-q");
    git("config", "user.name", "t");
    git("config", "user.email", "t@example.com");
    mkdirSync(join(root, "tests"), { recursive: true });
    mkdirSync(join(root, "src"), { recursive: true });
    writeFileSync(join(root, "tests/a.test.js"), "old\n");
    writeFileSync(join(root, "src/a.js"), "old\n");
    writeFileSync(join(root, "README.md"), "old\n");
    git("add", "-A");
    git("commit", "-q", "-m", "init");
    const base = git("rev-parse", "HEAD");

    writeFileSync(join(root, "tests/a.test.js"), "new\n");
    writeFileSync(join(root, "src/a.js"), "new\n");
    writeFileSync(join(root, "README.md"), "new\n");
    const hit = protectedChanges({ cwd: root, base });
    assert.deepEqual(hit.sort(), ["tests/a.test.js"]);

    const all = changedPaths({ cwd: root, base }).sort();
    assert.deepEqual(all, ["README.md", "src/a.js", "tests/a.test.js"]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("state: goal.json / GOAL.md / listGoals", () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-v4-state-"));
  try {
    const g = goal({ id: "goal-abc", worktree: "/tmp/wt", createdAt: new Date().toISOString() });
    writeGoal(dir, g.id, g);
    writeGoalDoc(dir, g.id, g);
    assert.equal(readGoal(dir, g.id).objective, "把测试修绿");
    assert.match(readFileSync(goalDocPath(dir, g.id), "utf8"), /npm test/);
    assert.match(readFileSync(goalDocPath(dir, g.id), "utf8"), /tests\/\*\*/);
    assert.equal(listGoals(dir).length, 1);
    rmSync(join(dir, "goals", g.id), { recursive: true, force: true });
    assert.equal(listGoals(dir).length, 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("renderPrompt: 带目标、验收、失败输出", () => {
  const text = renderPrompt({ goal: goal(), check: check({ output: "AssertionError: expected 1" }), attempt: 3 });
  assert.match(text, /把测试修绿/);
  assert.match(text, /npm test/);
  assert.match(text, /AssertionError/);
  assert.match(text, /第 3 \/ 8 次/);
  const answered = renderPrompt({ goal: goal({ lastAnswer: "用 mock 替换外部 API" }), check: check(), attempt: 2 });
  assert.match(answered, /人对上一次 blocked 的回答/);
  assert.match(answered, /用 mock 替换外部 API/);
});
