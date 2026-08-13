// Agent Orchestration Runtime — 真实 agent 业务语义
// 每个 agent 一个 Durable Object cell:
//   - 会话状态 + 任务状态机(checkpoint 逐步推进)
//   - 工具调用(模拟外部系统,带幂等键 + execution ledger)
//   - alarm 驱动(定时任务/重试退避/deadline)
//   - 跨 cell 委托调用(Agent A 委托 Agent B)
// v2 新增:
//   - 任务产物写对象存储(经 webhook 代理, worker 零凭证)
//   - 真实 webhook 副作用(HTTP 端点,服务端幂等去重)

// 工具调用记录(单 cell ledger 去重, 不是跨节点共识)
const LEDGER_KEY = 'ledger';

// ===== v2: 对象存储交互(经 webhook 代理, worker 零凭证) =====
// 凭证由 webhook 端点持有; worker token 经 v0.2 CELLD_VAR_* / wrangler vars 注入
// (v0.1 manifest vars=null, 进程 env 进不了 DO env → checkToken fail-open)
function isLoopbackHost(hostname) {
  return hostname === '127.0.0.1' || hostname === 'localhost' || hostname === '::1';
}

function webhookBase(env) {
  const raw = (env && env.CELAGENT_WEBHOOK_BASE) || 'http://127.0.0.1:19090';
  try {
    const u = new URL(raw);
    const allowRemote = env && (env.CELAGENT_WEBHOOK_ALLOW_REMOTE === '1' || env.CELAGENT_WEBHOOK_ALLOW_REMOTE === 'true');
    if (!isLoopbackHost(u.hostname) && !allowRemote) return 'http://127.0.0.1:19090';
    return String(raw).replace(/\/$/, '');
  } catch (e) {
    return 'http://127.0.0.1:19090';
  }
}

function mergeTurns(existingTurns, incomingTurns) {
  const byTurn = new Map();
  for (const t of existingTurns || []) {
    const n = Number(t?.turn);
    if (Number.isFinite(n)) byTurn.set(n, t);
  }
  for (const t of incomingTurns || []) {
    const n = Number(t?.turn);
    if (!Number.isFinite(n)) continue;
    const prev = byTurn.get(n);
    if (!prev) { byTurn.set(n, t); continue; }
    const merged = { ...prev, ...t, turn: n };
    if (prev.content && !t.content) merged.content = prev.content;
    if (prev.toolResults && !t.toolResults) merged.toolResults = prev.toolResults;
    if ((t.msg || '').length < (prev.msg || '').length) merged.msg = prev.msg;
    byTurn.set(n, merged);
  }
  return [...byTurn.values()].sort((a, b) => Number(a.turn) - Number(b.turn));
}

function tokenEquals(a, b) {
  const enc = new TextEncoder();
  const ba = enc.encode(String(a));
  const bb = enc.encode(String(b));
  const te = globalThis.crypto?.subtle?.timingSafeEqual;
  if (typeof te === 'function' && ba.byteLength === bb.byteLength) {
    try { return te.call(globalThis.crypto.subtle, ba, bb); } catch (e) { /* fall through */ }
  }
  return a === b;
}

function checkToken(req, env) {
  const expected = env && env.CELAGENT_WORKER_TOKEN;
  if (!expected) return true;
  const header = req.headers.get('X-Celagent-Token') || '';
  const auth = req.headers.get('Authorization') || '';
  const bearer = auth.toLowerCase().startsWith('bearer ') ? auth.slice(7).trim() : '';
  return tokenEquals(header, expected) || tokenEquals(bearer, expected);
}

async function bosPutProxy(env, key, content) {
  try {
    const resp = await fetch(`${webhookBase(env)}/obj-put`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ key, content }),
    });
    if (resp.ok) return await resp.json();
    return { ok: false, status: resp.status };
  } catch (e) {
    return { ok: false, status: 0, error: String(e) };
  }
}
async function webhookCall(env, tool, payload) {
  const opId = payload.opId;
  try {
    const resp = await fetch(`${webhookBase(env)}/webhook`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        'X-Idempotency-Key': opId,
      },
      body: JSON.stringify({ tool, ...payload }),
    });
    if (resp.ok) return await resp.json();
    return { error: `webhook ${resp.status}` };
  } catch (e) {
    return { error: String(e) };
  }
}

export default {
  async fetch(req, env) {
    const url = new URL(req.url);
    const name = url.searchParams.get('agent') || url.pathname.split('/')[2] || 'default';
    const id = env.AGENT_RUNTIME.idFromName(name);
    const stub = env.AGENT_RUNTIME.get(id);
    return stub.fetch(req);
  }
};

