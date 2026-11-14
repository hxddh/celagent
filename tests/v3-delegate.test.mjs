// tests/v3-delegate.test.mjs — spawn / wait / send / list 合同（不启 tmux）
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, existsSync, readFileSync, rmSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createDelegator } from "../src/v3/delegate.js";
import { listInbox, patchChildState, readChildState, taskPath, writeChildResult } from "../src/v3/child.js";

function git(cwd, args) {
  return execFileSync("git", ["-C", cwd, ...args], { encoding: "utf8" }).trim();
}
function setup() {
  const root = mkdtempSync(join(tmpdir(), "celagent-v3-dlg-"));
  git(root, ["init", "-q"]);
  git(root, ["config", "user.name", "test"]);
  git(root, ["config", "user.email", "test@example.com"]);
  writeFileSync(join(root, "a.txt"), "one\n");
  git(root, ["add", "-A"]);
  git(root, ["commit", "-q", "-m", "init"]);
  const configDir = join(root, ".celagent-test");
  mkdirSync(configDir, { recursive: true });
  const calls = [];
  const starts = [];
  const stops = [];
  const startSupervisor = async (opts) => { calls.push(opts); starts.push(opts); return { started: true }; };
  const stopSupervisor = async (opts) => { stops.push(opts); return { stopped: true }; };
  const delegator = createDelegator({
    configDir, startSupervisor, stopSupervisor, bin: "/bin/fake", node: process.execPath, maxChildren: 2,
  });
  return { root, configDir, delegator, calls, starts, stops };
}

test("spawn: 建 worktree + state + task，并以 child kind 启 supervisor", async () => {
  const { root, configDir, delegator, calls } = setup();
  try {
    const r = await delegator.spawn({ parent: "parent-1", cwd: root, task: "修 a.txt 的测试" });
    assert.match(r.session, /^dlg-/);
    assert.equal(r.isolated, true);
    assert.equal(existsSync(r.worktree), true);
    assert.equal(readFileSync(taskPath(configDir, r.session), "utf8").trim(), "修 a.txt 的测试");
    const st = readChildState(configDir, r.session);
    assert.equal(st.parent, "parent-1");
    assert.equal(st.status, "running");
    assert.equal(st.prompted, false);
    assert.equal(st.baseCommit, git(root, ["rev-parse", "HEAD"]));
    assert.equal(calls.length, 1);
    assert.equal(calls[0].kind, "child");
    assert.equal(calls[0].taskFile, taskPath(configDir, r.session));
    assert.equal(calls[0].cwd, r.worktree);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("spawn: 非 git 目录默认拒绝；isolate:false 允许且不建 worktree", async () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-v3-plain-"));
  const configDir = join(dir, "cfg");
  const starts = [];
  const d = createDelegator({ configDir, startSupervisor: async (o) => { starts.push(o); }, maxChildren: 2 });
  try {
    await assert.rejects(() => d.spawn({ parent: "p", cwd: dir, task: "只读检查" }), /不是 git 仓库/);
    const r = await d.spawn({ parent: "p", cwd: dir, task: "只读检查", isolate: false });
    assert.equal(r.isolated, false);
    assert.equal(r.worktree, dir);
    assert.equal(starts.length, 1);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("spawn: 同一父会话 running 达到上限后拒绝", async () => {
  const { root, delegator } = setup();
  try {
    await delegator.spawn({ parent: "p-max", cwd: root, task: "t1" });
    await delegator.spawn({ parent: "p-max", cwd: root, task: "t2" });
    await assert.rejects(() => delegator.spawn({ parent: "p-max", cwd: root, task: "t3" }), /上限 2/);
    // 别的父会话不受影响
    const r = await delegator.spawn({ parent: "p-other", cwd: root, task: "t3" });
    assert.equal(r.status, "running");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("wait: 有 result 立即返回；超时返回 running 而不是取消", async () => {
  const { root, configDir, delegator } = setup();
  try {
    const a = await delegator.spawn({ parent: "p-wait", cwd: root, task: "t1" });
    writeChildResult(configDir, a.session, { status: "done", text: "完成", commit: "abc123", diffStat: "a.txt | 1 +" });
    patchChildState(configDir, a.session, { status: "done" });
    const done = await delegator.wait({ child: a.session, timeoutMs: 1000 });
    assert.equal(done.status, "done");
    assert.equal(done.text, "完成");
    assert.equal(done.running, false);

    const b = await delegator.spawn({ parent: "p-wait", cwd: root, task: "t2" });
    const running = await delegator.wait({ child: b.session, timeoutMs: 1000 });
    assert.equal(running.status, "running");
    assert.equal(running.running, true);
    assert.equal(readChildState(configDir, b.session).status, "running");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("send: running 只排队；已结束则清残余 supervisor 并续跑", async () => {
  const { root, configDir, delegator, calls, stops } = setup();
  try {
    const a = await delegator.spawn({ parent: "p-send", cwd: root, task: "t1" });
    const q = await delegator.send({ child: a.session, message: "再补一个测试" });
    assert.equal(q.status, "queued");
    assert.equal(listInbox(configDir, a.session).length, 1);
    assert.equal(calls.length, 1);

    patchChildState(configDir, a.session, { status: "done" });
    const r = await delegator.send({ child: a.session, message: "reviewer 要求改" });
    assert.equal(r.status, "resumed");
    assert.equal(stops.length, 1);
    assert.equal(calls.length, 2);
    assert.equal(readChildState(configDir, a.session).status, "running");
    assert.equal(listInbox(configDir, a.session).length, 2);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("list: 按父会话过滤，结果状态覆盖 running", async () => {
  const { root, configDir, delegator } = setup();
  try {
    const a = await delegator.spawn({ parent: "p-list", cwd: root, task: "t1" });
    const b = await delegator.spawn({ parent: "p-other", cwd: root, task: "t2" });
    writeChildResult(configDir, b.session, { status: "interrupted", error: "turn-unfinished" });
    patchChildState(configDir, b.session, { status: "interrupted" });

    const rows = delegator.list({ parent: "p-list" });
    assert.equal(rows.length, 1);
    assert.equal(rows[0].session, a.session);
    assert.equal(rows[0].status, "running");
    const all = delegator.list();
    assert.equal(all.length, 2);
    assert.equal(all.find((x) => x.session === b.session).status, "interrupted");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
