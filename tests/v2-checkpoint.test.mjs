// tests/v2-checkpoint.test.mjs — 2.0 内容寻址工作区检查点
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync, symlinkSync, readlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createMemoryAdapter, appendFromFile } from "../src/v2/index.js";
import {
  checkpointWorkspace, listWorkspaceCheckpoints, restoreWorkspaceCheckpoint,
  readWorkspaceCheckpoint, createCheckpointRunner, DEFAULT_EXCLUDES,
} from "../src/v2/workspace-cas.js";

function entry(id, parentId) {
  return {
    type: "message", id, parentId,
    timestamp: new Date().toISOString(),
    message: { role: "user", content: [{ type: "text", text: "x" }], timestamp: Date.now() },
  };
}
function jsonl(entries, id = "sess-cp") {
  const header = { type: "session", version: 3, id, timestamp: new Date().toISOString(), cwd: "/tmp" };
  return [header, ...entries].map((o) => JSON.stringify(o)).join("\n") + "\n";
}
function makeSession() {
  const dir = mkdtempSync(join(tmpdir(), "celagent-cp-"));
  const local = join(dir, "s.jsonl");
  writeFileSync(local, jsonl([entry("a", null), entry("b", "a")]));
  const store = createMemoryAdapter();
  return { dir, local, store };
}
async function withManifest() {
  const s = makeSession();
  await appendFromFile(s.store, "sess-cp", s.local);
  const ws = join(s.dir, "ws");
  mkdirSync(ws);
  return { ...s, ws };
}

test("checkpoint: 首次上传文件, 无变化复用, 修改只上传变化 blob", async () => {
  const { dir, ws, store } = await withManifest();
  writeFileSync(join(ws, "a.txt"), "hello");
  mkdirSync(join(ws, "sub"));
  writeFileSync(join(ws, "sub", "b.txt"), "world");
  const c1 = await checkpointWorkspace({ store, sessionId: "sess-cp", cwd: ws });
  assert.equal(c1.files, 2);
  assert.equal(c1.uploaded, 2);
  assert.equal(c1.reused, false);

  const c2 = await checkpointWorkspace({ store, sessionId: "sess-cp", cwd: ws });
  assert.equal(c2.checkpointId, c1.checkpointId);
  assert.equal(c2.uploaded, 0);
  assert.equal(c2.reused, true);

  writeFileSync(join(ws, "a.txt"), "hello2");
  const c3 = await checkpointWorkspace({ store, sessionId: "sess-cp", cwd: ws });
  assert.notEqual(c3.checkpointId, c1.checkpointId);
  assert.equal(c3.uploaded, 1, "只上传变化的 a.txt");
  assert.equal(c3.reused, false);

  const list = await listWorkspaceCheckpoints(store, "sess-cp");
  assert.equal(list.latest, c3.checkpointId);
  assert.equal(list.checkpoints.length, 2);

  const cp1 = await readWorkspaceCheckpoint(store, "sess-cp", c1.checkpointId);
  assert.equal(cp1.files.find((f) => f.path === "a.txt").hash.length, 64);
  rmSync(dir, { recursive: true, force: true });
});

test("restore: 最新/指定检查点, 内容 hash 校验, force 只清理管理路径", async () => {
  const { dir, ws, store } = await withManifest();
  writeFileSync(join(ws, "a.txt"), "old");
  writeFileSync(join(ws, "keep.bin"), "keep");
  mkdirSync(join(ws, ".git"));
  writeFileSync(join(ws, ".git", "config"), "git");
  mkdirSync(join(ws, "node_modules"));
  writeFileSync(join(ws, "node_modules", "x.js"), "dep");
  const c1 = await checkpointWorkspace({ store, sessionId: "sess-cp", cwd: ws });

  writeFileSync(join(ws, "a.txt"), "new");
  writeFileSync(join(ws, "extra.txt"), "extra");
  const c2 = await checkpointWorkspace({ store, sessionId: "sess-cp", cwd: ws });
  assert.notEqual(c1.checkpointId, c2.checkpointId);

  const t1 = join(dir, "restore1");
  const r1 = await restoreWorkspaceCheckpoint({ store, sessionId: "sess-cp", checkpointId: c1.checkpointId, cwd: t1 });
  assert.equal(r1.files, 2);
  assert.equal(readFileSync(join(t1, "a.txt"), "utf8"), "old");
  assert.equal(readFileSync(join(t1, "keep.bin"), "utf8"), "keep");

  const t2 = join(dir, "restore2");
  const r2 = await restoreWorkspaceCheckpoint({ store, sessionId: "sess-cp", cwd: t2, force: true });
  assert.equal(readFileSync(join(t2, "a.txt"), "utf8"), "new");
  assert.equal(readFileSync(join(t2, "extra.txt"), "utf8"), "extra");
  assert.equal(r2.removed, 0);

  writeFileSync(join(t2, "extra.txt"), "changed");
  writeFileSync(join(t2, "stale.txt"), "stale");
  mkdirSync(join(t2, ".git"));
  writeFileSync(join(t2, ".git", "keep"), "keep");
  mkdirSync(join(t2, "node_modules"));
  writeFileSync(join(t2, "node_modules", "keep.js"), "keep");
  const r3 = await restoreWorkspaceCheckpoint({ store, sessionId: "sess-cp", cwd: t2, force: true });
  assert.equal(r3.removed, 1, "只删 stale.txt");
  assert.equal(existsSync(join(t2, "stale.txt")), false);
  assert.equal(readFileSync(join(t2, ".git", "keep"), "utf8"), "keep", "保留 .git");
  assert.equal(readFileSync(join(t2, "node_modules", "keep.js"), "utf8"), "keep", "保留 node_modules");
  rmSync(dir, { recursive: true, force: true });
});