export class AgentRuntime {
  constructor(state, env) {
    this.state = state;
    this.env = env;
  }

  async fetch(req) {
    const url = new URL(req.url);
    const action = url.searchParams.get('action') || 'status';
    const agent = url.searchParams.get('agent') || 'default';
    if (!checkToken(req, this.env)) {
      return new Response(JSON.stringify({ ok: false, error: 'unauthorized' }), { status: 401 });
    }

    switch (action) {
      case 'submit': {
        // 提交新任务: type=short|long|delegate|scheduled
        const type = url.searchParams.get('type') || 'short';
        const steps = parseInt(url.searchParams.get('steps') || (type === 'long' ? 15 : 3));
        const taskId = 'task_' + Date.now().toString(36) + Math.random().toString(36).slice(2, 6);
        const task = {
          id: taskId, type, steps, step: 0, status: 'pending',
          createdAt: Date.now(), updatedAt: Date.now(),
          completedSteps: [], retries: 0, target: url.searchParams.get('target') || null
        };
        // 写入任务并立即触发首步执行(alarm 驱动)
        await this.state.storage.put(`task:${taskId}`, task);
        await this.state.storage.put(`agent:${agent}:active_tasks`, (await this.state.storage.get(`agent:${agent}:active_tasks`) || 0) + 1);
        await this.scheduleNextAlarm();
        return new Response(JSON.stringify({ taskId, status: task.status, steps, agent }));
      }

      case 'status': {
        const taskId = url.searchParams.get('task');
        if (taskId) {
          const task = await this.state.storage.get(`task:${taskId}`);
          return new Response(JSON.stringify(task ?? { error: 'not found', taskId }));
        }
        // 全部任务摘要
        const tasks = await this.state.storage.list({ prefix: 'task:', limit: 200 });
        const out = [];
        for (const [k, v] of tasks) out.push({ id: k.slice(5), ...v });
        return new Response(JSON.stringify(out));
      }

      case 'ledger': {
        const ledger = await this.state.storage.get(LEDGER_KEY);
        return new Response(JSON.stringify(ledger ?? []));
      }

      case 'schedule': {
        // 定时任务: 每 intervalMs 周期执行一次
        const intervalMs = parseInt(url.searchParams.get('interval') || '30000');
        const name = url.searchParams.get('name') || 'cron';
        const cron = {
          name, intervalMs, lastRunAt: 0, runCount: 0,
          nextRunAt: Date.now() + intervalMs
        };
        await this.state.storage.put(`cron:${name}`, cron);
        await this.scheduleNextAlarm();
        return new Response(JSON.stringify({ scheduled: name, intervalMs }));
      }

      case 'delegate': {
        // 跨 cell 委托: agent A 委托给 agent B 的任务
        const target = url.searchParams.get('target') || 'delegatee';
        const steps = parseInt(url.searchParams.get('steps') || '3');
        // 直接调用目标 agent 的 submit
        const targetId = this.env.AGENT_RUNTIME.idFromName(target);
        const targetStub = this.env.AGENT_RUNTIME.get(targetId);
        const resp = await targetStub.fetch(
          `http://internal/delegate?action=submit&type=short&steps=${steps}`
        );
        const body = await resp.json();
        // 记录委托关系
        await this.state.storage.put(`delegation:${body.taskId}`, {
          from: agent, to: target, taskId: body.taskId, ts: Date.now()
        });
        return new Response(JSON.stringify({ delegated: body }));
      }

      // ===== checkpoint 桥: Pi 风格会话持久化 =====
      case 'checkpoint': {
        // 每轮对话后, 客户端把会话状态写到这里 (RPO=0)
        // 优先 POST JSON body (避免 msg 进 URL/日志); query 仅兼容旧客户端
        const sessionId = url.searchParams.get('session') || 'default';
        let body = {};
        if (req.method === 'POST') {
          body = await req.json().catch(() => ({})) || {};
        }
        const rawTurn = body.turn ?? url.searchParams.get('turn') ?? '0';
        const turn = Number.parseInt(String(rawTurn), 10);
        if (!Number.isFinite(turn) || turn < 0) {
          return new Response(JSON.stringify({ ok: false, error: 'invalid-turn', turn: rawTurn }), { status: 400 });
        }
        const msg = String(typeof body.msg === 'string' ? body.msg : (url.searchParams.get('msg') || '')).slice(0, 8000);
        const role = body.role || url.searchParams.get('role') || 'assistant';
        const key = `session:${sessionId}`;
        const existing = await this.state.storage.get(key) || { id: sessionId, turns: [] };
        // 按 turn 去重: 同 turn 替换; 不同 turn 追加到末尾(续写, 不覆盖旧数据)
        const idx = existing.turns.findIndex(t => t.turn === turn);
        const entry = { turn, role, msg, ts: Date.now() };
        if (idx >= 0) {
          existing.turns[idx] = entry;
        } else {
          // 续写: 若传入 turn 与历史不连续(如读失败后 seq 从 0), 追加到末尾
          existing.turns.push(entry);
        }
        existing.updatedAt = Date.now();
        await this.state.storage.put(key, existing);
        return new Response(JSON.stringify({ ok: true, sessionId, turn, turns: existing.turns.length }));
      }

      case 'sync': {
        // 冷启动对齐: 按 turn 合并, 禁止盲写覆盖 worker 上更新的轮次
        const sessionId = url.searchParams.get('session') || 'default';
        const body = await req.json().catch(() => null);
        if (!body || !Array.isArray(body.turns)) {
          return new Response(JSON.stringify({ ok: false, error: 'invalid-body' }), { status: 400 });
        }
        const key = `session:${sessionId}`;
        const existing = await this.state.storage.get(key) || { id: sessionId, turns: [] };
        const turns = mergeTurns(existing.turns, body.turns);
        await this.state.storage.put(key, { id: sessionId, turns, updatedAt: Date.now() });
        return new Response(JSON.stringify({ ok: true, sessionId, turns: turns.length }));
      }

      case 'resume': {
        // 崩溃/重启后, 客户端从这里恢复完整会话
        const sessionId = url.searchParams.get('session') || 'default';
        const key = `session:${sessionId}`;
        const existing = await this.state.storage.get(key) || null;
        return new Response(JSON.stringify({ ok: existing !== null, sessionId, session: existing }));
      }

      case 'hibernate': {
        // P1: 休眠 — agent 空闲时休眠, 状态完整保留在 cell (storage),
        // 唤醒后从 storage 恢复 (状态不依赖节点存活)
        const sessionId = url.searchParams.get('session') || 'default';
        const key = `session:${sessionId}`;
        const existing = await this.state.storage.get(key);
        if (!existing) return new Response(JSON.stringify({ ok: false, error: 'not-found' }), { status: 404 });
        await this.state.storage.put(`hibernate:${sessionId}`, {
          id: sessionId, turns: existing.turns, sleptAt: Date.now(),
        });
        // 休眠: 释放活跃状态 (cell 可被节点回收), 仅保留 hibernate 记录
        await this.state.storage.delete(key);
        return new Response(JSON.stringify({ ok: true, sessionId, sleptAt: Date.now(), turns: existing.turns.length }));
      }

      case 'wake': {
        // P1: 唤醒 — 从 hibernate 记录恢复完整状态到活跃 cell
        const sessionId = url.searchParams.get('session') || 'default';
        const key = `hibernate:${sessionId}`;
        const sleeping = await this.state.storage.get(key);
        if (!sleeping) return new Response(JSON.stringify({ ok: false, error: 'not-sleeping' }), { status: 404 });
        await this.state.storage.put(`session:${sessionId}`, {
          id: sessionId, turns: sleeping.turns, updatedAt: Date.now(), wokeAt: Date.now(),
        });
        await this.state.storage.delete(key);
        return new Response(JSON.stringify({ ok: true, sessionId, wokeAt: Date.now(), turns: sleeping.turns.length }));
      }

      case 'hibernate-status': {
        // P1: 查询休眠状态
        const sessionId = url.searchParams.get('session') || 'default';
        const sleeping = await this.state.storage.get(`hibernate:${sessionId}`);
        const active = await this.state.storage.get(`session:${sessionId}`);
        return new Response(JSON.stringify({
          ok: true, sessionId,
          state: sleeping ? 'hibernated' : (active ? 'active' : 'none'),
          turns: (sleeping?.turns || active?.turns || []).length,
        }));
      }

      // ===== 通用 KV API: SessionStorage 映射层 =====
      case 'kv-put': {
        const k = url.searchParams.get('k');
        const v = url.searchParams.get('v');
        if (!k) return new Response(JSON.stringify({ error: 'no key' }), { status: 400 });
        await this.state.storage.put(`kv:${k}`, v ?? '');
        return new Response(JSON.stringify({ ok: true, k }));
      }

      case 'kv-get': {
        const k = url.searchParams.get('k');
        if (!k) return new Response(JSON.stringify({ error: 'no key' }), { status: 400 });
        const v = await this.state.storage.get(`kv:${k}`);
        return new Response(JSON.stringify({ ok: v !== undefined, k, v: v ?? null }));
      }

      case 'kv-list': {
        const prefix = url.searchParams.get('prefix') || '';
        const limit = parseInt(url.searchParams.get('limit') || '100');
        const entries = await this.state.storage.list({ prefix: `kv:${prefix}`, limit });
        const out = {};
        for (const [k, v] of entries) out[k.slice(3)] = v;
        return new Response(JSON.stringify({ ok: true, entries: out }));
      }

      case 'kv-delete': {
        const k = url.searchParams.get('k');
        if (!k) return new Response(JSON.stringify({ error: 'no key' }), { status: 400 });
        await this.state.storage.delete(`kv:${k}`);
        return new Response(JSON.stringify({ ok: true, k }));
      }

      // ===== v2: 对象存储交互(经 webhook 代理) =====
      case 'obj-put': {
        const key = url.searchParams.get('key') || `workspace/${agent}/file-${Date.now()}.txt`;
        const content = url.searchParams.get('content') || 'agent-file-content';
        const result = await bosPutProxy(this.env, key, content);
        // 记录到 storage(可查询)
        await this.state.storage.put(`obj:${key}`, { key, content, ts: Date.now(), ok: result.ok });
        return new Response(JSON.stringify({ action, key, result }));
      }

      case 'obj-get': {
        const key = url.searchParams.get('key');
        if (!key) return new Response(JSON.stringify({ error: 'no key' }), { status: 400 });
        // 通过 webhook 代理读取(简化: 直接查询 storage 记录 + BOS 列表)
        const record = await this.state.storage.get(`obj:${key}`);
        return new Response(JSON.stringify({ action, key, record }));
      }

      case 'webhook-test': {
        const tool = url.searchParams.get('tool') || 'payment';
        const opId = url.searchParams.get('opId') || `test-${Date.now()}`;
        const result = await webhookCall(this.env, tool, { opId, ts: Date.now() });
        return new Response(JSON.stringify({ action, opId, result }));
      }

      // ===== v3: 并发写测试(epoch 语义 + 版本 CAS) =====
      case 'cwrite': {
        const clientId = url.searchParams.get('client') || 'c1';
        const seq = parseInt(url.searchParams.get('seq') || '1');
        const key = url.searchParams.get('key') || 'cw';
        const val = url.searchParams.get('val') || 'v';
        const expectVer = url.searchParams.get('ver');  // 期望版本(乐观锁)
        const session = url.searchParams.get('session') || 'default';
        // P1: epoch fencing — 会话级单写者锁 (原子获取: noOverwrite)
        // 修复竞态: 原实现'检查-写入-删除'三步非原子, 并发双方都能通过检查
        const writeKey = `writer:${session}`;
        const LOCK_TTL_MS = 15000;
        const meta = { clientId, seq, ts: Date.now() };
        try {
          await this.state.storage.put(writeKey, meta, { noOverwrite: true });
        } catch (e) {
          const current = await this.state.storage.get(writeKey);
          if (current?.ts && (Date.now() - current.ts) > LOCK_TTL_MS) {
            await this.state.storage.delete(writeKey);
            try {
              await this.state.storage.put(writeKey, meta, { noOverwrite: true });
            } catch (e2) {
              return new Response(JSON.stringify({ conflict: true, reason: 'writer-busy', current: await this.state.storage.get(writeKey) }));
            }
          } else {
            return new Response(JSON.stringify({ conflict: true, reason: 'writer-busy', current }));
          }
        }
        try {
          // 乐观锁: 若指定了期望版本, 校验当前值版本
          const existing = await this.state.storage.get(key);
          if (expectVer !== null && existing && String(existing.ver) !== expectVer) {
            return new Response(JSON.stringify({
              conflict: true, reason: 'version-mismatch',
              expected: expectVer, actual: existing.ver
            }));
          }
          // 版本递增(从 0 或现有版本 +1)
          const nextVer = (existing && existing.ver !== undefined ? existing.ver : 0) + 1;
          await this.state.storage.put(key, { clientId, seq, val, ver: nextVer, ts: Date.now() });
          return new Response(JSON.stringify({ ok: true, clientId, seq, key, val, ver: nextVer }));
        } finally {
          await this.state.storage.delete(writeKey);  // 释放锁
        }
      }

      case 'cget': {
        const key = url.searchParams.get('key') || 'cw';
        const v = await this.state.storage.get(key);
        return new Response(JSON.stringify({ key, value: v }));
      }

      default:
        return new Response(JSON.stringify({ error: 'unknown action' }), { status: 400 });
    }
  }

