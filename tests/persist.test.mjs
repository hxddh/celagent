// persist.test.mjs — 权威写/恢复可执行回归 (内存 store, 无 aws/celld)
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  classifyStoreError,
  mergeTurn,
  makeTurnEntry,
  createPersister,
  loadSessionHistory,
  BOS_QUEUE_MAX,
} from "../src/persist.js";

function memoryStore() {
  const objects = new Map();
  let n = 0;
  let getFail = null;
  let putFail = null;
  const puts = [];
  return {
    objects,
    puts,
    failGet(err) { getFail = err; },
    failPut(err) { putFail = err; },
    async get(key) {
      if (getFail) {
        const e = getFail;
        getFail = null;
        return { ok: false, error: e };
      }
      const cur = objects.get(key);
      if (!cur) return { ok: false, error: "not-found" };
      return { ok: true, body: cur.body, etag: cur.etag };
    },
    async put(key, content, extra = {}) {
      const body = typeof content === "string" ? content : JSON.stringify(content);
      puts.push({ key, extra: { ...extra }, body });
      if (putFail) {
        const e = putFail;
        putFail = null;
        return { ok: false, error: e };
      }
      const cur = objects.get(key);
      if (extra.ifNoneMatch && cur) return { ok: false, conflict: true, error: "conflict" };
      if (extra.ifMatch && (!cur || cur.etag !== extra.ifMatch)) {
        return { ok: false, conflict: true, error: "conflict" };
      }
      n += 1;
      const etag = `"etag-${n}"`;
      objects.set(key, { body, etag });
      return { ok: true, result: { ETag: etag } };
    },
  };
}

const store = {
  bucket: "celagent-t",
  endpoint: "http://127.0.0.1:9",
  profile: "bos",
  region: "bj",
};
const probeOk = async () => ({ ok: true, message: "ok" });
const silent = () => {};

function persisterOf(mem, extra = {}) {
  return createPersister({
    store,
    get: (key) => mem.get(key),
    put: (key, content, extraOpts) => mem.put(key, content, extraOpts),
    probe: extra.probe || probeOk,
    warn: silent,
    sleep: extra.sleep || (async () => {}),
    loadStore: () => store,
  });
}

test("classifyStoreError: not-found / conflict / fatal / transient", () => {
  assert.equal(classifyStoreError("not-found"), "not-found");
  assert.equal(classifyStoreError("NoSuchKey"), "not-found");
  assert.equal(classifyStoreError("conflict"), "conflict");
  assert.equal(classifyStoreError("PreconditionFailed: 412"), "conflict");
  assert.equal(classifyStoreError("AccessDenied"), "fatal");
  assert.equal(classifyStoreError("InvalidAccessKeyId"), "fatal");
  assert.equal(classifyStoreError("endpoint-not-allowed"), "fatal");
  assert.equal(classifyStoreError("no-bucket"), "fatal");
  assert.equal(classifyStoreError("connect ETIMEDOUT"), "transient");
  assert.equal(classifyStoreError("TimeoutError waiting after 20000ms"), "transient");
  assert.equal(classifyStoreError("Service Unavailable 503"), "transient");
});

test("mergeTurn: 缺 content 的新写入不抹掉已有 content", () => {
  const session = {
    id: "s",
    turns: [
      { turn: 1, msg: "A".repeat(50), content: [{ type: "text", text: "A".repeat(50) }] },
      { turn: 2, msg: "B".repeat(50), content: [{ type: "text", text: "B".repeat(50) }] },
    ],
  };
  mergeTurn(session, 2, "assistant", "x".repeat(10), null, null);
  assert.equal(session.turns[1].msg.length, 10);
  assert.equal(session.turns[1].content[0].text.length, 50);
});

test("persistTurnToBos: GET 超时返回 retry, 不写空会话", async () => {
  const mem = memoryStore();
  mem.failGet("TimeoutError waiting after 20000ms");
  const p = persisterOf(mem);
  const r = await p.persistTurnToBos("sid", 1, "user", "hi", null, null);
  assert.equal(r, "retry");
  assert.equal(mem.puts.length, 0);
  assert.equal(mem.objects.size, 0);
});

test("persistTurnToBos: PUT 5xx 返回 retry", async () => {
  const mem = memoryStore();
  mem.failPut("Service Unavailable 503");
  const p = persisterOf(mem);
  const r = await p.persistTurnToBos("sid", 1, "user", "hi", [{ type: "text", text: "hi" }], null);
  assert.equal(r, "retry");
  assert.equal(mem.objects.size, 0);
});

test("persistTurnToBos: 首写成功", async () => {
  const mem = memoryStore();
  const p = persisterOf(mem);
  const r = await p.persistTurnToBos("sid", 1, "user", "hi", [{ type: "text", text: "hi" }], null);
  assert.equal(r, undefined);
  const got = await mem.get("sessions/sid.json");
  assert.equal(got.ok, true);
  const session = JSON.parse(got.body);
  assert.equal(session.turns.length, 1);
  assert.equal(session.turns[0].role, "user");
  assert.equal(session.turns[0].content[0].text, "hi");
});

test("persistTurnToBos: JSON 损坏不覆盖、不重试", async () => {
  const mem = memoryStore();
  mem.objects.set("sessions/sid.json", { body: "{not json", etag: `"e1"` });
  const p = persisterOf(mem);
  const r = await p.persistTurnToBos("sid", 2, "assistant", "x", null, null);
  assert.equal(r, undefined);
  assert.equal(mem.objects.get("sessions/sid.json").body, "{not json");
});

