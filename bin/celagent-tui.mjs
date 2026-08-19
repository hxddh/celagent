#!/usr/bin/env node
// celagent — Pi TUI 版 (完全复刻 pi 交互 + Celld RPO=0 持久化)
// 组装: createAgentSessionServices → createAgentSessionFromServices
//       → createAgentSessionRuntime → InteractiveMode (完整 TUI)
//       + turn_end 钩子 → Celld 镜像
import { homedir } from "node:os";
import { join, dirname } from "node:path";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
// Bug 84: 静态 import pi 包 — Bun 编译可静态分析并打包全部依赖 (2983 modules),
// 动态 import(file://路径) 无法被 Bun 打包, 单二进制运行时找不到 chalk 等嵌套依赖。
// 开发模式 (源码目录有 node_modules) 时仍从本地解析; 编译时由 Bun 内联。
import * as pi from "@earendil-works/pi-coding-agent";
import { awsEnv, resolveEndpoint, isAllowedEndpoint, awsJson } from "../src/bos.js";
import { storeFromCfg, queueJsonlWrite, flushBosQueue, loadSessionHistory, STEER_LEGACY_HEADER, persistIdFromJsonlPath } from "../src/persist.js";

const AGENT_DIR = join(homedir(), ".config", "celagent", "pi-runtime");
const CELD_NODES = ["http://127.0.0.1:18090", "http://127.0.0.1:18091", "http://127.0.0.1:19000"];

const warned = { queue: false, persist: false, celld: false, worker: false };
function warnOnce(ch, msg) {
  if (warned[ch]) return;
  warned[ch] = true;
  console.warn(msg);
}

function loadWorkerToken() {
  if (process.env.CELAGENT_WORKER_TOKEN) return process.env.CELAGENT_WORKER_TOKEN;
  const t = loadConfig().worker?.token;
  return (typeof t === "string" && t.length >= 8) ? t : "";
}
function ensureWorkerToken() {
  let t = loadWorkerToken();
  if (t) return t;
  try {
    const { randomBytes } = require("node:crypto");
    t = randomBytes(24).toString("base64url");
    const cfg = loadConfig();
    cfg.worker = { ...(cfg.worker && typeof cfg.worker === "object" ? cfg.worker : {}), token: t };
    saveConfig(cfg);
    return t;
  } catch (e) {
    return "";
  }
}
function workerHeaders(extra = {}) {
  const headers = { ...extra };
  const t = loadWorkerToken();
  if (t) headers["X-Celagent-Token"] = t;
  return headers;
}
async function celldFetch(base, action, { search = {}, json, timeout = 2000 } = {}) {
  const u = new URL(`${base}/agent/celagent`);
  u.searchParams.set("action", action);
  for (const [k, v] of Object.entries(search)) {
    if (v !== undefined && v !== null && v !== "") u.searchParams.set(k, String(v));
  }
  const init = { signal: AbortSignal.timeout(timeout), headers: workerHeaders() };
  if (json !== undefined) {
    init.method = "POST";
    init.headers = workerHeaders({ "Content-Type": "application/json" });
    init.body = JSON.stringify(json);
  }
  return fetch(u, init);
}

