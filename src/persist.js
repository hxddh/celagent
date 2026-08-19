// persist.js — 会话权威写/恢复 (BOS 队列 + CAS 门禁 + BOS-first)
// TUI 只编排; 本模块可注入 get/put/probe, 单测不需要 aws CLI
// v0.4: 权威对象是 Pi JSONL (sessions/<id>.jsonl); 旧 sessions/<id>.json 仅兼容读
import { homedir } from "node:os";
import { join, basename } from "node:path";
import { existsSync, readFileSync } from "node:fs";
import { bosGet, bosPut, casGateSticky, probeStoreCas, resolveEndpoint, resolveRegion } from "./bos.js";

export const BOS_QUEUE_MAX = 50;
export const STEER_LEGACY_HEADER = "以下是本会话之前的对话历史";

export function sessionJsonlKey(sessionId) {
  return `sessions/${sessionId}.jsonl`;
}

export function sessionTurnsKey(sessionId) {
  return `sessions/${sessionId}.json`;
}

/** persistId = JSONL 文件名 stem, 与 celagent <id> / BOS key 同一条会话 */
export function persistIdFromJsonlPath(p) {
  const stem = basename(String(p || "")).replace(/\.jsonl$/i, "");
  if (/^[A-Za-z0-9._-]{1,128}$/.test(stem)) return stem;
  return `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

export function storeFromCfg(cfg) {
  const endpoint = resolveEndpoint(cfg?.persistence?.endpoint);
  const region = resolveRegion(endpoint, cfg?.persistence?.region);
  const profile = String(cfg?.persistence?.profile || "").trim() || "bos";
  return {
    bucket: cfg?.persistence?.bucket || null,
    endpoint,
    region,
    profile,
    awsExtra: { AWS_PROFILE: profile, ...(region ? { AWS_REGION: region } : {}) },
  };
}

export function defaultLoadStore() {
  const f = join(homedir(), ".config", "celagent", "settings.json");
  if (!existsSync(f)) {
    const err = new Error("no-config");
    err.code = "no-config";
    throw err;
  }
  return storeFromCfg(JSON.parse(readFileSync(f, "utf8")));
}

/** not-found / conflict / fatal(不重试) / transient(队列留队重试) */
export function classifyStoreError(error) {
  if (error == null || error === "") return "transient";
  const msg = String(error);
  if (error === "not-found" || /\bNoSuchKey\b|\b404\b/i.test(msg)) return "not-found";
  if (error === "conflict" || /\bPreconditionFailed\b|\b412\b/i.test(msg)) return "conflict";
  if (error === "endpoint-not-allowed" || error === "no-bucket" || error === "no-config") return "fatal";
  if (
    /AccessDenied|InvalidAccessKeyId|SignatureDoesNotMatch|ExpiredToken|PermanentRedirect|InvalidBucketName|NoSuchBucket|endpoint-not-allowed|\b403\b|\b401\b/i.test(msg)
  ) {
    return "fatal";
  }
  return "transient";
}

/** JSONL 条目 id 序列 (跳过无 id 行) — 覆盖保护的谱系判据。
 *  strict: 任何非空不可解析行 → 返回 null (谱系不可判) */
export function jsonlEntryIds(body, { strict = false } = {}) {
  const ids = [];
  if (typeof body !== "string") return strict ? null : ids;
  for (const line of body.split(/\r?\n/)) {
    if (!line.trim()) continue;
    try {
      const o = JSON.parse(line);
      if (o && typeof o.id === "string" && o.id) ids.push(o.id);
    } catch (e) {
      if (strict) return null;
    }
  }
  return ids;
}

/** Pi 会话文件是追加式日志 (compaction 也是追加条目, 不重写):
 *  远端 (旧快照) 的条目序列必须是本地 (新状态) 的前缀, 本地才可整体覆盖远端。
 *  否则本地是另一条谱系 (新建/分叉/别处已写入更多), 覆盖会永久丢远端数据。
 *  远端含坏行时谱系不可判 (isJsonlBody 只验首行, 坏行可能是可恢复数据) → 拒绝 */
export function jsonlSupersedes(localBody, remoteBody) {
  const remote = jsonlEntryIds(remoteBody, { strict: true });
  if (remote === null) return false;
  const local = jsonlEntryIds(localBody);
  if (remote.length > local.length) return false;
  for (let i = 0; i < remote.length; i++) {
    if (remote[i] !== local[i]) return false;
  }
  return true;
}

export function isJsonlBody(body) {
  if (typeof body !== "string") return false;
  const first = body.split(/\r?\n/).find((l) => l.trim());
  if (!first) return false;
  try {
    const h = JSON.parse(first);
    return h && h.type === "session";
  } catch (e) {
    return false;
  }
}

function textFromMessageContent(content) {
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  return content
    .filter((b) => b && b.type === "text" && b.text)
    .map((b) => b.text)
    .join(" ")
    .trim();
}

/** 从 Pi JSONL 抽出轮次摘要, 给 snapshot / worker sync / 旧搜索路径用 */
export function turnsFromJsonl(body) {
  const turns = [];
  if (typeof body !== "string") return turns;
  let n = 0;
  for (const line of body.split(/\r?\n/)) {
    if (!line.trim()) continue;
    let obj;
    try {
      obj = JSON.parse(line);
    } catch (e) {
      continue;
    }
    if (obj.type !== "message" || !obj.message) continue;
    const m = obj.message;
    n += 1;
    const role = m.role || "assistant";
    const entry = {
      turn: n,
      role,
      msg: textFromMessageContent(m.content) || m.toolName || "(无文本)",
      ts: Number(m.timestamp) || Date.parse(obj.timestamp) || Date.now(),
    };
    if (Array.isArray(m.content) && m.content.length) entry.content = m.content;
    else if (typeof m.content === "string" && m.content) {
      entry.content = [{ type: "text", text: m.content }];
    }
    if (role === "toolResult") {
      entry.toolResults = [{ toolName: m.toolName, content: Array.isArray(m.content) ? m.content : null }];
    }
    turns.push(entry);
  }
  return turns;
}

export function makeTurnEntry(sessionId, seq, role, msg, fullContent, fullToolResults) {
  const entry = { turn: seq, role, msg, ts: Date.now() };
  if (fullContent && fullContent.length > 0) entry.content = fullContent;
  if (fullToolResults && fullToolResults.length > 0) entry.toolResults = fullToolResults;
  return entry;
}

export function mergeTurn(session, seq, role, msg, fullContent, fullToolResults) {
  const maxTurnOf = () => {
    const nums = session.turns.map((t) => Number(t.turn)).filter((n) => Number.isFinite(n));
    return nums.length ? Math.max(...nums) : 0;
  };
  let finalSeq = seq;
  if (session.turns.length > 0 && !session.turns.some((t) => t.turn === seq)) {
    finalSeq = maxTurnOf() + 1;
  }
  const idx = session.turns.findIndex((t) => t.turn === finalSeq);
  if (idx >= 0) {
    const prev = session.turns[idx];
    // 同号且同源 (role+msg 相同) = 本端幂等重试 → 原位更新, 保留已有更全字段;
    // 同号但内容不同 = 并发写入者已占该号 (双端同起点续写) → 追加新号, 绝不覆盖对方的轮
    const sameOrigin = prev?.role === role && String(prev?.msg ?? "") === String(msg ?? "");
    if (!sameOrigin) {
      session.turns.push(makeTurnEntry(session.id, maxTurnOf() + 1, role, msg, fullContent, fullToolResults));
      session.updatedAt = Date.now();
      return;
    }
    const entry = makeTurnEntry(session.id, finalSeq, role, msg, fullContent, fullToolResults);
    if (prev?.content && !(fullContent && fullContent.length)) entry.content = prev.content;
    if (prev?.toolResults && !(fullToolResults && fullToolResults.length)) entry.toolResults = prev.toolResults;
    session.turns[idx] = entry;
  } else {
    session.turns.push(makeTurnEntry(session.id, finalSeq, role, msg, fullContent, fullToolResults));
  }
  session.updatedAt = Date.now();
}

export function storeCasKey(store) {
  return [store.endpoint, store.bucket, store.profile, store.region || ""].join("|");
}

/** 默认 warn 按 channel 去重 — 队列退避重试下同类警告只打一次, 不刷屏 TUI */
function makeChannelWarn() {
  const warned = new Set();
  return (ch, msg) => {
    if (warned.has(ch)) return;
    warned.add(ch);
    console.warn(msg);
  };
}

function defaultSleep(ms) {
  return new Promise((r) => {
    const t = setTimeout(r, ms);
    if (t.unref) t.unref();
  });
}

export function createPersister(deps = {}) {
  const warn = deps.warn || makeChannelWarn();
  const sleep = deps.sleep || defaultSleep;
  const get = deps.get || ((key, opts) => bosGet(key, opts));
  const put = deps.put || ((key, content, opts) => bosPut(key, content, opts));
  const probe = deps.probe || ((opts) => probeStoreCas(opts));
  const loadStore = deps.loadStore || defaultLoadStore;

  const bosPending = [];
  let bosPumping = false;
  let bosQueue = Promise.resolve();
  let casGateCache = { key: null, result: null };
  let casGateInflight = { key: null, promise: null };

  async function resolveStore() {
    try {
      return { store: deps.store || loadStore() };
    } catch (e) {
      if (e.code === "endpoint-not-allowed") {
        warn("persist", `  (警告: ${e.message})`);
        return { skip: true };
      }
      if (e.code === "no-config") {
        warn("persist", "  (警告: 未配置 persistence.bucket, 会话不会持久化)");
        return { skip: true };
      }
      if (e instanceof SyntaxError) {
        // settings.json 损坏是本地配置错误, 重试不会好转 — 与读路径 (当作 no-config) 一致, 不无限退避
        warn("persist", `  (警告: settings.json 损坏, 会话不会持久化: ${e.message})`);
        return { skip: true };
      }
      warn("persist", `  (警告: ${e.message})`);
      return { skip: true, retry: classifyStoreError(e.message) === "transient" };
    }
  }

  async function ensureStoreCas(store) {
    const key = storeCasKey(store);
    if (casGateCache.key === key && casGateCache.result) return casGateCache.result;
    if (casGateInflight.key === key && casGateInflight.promise) return casGateInflight.promise;
    const promise = (async () => {
      try {
        const r = await probe({
          bucket: store.bucket,
          endpoint: store.endpoint,
          profile: store.profile,
          region: store.region,
        });
        if (casGateSticky(r)) casGateCache = { key, result: r };
        return r;
      } finally {
        if (casGateInflight.promise === promise) casGateInflight = { key: null, promise: null };
      }
    })();
    casGateInflight = { key, promise };
    return promise;
  }

  async function gateStore() {
    const resolved = await resolveStore();
    if (resolved.skip) return { skip: true, retry: resolved.retry };
    const store = resolved.store;
    if (!store.bucket) {
      warn("persist", "  (警告: 未配置 persistence.bucket, 会话不会持久化)");
      return { skip: true };
    }
    const cas = await ensureStoreCas(store);
    if (!cas.ok) {
      if (cas.transient) {
        warn("cas-temp", `  (警告: CAS 探针暂未通过, 本轮写入将重试: ${cas.message || cas.error})`);
        return { skip: true, retry: true };
      }
      warn("cas", `  (警告: 此存储不能保证 RPO=0,拒绝权威写入: ${cas.message || cas.error})`);
      return { skip: true };
    }
    return { store };
  }

  async function persistJsonlToBos(sessionId, jsonlBody) {
    if (typeof jsonlBody !== "string" || !isJsonlBody(jsonlBody)) {
      warn("persist", `  (警告: 本地会话不是合法 Pi JSONL, 跳过权威写入: ${sessionId})`);
      return;
    }
    const gated = await gateStore();
    if (gated.skip) return gated.retry ? "retry" : undefined;
    const store = gated.store;
    const key = sessionJsonlKey(sessionId);
    const common = { bucket: store.bucket, endpoint: store.endpoint, profile: store.profile, region: store.region };
    for (let attempt = 0; attempt < 3; attempt++) {
      const existing = await get(key, common);
      if (existing.ok) {
        if (!isJsonlBody(existing.body)) {
          warn("persist", `  (警告: BOS JSONL 损坏, 跳过本轮以免覆盖历史: ${sessionId})`);
          return;
        }
        // RPO=0 覆盖保护: 远端必须是本地的谱系前缀。新建会话撞已有 id、
        // 另一实例已写入更多轮、本地落后于远端 — 都拒绝整体覆盖 (远端数据优先)
        if (!jsonlSupersedes(jsonlBody, existing.body)) {
          warn("persist", `  (警告: BOS 会话 ${sessionId} 与本地不同源或更完整, 拒绝整体覆盖以免丢历史 — 另起会话请换 ID)`);
          return;
        }
        const written = await put(key, jsonlBody, { ...common, ifMatch: existing.etag });
        if (written.ok) return;
        if (written.conflict) {
          await sleep(100);
          continue;
        }
        const kind = classifyStoreError(written.error);
        if (kind === "transient") return "retry";
        warn("persist", `  (警告: BOS JSONL 持久化失败: ${written.error || "未知错误"})`);
        return;
      }
      const kind = classifyStoreError(existing.error);
      if (kind === "not-found") {
        const created = await put(key, jsonlBody, { ...common, ifNoneMatch: true });
        if (created.ok) return;
        if (created.conflict) {
          await sleep(100);
          continue;
        }
        const ck = classifyStoreError(created.error);
        if (ck === "transient") return "retry";
        warn("persist", `  (警告: BOS JSONL 首写失败: ${created.error || "未知错误"})`);
        return;
      }
      if (kind === "transient") return "retry";
      warn("persist", `  (警告: BOS JSONL 读取失败, 跳过本轮持久化: ${existing.error})`);
      return;
    }
    return "retry";
  }

  async function persistTurnToBos(sessionId, seq, role, msg, fullContent, fullToolResults) {
    const gated = await gateStore();
    if (gated.skip) return gated.retry ? "retry" : undefined;
    const store = gated.store;
    const key = sessionTurnsKey(sessionId);
    const common = { bucket: store.bucket, endpoint: store.endpoint, profile: store.profile, region: store.region };
    for (let attempt = 0; attempt < 3; attempt++) {
      let session = { id: sessionId, turns: [] };
      let etag = undefined;
      const existing = await get(key, common);
      if (existing.ok) {
        try {
          session = JSON.parse(existing.body);
          if (!Array.isArray(session.turns)) session.turns = [];
        } catch (e) {
          warn("persist", `  (警告: BOS 会话 JSON 损坏, 跳过本轮以免覆盖历史: ${sessionId})`);
          return;
        }
        etag = existing.etag;
      } else if (classifyStoreError(existing.error) === "not-found") {
        session.turns.push(makeTurnEntry(sessionId, seq, role, msg, fullContent, fullToolResults));
        session.updatedAt = Date.now();
        const created = await put(key, session, { ...common, ifNoneMatch: true });
        if (created.ok) return;
        if (created.conflict) {
          await sleep(100);
          continue;
        }
        const kind = classifyStoreError(created.error);
        if (kind === "transient") return "retry";
        warn("persist", `  (警告: BOS 首写失败: ${created.error || "未知错误"})`);
        return;
      } else {
        const kind = classifyStoreError(existing.error);
        if (kind === "transient") return "retry";
        warn("persist", `  (警告: BOS 读取失败, 跳过本轮持久化: ${existing.error})`);
        return;
      }
      mergeTurn(session, seq, role, msg, fullContent, fullToolResults);
      const written = await put(key, session, { ...common, ifMatch: etag });
      if (written.ok) return;
      if (written.conflict) {
        await sleep(100);
        continue;
      }
      const kind = classifyStoreError(written.error);
      if (kind === "transient") return "retry";
      warn("persist", `  (警告: BOS 持久化失败: ${written.error || "未知错误"})`);
      return;
    }
    // 3 次 CAS 冲突仍未写入 — 留队, 对端写完后再合并
    return "retry";
  }

  function pumpBosQueue() {
    if (bosPumping) return;
    bosPumping = true;
    bosQueue = (async () => {
      let retries = 0;
      try {
        while (bosPending.length) {
          const job = bosPending[0];
          let outcome;
          try {
            if (job.kind === "jsonl") {
              outcome = await persistJsonlToBos(job.sessionId, job.jsonlBody);
            } else {
              outcome = await persistTurnToBos(job.sessionId, job.seq, job.role, job.msg, job.fullContent, job.fullToolResults);
            }
          } catch (e) {
            warn("persist", `  (警告: BOS 持久化异常: ${e.message})`);
            const kind = classifyStoreError(e.message);
            if (kind === "transient") outcome = "retry";
          }
          if (outcome === "retry") {
            retries += 1;
            const delay = Math.min(60000, 1000 * 2 ** Math.min(retries, 6));
            await sleep(delay);
            continue;
          }
          retries = 0;
          if (bosPending[0] === job) bosPending.shift();
        }
      } finally {
        bosPumping = false;
        if (bosPending.length) pumpBosQueue();
      }
    })();
  }

  function enqueue(job) {
    if (job.kind === "jsonl") {
      const start = bosPumping && bosPending.length ? 1 : 0;
      for (let i = bosPending.length - 1; i >= start; i--) {
        if (bosPending[i].kind === "jsonl" && bosPending[i].sessionId === job.sessionId) {
          bosPending.splice(i, 1);
        }
      }
    }
    if (bosPending.length >= BOS_QUEUE_MAX) {
      bosPending.shift();
      warn("queue", "  (警告: BOS 写队列过长, 丢弃最旧任务)");
    }
    bosPending.push(job);
    pumpBosQueue();
    return bosQueue;
  }

  function queueBosWrite(sessionId, seq, role, msg, opts = {}) {
    const { fullContent = null, fullToolResults = null } = opts || {};
    return enqueue({ kind: "turn", sessionId, seq, role, msg, fullContent, fullToolResults });
  }

  function queueJsonlWrite(sessionId, jsonlBody) {
    return enqueue({ kind: "jsonl", sessionId, jsonlBody });
  }

  async function flush(timeoutMs = 10000) {
    pumpBosQueue();
    await Promise.race([
      bosQueue,
      new Promise((r) => {
        const t = setTimeout(r, timeoutMs);
        if (t.unref) t.unref();
      }),
    ]).catch(() => {});
  }

  return {
    persistTurnToBos,
    persistJsonlToBos,
    queueBosWrite,
    queueJsonlWrite,
    flush,
    ensureStoreCas,
    get pending() {
      return bosPending;
    },
    get queue() {
      return bosQueue;
    },
  };
}

const defaultPersister = createPersister();
export const persistTurnToBos = (...args) => defaultPersister.persistTurnToBos(...args);
export const persistJsonlToBos = (...args) => defaultPersister.persistJsonlToBos(...args);
export const queueBosWrite = (...args) => defaultPersister.queueBosWrite(...args);
export const queueJsonlWrite = (...args) => defaultPersister.queueJsonlWrite(...args);
export const flushBosQueue = (...args) => defaultPersister.flush(...args);
export const getBosQueue = () => defaultPersister.queue;

async function getStoreOrFallback(opts, fallbackResume) {
  try {
    return { store: opts.store || (opts.loadStore || defaultLoadStore)() };
  } catch (e) {
    if (e.code === "endpoint-not-allowed") {
      return { error: { turns: null, source: null, error: e.message, fatal: true } };
    }
    if (fallbackResume) {
      try {
        const turns = await fallbackResume(opts.sessionId);
        if (turns?.length) return { error: { turns, source: "worker", miss: true, kind: "turns" } };
      } catch (e2) { /* ignore */ }
    }
    return { error: { turns: null, source: null, error: e.message || "no-config" } };
  }
}

/**
 * BOS-first 恢复。优先 JSONL (真 Pi 会话); 仅 miss 才读旧 turns JSON; 再 miss 才 worker。
 * - BOS 读成功 → { source:"bos", kind:"jsonl"|"turns", turns, jsonl? }
 * - BOS not-found / 无 bucket → 才调用 fallbackResume (worker 缓存)
 * - BOS 瞬时/权限失败 → 不回退 (避免 8000 字截断被当成完整历史)
 * - JSON/JSONL 损坏 → 不回退、不覆盖
 */
export async function loadSessionHistory(sessionId, opts = {}) {
  const get = opts.get || ((key, o) => bosGet(key, o));
  const fallbackResume = opts.fallbackResume;
  const resolved = await getStoreOrFallback({ ...opts, sessionId }, fallbackResume);
  if (resolved.error) return resolved.error;
  const store = resolved.store;
  const common = store.bucket
    ? { bucket: store.bucket, endpoint: store.endpoint, profile: store.profile, region: store.region }
    : null;

  async function readKey(key, asJsonl) {
    const existing = await get(key, common);
    if (existing.ok) {
      if (asJsonl) {
        if (!isJsonlBody(existing.body)) {
          return { done: true, value: { turns: null, source: "bos", error: "corrupt", corrupt: true, kind: "jsonl" } };
        }
        return {
          done: true,
          value: {
            turns: turnsFromJsonl(existing.body),
            source: "bos",
            kind: "jsonl",
            jsonl: existing.body,
          },
        };
      }
      try {
        const session = JSON.parse(existing.body);
        return { done: true, value: { turns: session.turns || [], source: "bos", kind: "turns" } };
      } catch (e) {
        return { done: true, value: { turns: null, source: "bos", error: "corrupt", corrupt: true, kind: "turns" } };
      }
    }
    const kind = classifyStoreError(existing.error);
    if (kind !== "not-found") {
      return {
        done: true,
        value: {
          turns: null,
          source: "bos",
          error: existing.error,
          transient: kind === "transient",
          fatal: kind === "fatal",
        },
      };
    }
    return { done: false };
  }

  if (store.bucket) {
    const jsonl = await readKey(sessionJsonlKey(sessionId), true);
    if (jsonl.done) return jsonl.value;
    const turns = await readKey(sessionTurnsKey(sessionId), false);
    if (turns.done) return turns.value;
  }
  if (fallbackResume) {
    try {
      const fb = await fallbackResume(sessionId);
      if (fb?.length) return { turns: fb, source: "worker", miss: true, kind: "turns" };
    } catch (e) { /* ignore */ }
  }
  return { turns: store.bucket ? [] : null, source: store.bucket ? "bos" : null, miss: true, kind: store.bucket ? "turns" : null };
}
