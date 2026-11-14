#!/usr/bin/env node
// celagent 1.0 — durable agent runtime
// Pi 1.5 TUI + 分段 JSONL + manifest CAS + 工具 journal + 租约 + 本地 outbox
import { homedir, hostname } from "node:os";
import { join, basename, dirname } from "node:path";
import { existsSync, readFileSync, writeFileSync, mkdirSync, renameSync, copyFileSync, unlinkSync } from "node:fs";
import { createRequire } from "node:module";
import { execFile } from "node:child_process";

import * as pi from "@earendil-works/pi-coding-agent";
import { awsEnv, resolveEndpoint, isAllowedEndpoint, probeStoreCas } from "../src/bos.js";
import { storeFromCfg, turnsFromJsonl } from "../src/persist.js";
import {
  createV2, createBosAdapter, readSession, materialize, appendFromFile,
  listSessions, deleteSession, importV0, durabilityExtension,
  legacyJsonlKey, legacyTurnsKey, readManifest, ManifestCorruptError,
  SessionLockedError, LeaseLostError, LineageMismatchError,
} from "../src/v2/index.js";
import { sessionStatus } from "../src/v2/status.js";
import { snapshotWorkspace, restoreWorkspace } from "../src/v2/workspace.js";
import { history_search, session_snapshot } from "../src/bos-tools.js";

const require = createRequire(import.meta.url);
const CONFIG_DIR = join(homedir(), ".config", "celagent");
const AGENT_DIR = join(CONFIG_DIR, "pi-runtime");
const CELAGENT_VERSION = "1.0.0";
const CELD_HEALTH = "/.well-known/celld/health";
const CELD_NODES = ["http://127.0.0.1:18090", "http://127.0.0.1:18091"];
const sleep = (ms) => new Promise((r) => { const t = setTimeout(r, ms); t.unref?.(); });

function warn(msg) { console.warn(msg); }
function configFile() { return join(CONFIG_DIR, "settings.json"); }
function loadConfig() {
  const f = configFile();
  if (!existsSync(f)) return {};
  try { return JSON.parse(readFileSync(f, "utf8")); } catch (e) { return {}; }
}
function saveConfig(cfg) {
  mkdirSync(CONFIG_DIR, { recursive: true });
  const disk = loadConfig();
  const out = { ...disk, ...cfg };
  for (const k of ["persistence", "worker", "workspace"]) {
    if (cfg[k] && typeof cfg[k] === "object") {
      const prev = (disk[k] && typeof disk[k] === "object") ? disk[k] : {};
      out[k] = { ...prev, ...cfg[k] };
    }
  }
  const f = configFile();
  const tmp = `${f}.tmp-${process.pid}`;
  writeFileSync(tmp, JSON.stringify(out, null, 2) + "\n", { encoding: "utf8", mode: 0o600 });
  renameSync(tmp, f);
  try { require("node:fs").chmodSync(f, 0o600); } catch (e) { /* ignore */ }
}

function assertSafeSessionId(id) {
  if (!id || typeof id !== "string" || id.includes("..") || !/^[A-Za-z0-9._-]{1,128}$/.test(id)) {
    console.error("✗ 无效会话 ID (仅允许字母数字、点、下划线、连字符, 最长 128)");
    process.exit(1);
  }
}

function cliStore(cfg = loadConfig()) {
  const s = storeFromCfg(cfg);
  if (!s.bucket) throw new Error("未配置 persistence.bucket（运行 setup.sh 或 celagent config set persistence.bucket）");
  return s;
}

function openV2(cfg = loadConfig()) {
  return createV2({
    storeCfg: cliStore(cfg),
    configDir: CONFIG_DIR,
    owner: `${hostname()}-${process.pid}`,
  });
}