// ---- Celld 自动启动 (检测无节点 → 从配置拉起 BOS 节点) ----
let ensureRan = false;
let ensureTime = 0;
const ENSURE_COOLDOWN_MS = 30000;  // 30s 冷却, 避免频繁尝试
let ensureLock = null;  // Bug 53: 进程内互斥 — 并发调用 ensureCelld 只执行一次自动启动
function tryEnsureFileLock(stateDir) {
  const { openSync, closeSync, unlinkSync, statSync, writeSync } = require("node:fs");
  const lockPath = join(stateDir, "ensure.lock");
  const staleMs = 60000;
  const acquire = () => {
    try {
      const fd = openSync(lockPath, "wx");
      try { writeSync(fd, String(process.pid)); } finally { closeSync(fd); }
      return true;
    } catch (e) {
      if (e.code !== "EEXIST") return true;
      try {
        const st = statSync(lockPath);
        if (Date.now() - st.mtimeMs > staleMs) {
          unlinkSync(lockPath);
          return acquire();
        }
      } catch (e2) { /* ignore */ }
      return false;
    }
  };
  return acquire();
}
function releaseEnsureFileLock(stateDir) {
  try { require("node:fs").unlinkSync(join(stateDir, "ensure.lock")); } catch (e) { /* ignore */ }
}
async function ensureCelld() {
  const now = Date.now();
  if (ensureRan && now - ensureTime < ENSURE_COOLDOWN_MS) return;
  if (ensureLock) return ensureLock;
  ensureRan = true;
  ensureTime = now;
  const run = async () => {
    let fileLockDir = null;
    try {
    for (const base of CELD_NODES) {
      try {
        const r = await fetch(`${base}/__celld/health`, { signal: AbortSignal.timeout(2000) });
        if (r.ok) return;
      } catch (e) { /* down */ }
    }
  const cfgFile = join(homedir(), ".config", "celagent", "settings.json");
  if (existsSync(cfgFile)) {
    try {
      const cfg = JSON.parse(readFileSync(cfgFile, "utf8"));
      const bucket = cfg.persistence?.bucket;
      let store;
      try { store = storeFromCfg(cfg); } catch (e) {
        warnOnce("persist", `  (警告: ${e.message})`);
        return;
      }
      if (!store.region) {
        warnOnce("persist", "  (警告: 非 BOS 需 config set persistence.region, 如 auto 或 us-east-1)");
        return;
      }
      const hasFullEnv = !!(process.env.AWS_ACCESS_KEY_ID && process.env.AWS_SECRET_ACCESS_KEY);
      let hasBosProfile = false;
      if (!hasFullEnv) {
        try {
          const { execFileSync } = await import("node:child_process");
          const ak = execFileSync("aws", ["configure", "get", "aws_access_key_id", "--profile", store.profile], {
            encoding: "utf8", timeout: 8000, stdio: ["ignore", "pipe", "ignore"],
          });
          hasBosProfile = !!(ak && String(ak).trim());
        } catch (e) { hasBosProfile = false; }
      }
      const bosChildEnv = () => awsEnv(store.awsExtra);
      if (bucket && (hasFullEnv || hasBosProfile)) {
        console.log("  (自动启动 Celld 节点, bucket=" + bucket + ")...");
        const { spawn } = await import("node:child_process");
        const { mkdirSync } = await import("node:fs");
        const stateDir = join(homedir(), ".local", "celagent", "state");
        mkdirSync(stateDir, { recursive: true });
        let celldBin = null;
        for (const cand of [join(homedir(), ".local", "bin", "celld"), "/usr/local/bin/celld"]) {
          if (existsSync(cand)) { celldBin = cand; break; }
        }
        if (celldBin) {
          if (!tryEnsureFileLock(stateDir)) {
            console.log("  (另一进程正在拉起 Celld, 跳过重复启动)");
            return;
          }
          fileLockDir = stateDir;
          // Bug 50: 先清理残留 own.json (旧节点被强杀后, own.json 指向已死节点,
          // 会阻塞新节点接管导致 RestoreFailed) — 必须在启动节点之前清理
          // (之前顺序是"启动→等待→清理", 已失败的节点不会自动重试接管)
          // 仅清理本产品专用 bucket (celagent-*) 的残留 own.json, 避免误删共享 bucket 上其他节点
          const allowOwnClean = process.env.CELAGENT_CLEAN_OWN === "1" || String(bucket).startsWith("celagent-");
          if (allowOwnClean) {
          try {
            const { execFileSync } = await import("node:child_process");
            const keys = execFileSync("aws", [
              "s3api", "list-objects-v2",
              "--bucket", bucket,
              "--prefix", "cells/",
              "--endpoint-url", store.endpoint,
              "--query", "Contents[?ends_with(Key, `own.json`)].Key",
              "--output", "json",
            ], { env: bosChildEnv(), encoding: "utf8" });
            const ownKeys = JSON.parse(keys || "[]");
            if (ownKeys.length > 0) {
              for (const k of ownKeys) {
                execFileSync("aws", [
                  "s3api", "delete-object",
                  "--bucket", bucket,
                  "--key", k,
                  "--endpoint-url", store.endpoint,
                ], { env: bosChildEnv(), stdio: "ignore" });
              }
              console.log(`  (已清理 ${ownKeys.length} 个残留 ownership)`);
            }
          } catch (e) { /* 清理失败不阻塞 */ }
          } else {
            console.warn("  (跳过 own.json 全量清理: bucket 非 celagent- 前缀, 设 CELAGENT_CLEAN_OWN=1 强制)");
          }

          for (const port of [18090, 18091]) {
            // Bug 53: 端口预检 — 另一进程可能已启动节点 (跨进程双启动竞态),
            // 端口已被监听则跳过, 避免重复 spawn 竞争
            try {
              const probe = await fetch(`http://127.0.0.1:${port}/__celld/health`, { signal: AbortSignal.timeout(800) });
              if (probe.ok) continue;
            } catch (e) { /* 端口空闲, 启动 */ }
            // Bug 57: spawn 必须挂 error handler — 否则 celld 二进制缺失/损坏/
            // 无权限/端口冲突时, unhandled 'error' 事件直接炸掉整个 celagent 进程
            // celld v0.2: --advertise 必须指向内部监听; 显式 advertise 必须带 --internal-listen
            const internalPort = port + 2;
            const workerToken = ensureWorkerToken();
            const child = spawn(celldBin, [
              "--bucket", `s3://${bucket}`,
              "--endpoint", store.endpoint,
              "--region", store.region,
              "--listen", `127.0.0.1:${port}`,
              "--internal-listen", `127.0.0.1:${internalPort}`,
              "--advertise", `127.0.0.1:${internalPort}`,
            ], {
              env: {
                ...bosChildEnv(),
                CELLD_WATCH: join(stateDir, `node${port}`),
                CELLD_IDLE_EVICT_S: "30",
                CELLD_ALARM_RESIDENT_MS: "60000",
                CELLD_ADMISSION_WAIT_MS: "2000",
                CELLD_MAX_RESIDENT_CELLS: "128",
                CELAGENT_WORKER_TOKEN: workerToken,
                CELLD_VAR_CELAGENT_WORKER_TOKEN: workerToken,
              },
              stdio: "ignore",
              detached: true,
            });
            child.on("error", (err) => {
              warnOnce("celld", `  (警告: Celld 节点 ${port} 启动失败: ${err.message})`);
            });
            // Bug 62: celld 自身端口冲突/启动即崩时, spawn 不报 error (进程已起来),
            // 但子进程会很快非零退出 — 监听 exit 事件给出明确诊断
            child.on("exit", (code, sig) => {
              if (code !== 0 && code !== null) {
                warnOnce("celld", `  (警告: Celld 节点 ${port} 启动后异常退出 code=${code} sig=${sig ?? ""}, 请检查端口占用或 node log)`);
              }
            });
            child.unref();
          }
          // Bug 61: 等待两个节点中任意一个就绪即可 (18090 失败不应空等 10s)
          let anyReady = false;
          for (let i = 0; i < 20 && !anyReady; i++) {
            for (const port of [18090, 18091]) {
              try {
                const r = await fetch(`http://127.0.0.1:${port}/__celld/health`, { signal: AbortSignal.timeout(1000) });
                if (r.ok) { anyReady = true; break; }
              } catch (e) { /* wait */ }
            }
            if (!anyReady) await new Promise(r => setTimeout(r, 500));
          }
          if (anyReady) { console.log("  (Celld 节点已启动)"); }
          else { console.warn("  (警告: Celld 节点未能就绪, 请检查 node log)"); }
          return;
        }
      }
      } catch (e) { /* 配置解析失败, 跳过自动启动 */ }
    }
    } finally {
      ensureLock = null;  // 健康早退 / 无配置 / 拉起完成 都必须释放
      if (fileLockDir) releaseEnsureFileLock(fileLockDir);
    }
  };
  ensureLock = run();
  return ensureLock;
}

// 会话权威写/队列: src/persist.js (CAS 门禁 + I/O transient 重试 + BOS-first)

// ---- Celld 镜像 (worker 同步 + BOS 异步, 不阻塞对话) ----
async function celldCheckpoint(sessionId, seq, role, content, opts = {}) {
  const msg = typeof content === "string" ? content : JSON.stringify(content ?? "");
  // 完整记忆: fullContent/fullToolResults 全量存 BOS; worker 缓存走 POST body (不再把 msg 放进 URL)
  const { fullContent = null, fullToolResults = null } = opts;

  ensureCelld();
  void (async () => {
    let workerOk = false;
    for (const base of CELD_NODES) {
      try {
        const resp = await celldFetch(base, "checkpoint", {
          search: { session: sessionId },
          json: { turn: seq, role, msg: msg.slice(0, 8000) },
          timeout: 2000,
        });
        const data = await resp.json();
        if (data.ok) { workerOk = true; break; }
      } catch (e) { /* try next */ }
    }
    // Bug C: worker 全失败时提示一次 (BOS 兜底仍会写)
    if (!workerOk) warnOnce("worker", "  (警告: Celld worker 写入失败, 仅 BOS 持久化)");
  })();

  // 2. BOS 权威写改走 JSONL 队列 (见 queueSessionJsonl); 本函数只镜像 worker 缓存
  return { worker: "async", bos: "queued" };
}

function extractText(content) {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content.filter(b => b.type === "text").map(b => b.text).join(" ").trim();
  }
  return "";
}

// ---- 从 BOS 读历史 (权威源, 重启后恢复) ----
async function workerResumeTurns(sessionId) {
  for (const base of CELD_NODES) {
    try {
      const url = `${base}/agent/celagent?action=resume&session=${encodeURIComponent(sessionId)}`;
      const resp = await fetch(url, { headers: workerHeaders(), signal: AbortSignal.timeout(1500) });
      const data = await resp.json();
      if (data.ok && data.session?.turns?.length > 0) return data.session.turns;
      break;
    } catch (e) { /* try next node */ }
  }
  return null;
}