  async scheduleNextAlarm() {
    const now = Date.now();
    let next = Infinity;
    const crons = await this.state.storage.list({ prefix: 'cron:', limit: 50 });
    for (const [, cron] of crons) {
      if (cron?.nextRunAt) next = Math.min(next, cron.nextRunAt);
    }
    const tasks = await this.state.storage.list({ prefix: 'task:', limit: 100 });
    for (const [, task] of tasks) {
      if (task?.status === 'pending' && task.step < task.steps) next = Math.min(next, now + 50);
    }
    if (Number.isFinite(next) && next < Infinity) {
      await this.state.storage.setAlarm(Math.max(next, now + 10));
    }
  }

  async alarm() {
    // alarm 触发: 检查任务续跑、定时任务、deadline
    const now = Date.now();

    // 1. 定时任务: 检查所有 cron
    const crons = await this.state.storage.list({ prefix: 'cron:', limit: 50 });
    for (const [key, cron] of crons) {
      if (cron.nextRunAt <= now) {
        cron.runCount += 1;
        cron.lastRunAt = now;
        cron.nextRunAt = now + cron.intervalMs;
        await this.state.storage.put(key, cron);
        await this.recordToolCall(`cron:${cron.name}`, { run: cron.runCount, at: now });
      }
    }

    // 2. 任务续跑: 找所有 pending 任务推进
    const tasks = await this.state.storage.list({ prefix: 'task:', limit: 100 });
    for (const [key, task] of tasks) {
      if (task.status === 'pending' && task.step < task.steps) {
        await this.executeStep(task);
        await this.state.storage.put(key, task);
      }
    }
    await this.scheduleNextAlarm();
  }

