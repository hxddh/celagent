// tests/v2-supervisor.test.mjs — tmux supervisor 生命周期（无 LLM/对象存储）
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync, readFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  startSupervisor, stopSupervisor, supervisorStatus, tmuxAvailable,
  tmuxHasSession, writeStop, hasStop, clearStop, listStates,
} from "../src/v2/supervisor.js";

test("supervisor: 启动 tmux + watchdog, stop 清理, stop 标记生效", async (t) => {
  if (!(await tmuxAvailable())) return t.skip("tmux 不可用");
  const dir = mkdtempSync(join(tmpdir(), "celagent-sup-"));
  const configDir = join(dir, "cfg");
  const cwd = join(dir, "ws");
  mkdirSync(cwd, { recursive: true });
  // 假 CLI：忽略 __tui/__watch，只保持进程存活；用于验证 tmux/watchdog 生命周期。
  const bin = join(dir, "fake.mjs");
  writeFileSync(bin, "setTimeout(() => {}, 60000)\n");
  const id = `sup-${Date.now().toString(36)}`;
  try {
    writeStop(configDir, id, "test");
    assert.equal(hasStop(configDir, id), true);
    clearStop(configDir, id);
    assert.equal(hasStop(configDir, id), false);

    const r = await startSupervisor({ configDir, id, cwd, bin, node: process.execPath });
    assert.equal(r.started, true);
    assert.equal(await tmuxHasSession(id), true);
    const st = await supervisorStatus({ configDir, id });
    assert.equal(st.running, true);
    assert.equal(st.watchdog?.alive, true);
    assert.equal(listStates(configDir).some((x) => x.id === id), true);

    await stopSupervisor({ configDir, id });
    assert.equal(await tmuxHasSession(id), false);
    const st2 = await supervisorStatus({ configDir, id });
    assert.equal(st2.running, false);
    assert.equal(st2.watchdog, null);
    assert.equal(st2.state, null);
  } finally {
    await stopSupervisor({ configDir, id }).catch(() => {});
    rmSync(dir, { recursive: true, force: true });
  }
});

test("supervisor: child kind 拉起 __child，并记录 parent/taskFile", async (t) => {
  if (!(await tmuxAvailable())) return t.skip("tmux 不可用");
  const dir = mkdtempSync(join(tmpdir(), "celagent-sup-child-"));
  const configDir = join(dir, "cfg");
  const cwd = join(dir, "wt");
  mkdirSync(cwd, { recursive: true });
  const taskFile = join(dir, "task.md");
  writeFileSync(taskFile, "只改 a.txt\n");
  const bin = join(dir, "fake.mjs");
  writeFileSync(bin, [
    'import { writeFileSync } from "node:fs";',
    'import { join } from "node:path";',
    'if (process.argv[2] === "__child") {',
    '  writeFileSync(join(process.cwd(), "child-args.json"), JSON.stringify(process.argv));',
    '}',
    "setTimeout(() => {}, 60000);",
  ].join("\n"));
  const id = `dlg-${Date.now().toString(36)}`;
  try {
    const r = await startSupervisor({
      configDir, id, cwd, bin, node: process.execPath,
      kind: "child", taskFile, parent: "parent-x",
    });
    assert.equal(r.started, true);
    const argsFile = join(cwd, "child-args.json");
    for (let i = 0; i < 30 && !existsSync(argsFile); i++) await new Promise((res) => setTimeout(res, 100));
    assert.equal(existsSync(argsFile), true, "fake child 没有收到参数");
    const argv = JSON.parse(readFileSync(argsFile, "utf8"));
    assert.equal(argv[2], "__child");
    assert.equal(argv[3], id);
    assert.equal(argv.includes("--task"), true);
    assert.equal(argv[argv.indexOf("--task") + 1], taskFile);
    assert.equal(argv[argv.indexOf("--parent") + 1], "parent-x");

    const st = await supervisorStatus({ configDir, id });
    assert.equal(st.state.kind, "child");
    assert.equal(st.state.parent, "parent-x");
    assert.equal(st.state.taskFile, taskFile);
  } finally {
    await stopSupervisor({ configDir, id }).catch(() => {});
    rmSync(dir, { recursive: true, force: true });
  }
});

test("supervisor: 同 ID 重复 start 返回 already, 不重复建 tmux", async (t) => {
  if (!(await tmuxAvailable())) return t.skip("tmux 不可用");
  const dir = mkdtempSync(join(tmpdir(), "celagent-sup2-"));
  const configDir = join(dir, "cfg");
  const cwd = join(dir, "ws");
  mkdirSync(cwd, { recursive: true });
  const bin = join(dir, "fake.mjs");
  writeFileSync(bin, "setTimeout(() => {}, 60000)\n");
  const id = `sup-${Date.now().toString(36)}`;
  try {
    const a = await startSupervisor({ configDir, id, cwd, bin, node: process.execPath });
    const b = await startSupervisor({ configDir, id, cwd, bin, node: process.execPath });
    assert.equal(a.started, true);
    assert.equal(b.already, true);
  } finally {
    await stopSupervisor({ configDir, id }).catch(() => {});
    rmSync(dir, { recursive: true, force: true });
  }
});

test("supervisor: goal kind 拉起 __goal", async (t) => {
  if (!(await tmuxAvailable())) return t.skip("tmux 不可用");
  const dir = mkdtempSync(join(tmpdir(), "celagent-sup-goal-"));
  const configDir = join(dir, "cfg");
  const cwd = join(dir, "ws");
  mkdirSync(cwd, { recursive: true });
  const bin = join(dir, "fake.mjs");
  writeFileSync(bin, [
    'import { writeFileSync } from "node:fs";',
    'import { join } from "node:path";',
    'if (process.argv[2] === "__goal") {',
    '  writeFileSync(join(process.cwd(), "goal-args.json"), JSON.stringify(process.argv));',
    '}',
    "setTimeout(() => {}, 60000);",
  ].join("\n"));
  const id = `goal-${Date.now().toString(36)}`;
  try {
    await startSupervisor({ configDir, id, cwd, bin, node: process.execPath, kind: "goal" });
    const argsFile = join(cwd, "goal-args.json");
    for (let i = 0; i < 30 && !existsSync(argsFile); i++) await new Promise((res) => setTimeout(res, 100));
    assert.equal(existsSync(argsFile), true, "fake goal 没有收到参数");
    const argv = JSON.parse(readFileSync(argsFile, "utf8"));
    assert.equal(argv[2], "__goal");
    assert.equal(argv[3], id);
    assert.equal(argv[argv.indexOf("--cwd") + 1], cwd);
    const st = await supervisorStatus({ configDir, id });
    assert.equal(st.state.kind, "goal");
  } finally {
    await stopSupervisor({ configDir, id }).catch(() => {});
    rmSync(dir, { recursive: true, force: true });
  }
});
