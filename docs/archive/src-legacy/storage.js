// CelldSessionStorage: 把 pi-agent-core 的 SessionStorage 抽象映射到 Celld KV API
// 会话数据经 Celld DO → SQLite → LTX → BOS (RPO=0, 跨节点恢复)
const NODES = ["http://127.0.0.1:18090", "http://127.0.0.1:18091"];

export class CelldSessionStorage {
  constructor(agentName, sessionId, nodes = NODES) {
    this.agent = agentName;
    this.sid = sessionId;
    this.nodes = nodes;
    this.entries = [];
    this.records = [];
    this._meta = { id: sessionId, createdAt: Date.now() };
  }

  // 底层: 调 Celld KV API (多节点 failover)
  async _kv(action, params = {}) {
    const q = new URLSearchParams({ ...params }).toString();
    const errs = [];
    for (const base of this.nodes) {
      try {
        const url = `${base}/agent/${this.agent}?action=${action}&${q}`;
        const resp = await fetch(url, { signal: AbortSignal.timeout(15000) });
        const data = await resp.json();
        if (data.ok !== false) return data;
        errs.push(`${base}: ${JSON.stringify(data).slice(0, 80)}`);
      } catch (e) {
        errs.push(`${base}: ${e.message}`);
      }
    }
    throw new Error(`celld kv ${action} failed: ${errs.join(" | ")}`);
  }

  _key(parts) { return `${this.sid}/${parts.join("/")}`; }

  // ---- SessionStorage 接口实现 ----
  async getMetadata() { return this._meta; }

  async getLanes() {
    const r = await this._kv("kv-get", { k: this._key(["lanes"]) });
    return r.v ? JSON.parse(r.v) : [{ lane: "main", leafId: null }];
  }

  async createLane(lane, at) {
    const lanes = await this.getLanes();
    if (!lanes.find(l => l.lane === lane)) {
      lanes.push({ lane, leafId: at });
      await this._kv("kv-put", { k: this._key(["lanes"]), v: JSON.stringify(lanes) });
    }
  }

  async moveLane(lane, to) {
    const lanes = await this.getLanes();
    const l = lanes.find(l => l.lane === lane);
    if (l) { l.leafId = to; await this._kv("kv-put", { k: this._key(["lanes"]), v: JSON.stringify(lanes) }); }
  }

  async appendEntry(entry) {
    // 每次 append 都是 checkpoint (RPO=0 写)
    this.entries.push(entry);
    const seq = this.entries.length;
    await this._kv("kv-put", { k: this._key(["entries", String(seq)]), v: JSON.stringify({ ...entry, seq }) });
    return entry;
  }

  async appendRecord(record) {
    this.records.push(record);
    const seq = this.records.length;
    await this._kv("kv-put", { k: this._key(["records", String(seq)]), v: JSON.stringify({ ...record, seq }) });
    return record;
  }

  async getEntry(id) {
    const r = await this._kv("kv-get", { k: this._key(["byId", id]) });
    return r.v ? JSON.parse(r.v) : undefined;
  }

  async findEntries() {
    const r = await this._kv("kv-list", { prefix: this._key(["entries"]), limit: 1000 });
    return Object.values(r.entries || {}).map(v => typeof v === "string" ? JSON.parse(v) : v);
  }

  async findEntriesOnBranch() { return this.findEntries(); }

  async findRecords() {
    const r = await this._kv("kv-list", { prefix: this._key(["records"]), limit: 1000 });
    return Object.values(r.entries || {}).map(v => typeof v === "string" ? JSON.parse(v) : v);
  }

  async setName(name) { this._meta.name = name; await this._kv("kv-put", { k: this._key(["meta"]), v: JSON.stringify(this._meta) }); }
  async getLabel() { return undefined; }
  async setLabel() {}
  async getStats() {
    return { messageCount: this.entries.length, cachedTokens: 0, uncachedTokens: 0, totalTokens: 0, costTotal: 0 };
  }
}