async function loadHistoryFromBos(sessionId) {
  // BOS-first: 仅 miss 才回退 worker; 瞬时/权限失败不回退, 避免 8000 字截断当完整历史
  const hist = await loadSessionHistory(sessionId, {
    loadStore: () => storeFromCfg(loadConfig()),
    fallbackResume: workerResumeTurns,
  });
  if (hist.corrupt) {
    console.warn(`  (警告: 会话 ${sessionId} 的 BOS 历史数据损坏, 跳过恢复)`);
    return null;
  }
  if (hist.transient || hist.fatal) {
    console.warn(`  (警告: BOS 读取失败, 未回退 worker 缓存: ${hist.error})`);
    return null;
  }
  return hist.turns;
}

function bindSessionFile(sm, dest) {
  if (!sm?.getSessionFile || !sm?.setSessionFile) return dest;
  const { mkdirSync, renameSync, copyFileSync, existsSync, unlinkSync } = require("node:fs");
  mkdirSync(dirname(dest), { recursive: true });
  const cur = sm.getSessionFile();
  if (!cur || cur === dest) return dest;
  try {
    if (existsSync(cur)) {
      try { renameSync(cur, dest); }
      catch (e) {
        copyFileSync(cur, dest);
        try { unlinkSync(cur); } catch (e2) { /* 源文件可留 */ }
      }
    }
    sm.setSessionFile(dest);
  } catch (e) { /* 绑定失败不阻塞; 仍用 Pi 原路径, persistId 仍指向 dest stem */ }
  return dest;
}

function queueSessionJsonl(persistId, sm) {
  try {
    const f = sm?.getSessionFile?.();
    if (!f || !existsSync(f)) return;
    const body = readFileSync(f, "utf8");
    queueJsonlWrite(persistId, body);
  } catch (e) { /* 读本地 JSONL 失败不阻塞对话 */ }
}

async function listSessions() {
  // 从 BOS 列出会话 (sessions/<id>.jsonl 优先, 旧 .json 兼容)
  // 降级链 (Bug 65): settings.json 丢失时自动发现账号下所有含会话的 bucket,
  // 保证“本地数据全丢, 只要凭证还在”仍能找回会话
  try {
    const cfg = loadConfig();
    let store;
    try { store = storeFromCfg(cfg); } catch (e) {
      console.error(`✗ ${e.message}`);
      return;
    }
    let bucket = null;
    let endpoint = store.endpoint;
    // 1) 命令行显式指定 (最高优先): celagent list --bucket <name>
    const argvIdx = process.argv.indexOf("--bucket");
    if (argvIdx > 0 && process.argv[argvIdx + 1]) {
      bucket = process.argv[argvIdx + 1];
    } else {
      bucket = store.bucket;
    }
    const awsOpts = { profile: store.profile, region: store.region };
    // 3) 无 bucket: 需显式 --scan 才枚举账号 (避免默认列出全部 bucket)
    if (!bucket) {
      if (!process.argv.includes("--scan")) {
        console.error("✗ 未找到 persistence.bucket (用 --bucket 指定, 或 celagent list --scan 扫描账号)");
        return;
      }
      console.log("(未找到 settings.json 配置, --scan 扫描账号下含会话的 bucket...)");
      const bucketsR = await awsJson(["s3api", "list-buckets", "--endpoint-url", endpoint, "--query", "Buckets[].Name", "--output", "json"], awsOpts);
      if (!bucketsR.ok) {
        console.error(`✗ 列举 bucket 失败: ${bucketsR.error}`);
        return;
      }
      const all = Array.isArray(bucketsR.data) ? bucketsR.data : [];
      const candidates = [];
      const worker = async (b) => {
        const hit = await awsJson(["s3api", "list-objects-v2", "--bucket", b, "--prefix", "sessions/", "--max-items", "1", "--endpoint-url", endpoint, "--query", "Contents[].Key", "--output", "json"], awsOpts);
        if (hit.ok && Array.isArray(hit.data) && hit.data.length > 0) candidates.push(b);
      };
      for (let i = 0; i < all.length; i += 6) {
        await Promise.all(all.slice(i, i + 6).map(worker));
      }
      if (candidates.length === 0) { console.log("(账号下没有找到含会话的 bucket)"); return; }
      if (candidates.length === 1) { bucket = candidates[0]; }
      else {
        console.log(`找到多个含会话的 bucket, 请指定: celagent list --bucket <name>`);
        for (const c of candidates) console.log(`  - ${c}`);
        return;
      }
    }
    const listed = await awsJson(["s3api", "list-objects-v2", "--bucket", bucket, "--prefix", "sessions/", "--endpoint-url", endpoint, "--query", "Contents[].{k:Key,s:Size,l:LastModified}", "--output", "json"], awsOpts);
    if (!listed.ok) {
      console.error(`✗ 列举会话失败: ${listed.error}`);
      return;
    }
    const list = listed.data;
    const sessions = (Array.isArray(list) ? list : [])
      .filter(i => (i.k?.endsWith(".jsonl") || i.k?.endsWith(".json")) && !i.k.includes("/verify/"))
      .map(i => {
        const name = i.k.replace("sessions/", "");
        const jsonl = name.endsWith(".jsonl");
        const id = name.replace(/\.jsonl$/, "").replace(/\.json$/, "");
        return { id, size: i.s || 0, modified: (i.l || "").slice(0, 16), jsonl };
      })
      .filter(i => !/^(bugtest-|stress-|takeover-)/.test(i.id));
    const byId = new Map();
    for (const s of sessions) {
      const prev = byId.get(s.id);
      if (!prev || (s.jsonl && !prev.jsonl) || (s.jsonl === prev.jsonl && s.modified > prev.modified)) {
        byId.set(s.id, s);
      }
    }
    const unique = [...byId.values()].sort((a, b) => b.modified.localeCompare(a.modified));
    if (unique.length === 0) { console.log("(BOS 暂无会话)"); return; }
    console.log(`celagent — BOS 会话列表 (${unique.length} 个, bucket=${bucket})\n`);
    console.log("  ID (用于 celagent <id> 续写)                                           大小    更新");
    console.log("  " + "-".repeat(95));
    for (const s of unique) {
      const id = s.id.length > 52 ? s.id.slice(0, 49) + "..." : s.id;
      console.log(`  ${id.padEnd(52)} ${String(s.size).padStart(8)}B  ${s.modified}`);
    }
    console.log("\n  续写: celagent <id>    新会话: celagent (不带参数)");
  } catch (e) {
    console.error(`列表失败: ${e.message}`);
  }
}

