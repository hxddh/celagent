// tests/v4-runner.test.mjs — goal runner 循环（注入 check/attempt，不启进程）
import { test } from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  newGoalId, readGoal, runGoal, writeAnswer, writeGoal, writeStop,
} from "../src/v4/index.js";
import { stopPath } from "../src/v2/supervisor.js";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const at = () => new Date().toISOString();
const pass = () => ({ exitCode: 0, output: "ok", hash: "ok", timedOut: false, durationMs: 1, at: at() });
const fail = () => ({ exitCode: 1, output: "1 failing", hash: "h1", timedOut: false, durationMs: 1, at: at() });

function setup(over = {}) {
  const dir = mkdtempSync(join(tmpdir(), "celagent-v4-runner-"));
  const id = newGoalId();
  writeGoal(dir, id, {
    id, objective: "smoke", accept: "true", status: "running", phase: "starting",
    attempts: 0, checks: 0, cost: 0, tokens: 0,
    maxAttempts: 8, budgetUsd: 5, deadline: new Date(Date.now() + 3600_000).toISOString(),
    protect: [], allowProtected: false, failureStreak: 0,
    worktree: dir, cwd: dir, baseCommit: null, sessionId: id, ...over,
  });
  return { dir, id };
}
function attemptResult(over = {}) {
  return { exitCode: 0, text: "", error: null, stopped: false, reason: null, ...over };
}

