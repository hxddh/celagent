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
  isJsonlBody,
  turnsFromJsonl,
  jsonlEntryIds,
  jsonlSupersedes,
  jsonlFromTurns,
  MIGRATED_MODEL,
  JSONL_SIZE_WARN_BYTES,
  persistIdFromJsonlPath,
  sessionJsonlKey,
  sessionTurnsKey,
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

test("mergeTurn: 同源幂等重试缺 content 不抹掉已有 content", () => {
  const session = {
    id: "s",
    turns: [
      { turn: 1, role: "user", msg: "A".repeat(50), content: [{ type: "text", text: "A".repeat(50) }] },
      { turn: 2, role: "assistant", msg: "B".repeat(50), content: [{ type: "text", text: "B".repeat(50) }] },
    ],
  };
  mergeTurn(session, 2, "assistant", "B".repeat(50), null, null);
  assert.equal(session.turns.length, 2, "幂等重试不追加新轮");
  assert.equal(session.turns[1].content[0].text.length, 50);
});

test("mergeTurn: 同号但内容不同 = 并发写入者, 追加不覆盖 (评审 P1)", () => {
  const session = {
    id: "s",
    turns: [
      { turn: 10, role: "assistant", msg: "base" },
      { turn: 11, role: "assistant", msg: "client-A 的轮次", content: [{ type: "text", text: "A-full" }] },
    ],
  };
  // client B 与 A 从同一 maxTurn=10 起号, CAS 冲突重试后读到 A 的 turn 11
  mergeTurn(session, 11, "assistant", "client-B 的轮次", [{ type: "text", text: "B-full" }], null);
  assert.equal(session.turns.length, 3, "B 的轮次应追加为新号");
  assert.equal(session.turns[1].msg, "client-A 的轮次", "A 的轮次必须原样保留");
  assert.equal(session.turns[1].content[0].text, "A-full");
  assert.equal(session.turns[2].turn, 12);
  assert.equal(session.turns[2].msg, "client-B 的轮次");
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

function sampleJsonl(userText = "hello from jsonl", assistantText = "hi there") {
  return [
    JSON.stringify({ type: "session", version: 3, id: "uuid-test", timestamp: "2026-08-19T00:00:00.000Z", cwd: "/tmp" }),
    JSON.stringify({
      type: "message",
      id: "a1b2c3d4",
      parentId: null,
      timestamp: "2026-08-19T00:00:01.000Z",
      message: { role: "user", content: [{ type: "text", text: userText }], timestamp: 1720000000000 },
    }),
    JSON.stringify({
      type: "message",
      id: "b2c3d4e5",
      parentId: "a1b2c3d4",
      timestamp: "2026-08-19T00:00:02.000Z",
      message: { role: "assistant", content: [{ type: "text", text: assistantText }], timestamp: 1720000001000 },
    }),
  ].join("\n") + "\n";
}

test("isJsonlBody: 仅首行 type=session 才算 Pi JSONL", () => {
  assert.equal(isJsonlBody(sampleJsonl()), true);
  assert.equal(isJsonlBody('{"id":"s","turns":[]}'), false);
  assert.equal(isJsonlBody("{not json\n"), false);
  assert.equal(isJsonlBody(""), false);
});

test("turnsFromJsonl: 抽出 user/assistant 文本, 跳过坏行", () => {
  const body = sampleJsonl("ping", "pong") + "not-json\n";
  const turns = turnsFromJsonl(body);
  assert.equal(turns.length, 2);
  assert.equal(turns[0].role, "user");
  assert.equal(turns[0].msg, "ping");
  assert.equal(turns[1].role, "assistant");
  assert.equal(turns[1].msg, "pong");
});

test("persistIdFromJsonlPath: stem 即 persistId", () => {
  assert.equal(persistIdFromJsonlPath("/tmp/sess-abc.jsonl"), "sess-abc");
  assert.equal(persistIdFromJsonlPath("foo.jsonl"), "foo");
  assert.equal(sessionJsonlKey("sess-abc"), "sessions/sess-abc.jsonl");
  assert.equal(sessionTurnsKey("sess-abc"), "sessions/sess-abc.json");
});

test("persistJsonlToBos: 首写成功", async () => {
  const mem = memoryStore();
  const p = persisterOf(mem);
  const body = sampleJsonl();
  const r = await p.persistJsonlToBos("sid", body);
  assert.equal(r, undefined);
  const got = await mem.get("sessions/sid.jsonl");
  assert.equal(got.ok, true);
  assert.equal(isJsonlBody(got.body), true);
  assert.match(got.body, /hello from jsonl/);
});

test("persistJsonlToBos: 非法 body 不写", async () => {
  const mem = memoryStore();
  const p = persisterOf(mem);
  const r = await p.persistJsonlToBos("sid", '{"turns":[]}');
  assert.equal(r, undefined);
  assert.equal(mem.objects.size, 0);
});

test("persistJsonlToBos: GET 超时返回 retry, 不写空对象", async () => {
  const mem = memoryStore();
  mem.failGet("TimeoutError waiting after 20000ms");
  const p = persisterOf(mem);
  const r = await p.persistJsonlToBos("sid", sampleJsonl());
  assert.equal(r, "retry");
  assert.equal(mem.objects.size, 0);
});

test("persistJsonlToBos: 损坏的已有 JSONL 不覆盖", async () => {
  const mem = memoryStore();
  mem.objects.set("sessions/sid.jsonl", { body: "{not a session", etag: `"e1"` });
  const p = persisterOf(mem);
  const r = await p.persistJsonlToBos("sid", sampleJsonl());
  assert.equal(r, undefined);
  assert.equal(mem.objects.get("sessions/sid.jsonl").body, "{not a session");
});

test("persistJsonlToBos: PUT 5xx 返回 retry", async () => {
  const mem = memoryStore();
  mem.failPut("Service Unavailable 503");
  const p = persisterOf(mem);
  const r = await p.persistJsonlToBos("sid", sampleJsonl());
  assert.equal(r, "retry");
  assert.equal(mem.objects.size, 0);
});

test("persistJsonlToBos: PUT AccessDenied 不重试", async () => {
  const mem = memoryStore();
  mem.failPut("AccessDenied");
  const p = persisterOf(mem);
  const r = await p.persistJsonlToBos("sid", sampleJsonl());
  assert.equal(r, undefined);
  assert.equal(mem.objects.size, 0);
});

test("loadSessionHistory: JSONL 优先于旧 turns JSON", async () => {
  const mem = memoryStore();
  await mem.put("sessions/sid.jsonl", sampleJsonl("from-jsonl"));
  await mem.put("sessions/sid.json", { turns: [{ turn: 1, msg: "from-json" }] });
  let fallback = 0;
  const r = await loadSessionHistory("sid", {
    store,
    get: (key) => mem.get(key),
    fallbackResume: async () => { fallback += 1; return [{ turn: 1, msg: "from-worker" }]; },
  });
  assert.equal(r.source, "bos");
  assert.equal(r.kind, "jsonl");
  assert.equal(r.turns[0].msg, "from-jsonl");
  assert.equal(typeof r.jsonl, "string");
  assert.equal(fallback, 0);
});

test("loadSessionHistory: JSONL miss 才读旧 JSON", async () => {
  const mem = memoryStore();
  await mem.put("sessions/sid.json", { turns: [{ turn: 1, msg: "legacy" }] });
  const r = await loadSessionHistory("sid", {
    store,
    get: (key) => mem.get(key),
  });
  assert.equal(r.source, "bos");
  assert.equal(r.kind, "turns");
  assert.equal(r.turns[0].msg, "legacy");
});

test("loadSessionHistory: JSONL 损坏不回退旧 JSON 或 worker", async () => {
  const mem = memoryStore();
  mem.objects.set("sessions/sid.jsonl", { body: "nope", etag: `"e"` });
  await mem.put("sessions/sid.json", { turns: [{ turn: 1, msg: "legacy" }] });
  let fallback = 0;
  const r = await loadSessionHistory("sid", {
    store,
    get: (key) => mem.get(key),
    fallbackResume: async () => { fallback += 1; return [{ turn: 1, msg: "worker" }]; },
  });
  assert.equal(r.corrupt, true);
  assert.equal(r.kind, "jsonl");
  assert.equal(r.turns, null);
  assert.equal(fallback, 0);
});

// ---- P0: JSONL 整体写的谱系覆盖保护 ----
function jsonlLine(id, parentId, role, text) {
  return JSON.stringify({
    type: "message", id, parentId, timestamp: "2026-08-19T00:00:03.000Z",
    message: { role, content: [{ type: "text", text }], timestamp: 1720000002000 },
  });
}

test("jsonlSupersedes: 追加扩展可覆盖, 分叉/落后不可覆盖", () => {
  const base = sampleJsonl();
  const extended = base + jsonlLine("c3d4e5f6", "b2c3d4e5", "user", "more") + "\n";
  assert.equal(jsonlSupersedes(extended, base), true, "追加扩展是合法覆盖");
  assert.equal(jsonlSupersedes(base, base), true, "等同也合法");
  assert.equal(jsonlSupersedes(base, extended), false, "本地落后于远端不可覆盖");
  const other = [
    JSON.stringify({ type: "session", version: 3, id: "uuid-other", timestamp: "2026-08-19T01:00:00.000Z", cwd: "/tmp" }),
    jsonlLine("zzzz1111", null, "user", "fresh"),
  ].join("\n") + "\n";
  assert.equal(jsonlSupersedes(other, base), false, "不同谱系 (新建会话撞 id) 不可覆盖");
  assert.ok(jsonlEntryIds(base).length >= 2);
});

test("jsonlSupersedes: 远端含坏行谱系不可判, 拒绝覆盖 (评审 P2)", () => {
  const base = sampleJsonl();
  const remoteTorn = base + '{"type":"message","id":"torn-li' + "\n";
  const localExtended = base + jsonlLine("c3d4e5f6", "b2c3d4e5", "user", "more") + "\n";
  assert.equal(jsonlSupersedes(localExtended, remoteTorn), false, "远端坏行可能是可恢复数据");
  assert.equal(jsonlEntryIds(remoteTorn, { strict: true }), null);
});

test("persistJsonlToBos: 远端含坏行拒绝覆盖", async () => {
  const mem = memoryStore();
  const base = sampleJsonl();
  const remoteTorn = base + '{"type":"message","id":"torn-li' + "\n";
  await mem.put("sessions/sid.jsonl", remoteTorn);
  const p = persisterOf(mem);
  const r = await p.persistJsonlToBos("sid", base + jsonlLine("c3d4e5f6", "b2c3d4e5", "user", "next") + "\n");
  assert.equal(r, undefined);
  assert.equal(mem.objects.get("sessions/sid.jsonl").body, remoteTorn, "含坏行的远端必须原样保留");
});

test("persistJsonlToBos: 新会话撞已有 id 拒绝整体覆盖 (RPO=0)", async () => {
  const mem = memoryStore();
  const full = sampleJsonl("old-history-1", "old-history-2");
  await mem.put("sessions/sid.jsonl", full);
  const fresh = [
    JSON.stringify({ type: "session", version: 3, id: "uuid-fresh", timestamp: "2026-08-19T02:00:00.000Z", cwd: "/tmp" }),
    jsonlLine("ffff0001", null, "user", "brand new"),
  ].join("\n") + "\n";
  const p = persisterOf(mem);
  const r = await p.persistJsonlToBos("sid", fresh);
  assert.equal(r, undefined, "拒绝是终态, 不重试");
  assert.equal(mem.objects.get("sessions/sid.jsonl").body, full, "远端历史必须原样保留");
});

test("persistJsonlToBos: 本地落后于远端 (别处已写更多) 拒绝覆盖", async () => {
  const mem = memoryStore();
  const base = sampleJsonl();
  const remoteAhead = base + jsonlLine("c3d4e5f6", "b2c3d4e5", "assistant", "written elsewhere") + "\n";
  await mem.put("sessions/sid.jsonl", remoteAhead);
  const p = persisterOf(mem);
  const r = await p.persistJsonlToBos("sid", base);
  assert.equal(r, undefined);
  assert.equal(mem.objects.get("sessions/sid.jsonl").body, remoteAhead);
});

test("persistJsonlToBos: 同谱系追加扩展正常覆盖", async () => {
  const mem = memoryStore();
  const base = sampleJsonl();
  await mem.put("sessions/sid.jsonl", base);
  const extended = base + jsonlLine("c3d4e5f6", "b2c3d4e5", "user", "next turn") + "\n";
  const p = persisterOf(mem);
  const r = await p.persistJsonlToBos("sid", extended);
  assert.equal(r, undefined);
  assert.equal(mem.objects.get("sessions/sid.jsonl").body, extended);
});

test("resolveStore: settings.json 损坏 (SyntaxError) 不无限重试", async () => {
  const mem = memoryStore();
  const p = createPersister({
    get: (key) => mem.get(key),
    put: (key, content, extra) => mem.put(key, content, extra),
    probe: probeOk,
    warn: silent,
    sleep: async () => {},
    loadStore: () => JSON.parse("{corrupt"),
  });
  const r = await p.persistJsonlToBos("sid", sampleJsonl());
  assert.equal(r, undefined, "配置损坏是终态, 不能返回 retry");
  assert.equal(mem.puts.length, 0);
});

test("默认 warn 按 channel 去重, 重试不刷屏", async () => {
  const seen = [];
  const orig = console.warn;
  console.warn = (m) => seen.push(m);
  try {
    const mem = memoryStore();
    const p = createPersister({
      store,
      get: (key) => mem.get(key),
      put: (key, content, extra) => mem.put(key, content, extra),
      probe: async () => ({ ok: false, transient: true, error: "create-failed", message: "网络抖动" }),
      sleep: async () => {},
      loadStore: () => store,
    });
    const r1 = await p.persistJsonlToBos("sid", sampleJsonl());
    const r2 = await p.persistJsonlToBos("sid", sampleJsonl());
    assert.equal(r1, "retry");
    assert.equal(r2, "retry");
    assert.equal(seen.length, 1, `同 channel 警告只打一次, 实际 ${seen.length} 次`);
  } finally {
    console.warn = orig;
  }
});

test("队列: 同会话 JSONL 合并为最新快照", async () => {
  const mem = memoryStore();
  let hold;
  const gate = new Promise((r) => { hold = r; });
  let firstGet = true;
  const origGet = mem.get.bind(mem);
  mem.get = async (key) => {
    if (firstGet) {
      firstGet = false;
      await gate;
    }
    return origGet(key);
  };
  const p = persisterOf(mem);
  p.queueJsonlWrite("sid", sampleJsonl("one"));
  p.queueJsonlWrite("sid", sampleJsonl("two"));
  p.queueJsonlWrite("sid", sampleJsonl("three"));
  await new Promise((r) => setTimeout(r, 20));
  assert.ok(p.pending.length <= 2, `pending=${p.pending.length}, 同会话 JSONL 应合并`);
  hold();
  await p.flush(500);
  assert.equal(p.pending.length, 0);
  const got = await mem.get("sessions/sid.jsonl");
  assert.equal(got.ok, true);
  assert.match(got.body, /three/);
  assert.doesNotMatch(got.body, /"text":"one"/);
});

// ---- v0.4.2: 旧 turns → Pi JSONL 迁移 ----
const legacyTurns = [
  { turn: 1, role: "user", ts: 1720000000000, msg: "修并发 bug", content: [{ type: "text", text: "修并发 bug" }] },
  { turn: 2, role: "assistant", ts: 1720000001000, msg: "看一下 [工具调用: read(...)]",
    content: [
      { type: "thinking", thinking: "先读文件" },
      { type: "text", text: "看一下" },
      { type: "toolCall", id: "call_abc123", name: "read", arguments: { path: "a.js" } },
    ],
    toolResults: [{ toolName: "read", content: [{ type: "text", text: "文件内容" }] }] },
];

test("jsonlFromTurns: 产出合法 Pi JSONL 首行 + 谱系链", () => {
  const { jsonl, turns, messages } = jsonlFromTurns("sess-x", legacyTurns, { cwd: "/tmp" });
  assert.equal(turns, 2);
  assert.equal(messages, 3, "assistant 轮的 toolResult 展开成独立 message");
  assert.equal(isJsonlBody(jsonl), true);
  const lines = jsonl.trim().split("\n").map((l) => JSON.parse(l));
  assert.equal(lines[0].type, "session");
  assert.equal(lines[0].version, 3);
  assert.equal(lines[0].id, "sess-x");
  assert.ok(lines[0].migratedFrom, "必须标注迁移来源, 不冒充原始采样");
  // parentId 串成单链
  assert.equal(lines[1].parentId, null);
  for (let i = 2; i < lines.length; i++) assert.equal(lines[i].parentId, lines[i - 1].id);
});

test("jsonlFromTurns: toolCallId 按顺序+名字重链回 toolCall", () => {
  const { jsonl } = jsonlFromTurns("sess-x", legacyTurns);
  const msgs = jsonl.trim().split("\n").map((l) => JSON.parse(l)).filter((e) => e.type === "message");
  const tr = msgs.find((e) => e.message.role === "toolResult");
  assert.equal(tr.message.toolCallId, "call_abc123");
  assert.equal(tr.message.toolName, "read");
  assert.equal(tr.message.content[0].text, "文件内容");
});

test("jsonlFromTurns: 不可恢复的元数据标为迁移占位, 不伪造真实模型名", () => {
  const { jsonl } = jsonlFromTurns("sess-x", legacyTurns);
  const a = jsonl.trim().split("\n").map((l) => JSON.parse(l))
    .find((e) => e.type === "message" && e.message.role === "assistant");
  assert.equal(a.message.model, MIGRATED_MODEL);
  assert.match(a.message.model, /migrated/, "占位必须自我标识, 不能像真实模型名");
  assert.equal(a.message.usage.input, 0);
  assert.equal(a.message.stopReason, "stop");
  // thinking / text 块原样保留
  assert.equal(a.message.content.find((b) => b.type === "thinking").thinking, "先读文件");
});

test("jsonlFromTurns: 乱序 turn 按序号排序, 空输入产出仅头部", () => {
  const { jsonl } = jsonlFromTurns("s", [{ turn: 2, role: "assistant", msg: "第二" }, { turn: 1, role: "user", msg: "第一" }]);
  const msgs = jsonl.trim().split("\n").map((l) => JSON.parse(l)).filter((e) => e.type === "message");
  assert.equal(msgs[0].message.role, "user");
  const empty = jsonlFromTurns("s", []);
  assert.equal(empty.messages, 0);
  assert.equal(isJsonlBody(empty.jsonl), true);
});

test("jsonlFromTurns 产物可被 jsonlSupersedes 视为自身前缀 (可继续追加)", () => {
  const { jsonl } = jsonlFromTurns("sess-x", legacyTurns);
  assert.equal(jsonlSupersedes(jsonl, jsonl), true);
  const truncated = jsonl.trim().split("\n").slice(0, 2).join("\n") + "\n";
  assert.equal(jsonlSupersedes(jsonl, truncated), true, "迁移后继续写入不被谱系保护挡住");
});

test("写放大告警: 超阈值时提示整文件重传成本", async () => {
  const mem = memoryStore();
  const seen = [];
  const p = createPersister({
    store, get: (k) => mem.get(k), put: (k, c, e) => mem.put(k, c, e),
    probe: probeOk, sleep: async () => {}, loadStore: () => store,
    warn: (ch, msg) => seen.push([ch, msg]),
  });
  const pad = "x".repeat(JSONL_SIZE_WARN_BYTES);
  const big = sampleJsonl(pad, "ok");
  await p.persistJsonlToBos("sid", big);
  assert.ok(seen.some(([ch]) => ch === "jsonl-size"), `应有 jsonl-size 提示: ${JSON.stringify(seen.map(x=>x[0]))}`);
  assert.equal((await mem.get("sessions/sid.jsonl")).ok, true, "告警不阻止写入");
});

test("迁移产物必须真能被 Pi SessionManager.open 打开 (无 pi 则跳过)", async (t) => {
  let pi;
  try { pi = await import("@earendil-works/pi-coding-agent"); }
  catch (e) { return t.skip("pi 引擎未安装"); }
  const { mkdtempSync, writeFileSync, rmSync } = await import("node:fs");
  const { tmpdir } = await import("node:os");
  const { join } = await import("node:path");
  const dir = mkdtempSync(join(tmpdir(), "celagent-mig-test-"));
  try {
    const { jsonl, messages } = jsonlFromTurns("sess-mig", legacyTurns, { cwd: dir });
    const file = join(dir, "sess-mig.jsonl");
    writeFileSync(file, jsonl, "utf8");
    const sm = pi.SessionManager.open(file, dir, dir);
    const entries = (typeof sm.getEntries === "function" ? sm.getEntries() : []) || [];
    const msgs = entries.filter((e) => e?.type === "message");
    assert.equal(msgs.length, messages, "Pi 载入的 message 数必须与转换数一致");
    assert.deepEqual(msgs.map((e) => e.message.role), ["user", "assistant", "toolResult"]);
    assert.equal(msgs[2].message.toolCallId, "call_abc123");
  } finally {
    try { rmSync(dir, { recursive: true, force: true }); } catch (e) { /* ignore */ }
  }
});