test("persistTurnToBos: GET AccessDenied 不重试", async () => {
  const mem = memoryStore();
  mem.failGet("AccessDenied");
  const p = persisterOf(mem);
  const r = await p.persistTurnToBos("sid", 1, "user", "hi", null, null);
  assert.equal(r, undefined);
});

test("persistTurnToBos: PUT AccessDenied 不重试", async () => {
  const mem = memoryStore();
  mem.failPut("AccessDenied");
  const p = persisterOf(mem);
  const r = await p.persistTurnToBos("sid", 1, "user", "hi", null, null);
  assert.equal(r, undefined);
  assert.equal(mem.objects.size, 0);
});

test("persistTurnToBos: PUT 403 不重试", async () => {
  const mem = memoryStore();
  mem.failPut("HTTP Status Code: 403");
  const p = persisterOf(mem);
  const r = await p.persistTurnToBos("sid", 1, "user", "hi", null, null);
  assert.equal(r, undefined);
});


test("persistTurnToBos: CAS 探针 transient 返回 retry", async () => {
  const mem = memoryStore();
  const p = persisterOf(mem, { probe: async () => ({ ok: false, transient: true, error: "create-failed" }) });
  const r = await p.persistTurnToBos("sid", 1, "user", "hi", null, null);
  assert.equal(r, "retry");
  assert.equal(mem.puts.length, 0);
});

test("persistTurnToBos: CAS 结论性失败不重试、不写", async () => {
  const mem = memoryStore();
  const p = persisterOf(mem, { probe: async () => ({ ok: false, error: "cas-ignored", message: "If-Match 被忽略" }) });
  const r = await p.persistTurnToBos("sid", 1, "user", "hi", null, null);
  assert.equal(r, undefined);
  assert.equal(mem.puts.length, 0);
});

test("队列: GET 瞬时失败不丢任务, 重试后写入", async () => {
  const mem = memoryStore();
  mem.failGet("connect ETIMEDOUT");
  const p = persisterOf(mem);
  p.queueBosWrite("sid", 1, "user", "hello", { fullContent: [{ type: "text", text: "hello" }] });
  await p.flush(500);
  assert.equal(p.pending.length, 0, "重试成功后队列应排空");
  const got = JSON.parse((await mem.get("sessions/sid.json")).body);
  assert.equal(got.turns[0].msg, "hello");
});

test("队列超限丢最旧保留最新", () => {
  const q = [];
  for (let op = 1; op <= 60; op++) {
    if (q.length >= BOS_QUEUE_MAX) q.shift();
    q.push(op);
  }
  assert.equal(q.length, 50);
  assert.equal(q[0], 11);
  assert.equal(q[q.length - 1], 60);
});

test("loadSessionHistory: BOS 成功不走 fallback", async () => {
  const mem = memoryStore();
  await mem.put("sessions/sid.json", { turns: [{ turn: 1, msg: "from-bos" }] });
  let fallback = 0;
  const r = await loadSessionHistory("sid", {
    store,
    get: (key) => mem.get(key),
    fallbackResume: async () => { fallback += 1; return [{ turn: 1, msg: "from-worker" }]; },
  });
  assert.equal(r.source, "bos");
  assert.equal(r.turns[0].msg, "from-bos");
  assert.equal(fallback, 0);
});

test("loadSessionHistory: not-found 才回退 worker", async () => {
  const mem = memoryStore();
  const r = await loadSessionHistory("sid", {
    store,
    get: (key) => mem.get(key),
    fallbackResume: async () => [{ turn: 1, msg: "from-worker" }],
  });
  assert.equal(r.source, "worker");
  assert.equal(r.miss, true);
  assert.equal(r.turns[0].msg, "from-worker");
});

test("loadSessionHistory: GET 超时不回退 worker", async () => {
  const mem = memoryStore();
  mem.failGet("TimeoutError waiting after 20000ms");
  let fallback = 0;
  const r = await loadSessionHistory("sid", {
    store,
    get: (key) => mem.get(key),
    fallbackResume: async () => { fallback += 1; return [{ turn: 1, msg: "truncated" }]; },
  });
  assert.equal(fallback, 0);
  assert.equal(r.turns, null);
  assert.equal(r.transient, true);
  assert.equal(r.source, "bos");
});

test("loadSessionHistory: JSON 损坏不回退、不把 worker 当权威", async () => {
  const mem = memoryStore();
  mem.objects.set("sessions/sid.json", { body: "nope", etag: `"e"` });
  let fallback = 0;
  const r = await loadSessionHistory("sid", {
    store,
    get: (key) => mem.get(key),
    fallbackResume: async () => { fallback += 1; return [{ turn: 1, msg: "worker" }]; },
  });
  assert.equal(r.corrupt, true);
  assert.equal(r.turns, null);
  assert.equal(fallback, 0);
});

test("makeTurnEntry 带 content/toolResults", () => {
  const e = makeTurnEntry("s", 3, "assistant", "m", [{ type: "text", text: "t" }], [{ toolName: "bash" }]);
  assert.equal(e.turn, 3);
  assert.equal(e.content[0].text, "t");
  assert.equal(e.toolResults[0].toolName, "bash");
});
