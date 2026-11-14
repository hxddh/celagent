// Celld worker — optional session cache for celagent 1.0.
// 会话权威在对象存储（v2 segments）；这个 worker 只做节点本地的快速缓存，
// 丢了可重建。任务/ledger/obj 模拟接口已在 1.0 删除。
const MAX_MSG = 8000;

function json(body, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function checkToken(req, env) {
  const expected = env && env.CELAGENT_WORKER_TOKEN;
  if (!expected) return false; // fail-closed；setup/install 会注入 CELLD_VAR_CELAGENT_WORKER_TOKEN
  const header = req.headers.get("X-Celagent-Token") || "";
  const auth = req.headers.get("Authorization") || "";
  const bearer = auth.toLowerCase().startsWith("bearer ") ? auth.slice(7).trim() : "";
  return header === expected || bearer === expected;
}

function mergeTurns(existing, incoming) {
  const by = new Map();
  for (const t of existing || []) {
    const n = Number(t?.turn);
    if (Number.isFinite(n)) by.set(n, t);
  }
  for (const t of incoming || []) {
    const n = Number(t?.turn);
    if (!Number.isFinite(n)) continue;
    const prev = by.get(n);
    if (!prev || (t.msg || "").length >= (prev.msg || "").length) by.set(n, { ...prev, ...t, turn: n });
  }
  return [...by.values()].sort((a, b) => Number(a.turn) - Number(b.turn));
}

export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    const name = url.searchParams.get("agent") || url.pathname.split("/")[2] || "default";
    const id = env.AGENT_RUNTIME.idFromName(name);
    return env.AGENT_RUNTIME.get(id).fetch(req);
  },
};

export class AgentRuntime {
  constructor(state, env) {
    this.state = state;
    this.env = env;
  }

  async fetch(req) {
    const url = new URL(req.url);
    const action = url.searchParams.get("action") || "status";
    if (!checkToken(req, this.env)) return json({ ok: false, error: "unauthorized" }, 401);
    const sessionId = url.searchParams.get("session") || "default";
    const key = `session:${sessionId}`;

    if (action === "resume" || action === "status") {
      const session = (await this.state.storage.get(key)) || null;
      return json({ ok: Boolean(session), sessionId, session });
    }
    if (action === "checkpoint") {
      const body = req.method === "POST" ? await req.json().catch(() => ({})) : {};
      const turn = Number.parseInt(String(body.turn ?? url.searchParams.get("turn") ?? "0"), 10);
      if (!Number.isFinite(turn) || turn < 0) return json({ ok: false, error: "invalid-turn" }, 400);
      const msg = String(body.msg ?? url.searchParams.get("msg") ?? "").slice(0, MAX_MSG);
      const role = body.role || url.searchParams.get("role") || "assistant";
      const existing = (await this.state.storage.get(key)) || { id: sessionId, turns: [] };
      existing.turns = mergeTurns(existing.turns, [{ turn, role, msg, ts: Date.now() }]);
      existing.updatedAt = Date.now();
      await this.state.storage.put(key, existing);
      return json({ ok: true, sessionId, turn, turns: existing.turns.length });
    }
    if (action === "sync") {
      const body = await req.json().catch(() => null);
      if (!body || !Array.isArray(body.turns)) return json({ ok: false, error: "invalid-body" }, 400);
      const existing = (await this.state.storage.get(key)) || { id: sessionId, turns: [] };
      const turns = mergeTurns(existing.turns, body.turns);
      await this.state.storage.put(key, { id: sessionId, turns, updatedAt: Date.now() });
      return json({ ok: true, sessionId, turns: turns.length });
    }
    if (action === "hibernate") {
      const existing = await this.state.storage.get(key);
      if (!existing) return json({ ok: false, error: "not-found" }, 404);
      await this.state.storage.put(`hibernate:${sessionId}`, { id: sessionId, turns: existing.turns, sleptAt: Date.now() });
      await this.state.storage.delete(key);
      return json({ ok: true, sessionId, turns: existing.turns.length });
    }
    if (action === "wake") {
      const sleeping = await this.state.storage.get(`hibernate:${sessionId}`);
      if (!sleeping) return json({ ok: false, error: "not-sleeping" }, 404);
      await this.state.storage.put(key, { id: sessionId, turns: sleeping.turns, wokeAt: Date.now() });
      await this.state.storage.delete(`hibernate:${sessionId}`);
      return json({ ok: true, sessionId, turns: sleeping.turns.length });
    }
    return json({ ok: false, error: "unknown action" }, 404);
  }
}
