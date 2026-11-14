// tests/v2.test.mjs — v1.0 分段存储 / 租约 / outbox / journal / workspace 行为测试
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync, existsSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  createMemoryAdapter, createCrypto, createOutbox, createLeaseManager, createJournal,
  appendFromFile, materialize, readSession, importV0, parseLocalJsonl,
  readManifest, manifestKey, segmentKey, legacyJsonlKey, sessionPrefix,
  SessionLockedError, LeaseLostError, durabilityExtension,
} from "../src/v2/index.js";
import { snapshotWorkspace, restoreWorkspace } from "../src/v2/workspace.js";

function entry(id, parentId, role = "user", text = "hello") {
  return {
    type: "message", id, parentId,
    timestamp: new Date().toISOString(),
    message: { role, content: [{ type: "text", text }], timestamp: Date.now() },
  };
}
function jsonl(entries, id = "sess-test") {
  const header = { type: "session", version: 3, id, timestamp: new Date().toISOString(), cwd: "/tmp" };
  return [header, ...entries].map((o) => JSON.stringify(o)).join("\n") + "\n";
}
function tempFile(name, body) {
  const dir = mkdtempSync(join(tmpdir(), "celagent-v2-"));
  const f = join(dir, name);
  writeFileSync(f, body, { mode: 0o600 });
  return { dir, f };
}

test("parseLocalJsonl: header + entries; 坏行拒绝", () => {
  const text = jsonl([entry("a", null), entry("b", "a", "assistant", "hi")]);
  const p = parseLocalJsonl(text);
  assert.equal(p.header.type, "session");
  assert.equal(p.entries.length, 2);
  assert.equal(p.entries[1].id, "b");
  assert.throws(() => parseLocalJsonl("{\"type\":\"session\"}\n{bad\n"), /JSONL/);
});

test("appendFromFile: 首写 manifest + segment, 再追加只传新 entry", async () => {
  const store = createMemoryAdapter();
  const base = jsonl([entry("a", null), entry("b", "a", "assistant")]);
  const { dir, f } = tempFile("s.jsonl", base);
  const m1 = await appendFromFile(store, "sess-test", f);
  assert.equal(m1.head.seq, 2);
  assert.equal(m1.segments.length, 1);
  assert.equal((await store.list("sessions/sess-test/segments/")).keys.length, 1);

  writeFileSync(f, base + JSON.stringify(entry("c", "b")) + "\n");
  const m2 = await appendFromFile(store, "sess-test", f);
  assert.equal(m2.head.seq, 3);
  assert.equal(m2.segments.length, 2, "第二次只写新 segment");
  assert.equal((await store.list("sessions/sess-test/segments/")).keys.length, 2);
  rmSync(dir, { recursive: true, force: true });
});

test("materialize: 从远端重建完整 JSONL, 可被 SessionManager.open", async () => {
  const store = createMemoryAdapter();
  const entries = [entry("a", null), entry("b", "a", "assistant", "world")];
  const { dir, f } = tempFile("s.jsonl", jsonl(entries));
  await appendFromFile(store, "sess-test", f);
  const out = join(dir, "restored.jsonl");
  const got = await materialize(store, "sess-test", out);
  assert.equal(got.local, false);
  const text = readFileSync(out, "utf8");
  assert.match(text, /"id":"a"/);
  assert.match(text, /"id":"b"/);
  const pi = await import("@earendil-works/pi-coding-agent");
  const sm = pi.SessionManager.open(out, dir, dir);
  assert.equal(sm.getEntries().filter((e) => e.type === "message").length, 2);
  rmSync(dir, { recursive: true, force: true });
});

test("materialize: 本地领先 (outbox tail) 时保留本地", async () => {
  const store = createMemoryAdapter();
  const base = jsonl([entry("a", null)]);
  const { dir, f } = tempFile("s.jsonl", base);
  await appendFromFile(store, "sess-test", f);
  writeFileSync(f, base + JSON.stringify(entry("b", "a", "assistant")) + "\n");
  const got = await materialize(store, "sess-test", f);
  assert.equal(got.local, true);
  assert.equal(got.needsAppend, true);
  rmSync(dir, { recursive: true, force: true });
});

test("segment 损坏时 readSession 拒绝", async () => {
  const store = createMemoryAdapter();
  const { dir, f } = tempFile("s.jsonl", jsonl([entry("a", null)]));
  const m = await appendFromFile(store, "sess-test", f);
  const key = m.segments[0].key;
  store.objects.set(key, { body: "tampered\n", etag: '"x"' });
  await assert.rejects(() => readSession(store, "sess-test"), /hash/);
  rmSync(dir, { recursive: true, force: true });
});

test("lease: 第二写者被拒, 过期后可接管, 旧写者失去租约", async () => {
  const store = createMemoryAdapter();
  let clock = 1000;
  const a = createLeaseManager(store, { owner: "A", now: () => clock, ttlMs: 100 });
  const b = createLeaseManager(store, { owner: "B", now: () => clock, ttlMs: 100 });
  const la = await a.acquire("s");
  assert.equal(la.epoch, 1);
  await assert.rejects(() => b.acquire("s"), SessionLockedError);
  clock += 200;
  const lb = await b.acquire("s");
  assert.equal(lb.epoch, 2);
  await assert.rejects(() => a.assert("s"), LeaseLostError);
  await b.assert("s");
});