test("restore: 非空目录不加 --force 拒绝", async () => {
  const { dir, ws, store } = await withManifest();
  writeFileSync(join(ws, "a.txt"), "a");
  await checkpointWorkspace({ store, sessionId: "sess-cp", cwd: ws });
  const target = join(dir, "target");
  mkdirSync(target);
  writeFileSync(join(target, "keep.txt"), "keep");
  await assert.rejects(
    () => restoreWorkspaceCheckpoint({ store, sessionId: "sess-cp", cwd: target }),
    /非空/,
  );
  rmSync(dir, { recursive: true, force: true });
});

test("symlink: 相对目标可恢复, 越界目标跳过", async () => {
  const { dir, ws, store } = await withManifest();
  writeFileSync(join(ws, "a.txt"), "a");
  symlinkSync("a.txt", join(ws, "link"));
  symlinkSync("../outside", join(ws, "bad"));
  const c = await checkpointWorkspace({ store, sessionId: "sess-cp", cwd: ws });
  assert.equal(c.warnings.length, 1);
  assert.match(c.warnings[0], /bad/);
  const target = join(dir, "target");
  const r = await restoreWorkspaceCheckpoint({ store, sessionId: "sess-cp", cwd: target });
  assert.equal(r.files, 1);
  assert.equal(readlinkSync(join(target, "link")), "a.txt");
  assert.equal(existsSync(join(target, "bad")), false);
  rmSync(dir, { recursive: true, force: true });
});

test("checkpoint: 单文件超限拒绝, 不写半成品", async () => {
  const { dir, ws, store } = await withManifest();
  writeFileSync(join(ws, "big.bin"), Buffer.alloc(2048));
  await assert.rejects(
    () => checkpointWorkspace({ store, sessionId: "sess-cp", cwd: ws, maxFileBytes: 1024 }),
    /超过单文件上限/,
  );
  const m = await import("../src/v2/index.js").then((m) => m.readManifest(store, "sess-cp"));
  assert.equal(m.workspace, null);
  rmSync(dir, { recursive: true, force: true });
});

test("runner: 去抖后自动检查点, flush 等待最后一次", async () => {
  const { dir, ws, store } = await withManifest();
  writeFileSync(join(ws, "a.txt"), "one");
  const runner = createCheckpointRunner({ store, sessionId: "sess-cp", cwd: ws, debounceMs: 10 });
  runner.request("test");
  await runner.flush(5000);
  const m1 = await import("../src/v2/index.js").then((m) => m.readManifest(store, "sess-cp"));
  assert.ok(m1.workspace?.latest);
  writeFileSync(join(ws, "a.txt"), "two");
  runner.request("test2");
  await runner.flush(5000);
  const m2 = await import("../src/v2/index.js").then((m) => m.readManifest(store, "sess-cp"));
  assert.notEqual(m2.workspace.latest, m1.workspace.latest);
  runner.close();
  rmSync(dir, { recursive: true, force: true });
});

test("DEFAULT_EXCLUDES 覆盖依赖与大目录", () => {
  for (const x of [".git", "node_modules", ".venv", "dist", "build", ".cache", "target", "vendor"]) {
    assert.ok(DEFAULT_EXCLUDES.includes(x), x);
  }
});