test("runner: 终态 goal 补写 supervisor stop，避免 watchdog 空转", async () => {
  const { dir, id } = setup({ status: "done" });
  try {
    await runGoal({
      configDir: dir, goalId: id, cwd: dir,
      runCheck: async () => pass(),
      runAttempt: async () => attemptResult(),
      getStats: async () => ({ cost: 0, tokens: 0 }),
    });
    assert.equal(existsSync(stopPath(dir, id)), true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("runner: baseline 已通过 → done，不跑 attempt", async () => {
  const { dir, id } = setup();
  const attempts = [];
  try {
    await runGoal({
      configDir: dir, goalId: id, cwd: dir,
      runCheck: async () => pass(),
      runAttempt: async (a) => { attempts.push(a); return attemptResult(); },
      getStats: async () => ({ cost: 0, tokens: 0 }),
    });
    assert.equal(readGoal(dir, id).status, "done");
    assert.equal(attempts.length, 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("runner: 失败自动重试，同一失败 ×3 → blocked；STOP 退出", async () => {
  const { dir, id } = setup();
  let checks = 0;
  let attempts = 0;
  const p = runGoal({
    configDir: dir, goalId: id, cwd: dir,
    runCheck: async () => { checks += 1; return fail(); },
    runAttempt: async () => { attempts += 1; return attemptResult(); },
    getStats: async () => ({ cost: 0, tokens: 0 }),
    sleep: () => sleep(5),
  });
  for (let i = 0; i < 100 && readGoal(dir, id)?.status !== "blocked"; i++) await sleep(10);
  const blocked = readGoal(dir, id);
  assert.equal(blocked.status, "blocked");
  assert.equal(blocked.blockedReason, "same-failure");
  assert.equal(attempts, 3);
  assert.equal(checks, 4);
  writeStop(dir, id, "test");
  await p;
  assert.equal(readGoal(dir, id).status, "stopped");
  rmSync(dir, { recursive: true, force: true });
});

test("runner: 验收期间收到 STOP → 不拉起 attempt", async () => {
  const { dir, id } = setup();
  let attempts = 0;
  try {
    await runGoal({
      configDir: dir, goalId: id, cwd: dir,
      runCheck: async () => { writeStop(dir, id, "test"); return fail(); },
      runAttempt: async () => { attempts += 1; return attemptResult(); },
      getStats: async () => ({ cost: 0, tokens: 0 }),
    });
    const g = readGoal(dir, id);
    assert.equal(g.status, "stopped");
    assert.equal(g.stoppedReason, "manual");
    assert.equal(attempts, 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("runner: 失败在变化时不会误判 same-failure", async () => {
  const { dir, id } = setup({ maxAttempts: 4 });
  let checks = 0;
  try {
    await runGoal({
      configDir: dir, goalId: id, cwd: dir,
      runCheck: async () => { checks += 1; return { ...fail(), hash: `h${checks}` }; },
      runAttempt: async () => attemptResult(),
      getStats: async () => ({ cost: 0, tokens: 0 }),
    });
    const g = readGoal(dir, id);
    assert.equal(g.status, "stopped");
    assert.equal(g.stoppedReason, "max-attempts");
    assert.equal(g.attempts, 4);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("runner: blocked 回答后继续，验收通过 → done", async () => {
  const { dir, id } = setup({ status: "blocked", phase: "blocked", blockedReason: "same-failure", blockedQuestion: "继续？", attempts: 2 });
  const p = runGoal({
    configDir: dir, goalId: id, cwd: dir,
    runCheck: async () => pass(),
    runAttempt: async () => attemptResult(),
    getStats: async () => ({ cost: 0, tokens: 0 }),
    sleep: () => sleep(5),
  });
  await sleep(20);
  writeAnswer(dir, id, "继续", { allowProtected: false });
  await p;
  const g = readGoal(dir, id);
  assert.equal(g.status, "done");
  assert.ok(g.answeredAt);
  rmSync(dir, { recursive: true, force: true });
});

test("runner: 预算/墙钟在 attempt 前硬停", async () => {
  const a = setup();
  try {
    await runGoal({
      configDir: a.dir, goalId: a.id, cwd: a.dir,
      runCheck: async () => pass(),
      runAttempt: async () => attemptResult(),
      getStats: async () => ({ cost: 6, tokens: 1 }),
    });
    assert.equal(readGoal(a.dir, a.id).status, "stopped");
    assert.equal(readGoal(a.dir, a.id).stoppedReason, "budget");
  } finally { rmSync(a.dir, { recursive: true, force: true }); }

  const b = setup({ deadline: new Date(Date.now() - 1000).toISOString() });
  try {
    await runGoal({
      configDir: b.dir, goalId: b.id, cwd: b.dir,
      runCheck: async () => pass(),
      runAttempt: async () => attemptResult(),
      getStats: async () => ({ cost: 0, tokens: 0 }),
    });
    assert.equal(readGoal(b.dir, b.id).status, "stopped");
    assert.equal(readGoal(b.dir, b.id).stoppedReason, "deadline");
  } finally { rmSync(b.dir, { recursive: true, force: true }); }
});

test("runner: attempt 被 deadline/budget 杀掉 → stopped", async () => {
  const { dir, id } = setup();
  try {
    await runGoal({
      configDir: dir, goalId: id, cwd: dir,
      runCheck: async () => fail(),
      runAttempt: async () => attemptResult({ stopped: true, reason: "deadline" }),
      getStats: async () => ({ cost: 0, tokens: 0 }),
    });
    const g = readGoal(dir, id);
    assert.equal(g.status, "stopped");
    assert.equal(g.stoppedReason, "deadline");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("runner: agent 输出 BLOCKED → 停下等人", async () => {
  const { dir, id } = setup();
  try {
    const p = runGoal({
      configDir: dir, goalId: id, cwd: dir,
      runCheck: async () => fail(),
      runAttempt: async () => attemptResult({ text: "做完一部分\nBLOCKED: 需要 API key" }),
      getStats: async () => ({ cost: 0, tokens: 0 }),
      sleep: () => sleep(5),
    });
    for (let i = 0; i < 100 && readGoal(dir, id)?.status !== "blocked"; i++) await sleep(10);
    const g = readGoal(dir, id);
    assert.equal(g.status, "blocked");
    assert.equal(g.blockedReason, "agent-question");
    assert.match(g.blockedQuestion, /API key/);
    writeStop(dir, id, "test");
    await p;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