  // 模拟真实 agent 的工具调用: 外部副作用 + 幂等 ledger
  async executeStep(task) {
    const agent = task.target || 'default';
    task.step += 1;
    task.updatedAt = Date.now();

    // 工具调用(带幂等键) — v3: webhook 尽力而为, 失败不阻塞任务推进
    const toolName = task.step % 3 === 0 ? 'payment' : (task.step % 3 === 1 ? 'email' : 'search');
    const opId = `${task.id}:${task.step}:${toolName}`;
    const result = await this.recordToolCall(toolName, { taskId: task.id, step: task.step, opId });

    task.completedSteps.push({
      step: task.step, tool: toolName, result, ts: Date.now()
    });

    // checkpoint(每步都写,模拟逐步推进)
    if (task.step >= task.steps) {
      task.status = 'done';
      task.completedAt = Date.now();
      // 减活跃计数
      const active = await this.state.storage.get(`agent:${agent}:active_tasks`) || 0;
      await this.state.storage.put(`agent:${agent}:active_tasks`, Math.max(0, active - 1));
      // v3: 任务完成时把产物经 webhook 代理写到 BOS workspace(尽力而为, 失败记入任务)
      const putResult = await bosPutProxy(this.env, `workspace/${agent}/task-${task.id}.json`, JSON.stringify(task));
      task.bosResult = putResult;
    } else {
      task.status = 'pending';
    }
    await this.state.storage.put(`task:${task.id}`, task);
    return result;
  }

  // 幂等工具调用记录(execution ledger) — 先写 pending 再副作用, 崩溃重放靠 X-Idempotency-Key
  async recordToolCall(tool, payload) {
    const ledger = await this.state.storage.get(LEDGER_KEY) || [];
    const opId = payload.opId || `${Date.now()}:${tool}:${Math.random().toString(36).slice(2, 8)}`;
    const existing = ledger.find(e => e.opId === opId);
    if (existing && existing.status !== 'pending') {
      return { ...existing, deduped: true };
    }
    let entry = existing;
    if (!entry) {
      entry = { opId, tool, ts: Date.now(), ...payload, status: 'pending', deduped: false };
      ledger.push(entry);
      await this.state.storage.put(LEDGER_KEY, ledger);
    }
    const webhookResult = await webhookCall(this.env, tool, { opId, ...payload });
    entry.status = 'done';
    entry.webhookResult = webhookResult;
    await this.state.storage.put(LEDGER_KEY, ledger);
    return entry;
  }
}