function sessionDirFor(cwd) {
  return join(AGENT_DIR, "sessions", encodeURIComponent(cwd.replace(/\//g, "-")));
}

function bindSessionFile(sm, dest) {
  if (!sm?.getSessionFile || !sm?.setSessionFile) return dest;
  mkdirSync(dirname(dest), { recursive: true });
  const cur = sm.getSessionFile();
  if (!cur || cur === dest) { try { sm.setSessionFile(dest); } catch (e) { /* ignore */ } return dest; }
  try {
    if (existsSync(cur)) {
      try { renameSync(cur, dest); }
      catch (e) { copyFileSync(cur, dest); try { unlinkSync(cur); } catch (e2) { /* ignore */ } }
    }
    sm.setSessionFile(dest);
  } catch (e) { /* 绑定失败不阻塞；Pi 仍用原路径 */ }
  return dest;
}

async function prepareSession(v2, sessionId, localFile, { allowNew = true } = {}) {
  await v2.lease.acquire(sessionId);
  let mat = null;
  try {
    mat = await materialize(v2.store, sessionId, localFile);
  } catch (e) {
    await v2.lease.release(sessionId).catch(() => {});
    throw e;
  }
  if (!mat) {
    const lj = await v2.store.get(legacyJsonlKey(sessionId));
    const lt = await v2.store.get(legacyTurnsKey(sessionId));
    if (lj.ok || lt.ok) {
      await v2.lease.release(sessionId).catch(() => {});
      throw new Error(`会话 ${sessionId} 是旧格式；先运行 celagent import-v0 ${sessionId}`);
    }
    if (!allowNew) {
      await v2.lease.release(sessionId).catch(() => {});
      throw new Error(`会话 ${sessionId} 不存在`);
    }
  }
  return mat;
}

function makeAppender({ v2, sessionId, localFile, onError }) {
  let chain = Promise.resolve();
  let stopped = false;
  let lastError = null;
  async function attempt() {
    for (let i = 0; i < 5; i++) {
      try {
        await appendFromFile(v2.store, sessionId, localFile, { lease: v2.lease });
        v2.outbox.clear(sessionId);
        return;
      } catch (e) {
        lastError = e;
        if (["lease-lost", "session-locked", "lineage-mismatch", "invalid-jsonl"].includes(e.code)) {
          stopped = true;
          onError?.(e);
          throw e;
        }
        await sleep(300 * 2 ** i);
      }
    }
    onError?.(lastError);
    throw lastError;
  }
  function append() {
    if (stopped) return chain;
    try { v2.outbox.mark(sessionId, localFile); } catch (e) { onError?.(e); }
    chain = chain.then(attempt).catch(() => { /* 保留 outbox，下次启动重试 */ });
    return chain;
  }
  async function flush(timeoutMs = 10000) {
    await Promise.race([chain, sleep(timeoutMs)]);
    return { pending: Boolean(v2.outbox.get(sessionId)), lastError };
  }
  return { append, flush, stop() { stopped = true; }, get lastError() { return lastError; } };
}

function printVersion() {
  console.log(`celagent v${CELAGENT_VERSION} — durable agent runtime`);
}

function printHelp() {
  printVersion();
  console.log(`
用法:
  celagent                         启动 TUI（新会话）
  celagent <id>                    续写指定会话（v2 manifest）
  celagent run --prompt "..."      无头运行（CI/脚本）
      [--session <id>] [--json] [--cwd <dir>]
  celagent list [--bucket B]       列出 v2 会话（旧格式单独标注）
  celagent export <id> [--bucket B]  导出 Pi JSONL
  celagent rm <id> [--yes]         删除会话（v2 + 旧对象）
  celagent import-v0 <id> [--yes]  旧 .jsonl/.json 一次性导入 v2
  celagent status [id]             持久化状态（durable seq / 租约 / outbox）
  celagent workspace snapshot <id> [--cwd dir]
  celagent workspace restore <id> [--cwd dir] [--force]
  celagent doctor                  自检（配置/凭证/aws CLI/CAS/版本）
  celagent cas-probe               存储条件写探测（RPO 门禁）
  celagent config get [key]        读取配置
  celagent config set <key> <val>  写入配置（如 model deepseek-v4-flash）
  celagent version                 显示版本
  celagent help                    显示帮助

保证:
  已提交 step 不丢；进行中的工具最多一个 unknown；
  有幂等键的副作用不重复；没有幂等键的副作用绝不静默重放。

示例:
  celagent run --prompt "修复这个测试" --session fix-1
  celagent status fix-1
  celagent import-v0 sess-old-1
`);
}

async function configCommand(args) {
  const [op, key, value] = args;
  const cfg = loadConfig();
  if (op === "get") {
    if (!key) { console.log(JSON.stringify(cfg, null, 2)); return; }
    const v = key.split(".").reduce((o, k) => o?.[k], cfg);
    console.log(v === undefined ? "(未设置)" : typeof v === "object" ? JSON.stringify(v) : String(v));
    return;
  }
  if (op !== "set") {
    console.error("用法: celagent config get [key] | config set <key> <value>");
    process.exit(1);
  }
  if (!key || value === undefined) {
    console.error("用法: celagent config set <key> <value>");
    process.exit(1);
  }
  if (!/^[A-Za-z0-9._-]+$/.test(key) || key.includes("..")) {
    console.error("✗ 非法配置键");
    process.exit(1);
  }
  const parts = key.split(".");
  const protectedObjs = new Set(["persistence", "worker", "workspace"]);
  if (parts.length === 1 && protectedObjs.has(parts[0]) && cfg[parts[0]] && typeof cfg[parts[0]] === "object") {
    console.error(`✗ ${parts[0]} 是对象, 请用 ${parts[0]}.<field> 设置`);
    process.exit(1);
  }
  if (key === "persistence.endpoint" && !isAllowedEndpoint(value)) {
    console.error("✗ persistence.endpoint 不允许（合格 https host / 本机；或设 CELAGENT_ALLOW_ENDPOINT=1）");
    process.exit(1);
  }
  let o = cfg;
  for (let i = 0; i < parts.length - 1; i++) { o[parts[i]] ??= {}; o = o[parts[i]]; }
  if (value === "" || value === "null") delete o[parts[parts.length - 1]];
  else o[parts[parts.length - 1]] = value;
  saveConfig(cfg);
  if (key === "model" || key === "provider") {
    const piSettingsFile = join(AGENT_DIR, "settings.json");
    if (existsSync(piSettingsFile)) {
      try {
        const piSettings = JSON.parse(readFileSync(piSettingsFile, "utf8"));
        if (key === "model") piSettings.defaultModel = value;
        if (key === "provider") piSettings.defaultProvider = value;
        writeFileSync(piSettingsFile, JSON.stringify(piSettings, null, 2) + "\n", "utf8");
        console.log(`  ↳ 已同步 pi-runtime 配置`);
      } catch (e) { warn(`  (警告: pi-runtime 配置同步失败: ${e.message})`); }
    }
  }
  console.log(value === "" || value === "null" ? `✓ 已删除 ${key}` : `✓ 已设置 ${key} = ${value}`);
}

async function doctorCommand() {
  console.log("celagent doctor — 自检\n");
  let ok = true;
  const cfg = loadConfig();
  let store;
  try { store = storeFromCfg(cfg); } catch (e) { console.log(`[1/6] 配置: ✗ ${e.message}`); process.exit(1); }
  console.log(`[1/6] 配置: ${store.bucket ? "✓ bucket=" + store.bucket : "✗ 缺 persistence.bucket"} endpoint=${store.endpoint} region=${store.region || "(缺)"} profile=${store.profile}`);
  if (!store.bucket) ok = false;
  if (!store.region) ok = false;

  const aws = await new Promise((resolve) => {
    execFile("aws", ["--version"], { timeout: 8000 }, (err, stdout, stderr) => resolve(err ? null : String(stdout || stderr).trim()));
  });
  console.log(`[2/6] aws CLI: ${aws ? "✓ " + aws.split("\n")[0] : "✗ 未安装/不在 PATH（BOS/S3 读写必需）"}`);
  if (!aws) ok = false;

  const hasKey = Boolean(process.env.DEEPSEEK_API_KEY || process.env.OPENAI_API_KEY || process.env.ANTHROPIC_API_KEY);
  const authFile = join(AGENT_DIR, "auth.json");
  console.log(`[3/6] LLM 凭证: ${hasKey ? "✓ 环境变量" : existsSync(authFile) ? "✓ auth.json" : "⚠ 未发现；Pi 首次运行会要求 /login"}`);
  console.log(`[4/6] Pi: ${pi.VERSION}（要求 1.5.x）`);

  if (store.bucket && aws) {
    const cas = await probeStoreCas({ bucket: store.bucket, endpoint: store.endpoint, profile: store.profile, region: store.region });
    console.log(`[5/6] CAS: ${cas.ok ? "✓ If-Match / If-None-Match / 写后读" : "✗ " + (cas.message || cas.error)}`);
    if (!cas.ok) ok = false;
  } else {
    console.log("[5/6] CAS: ⚠ 跳过（先修复 aws/配置）");
  }

  let celldUp = false;
  for (const base of CELD_NODES) {
    try {
      const r = await fetch(`${base}${CELD_HEALTH}`, { signal: AbortSignal.timeout(1500) });
      if (r.ok) { celldUp = true; break; }
    } catch (e) { /* down */ }
  }
  const enc = Boolean(process.env.CELAGENT_ENCRYPTION_KEY);
  console.log(`[6/6] 可选: celld ${celldUp ? "✓ 在线" : "⚠ 离线（v1 核心不依赖）"}；加密 ${enc ? "✓ 开启" : "⚠ 未开启"}`);
  if (ok && store.bucket) {
    try {
      const listed = await listSessions(createBosAdapter(store));
      console.log(`       v2 会话: ${listed.sessions.length}；旧格式待导入: ${listed.legacy.length}`);
    } catch (e) { console.log(`       (列举会话失败: ${e.message})`); }
  }
  console.log(ok ? "\n结论: ✓ 核心正常" : "\n结论: ✗ 存在异常，按上面 ✗ 项处理");
  process.exit(ok ? 0 : 1);
}

async function casProbeCommand() {
  const cfg = loadConfig();
  let store;
  try { store = storeFromCfg(cfg); } catch (e) { console.error(`✗ ${e.message}`); process.exit(1); }
  const idx = process.argv.indexOf("--bucket");
  const bucket = idx > 0 ? process.argv[idx + 1] : store.bucket;
  if (!bucket) { console.error("✗ 未找到 bucket"); process.exit(1); }
  const cas = await probeStoreCas({ bucket, endpoint: store.endpoint, profile: store.profile, region: store.region });
  if (!cas.ok) {
    console.error(`✗ ${cas.message || cas.error}`);
    process.exit(cas.transient ? 2 : 1);
  }
  console.log(`✓ ${cas.message}`);
}

async function listCommand() {
  const cfg = loadConfig();
  const idx = process.argv.indexOf("--bucket");
  if (idx > 0 && process.argv[idx + 1]) {
    cfg.persistence = { ...(cfg.persistence || {}), bucket: process.argv[idx + 1] };
  }
  const store = createBosAdapter(cliStore(cfg));
  const r = await listSessions(store);
  if (r.sessions.length === 0 && r.legacy.length === 0) { console.log("(暂无会话)"); return; }
  console.log(`celagent — v2 会话 (${r.sessions.length})\n`);
  console.log("  ID                                                               seq      更新");
  console.log("  " + "-".repeat(86));
  for (const s of r.sessions) {
    console.log(`  ${s.id.padEnd(64)} ${String(s.seq).padStart(5)}  ${String(s.updatedAt || "").slice(0, 19)}`);
  }
  if (r.legacy.length > 0) {
    console.log(`\n旧格式（需 celagent import-v0 <id>）: ${r.legacy.join(", ")}`);
  }
  console.log("\n续写: celagent <id>    新会话: celagent");
}

async function exportCommand(id) {
  if (!id || id.startsWith("-")) { console.error("用法: celagent export <会话ID> [--bucket B]"); process.exit(1); }
  assertSafeSessionId(id);
  const cfg = loadConfig();
  const idx = process.argv.indexOf("--bucket");
  if (idx > 0 && process.argv[idx + 1]) cfg.persistence = { ...(cfg.persistence || {}), bucket: process.argv[idx + 1] };
  const store = createBosAdapter(cliStore(cfg));
  const session = await readSession(store, id);
  if (session) { process.stdout.write(session.text); return; }
  const legacy = await store.get(legacyJsonlKey(id));
  if (legacy.ok) { process.stdout.write(legacy.body.endsWith("\n") ? legacy.body : legacy.body + "\n"); return; }
  const turns = await store.get(legacyTurnsKey(id));
  if (!turns.ok) { console.error(`✗ 会话不存在: ${id}`); process.exit(1); }
  const parsed = JSON.parse(turns.body);
  console.log(JSON.stringify({ id, exportedAt: new Date().toISOString(), turns: parsed.turns || [] }, null, 2));
}

async function rmCommand(id) {
  if (!id || id.startsWith("-")) { console.error("用法: celagent rm <会话ID> [--yes]"); process.exit(1); }
  assertSafeSessionId(id);
  const cfg = loadConfig();
  const idx = process.argv.indexOf("--bucket");
  if (idx > 0 && process.argv[idx + 1]) cfg.persistence = { ...(cfg.persistence || {}), bucket: process.argv[idx + 1] };
  const store = createBosAdapter(cliStore(cfg));
  const force = process.argv.includes("--yes") || process.argv.includes("-y");
  let confirm = force;
  if (!confirm) {
    if (!process.stdin.isTTY) { console.error("✗ 非交互删除需要 --yes"); process.exit(1); }
    confirm = await new Promise((resolve) => {
      const readline = require("node:readline").createInterface({ input: process.stdin, output: process.stdout });
      readline.question(`确定删除会话 "${id}"（包括 v2 segments 与旧对象，不可恢复）? [y/N] `, (a) => { readline.close(); resolve(/^y/i.test(a.trim())); });
    });
  }
  if (!confirm) { console.log("已取消"); return; }
  const n = await deleteSession(store, id);
  if (n === 0) { console.error("✗ 会话不存在"); process.exit(1); }
  console.log(`✓ 已删除 ${id}（${n} 个对象）`);
}

async function importCommand(id) {
  if (!id || id.startsWith("-")) { console.error("用法: celagent import-v0 <会话ID> [--yes]"); process.exit(1); }
  assertSafeSessionId(id);
  const cfg = loadConfig();
  const idx = process.argv.indexOf("--bucket");
  if (idx > 0 && process.argv[idx + 1]) cfg.persistence = { ...(cfg.persistence || {}), bucket: process.argv[idx + 1] };
  const v2 = openV2(cfg);
  const cwd = process.cwd();
  const dir = sessionDirFor(cwd);
  const localFile = join(dir, `${id}.jsonl`);
  mkdirSync(dir, { recursive: true });
  const force = process.argv.includes("--yes") || process.argv.includes("-y");
  if (!force) {
    if (!process.stdin.isTTY) { console.error("✗ 非交互导入需要 --yes"); process.exit(1); }
    const ok = await new Promise((resolve) => {
      const readline = require("node:readline").createInterface({ input: process.stdin, output: process.stdout });
      readline.question(`把旧会话 "${id}" 导入 v2（旧对象保留）? [y/N] `, (a) => { readline.close(); resolve(/^y/i.test(a.trim())); });
    });
    if (!ok) { console.log("已取消"); return; }
  }
  try {
    const r = await importV0(v2.store, id, localFile, { lease: v2.lease, cwd });
    console.log(`✓ 已导入 ${id}: ${r.entries} 条 entry，durable seq=${r.manifest.head.seq}`);
    console.log(`  旧对象保留；下次 celagent ${id} 走 v2 manifest`);
  } finally {
    await v2.lease.release(id).catch(() => {});
  }
}

async function statusCommand(id) {
  const cfg = loadConfig();
  const store = createBosAdapter(cliStore(cfg));
  const v2 = createV2({ storeCfg: cliStore(cfg), configDir: CONFIG_DIR, owner: `${hostname()}-${process.pid}` });
  if (!id) {
    const r = await listSessions(store);
    for (const s of r.sessions) {
      const st = await sessionStatus({ store, outbox: v2.outbox, journal: v2.journal, sessionId: s.id });
      console.log(`${s.id}\tseq=${st.durableSeq}\tsegments=${st.segments}\tlease=${st.lease ? st.lease.owner + "@" + st.lease.epoch : "-"}\toutbox=${st.outbox.pending ? "pending" : "clean"}`);
    }
    if (r.sessions.length === 0) console.log("(暂无 v2 会话)");
    return;
  }
  assertSafeSessionId(id);
  const st = await sessionStatus({ store, outbox: v2.outbox, journal: v2.journal, sessionId: id });
  console.log(JSON.stringify(st, null, 2));
}

async function workspaceCommand(args) {
  const [op, id] = args;
  if (!op || !id || !["snapshot", "restore"].includes(op)) {
    console.error("用法: celagent workspace snapshot <id> [--cwd dir] | workspace restore <id> [--cwd dir] [--force]");
    process.exit(1);
  }
  assertSafeSessionId(id);
  const cwdIdx = process.argv.indexOf("--cwd");
  const cwd = cwdIdx > 0 ? process.argv[cwdIdx + 1] : process.cwd();
  const v2 = openV2();
  try {
    if (op === "snapshot") {
      const r = await snapshotWorkspace({ store: v2.store, sessionId: id, cwd });
      console.log(`✓ 工作区快照 ${r.snapshotId}（${(r.bytes / 1024).toFixed(1)} KB）`);
    } else {
      const r = await restoreWorkspace({ store: v2.store, sessionId: id, cwd, force: process.argv.includes("--force") });
      console.log(`✓ 已恢复工作区 ${r.snapshotId}（${(r.bytes / 1024).toFixed(1)} KB）`);
    }
  } finally {
    await v2.lease.release(id).catch(() => {});
  }
}

function buildServices(cwd, holder, v2) {
  const settingsManager = pi.SettingsManager.create(cwd, AGENT_DIR, { projectTrusted: false });
  const ext = durabilityExtension({
    getSessionId: () => holder.id,
    journal: v2.journal,
    onError: (e) => warn(`  (journal 警告: ${e.message})`),
  });
  return pi.createAgentSessionServices({
    cwd,
    agentDir: AGENT_DIR,
    settingsManager,
    modelRuntimeSignal: AbortSignal.timeout(15000),
    resourceLoaderOptions: { extensionFactories: [ext] },
  });
}

function attachSnapshotTurns(v2, holder) {
  globalThis.__celagentPersistId = holder.id;
  globalThis.__celagentSnapshotTurns = async () => {
    try {
      const s = await readSession(v2.store, holder.id);
      return s ? turnsFromJsonl(s.text) : [];
    } catch (e) { return []; }
  };
}

async function runHeadless(args) {
  const promptIdx = args.indexOf("--prompt");
  const prompt = promptIdx >= 0 ? args[promptIdx + 1] : "";
  if (!prompt) { console.error('用法: celagent run --prompt "..." [--session id] [--json] [--cwd dir]'); process.exit(1); }
  const sidIdx = args.indexOf("--session");
  const cwdIdx = args.indexOf("--cwd");
  const cwd = cwdIdx >= 0 ? args[cwdIdx + 1] : process.cwd();
  const sessionId = sidIdx >= 0 ? args[sidIdx + 1] : `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  assertSafeSessionId(sessionId);
  const json = args.includes("--json");
  const dir = sessionDirFor(cwd);
  const localFile = join(dir, `${sessionId}.jsonl`);
  mkdirSync(dir, { recursive: true });

  const v2 = openV2();
  const holder = { id: sessionId };
  let session = null;
  let release = null;
  try {
    const mat = await prepareSession(v2, sessionId, localFile);
    const sm = mat
      ? pi.SessionManager.open(localFile, dir, cwd)
      : pi.SessionManager.create(cwd, dir);
    if (!mat) bindSessionFile(sm, localFile);
    const services = await buildServices(cwd, holder, v2);
    attachSnapshotTurns(v2, holder);
    const created = await pi.createAgentSessionFromServices({
      services, sessionManager: sm,
      sessionStartEvent: { type: "session_start", reason: "startup" },
      customTools: [history_search, session_snapshot],
    });
    session = created.session;
    const app = makeAppender({
      v2, sessionId, localFile,
      onError: (e) => warn(`  (持久化警告: ${e.message})`),
    });
    activeRuntime = { v2, sessionId, app };
    if (mat?.needsAppend || v2.outbox.get(sessionId)) app.append();
    release = async () => {
      await app.flush(10000);
      await v2.lease.release(sessionId).catch(() => {});
    };
    session.subscribe((event) => {
      if (["message_end", "turn_end", "session_compact"].includes(event?.type)) app.append();
      if (json && event?.type === "message_end" && event.message?.role === "assistant") {
        const text = Array.isArray(event.message.content)
          ? event.message.content.filter((b) => b.type === "text").map((b) => b.text).join("\n")
          : String(event.message.content || "");
        if (text) process.stdout.write(JSON.stringify({ type: "assistant", text }) + "\n");
      }
    });
    await session.prompt(prompt);
    if (!json) {
      const text = session.getLastAssistantText?.();
      if (text) process.stdout.write(text + "\n");
    }
  } catch (e) {
    console.error(`✗ run 失败: ${e.message}`);
    process.exitCode = 1;
  } finally {
    if (release) await release();
    activeRuntime = null;
    try { session?.dispose?.(); } catch (e) { /* ignore */ }
  }
}

let activeRuntime = null;
let flushPromise = null;
async function flushOnExit() {
  if (flushPromise) return flushPromise;
  flushPromise = (async () => {
    if (!activeRuntime) return;
    try {
      const r = await activeRuntime.app.flush(10000);
      if (r.pending) warn("  (警告: 仍有未上传的会话尾部，已保存在本地 outbox，下次启动重试)");
      await activeRuntime.v2.lease.release(activeRuntime.sessionId).catch(() => {});
    } catch (e) { /* ignore */ }
  })();
  return flushPromise;
}
function scheduleSignalExit(code) {
  setTimeout(() => process.exit(code), 12000).unref();
}
process.on("SIGINT", () => { scheduleSignalExit(130); void flushOnExit(); });
process.on("SIGTERM", () => { scheduleSignalExit(143); void flushOnExit(); });

// Pi 的 /quit 会直接 process.exit(0)，绕过 main 的 finally；拦截一次，先 flush + release lease。
const realExit = process.exit.bind(process);
let exitFlushStarted = false;
process.exit = (code) => {
  if (exitFlushStarted) return realExit(code);
  exitFlushStarted = true;
  Promise.race([flushOnExit(), sleep(12000)]).finally(() => realExit(code));
};

async function main() {
  const cwd = process.cwd();
  const cmd = process.argv[2];
  if (cmd === "help" || cmd === "--help" || cmd === "-h") { printHelp(); return; }
  if (cmd === "version" || cmd === "--version" || cmd === "-v") { printVersion(); return; }
  if (cmd === "config") { await configCommand(process.argv.slice(3)); return; }
  if (cmd === "doctor") { await doctorCommand(); return; }
  if (cmd === "cas-probe") { await casProbeCommand(); return; }
  if (cmd === "list" || cmd === "--list") { await listCommand(); return; }
  if (cmd === "export") { await exportCommand(process.argv[3]); return; }
  if (cmd === "rm") { await rmCommand(process.argv[3]); return; }
  if (cmd === "import-v0") { await importCommand(process.argv[3]); return; }
  if (cmd === "status") { await statusCommand(process.argv[3]); return; }
  if (cmd === "workspace") { await workspaceCommand(process.argv.slice(3)); return; }
  if (cmd === "run") { await runHeadless(process.argv.slice(3)); return; }
  if (cmd && cmd.startsWith("-")) { console.error(`未知选项: ${cmd}（celagent help）`); process.exit(1); }

  if (process.argv[2]) assertSafeSessionId(process.argv[2]);
  const sessionId = process.argv[2] || `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  const dir = sessionDirFor(cwd);
  const localFile = join(dir, `${sessionId}.jsonl`);
  mkdirSync(dir, { recursive: true });

  let v2;
  try { v2 = openV2(); }
  catch (e) { console.error(`✗ ${e.message}`); process.exit(1); }

  let mat;
  try { mat = await prepareSession(v2, sessionId, localFile); }
  catch (e) {
    if (e instanceof SessionLockedError || e instanceof LeaseLostError || e instanceof LineageMismatchError) {
      console.error(`✗ ${e.message}`); process.exit(1);
    }
    throw e;
  }

  let manager;
  try {
    manager = mat
      ? pi.SessionManager.open(localFile, dir, cwd)
      : pi.SessionManager.create(cwd, dir);
    if (!mat) bindSessionFile(manager, localFile);
  } catch (e) { console.error(`✗ 会话文件无法打开: ${e.message}`); process.exit(1); }

  const holder = { id: sessionId };
  attachSnapshotTurns(v2, holder);
  const app = makeAppender({ v2, sessionId, localFile, onError: (e) => warn(`  (持久化警告: ${e.message})`) });
  activeRuntime = { v2, sessionId, app };
  if (mat?.needsAppend || v2.outbox.get(sessionId)) app.append();

  let services;
  try { services = await buildServices(cwd, holder, v2); }
  catch (e) { console.error(`services 失败: ${e.message}`); process.exit(1); }

  let current = { sessionId, localFile, manager, app };

  const switchSession = async (nextId, nextFile) => {
    if (current) {
      await current.app.flush(5000);
      await v2.lease.release(current.sessionId).catch(() => {});
      current.app.stop();
    }
    await v2.lease.acquire(nextId);
    holder.id = nextId;
    globalThis.__celagentPersistId = nextId;
    const nextApp = makeAppender({ v2, sessionId: nextId, localFile: nextFile, onError: (e) => warn(`  (持久化警告: ${e.message})`) });
    current = { sessionId: nextId, localFile: nextFile, manager: manager, app: nextApp };
    activeRuntime = { v2, sessionId: nextId, app: nextApp };
    return nextApp;
  };

  try {
    const runtime = await pi.createAgentSessionRuntime(async (opts = {}) => {
      const effManager = opts?.sessionManager || manager;
      const startReason = opts?.sessionStartEvent?.reason || "startup";
      if (startReason === "new" || startReason === "fork") {
        const nextId = `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
        const nextFile = join(dir, `${nextId}.jsonl`);
        bindSessionFile(effManager, nextFile);
        await switchSession(nextId, nextFile);
        console.log(`  ↳ ${startReason === "fork" ? "fork" : "新"}会话持久化 ID: ${nextId}`);
      } else if (startReason === "resume") {
        const f = effManager.getSessionFile?.();
        const nextId = f ? basename(f).replace(/\.jsonl$/i, "") : sessionId;
        const nextFile = join(dir, `${nextId}.jsonl`);
        bindSessionFile(effManager, nextFile);
        if (nextId !== current.sessionId) await switchSession(nextId, nextFile);
      }
      const result = await pi.createAgentSessionFromServices({
        cwd, agentDir: AGENT_DIR, services,
        sessionManager: effManager,
        sessionStartEvent: opts?.sessionStartEvent || { type: "session_start", reason: "startup" },
        customTools: [history_search, session_snapshot],
      });
      result.session.subscribe((event) => {
        if (["message_end", "turn_end", "session_compact"].includes(event?.type)) current.app.append();
      });
      if (current.app) current.app.append();
      return { ...result, services, diagnostics: [] };
    }, {
      cwd, agentDir: AGENT_DIR, sessionManager: manager,
      sessionStartEvent: { type: "session_start", reason: "startup" },
    });

    console.log(`celagent v${CELAGENT_VERSION} — 会话 ${sessionId}\n`);
    const interactive = new pi.InteractiveMode(runtime, { verbose: true, tuiMode: "regular" });
    await interactive.init();
    await interactive.run();
  } finally {
    await flushOnExit();
    try { await manager.dispose?.(); } catch (e) { /* ignore */ }
  }
}

main().catch((e) => {
  if (/encryption-key-required/.test(String(e.message))) {
    console.error("错误: 这些对象已加密；请设置 CELAGENT_ENCRYPTION_KEY（32 字节 base64）后重试");
  } else {
    console.error(`错误: ${e.message}`);
  }
  process.exit(1);
});
