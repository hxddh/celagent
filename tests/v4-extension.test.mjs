// tests/v4-extension.test.mjs — 不接 LLM，用假 Pi 驱动 goal loop
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { goalExtension, readGoal, writeGoal, writeAnswer, newGoalId } from "../src/v4/index.js";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function fakePi() {
  const handlers = {};
  const sent = [];
  const pi = {
    on: (name, fn) => { handlers[name] = fn; },
    sendMessage: (message, options) => { sent.push({ message, options }); },
  };
  return { pi, handlers, sent };
}
function ctx() {
  return { isIdle: () => true, ui: { notify: () => {} } };
}
function goal(id, dir, accept) {
  return {
    id, objective: "smoke goal", accept, status: "running", attempts: 0, checks: 0,
    budgetUsd: 5, maxAttempts: 8, cost: 0, tokens: 0,
    deadline: new Date(Date.now() + 3600_000).toISOString(),
    protect: [], allowProtected: false, failureStreak: 0,
    worktree: dir, cwd: dir, baseCommit: null, sessionId: id,
  };
}

test("goal extension: 基线已通过 → done，不烧 token", async () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-v4-ext-ok-"));
  const id = newGoalId();
  try {
    writeGoal(dir, id, goal(id, dir, "true"));
    const { pi, handlers, sent } = fakePi();
    goalExtension({ configDir: dir, goalId: id, cwd: dir })(pi);
    await handlers.session_start({}, ctx());
    await sleep(1500);
    assert.equal(readGoal(dir, id).status, "done");
    assert.equal(sent.length, 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("goal extension: 失败 → 自动重试 → 同一失败 ×3 → blocked → 回答后续跑", async () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-v4-ext-loop-"));
  const id = newGoalId();
  try {
    writeGoal(dir, id, goal(id, dir, "echo fail >&2; exit 1"));
    const { pi, handlers, sent } = fakePi();
    goalExtension({ configDir: dir, goalId: id, cwd: dir })(pi);
    await handlers.session_start({}, ctx());
    await sleep(1500);
    assert.equal(sent.length, 1, "baseline 失败后应发第一轮 prompt");
    assert.match(sent[0].message.content, /继续执行 goal/);
    assert.equal(readGoal(dir, id).attempts, 1);

    await handlers.agent_settled({}, ctx());
    assert.equal(readGoal(dir, id).attempts, 2);
    await handlers.agent_settled({}, ctx());
    assert.equal(readGoal(dir, id).attempts, 3);
    await handlers.agent_settled({}, ctx());

    const blocked = readGoal(dir, id);
    assert.equal(blocked.status, "blocked");
    assert.equal(blocked.blockedReason, "same-failure");
    assert.match(blocked.blockedQuestion, /同一个失败/);
    assert.equal(sent.length, 3, "blocked 后不再自动发 prompt");

    writeAnswer(dir, id, "允许继续", { allowProtected: true });
    await sleep(1800);
    assert.equal(readGoal(dir, id).status, "running");
    assert.equal(sent.length, 4);
    assert.match(sent[3].message.content, /人的回答/);
    assert.equal(readGoal(dir, id).allowProtected, true);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
