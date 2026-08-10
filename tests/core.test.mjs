// celagent 核心链路测试 (node:test)
// 覆盖: 配置读写 / Celld KV 交互 / 历史 checkpoint+恢复 / Agent 构造
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

// 测试环境: 需要 celld 节点在跑 (node_mgr.sh start)
const NODES = ["http://127.0.0.1:18090", "http://127.0.0.1:18091"];
const AGENT = "celagent-test";

async function celld(action, params = {}) {
  const q = new URLSearchParams(params).toString();
  for (const base of NODES) {
    try {
      const resp = await fetch(`${base}/agent/${AGENT}?action=${action}&${q}`, {
        signal: AbortSignal.timeout(8000),
      });
      return await resp.json();
    } catch (e) { /* try next */ }
  }
  return { error: "celld unreachable" };
}

function mockLlm(prompt) {
  return `[mock] ${prompt.slice(0, 30)}`;
}

test("1. Celld 可达 (checkpoint/resume API)", async () => {
  const r = await celld("resume", { session: "t1" });
  assert.ok(r.ok === false || r.ok === true, "resume API 响应正常");
});

test("2. checkpoint + resume 往返", async () => {
  const sid = `t-${Date.now()}`;
  const r1 = await celld("checkpoint", { session: sid, turn: 1, msg: "hello" });
  assert.equal(r1.ok, true);
  const r2 = await celld("checkpoint", { session: sid, turn: 2, msg: "world" });
  assert.equal(r2.ok, true);
  const r3 = await celld("resume", { session: sid });
  assert.equal(r3.ok, true);
  assert.equal(r3.session.turns.length, 2, "两轮历史完整");
});

test("3. kv-put/get/list/delete 往返", async () => {
  const k = `k-${Date.now()}`;
  await celld("kv-put", { k, v: "v1" });
  const g = await celld("kv-get", { k });
  assert.equal(g.v, "v1");
  const l = await celld("kv-list", { prefix: k.slice(0, 5), limit: 10 });
  assert.ok(Object.keys(l.entries).length >= 1);
  await celld("kv-delete", { k });
  const g2 = await celld("kv-get", { k });
  assert.equal(g2.ok, false, "删除后不存在");
});

test("4. Agent 构造 (mock streamFn, 无 key 也工作)", async () => {
  const { Agent } = await import("@earendil-works/pi-agent-core");
  const { TOOLS } = await import("../src/tools.js");
  // 无 key: buildStreamFn 返回错误事件, Agent 应能构造
  const { buildStreamFn } = await import("../src/llm.js");
  const agent = new Agent({
    streamFn: buildStreamFn(),
    initialState: { systemPrompt: "test", tools: TOOLS, messages: [] },
    getApiKey: () => undefined,
  });
  assert.ok(agent, "Agent 构造成功");
});

test("5. 工具定义合法 (TypeBox schema + execute)", async () => {
  const { TOOLS } = await import("../src/tools.js");
  assert.ok(Array.isArray(TOOLS) && TOOLS.length >= 3, "至少 3 个工具");
  for (const t of TOOLS) {
    assert.ok(t.name && t.description && t.execute, `工具 ${t.name} 完整`);
    assert.ok(t.parameters, `工具 ${t.name} 有 schema`);
  }
  // get_time 执行
  const timeResult = await TOOLS[0].execute("id", {});
  assert.ok(timeResult.content[0].type === "text", "get_time 返回文本");
});