// ---- 版本/帮助 ----
const CELAGENT_VERSION = "0.4.0";
function printVersion() {
  console.log(`celagent v${CELAGENT_VERSION} — Pi TUI + Celld/BOS 对象存储持久化`);
}
function printHelp() {
  printVersion();
  console.log(`
用法:
  celagent                     启动 TUI (自动生成唯一会话 ID)
  celagent <id>                续写指定会话 (打开 BOS 上的 Pi JSONL; 旧 .json 走文本注入)
  celagent list [--bucket B] [--scan]  列出 BOS 会话 (--scan 才枚举账号下全部 bucket)
  celagent export <id> [--bucket B]  导出会话 (优先 JSONL, 旧对象为 JSON)
  celagent rm <id> [--bucket B] [--yes]  删除 BOS 里的会话 (非 TTY 必须 --yes)
  celagent config get <key>   读取配置 (如 persistence.bucket)
  celagent config set <key> <value>  写入配置 (如 model deepseek-v4-flash)
  celagent doctor             自检: 配置/凭证/节点/存储连通/CAS
  celagent cas-probe          探测存储条件写 (RPO=0 门禁)
  celagent task submit <type> [steps]  提交分布式任务 (celld 状态机)
  celagent task status [taskId]        任务状态 (断点续跑)
  celagent task ledger                 幂等 ledger (单 cell 去重)
  celagent version            显示版本
  celagent help               显示帮助

示例:
  celagent list
  celagent sess-demo-xxxxxxxx
  celagent export sess-demo-xxxxxxxx > backup.jsonl
`);
}

// ---- 配置管理 (config get/set) ----
function configFile() { return join(homedir(), ".config", "celagent", "settings.json"); }
function loadConfig() {
  const f = configFile();
  if (!existsSync(f)) return {};
  try { return JSON.parse(readFileSync(f, "utf8")); } catch (e) { return {}; }
}
function saveConfig(cfg) {
  const { mkdirSync, writeFileSync, chmodSync } = require("node:fs");
  const disk = loadConfig();
  const out = { ...disk, ...cfg };
  for (const k of ["persistence", "worker"]) {
    if (cfg[k] && typeof cfg[k] === "object") {
      const prev = (disk[k] && typeof disk[k] === "object") ? disk[k] : {};
      out[k] = { ...prev, ...cfg[k] };
    }
  }
  mkdirSync(join(homedir(), ".config", "celagent"), { recursive: true });
  writeFileSync(configFile(), JSON.stringify(out, null, 2) + "\n", { encoding: "utf8", mode: 0o600 });
  try { chmodSync(configFile(), 0o600); } catch (e) { /* ignore */ }
}
async function configCommand(args) {
  const [op, key, value] = args;
  const cfg = loadConfig();
  if (op === "get") {
    if (!key) { console.log(JSON.stringify(cfg, null, 2)); return; }
    const v = key.split(".").reduce((o, k) => o?.[k], cfg);
    console.log(v === undefined ? "(未设置)" : typeof v === "object" ? JSON.stringify(v) : String(v));
  } else if (op === "set") {
    if (!key || value === undefined) { console.error("用法: celagent config set <key> <value>"); process.exit(1); }
    if (!/^[A-Za-z0-9._-]+$/.test(key) || key.includes("..")) {
      console.error("✗ 非法配置键");
      process.exit(1);
    }
    const parts = key.split(".");
    const protectedObjs = new Set(["persistence", "worker"]);
    if (parts.length === 1 && protectedObjs.has(parts[0]) && cfg[parts[0]] && typeof cfg[parts[0]] === "object") {
      console.error(`✗ ${parts[0]} 是对象, 请用 ${parts[0]}.<field> 设置 (避免把嵌套配置写成标量)`);
      process.exit(1);
    }
    if (key === "persistence.endpoint") {
      if (!isAllowedEndpoint(value)) {
        console.error("✗ persistence.endpoint 不允许 (合格 https host / 本机; 或设 CELAGENT_ALLOW_ENDPOINT=1)");
        process.exit(1);
      }
    }
    let o = cfg;
    for (let i = 0; i < parts.length - 1; i++) { o[parts[i]] ??= {}; o = o[parts[i]]; }
    if (value === "" || value === "null") {
      // 空值 = 删除 key (避免残留空配置)
      delete o[parts[parts.length - 1]];
      saveConfig(cfg);
      console.log(`✓ 已删除配置项 ${key}`);
    } else {
      o[parts[parts.length - 1]] = value;
      saveConfig(cfg);
      // Bug 79: model/provider 必须同步到 pi-runtime 配置 — TUI 实际模型由
      // pi-runtime/settings.json (defaultModel) + models.json 决定,
      // 只写 celagent settings.json 会让 `config set model` 静默不生效
      if (key === "model" || key === "provider") {
        const piSettingsFile = join(homedir(), ".config", "celagent", "pi-runtime", "settings.json");
        if (existsSync(piSettingsFile)) {
          try {
            const piSettings = JSON.parse(readFileSync(piSettingsFile, "utf8"));
            if (key === "model") piSettings.defaultModel = value;
            if (key === "provider") piSettings.defaultProvider = value;
            const { writeFileSync } = require("node:fs");
            writeFileSync(piSettingsFile, JSON.stringify(piSettings, null, 2) + "\n", "utf8");
            console.log(`  ↳ 已同步 pi-runtime 配置 (${piSettingsFile})`);
          } catch (e) {
            console.warn(`  (警告: pi-runtime 配置同步失败: ${e.message})`);
          }
        }
      }
      console.log(`✓ 已设置 ${key} = ${value} (${configFile()})`);
    }
  } else {
    console.error("用法: celagent config get [key] | config set <key> <value>");
    process.exit(1);
  }
}