test("outbox: mark 后崩溃, flush 成功才 clear", async () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-outbox-"));
  const ob = createOutbox(dir);
  ob.mark("s1", "/tmp/s1.jsonl");
  assert.equal(ob.list().length, 1);
  const bad = await ob.flush(async () => { throw new Error("offline"); });
  assert.equal(bad.failed, 1);
  assert.equal(ob.list().length, 1, "失败保留");
  const ok = await ob.flush(async () => {});
  assert.equal(ok.ok, 1);
  assert.equal(ob.list().length, 0);
  rmSync(dir, { recursive: true, force: true });
});

test("journal: start/finish, 远端记录完成; 未完成可 reconcile 成 unknown", async () => {
  const store = createMemoryAdapter();
  const dir = mkdtempSync(join(tmpdir(), "celagent-journal-"));
  const j = createJournal({ store, localDir: dir });
  assert.equal(await j.start("s", { toolCallId: "call/1", toolName: "bash", input: { command: "ls" } }), true);
  assert.equal(await j.finish("s", { toolCallId: "call/1", result: { ok: true }, isError: false }), true);
  const list = await j.list("s");
  assert.equal(list.length, 1);
  assert.equal(list[0].status, "finished");

  assert.equal(await j.start("s", { toolCallId: "call/2", toolName: "bash", input: {} }), true);
  const j2 = createJournal({ store, localDir: dir });
  const rec = await j2.reconcile("s");
  assert.equal(rec.some((x) => x.toolCallId === "call/2" && x.unknown), true);
  rmSync(dir, { recursive: true, force: true });
});

test("加密适配器: 对象 body 不含明文, 读回可解密", async () => {
  const key = Buffer.alloc(32, 7).toString("base64");
  const store = createMemoryAdapter({ crypto: createCrypto(key) });
  await store.put("k", "TOP-SECRET-TEXT");
  const raw = store.objects.get("k").body;
  assert.equal(raw.includes("TOP-SECRET-TEXT"), false);
  const got = await store.get("k");
  assert.equal(got.body, "TOP-SECRET-TEXT");
});

test("importV0: 旧 JSONL 导入 v2, 旧对象保留", async () => {
  const store = createMemoryAdapter();
  await store.put(legacyJsonlKey("old"), jsonl([entry("a", null), entry("b", "a", "assistant")]));
  const { dir, f } = tempFile("old.jsonl", "");
  const out = await importV0(store, "old", f, { cwd: "/tmp" });
  assert.equal(out.manifest.head.seq, 2);
  assert.equal((await store.get(legacyJsonlKey("old"))).ok, true, "旧对象不删");
  assert.match(readFileSync(f, "utf8"), /"id":"b"/);
  rmSync(dir, { recursive: true, force: true });
});

test("importV0: 已存在 manifest 拒绝", async () => {
  const store = createMemoryAdapter();
  const { dir, f } = tempFile("s.jsonl", jsonl([entry("a", null)]));
  await appendFromFile(store, "s", f);
  await assert.rejects(() => importV0(store, "s", f), /已是 v2/);
  rmSync(dir, { recursive: true, force: true });
});

test("workspace: snapshot 后删除文件, restore 恢复", async () => {
  const store = createMemoryAdapter();
  const { dir, f } = tempFile("s.jsonl", jsonl([entry("a", null)]));
  await appendFromFile(store, "s", f);
  const ws = join(dir, "ws");
  mkdirSync(ws);
  writeFileSync(join(ws, "hello.txt"), "hello workspace");
  const snap = await snapshotWorkspace({ store, sessionId: "s", cwd: ws });
  assert.ok(snap.snapshotId);
  rmSync(join(ws, "hello.txt"));
  await restoreWorkspace({ store, sessionId: "s", cwd: ws, force: true });
  assert.equal(readFileSync(join(ws, "hello.txt"), "utf8"), "hello workspace");
  rmSync(dir, { recursive: true, force: true });
});

test("manifest: 未知版本拒绝", async () => {
  const store = createMemoryAdapter();
  await store.put(manifestKey("s"), JSON.stringify({ v: 99, session: "s" }));
  await assert.rejects(() => readManifest(store, "s"), /不支持的版本/);
});

test("durability extension: journal 不可用时 block 工具; tool_result 传 content", async () => {
  const calls = [];
  const journal = {
    start: async () => false,
    finish: async (sessionId, info) => { calls.push({ sessionId, info }); },
  };
  const handlers = {};
  const pi = { on: (name, fn) => { handlers[name] = fn; } };
  durabilityExtension({ getSessionId: () => "s", journal })(pi);
  const blocked = await handlers.tool_call({ toolCallId: "c1", toolName: "bash", input: { command: "rm -rf /" } });
  assert.equal(blocked.block, true);
  await handlers.tool_result({ toolCallId: "c1", content: [{ type: "text", text: "ok" }], isError: false });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].info.result.content[0].text, "ok");
});

test("lease: 心跳在 TTL 内续租", async () => {
  const store = createMemoryAdapter();
  const a = createLeaseManager(store, { owner: "A", ttlMs: 50 });
  const first = await a.acquire("s-heartbeat");
  const stop = a.startHeartbeat("s-heartbeat");
  try {
    await new Promise((r) => setTimeout(r, 1200));
    const got = await a.read("s-heartbeat");
    assert.ok(got.lease.expiresAt > first.expiresAt, "心跳应延长 expiresAt");
  } finally {
    stop();
    await a.release("s-heartbeat");
  }
});
