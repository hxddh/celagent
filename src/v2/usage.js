// v2/usage.js — 会话 token/cost 统计（预算的基础）
// 独立于 append 的 manifest CAS；失败留待下次 flush，不阻塞工具。
import { readManifest, writeManifest } from "./manifest.js";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export function createUsageRecorder({ store, sessionId, lease, now = Date.now, onError } = {}) {
  let totalTokens = 0;
  let totalCost = 0;
  let flushedTokens = 0;
  let flushedCost = 0;
  let queue = Promise.resolve();
  let timer = null;
  let closed = false;
  let lastError = null;

  function add(usage) {
    if (!usage || typeof usage !== "object") return;
    const tokens = Number(usage.totalTokens ?? (Number(usage.input || 0) + Number(usage.output || 0) + Number(usage.cacheRead || 0) + Number(usage.cacheWrite || 0)));
    const cost = Number(usage.cost?.total ?? usage.cost ?? 0);
    if (!Number.isFinite(tokens) || tokens <= 0) return;
    totalTokens += tokens;
    if (Number.isFinite(cost) && cost > 0) totalCost += cost;
    if (timer) return;
    timer = setTimeout(() => { timer = null; void flush(); }, 300);
    timer.unref?.();
  }

  async function writeOnce(deltaTokens, deltaCost) {
    for (let attempt = 0; attempt < 5; attempt++) {
      if (lease) await lease.assert(sessionId);
      const cur = await readManifest(store, sessionId);
      if (!cur) {
        const e = new Error(`会话 ${sessionId} 没有 manifest`);
        e.code = "no-manifest";
        throw e;
      }
      const stats = cur.stats && typeof cur.stats === "object" ? cur.stats : { tokens: 0, cost: 0 };
      const next = {
        ...cur,
        stats: {
          tokens: Number(stats.tokens || 0) + deltaTokens,
          cost: Number(stats.cost || 0) + deltaCost,
          updatedAt: new Date(now()).toISOString(),
        },
        updatedAt: new Date(now()).toISOString(),
      };
      const w = await writeManifest(store, next, cur.etag);
      if (w.ok) return true;
      if (!w.conflict) throw new Error(`写 usage 失败: ${w.error || "unknown"}`);
      await sleep(100 * (attempt + 1));
    }
    throw new Error("写 usage 失败（CAS 重试耗尽）");
  }

  function flush() {
    queue = queue.then(async () => {
      if (closed) return;
      const dt = totalTokens - flushedTokens;
      const dc = totalCost - flushedCost;
      if (dt <= 0 && dc <= 0) return;
      try {
        await writeOnce(dt, dc);
        flushedTokens += dt;
        flushedCost += dc;
      } catch (e) {
        lastError = e;
        onError?.(e);
      }
    });
    return queue;
  }

  async function close() {
    closed = true;
    if (timer) { clearTimeout(timer); timer = null; }
    await queue.catch(() => {});
  }

  return { add, flush, close, get tokens() { return totalTokens; }, get cost() { return totalCost; }, get lastError() { return lastError; } };
}