// ---- doctor 自检 ----
async function doctorCommand() {
  console.log("celagent doctor — 自检\n");
  let ok = true;
  // 0. pi-runtime 前置依赖 (pi 0.84 用 models-store.json, 旧版 models.json)
  const piDir = join(homedir(), ".config", "celagent", "pi-runtime");
  const checkJson = (f) => {
    const p = join(piDir, f);
    if (!existsSync(p)) return { f, state: "缺失" };
    try { JSON.parse(readFileSync(p, "utf8")); return { f, state: "✓" }; }
    catch (e) { return { f, state: "损坏" }; }
  };
  const piStates = ["settings.json", "auth.json"].map(checkJson);
  const modelsStore = checkJson("models-store.json");
  const modelsLegacy = checkJson("models.json");
  const modelsState = modelsStore.state === "✓" ? modelsStore
    : modelsLegacy.state === "✓" ? modelsLegacy
    : modelsStore.state !== "缺失" ? modelsStore
    : modelsLegacy;
  piStates.push(modelsState);
  const piOk = piStates.every(s => s.state === "✓");
  console.log(`[0/6] pi-runtime: ${piStates.map(s => `${s.f}${s.state === "✓" ? "" : " " + s.state}`).join(", ")} ${piOk ? "" : "✗ (TUI 可能无法启动)"}`);
  if (!piOk) ok = false;
  // 1. 配置
  const cfg = loadConfig();
  let store;
  try { store = storeFromCfg(cfg); } catch (e) {
    console.log(`[1/6] 配置: ✗ ${e.message}`);
    process.exit(1);
  }
  const bucket = store.bucket;
  const regionDisp = store.region || "(缺,非 BOS 必须配置)";
  console.log(`[1/6] 配置: ${bucket ? "✓ bucket=" + bucket : "✗ 缺 persistence.bucket (运行 setup.sh 或 config set)"} endpoint=${store.endpoint} region=${regionDisp} profile=${store.profile}`);
  if (!bucket) ok = false;
  if (!store.region) ok = false;
  // 2. 凭证
  const { execFile } = await import("node:child_process");
  const cred = await new Promise((resolve) => {
    execFile("aws", ["configure", "get", "aws_access_key_id", "--profile", store.profile], { timeout: 10000, encoding: "utf8" }, (err, stdout) => resolve(err ? null : (stdout || "").trim()));
  });
  const hasEnv = process.env.AWS_ACCESS_KEY_ID && process.env.AWS_SECRET_ACCESS_KEY;
  console.log(`[2/6] 凭证: ${hasEnv ? "✓ 环境变量" : cred ? "✓ [" + store.profile + "] profile" : "✗ 无凭证 (需 ~/.aws/credentials [" + store.profile + "] 或环境变量)"}`);
  if (!cred && !hasEnv) ok = false;
  // 3. Celld 节点
  const nodes = [];
  for (const base of CELD_NODES) {
    try {
      const r = await fetch(`${base}/__celld/health`, { signal: AbortSignal.timeout(2000) });
      if (r.ok) nodes.push(base);
    } catch (e) { /* down */ }
  }
  console.log(`[3/6] Celld 节点: ${nodes.length > 0 ? "✓ " + nodes.join(", ") : "⚠ 全部离线 (celagent 会自动拉起, 或 node_mgr.sh start)"}`);
  const celldUp = nodes.length > 0;
  for (const port of [18090, 18091]) {
    try {
      const r = await fetch(`http://127.0.0.1:${port + 2}/state`, { signal: AbortSignal.timeout(1500) });
      if (!r.ok) continue;
      const j = await r.json();
      console.log(`      :${port + 2}/state occupied=${j.occupied ?? "?"} evicting=${j.evicting ?? "?"} restoring=${j.restoring ?? "?"}`);
    } catch (e) { /* 内部口离线不判失败 */ }
  }
  // 4. 存储连通
  let storeReachable = false;
  if (bucket) {
    storeReachable = await new Promise((resolve) => {
      execFile("aws", ["s3api", "list-objects-v2", "--bucket", bucket, "--prefix", "sessions/", "--max-items", "1", "--endpoint-url", store.endpoint, "--query", "Contents[].Key", "--output", "json"], { env: awsEnv(store.awsExtra), timeout: 15000, encoding: "utf8" }, (err, stdout) => resolve(!err));
    });
    console.log(`[4/6] 存储连通: ${storeReachable ? "✓ 可读写 bucket=" + bucket : "✗ 访问失败 (检查凭证/endpoint/网络)"}`);
    if (!storeReachable) ok = false;
  } else {
    console.log(`[4/6] 存储连通: ⚠ 跳过 (无 bucket)`);
  }
  // 5. CAS — 条件写必须真正执行,不能只看 PUT 200
  if (bucket && (cred || hasEnv) && storeReachable) {
    const { probeStoreCas } = await import("../src/bos.js");
    const cas = await probeStoreCas({ bucket, endpoint: store.endpoint, profile: store.profile, region: store.region });
    console.log(`[5/6] CAS: ${cas.ok ? "✓ If-Match / If-None-Match / 写后读" : "✗ " + (cas.message || cas.error)}`);
    if (!cas.ok) ok = false;
  } else {
    console.log(`[5/6] CAS: ⚠ 跳过 (先修复配置/凭证/连通)`);
  }
  if (ok && celldUp) console.log("\n结论: ✓ 全部正常");
  else if (ok) console.log("\n结论: ✓ 核心正常 (Celld 离线 — 会话仍可走 BOS; 任务/缓存需 node_mgr.sh start)");
  else console.log("\n结论: ✗ 存在异常, 按上面 ✗ 项处理");
  process.exit(ok ? 0 : 1);
}

// ---- 导出/删除会话 ----
async function getBucketArg() {
  // 返回 { bucket, endpoint, profile, region } — 显式 --bucket > settings.json
  const cfg = loadConfig();
  let store;
  try { store = storeFromCfg(cfg); } catch (e) {
    console.error(`✗ ${e.message}`);
    process.exit(1);
  }
  const base = { endpoint: store.endpoint, profile: store.profile, region: store.region };
  const argvIdx = process.argv.indexOf("--bucket");
  if (argvIdx > 0 && process.argv[argvIdx + 1]) {
    return { bucket: process.argv[argvIdx + 1], ...base };
  }
  if (store.bucket) return { bucket: store.bucket, ...base };
  return { bucket: null, ...base };
}
async function casProbeCommand() {
  const { bucket, endpoint, profile, region } = await getBucketArg();
  if (!bucket) { console.error("✗ 未找到 bucket (用 --bucket 指定或先 setup.sh)"); process.exit(1); }
  const { probeStoreCas } = await import("../src/bos.js");
  const cas = await probeStoreCas({ bucket, endpoint, profile, region });
  if (!cas.ok) {
    console.error(`✗ ${cas.message || cas.error}`);
    // exit 2 = 探针未完成 (transient, 无法判定), 区别于 exit 1 = 存储不合格;
    // install.sh/setup.sh 按此区分提示, 不把网络抖动误报成 "换后端"
    process.exit(cas.transient ? 2 : 1);
  }
  console.log(`✓ ${cas.message}`);
}
// ---- P1: agent 任务化 — 任务状态机 (celld submit/status/ledger) ----
async function taskCommand(args) {
  const [op, ...rest] = args;
  // celagent task submit <type> [steps]  |  task status [taskId]  |  task ledger
  if (op === "submit") {
    const type = rest[0] || "short";
    const steps = rest[1] || (type === "long" ? "15" : "3");
    let lastErr = null;
    for (const base of CELD_NODES) {
      try {
        const r = await celldFetch(base, "submit", { search: { type, steps }, timeout: 3000 });
        const j = await r.json();
        if (j.taskId) {
          console.log(`✓ 任务已提交: ${j.taskId} (type=${j.type || type}, steps=${j.steps})`);
          console.log(`  查看: celagent task status ${j.taskId}`);
          return;
        }
        lastErr = JSON.stringify(j);
      } catch (e) { lastErr = e.message; }
    }
    console.error(`✗ 任务提交失败: ${lastErr}`);
    process.exit(1);
  }
  if (op === "status") {
    const taskId = rest[0] || "";
    let lastErr = null;
    for (const base of CELD_NODES) {
      try {
        const r = await celldFetch(base, "status", { search: taskId ? { task: taskId } : {}, timeout: 3000 });
        const j = await r.json();
        if (Array.isArray(j)) {
          console.log(`任务列表 (${j.length}):`);
          for (const t of j) console.log(`  ${t.id} [${t.status}] step ${t.step}/${t.steps} ${t.type}`);
        } else {
          console.log(JSON.stringify(j, null, 1));
        }
        return;
      } catch (e) { lastErr = e.message; }
    }
    console.error(`✗ 查询失败: ${lastErr}`);
    process.exit(1);
  }
  if (op === "ledger") {
    // 幂等 ledger — 单 cell 去重 (不是跨节点共识)
    let lastErr = null;
    for (const base of CELD_NODES) {
      try {
        const r = await celldFetch(base, "ledger", { timeout: 3000 });
        const j = await r.json();
        if (Array.isArray(j)) {
          console.log(`execution ledger (${j.length} 条):`);
          const dedup = j.filter(e => e.deduped).length;
          const byTool = {};
          for (const e of j) byTool[e.tool] = (byTool[e.tool] || 0) + 1;
          console.log(`  工具调用: ${JSON.stringify(byTool)}`);
          console.log(`  去重命中: ${dedup} 次 (单 cell ledger)`);
          return;
        }
        lastErr = JSON.stringify(j);
      } catch (e) { lastErr = e.message; }
    }
    console.error(`✗ 查询失败: ${lastErr}`);
    process.exit(1);
  }
  console.error(`用法: celagent task submit <short|long> [steps] | task status [taskId] | task ledger`);
  process.exit(1);
}

function assertSafeSessionId(id) {
  if (!id || typeof id !== "string" || id.includes("..") || !/^[A-Za-z0-9._-]{1,128}$/.test(id)) {
    console.error("✗ 无效会话 ID (仅允许字母数字、点、下划线、连字符, 最长 128)");
    process.exit(1);
  }
}

async function exportCommand(id) {
  if (!id || id.startsWith("-")) { console.error("用法: celagent export <会话ID> [--bucket B] (ID 可用 celagent list 查看)"); process.exit(1); }
  assertSafeSessionId(id);
  const { bucket, endpoint, profile, region } = await getBucketArg();
  if (!bucket) { console.error("✗ 未找到 bucket (用 --bucket 指定)"); process.exit(1); }
  const { bosGet } = await import("../src/bos.js");
  const jsonl = await bosGet(`sessions/${id}.jsonl`, { bucket, endpoint, profile, region });
  if (jsonl.ok) {
    process.stdout.write(jsonl.body.endsWith("\n") ? jsonl.body : jsonl.body + "\n");
    return;
  }
  if (jsonl.error && jsonl.error !== "not-found") {
    console.error(`✗ 会话读取失败: ${jsonl.error}`);
    process.exit(1);
  }
  const r = await bosGet(`sessions/${id}.json`, { bucket, endpoint, profile, region });
  if (!r.ok) { console.error(`✗ 会话不存在或读取失败: ${r.error}`); process.exit(1); }
  const session = JSON.parse(r.body);
  console.log(JSON.stringify({ id, exportedAt: new Date().toISOString(), turns: session.turns || [] }, null, 2));
}
async function rmCommand(id) {
  if (!id || id.startsWith("-")) { console.error("用法: celagent rm <会话ID> [--bucket B] [--yes] (ID 可用 celagent list 查看)"); process.exit(1); }
  assertSafeSessionId(id);
  const { bucket, endpoint, profile, region } = await getBucketArg();
  if (!bucket) { console.error("✗ 未找到 bucket (用 --bucket 指定)"); process.exit(1); }
  const force = process.argv.includes("--yes") || process.argv.includes("-y");
  let confirm = force;
  if (!confirm) {
    if (!process.stdin.isTTY) {
      console.error("✗ 非交互删除需要 --yes");
      process.exit(1);
    }
    confirm = await new Promise((resolve) => {
      const readline = require("node:readline").createInterface({ input: process.stdin, output: process.stdout });
      readline.question(`确定删除会话 "${id}" (BOS 永久删除, 不可恢复)? [y/N] `, (a) => { readline.close(); resolve(/^y/i.test(a.trim())); });
    });
  }
  if (!confirm) { console.log("已取消"); return; }
  const { bosDelete } = await import("../src/bos.js");
  const jsonl = await bosDelete(`sessions/${id}.jsonl`, { bucket, endpoint, profile, region });
  const json = await bosDelete(`sessions/${id}.json`, { bucket, endpoint, profile, region });
  const jsonlMiss = !jsonl.ok && (jsonl.error === "not-found" || /\bNoSuchKey\b|\b404\b/i.test(String(jsonl.error || "")));
  const jsonMiss = !json.ok && (json.error === "not-found" || /\bNoSuchKey\b|\b404\b/i.test(String(json.error || "")));
  if (!jsonl.ok && !jsonlMiss) {
    console.error(`✗ 删除失败: ${jsonl.error}`);
    process.exit(1);
  }
  if (!json.ok && !jsonMiss && jsonlMiss) {
    console.error(`✗ 删除失败: ${json.error}`);
    process.exit(1);
  }
  if (jsonlMiss && jsonMiss) {
    console.error("✗ 会话不存在");
    process.exit(1);
  }
  console.log(`✓ 已删除会话 ${id}`);
}

async function main() {
  const cwd = process.cwd();
  const cmd = process.argv[2];
  // 非交互子命令
  if (cmd === "list" || cmd === "--list") { await listSessions(); return; }
  if (cmd === "help" || cmd === "--help" || cmd === "-h") { printHelp(); return; }
  if (cmd === "version" || cmd === "--version" || cmd === "-v") { printVersion(); return; }
  if (cmd === "config") { await configCommand(process.argv.slice(3)); return; }
  if (cmd === "doctor") { await doctorCommand(); return; }
  if (cmd === "cas-probe") { await casProbeCommand(); return; }
  if (cmd === "export") { await exportCommand(process.argv[3]); return; }
  if (cmd === "rm") { await rmCommand(process.argv[3]); return; }
  if (cmd === "task") { await taskCommand(process.argv.slice(3)); return; }
  // Bug 80: 未知的 - 开头参数 (拼错的 --xxx) 不应被静默当 sessionId 进 TUI
  if (cmd && cmd.startsWith("-")) {
    console.error(`未知选项: ${cmd} (用 celagent help 查看用法)`);
    process.exit(1);
  }
  // Bug 46: 无参数时不再共用 "default" — 生成唯一会话名 (时间戳+随机),
  // 避免多实例/多用户写入同一 key 造成串扰覆盖
  // 显式传 sessionId 时保留原名 (用户明确想续写该会话)
  if (process.argv[2]) assertSafeSessionId(process.argv[2]);
  const sessionId = process.argv[2] || `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  console.log(`celagent — Pi TUI, 会话 ${sessionId}, Celld RPO=0 持久化\n`);

  // 0. Bug 87: celagent settings.json 的 provider/model 是单源配置 —
  // 启动时同步到 pi-runtime (手改 celagent settings 也生效, 不再有死配置)
  try {
    const cfg = loadConfig();
    const piSettingsFile = join(homedir(), ".config", "celagent", "pi-runtime", "settings.json");
    if (cfg.model && existsSync(piSettingsFile)) {
      const piSettings = JSON.parse(readFileSync(piSettingsFile, "utf8"));
      let changed = false;
      if (cfg.model && piSettings.defaultModel !== cfg.model) { piSettings.defaultModel = cfg.model; changed = true; }
      if (cfg.provider && piSettings.defaultProvider !== cfg.provider) { piSettings.defaultProvider = cfg.provider; changed = true; }
      if (changed) {
        const { writeFileSync } = require("node:fs");
        writeFileSync(piSettingsFile, JSON.stringify(piSettings, null, 2) + "\n", "utf8");
        console.log(`  (已同步模型配置: ${cfg.provider || "deepseek"}/${cfg.model})`);
      }
    }
  } catch (e) { /* 同步失败不阻塞 */ }

  // 0. 确保 Celld 节点在跑 (自动启动)
  ensureWorkerToken();
  await ensureCelld();

  // 0.5 从 BOS 恢复 (权威源) — JSONL 优先, 旧 turns 仅兼容
  const hist = await loadSessionHistory(sessionId, {
    loadStore: () => storeFromCfg(loadConfig()),
    fallbackResume: workerResumeTurns,
  });
  if (hist.corrupt) {
    console.warn(`  (警告: 会话 ${sessionId} 的 BOS 历史数据损坏, 跳过恢复)`);
  } else if (hist.transient || hist.fatal) {
    console.warn(`  (警告: BOS 读取失败, 未回退 worker 缓存: ${hist.error})`);
  }
  const savedHistory = hist.turns;
  const openedFromJsonl = hist.kind === "jsonl" && typeof hist.jsonl === "string";
  if (openedFromJsonl) {
    console.log("  (已打开 BOS 会话)");
  } else if (savedHistory && savedHistory.length > 0) {
    const src = hist.source === "worker" ? "worker 缓存(BOS miss)" : "BOS";
    console.log(`  (旧格式, 文本注入; 已从 ${src} 恢复 ${savedHistory.length} 轮历史)`);
  }

  // P0: 冷启动对齐 — 把 BOS 权威状态同步到 worker 缓存 (sync),
  // 使 worker 与 BOS 一致 (节点迁移/重启后, 后续读 worker = 完整历史)
  if (savedHistory && savedHistory.length > 0) {
    void (async () => {
      for (const base of CELD_NODES) {
        try {
          await celldFetch(base, "sync", {
            search: { session: sessionId },
            json: { turns: savedHistory },
            timeout: 2000,
          });
          break;
        } catch (e) { /* try next */ }
      }
    })();
  }

  // 1. 组装 services (独立 agentDir)
  let services;
  try {
    const settingsManager = pi.SettingsManager.create(cwd, AGENT_DIR, { projectTrusted: false });
    services = await pi.createAgentSessionServices({
      cwd,
      agentDir: AGENT_DIR,
      settingsManager,
      modelRuntimeSignal: AbortSignal.timeout(15000),
    });
    console.log("✓ services 就绪");
  } catch (e) {
    console.error(`services 失败: ${e.message}`);
    process.exit(1);
  }

  // 2. 创建会话 (含 Celld 镜像钩子)
  let session, runtime;
  try {
    // Bug 修复: SessionManager 用独立会话目录, 绝不碰 ~/.pi/agent/sessions (本机 Pi 数据)
    const sessionDir = join(AGENT_DIR, "sessions", encodeURIComponent(cwd.replace(/\//g, "-")));
    const localJsonl = join(sessionDir, `${sessionId}.jsonl`);
    const { mkdirSync, writeFileSync } = require("node:fs");
    mkdirSync(sessionDir, { recursive: true });
    let sessionManager;
    if (openedFromJsonl) {
      writeFileSync(localJsonl, hist.jsonl, { encoding: "utf8", mode: 0o600 });
      try {
        sessionManager = pi.SessionManager.open(localJsonl, sessionDir, cwd);
      } catch (e) {
        console.error(`会话 JSONL 无法被 Pi 打开: ${e.message}`);
        process.exit(1);
      }
    } else {
      sessionManager = pi.SessionManager.create(cwd, sessionDir);
      bindSessionFile(sessionManager, localJsonl);
    }
    const createRuntime = async (opts = {}) => {
      // Bug: 接受 pi 传入的 sessionManager/sessionStartEvent (newSession/resume 会传)
      // 并返回完整契约: {session, services, diagnostics, ...} 防止 /new 崩溃退出
      const effSessionManager = opts?.sessionManager || sessionManager;
      const effStartEvent = opts?.sessionStartEvent || { type: "session_start", reason: "startup" };
      // P1 记忆增强: 注入 BOS 记忆工具 (history_search / session_snapshot)
      const { history_search, session_snapshot } = await import("../src/bos-tools.js");
      const result = await pi.createAgentSessionFromServices({
        cwd, agentDir: AGENT_DIR, services,
        sessionManager: effSessionManager,
        sessionStartEvent: effStartEvent,
        // P1: agent 可主动检索历史记忆 / 打显式快照
        customTools: [history_search, session_snapshot],
      });
      // 旧 turns JSON 才 steer; JSONL 已由 SessionManager.open 载入, 禁止作文注入
      const startReason = effStartEvent?.reason;
      if (startReason === "startup" && !openedFromJsonl && savedHistory && savedHistory.length > 0) {
        try {
          // Bug 78: 注入历史有长度上限 — 超长会话 (几百轮) 全量拼进一条 steer
          // 会直接撑爆模型上下文窗口。只注入最近 MAX_INJECT_TURNS 轮 + 提示省略。
          const MAX_INJECT_TURNS = 50;
          const recent = savedHistory.slice(-MAX_INJECT_TURNS);
          const omitted = savedHistory.length - recent.length;
          const turnInjectText = (t) => {
            const fromContent = Array.isArray(t.content)
              ? t.content.filter(b => b.type === "text" && b.text).map(b => b.text).join(" ").trim()
              : "";
            return fromContent || t.msg || "";
          };
          const historySummary = recent
            .map(t => `[第${t.turn}轮(${t.role || "assistant"})] ${turnInjectText(t)}`)
            .join("\n");
          result.session.steer(
            `${STEER_LEGACY_HEADER}(请以此作为继续对话的上下文, 不要重复回答这些内容):\n${historySummary}` +
            (omitted > 0 ? `\n\n(注: 较早的 ${omitted} 轮历史已省略, 完整历史在 BOS 中)` : "")
          );
        } catch (e) { /* 注入失败不阻塞 */ }
      }
      // 挂 Celld 镜像钩子
      // 持久化 id: startup 用 argv sessionId (续写原会话); /new 生成独立 key (Bug 47 修复)
      let persistId = sessionId;
      let persistHistory = savedHistory;
      if (startReason === "new" || startReason === "fork") {
        persistId = `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
        persistHistory = [];
        bindSessionFile(effSessionManager, join(sessionDir, `${persistId}.jsonl`));
        console.log(`  ↳ ${startReason === "fork" ? "fork" : "新"}会话持久化 ID: ${persistId} (下次: celagent ${persistId})`);
      } else if (startReason === "resume" && effSessionManager?.getSessionFile) {
        // persistId = JSONL 文件名 stem, 与 celagent <id> / BOS key 同一条会话
        const resumedFile = effSessionManager.getSessionFile();
        if (resumedFile) {
          persistId = persistIdFromJsonlPath(resumedFile);
          const dest = join(sessionDir, `${persistId}.jsonl`);
          bindSessionFile(effSessionManager, dest);
          try {
            const resumedHistory = await loadHistoryFromBos(persistId);
            persistHistory = (resumedHistory && resumedHistory.length) ? resumedHistory : [];
          } catch (e) { persistHistory = []; }
          console.log(`  ↳ 已恢复本地会话, 持久化 ID: ${persistId} (续写 ${persistHistory.length} 轮)`);
        }
      } else if (startReason === "startup") {
        bindSessionFile(effSessionManager, localJsonl);
      }
      globalThis.__celagentPersistId = persistId;
      const maxTurn = (turns) => {
        if (!turns?.length) return 0;
        const nums = turns.map(t => Number(t.turn)).filter(n => Number.isFinite(n));
        return nums.length ? Math.max(...nums) : 0;
      };
      let seq = maxTurn(persistHistory);
      // P1: 进程内只留每轮摘要 (turn/role/msg/ts) — 完整 content/toolResults 已在
      // BOS 权威会话里, 全量驻留内存会让长会话无界增长。session_snapshot 取全量时
      // 从 BOS 重建, 队列尚未刷到的最新轮用内存摘要补齐
      let snapshotTurns = (persistHistory || []).map(t => ({ turn: t.turn, role: t.role || "assistant", msg: t.msg, ts: t.ts }));
      globalThis.__celagentSnapshotTurns = async () => {
        let full = [];
        try { full = (await loadHistoryFromBos(persistId)) || []; } catch (e) { full = []; }
        const have = new Set(full.map(t => Number(t.turn)).filter(Number.isFinite));
        return full
          .concat(snapshotTurns.filter(t => !have.has(Number(t.turn))))
          .sort((a, b) => Number(a.turn) - Number(b.turn));
      };
      result.session.subscribe(async (event) => {
        if (event?.type === "message_end" && event.message?.role === "user") {
          seq++;
          const text = extractText(event.message?.content);
          const fullContent = Array.isArray(event.message?.content) ? event.message.content : [];
          void celldCheckpoint(persistId, seq, "user", text || "(无文本)", { fullContent });
          queueSessionJsonl(persistId, effSessionManager);
          snapshotTurns.push({ turn: seq, role: "user", msg: text || "(无文本)", ts: Date.now() });
          return;
        }
        if (event?.type === "turn_end") {
          seq++;
          const text = extractText(event.message?.content);
          const fullContent = Array.isArray(event.message?.content) ? event.message.content : [];
          const fullToolResults = (event.toolResults || []).map(tr => ({
            toolName: tr.toolName,
            content: Array.isArray(tr.content) ? tr.content : null,
          }));
          const toolCalls = fullContent.filter(b => b.type === "toolCall").map(b => `${b.name}(${JSON.stringify(b.arguments)})`);
          const toolResults = fullToolResults.map(tr => {
            const resultText = Array.isArray(tr.content)
              ? tr.content.filter(b => b.type === "text").map(b => b.text).join(" ").trim()
              : "";
            return `${tr.toolName}: ${(resultText || "(无文本结果)").slice(0, 120)}`;
          });
          let msg = text || "(无文本)";
          if (toolCalls.length > 0) msg += ` [工具调用: ${toolCalls.join(", ").slice(0, 100)}]`;
          if (toolResults.length > 0) msg += ` [工具结果: ${toolResults.join(" | ").slice(0, 300)}]`;
          void celldCheckpoint(persistId, seq, "assistant", msg, { fullContent, fullToolResults });
          queueSessionJsonl(persistId, effSessionManager);
          snapshotTurns.push({ turn: seq, role: "assistant", msg, ts: Date.now() });
        }
      });
      queueSessionJsonl(persistId, effSessionManager);
      return { ...result, services, diagnostics: [] };  // 完整契约 (Bug: /new 需 services+diagnostics)
    };
    runtime = await pi.createAgentSessionRuntime(createRuntime, {
      cwd,
      agentDir: AGENT_DIR,
      sessionManager,
      sessionStartEvent: { type: "session_start", reason: "startup" },
    });
    session = runtime.session;
    console.log("✓ 会话就绪\n");
  } catch (e) {
    console.error(`会话失败: ${e.message}`);
    process.exit(1);
  }

  // 3. 启动完整 TUI
  const interactive = new pi.InteractiveMode(runtime, {
    verbose: true,
    tuiMode: undefined,
  });
  await interactive.init();
  await interactive.run();

  // 4. 退出前 flush BOS 队列 (Bug 17 修复: 避免丢最后几轮)
  // 走 flushOnExit 的 10s 上限 — 队列引入 transient 重试后, 裸 await bosQueue
  // 会在网络长时间不可用时挂死正常退出路径
  await flushOnExit();
}

// 退出时 flush 队列 — Bug 48/59: 信号处理策略
// pi 的 InteractiveMode 用 prependListener 注册 SIGTERM 优雅关闭 (dispose → 恢复终端 → exit(0)),
// 我们不能直接 process.exit 抢占它 (会导致终端未恢复), 也不能不注册 (注册 handler 会取消
// 信号的默认终止行为, 若 pi 尚未接管则进程永久挂死 — Bug 60 实测)。
// 方案: 收到信号 → 启动 flush + 启动一个 unref 兜底退出定时器。
//   - pi 正常接管时, 其 process.exit(0) 先发生, 定时器随进程退出消失 (unref 不阻塞);
//   - pi 未接管 (TUI 未初始化/失败) 时, 定时器 (12s > flush 10s 上限) 触发兜底退出, 不挂死。
// 正常退出路径 (run() 返回) 的 flush 在 main() 中 await bosQueue, 这里仅信号兜底。
let flushedOnExit = false;
async function flushOnExit() {
  if (flushedOnExit) return;
  flushedOnExit = true;
  // 超时保护: 队列最多等 10s (BOS 写失败不阻塞退出)
  await flushBosQueue(10000);
}
function scheduleSignalExit(code) {
  // unref: 若 pi 的 process.exit 先执行, 此定时器不会阻止进程退出
  setTimeout(() => process.exit(code), 12000).unref();
}
process.on("SIGINT", () => { scheduleSignalExit(130); void flushOnExit(); });
process.on("SIGTERM", () => { scheduleSignalExit(143); void flushOnExit(); });

main().catch(e => { console.error(`错误: ${e.message}`); process.exit(1); });
