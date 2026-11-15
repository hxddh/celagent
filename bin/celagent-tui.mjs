#!/usr/bin/env node
// celagent 2.0 — durable agent computer
// tmux supervisor + Pi 1.5 TUI + 分段 JSONL + manifest CAS + 工具 journal + 租约 + 增量工作区检查点
import { homedir, hostname } from "node:os";
import { join, basename, dirname, resolve } from "node:path";
import { existsSync, readFileSync, writeFileSync, mkdirSync, renameSync, copyFileSync, unlinkSync } from "node:fs";
import { createRequire } from "node:module";
import { execFile } from "node:child_process";
import { fileURLToPath } from "node:url";

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
import { restoreWorkspace as restoreLegacyWorkspace } from "../src/v2/workspace.js";
import { createUsageRecorder } from "../src/v2/usage.js";
import {
  checkpointWorkspace, listWorkspaceCheckpoints, restoreWorkspaceCheckpoint,
  readWorkspaceCheckpoint, createCheckpointRunner,
} from "../src/v2/workspace-cas.js";
import {
  attachTmux, listStates, logPath, startSupervisor, stopSupervisor, supervisorStatus,
  tmuxAvailable, tmuxHasSession, tmuxListSessions, watchSupervisor, writeStop,
  clearStop as clearSupervisorStop,
} from "../src/v2/supervisor.js";
import { history_search, session_snapshot } from "../src/bos-tools.js";
import {
  addTrigger, listTriggers, removeTrigger, runTriggerForeground,
  startTriggerDaemon, stopTriggerDaemon, triggerDaemonStatus, runTriggerDaemon,
} from "../src/v2/triggers.js";
import {
  createDelegateTool, createDelegator, readChildState, readChildResult, listInbox, consumeInbox,
  takeNextPrompt, patchChildState, writeChildResult, lastAssistantTextFromTurns,
  commitWorktree, worktreeSummary, appendOutput, outputLogPath,
  repoRoot, repoHead, repoDirty, createWorktree, removeWorktree, branchName, worktreePathFor,
} from "../src/v3/index.js";
import {
  newGoalId, validGoalId, readGoal, patchGoal, writeGoal, writeGoalDoc,
  writeAnswer, listGoals, DEFAULT_PROTECT,
  DEFAULT_MAX_ATTEMPTS, DEFAULT_BUDGET_USD, DEFAULT_MAX_HOURS,
  writeStop as writeGoalStop, clearStop as clearGoalStop,
  attemptPidPath, checkPidPath, outputLogPath as goalOutputLogPath,
  runGoal, makeCheckRunner, makeAttemptRunner,
} from "../src/v4/index.js";

const require = createRequire(import.meta.url);
const CONFIG_DIR = join(homedir(), ".config", "celagent");
const AGENT_DIR = join(CONFIG_DIR, "pi-runtime");
const CELAGENT_VERSION = "4.1.0";
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

function launcher() {
  const isBun = Boolean(process.versions.bun);
  if (isBun) return { node: process.execPath, bin: null };
  const bin = resolve(process.argv[1] || fileURLToPath(import.meta.url));
  return { node: process.execPath, bin };
}

function workspaceOpts(cfg = loadConfig()) {
  const w = cfg.workspace && typeof cfg.workspace === "object" ? cfg.workspace : {};
  return {
    excludes: Array.isArray(w.excludes) && w.excludes.length > 0 ? w.excludes : undefined,
    maxBytes: Number(w.maxBytes) > 0 ? Number(w.maxBytes) : undefined,
    maxFileBytes: Number(w.maxFileBytes) > 0 ? Number(w.maxFileBytes) : undefined,
    auto: w.auto !== false,
  };
}

function makeCheckpointRunner(v2, sessionId, cwd, { quiet = false } = {}) {
  const opts = workspaceOpts();
  if (!opts.auto) return null;
  return createCheckpointRunner({
    store: v2.store, sessionId, cwd, lease: v2.lease,
    excludes: opts.excludes, maxBytes: opts.maxBytes, maxFileBytes: opts.maxFileBytes,
    onProgress: (r) => {
      if (!quiet && r.uploaded > 0) {
        warn(`  ✓ 工作区检查点 ${r.checkpointId}（seq ${r.seq}，${r.files} 文件，上传 ${r.uploaded} blob）`);
      }
    },
    onError: (e) => { if (e.code !== "no-manifest") warn(`  (工作区检查点警告: ${e.message})`); },
  });
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
  // 心跳从 acquire 后立即开始：大历史 materialize 可能超过 15 秒 TTL。
  const stopHeartbeat = v2.lease.startHeartbeat(sessionId, (e) => warn(`  (租约警告: ${e.message})`));
  let mat = null;
  try {
    mat = await materialize(v2.store, sessionId, localFile);
  } catch (e) {
    stopHeartbeat();
    await v2.lease.release(sessionId).catch(() => {});
    throw e;
  }
  if (!mat) {
    const lj = await v2.store.get(legacyJsonlKey(sessionId));
    const lt = await v2.store.get(legacyTurnsKey(sessionId));
    if (lj.ok || lt.ok) {
      stopHeartbeat();
      await v2.lease.release(sessionId).catch(() => {});
      throw new Error(`会话 ${sessionId} 是旧格式；先运行 celagent import-v0 ${sessionId}`);
    }
    if (!allowNew) {
      stopHeartbeat();
      await v2.lease.release(sessionId).catch(() => {});
      throw new Error(`会话 ${sessionId} 不存在`);
    }
  }
  return { mat, stopHeartbeat };
}

function makeAppender({ v2, sessionId, localFile, onError, onAppended }) {
  let chain = Promise.resolve();
  let stopped = false;
  let lastError = null;
  async function attempt() {
    for (let i = 0; i < 5; i++) {
      try {
        await appendFromFile(v2.store, sessionId, localFile, { lease: v2.lease });
        v2.outbox.clear(sessionId);
        try { onAppended?.(); } catch (e) { onError?.(e); }
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
  console.log(`celagent v${CELAGENT_VERSION} — durable agent computer`);
}

function printHelp() {
  printVersion();
  console.log(`
用法:
  celagent                         启动/附着本会话的 supervisor（默认）
  celagent <id>                    启动/附着指定会话
  celagent up [id] [--cwd dir]     后台启动 supervisor（--attach 可直接附着）
  celagent attach <id>             附着到运行中的 supervisor（关闭终端 = detach）
  celagent down <id>               停止 supervisor（会话与检查点保留）
  celagent ps                      列出 supervisor（--tree 按父子关系）
  celagent logs <id>               查看 supervisor 日志
  celagent run --prompt "..."      无头运行（CI/脚本）
      [--session <id>] [--json] [--cwd <dir>]
  celagent delegate spawn "<task>" [--cwd dir] [--no-isolate]  派一个子 agent
  celagent delegate list [--all]   列出子 agent
  celagent delegate wait <id> [--timeout ms]
  celagent delegate send <id> "<msg>"
  celagent goal start "<objective>" --accept "npm test" [--cwd dir]
      [--budget 5] [--max-attempts 8] [--max-hours 2] [--protect tests/**,...] [--in-place]
  celagent goal status [id] | list
  celagent goal answer <id> "<answer>"
  celagent goal stop <id> | resume <id>
  celagent list [--bucket B]       列出 v2 会话（旧格式单独标注）
  celagent export <id> [--bucket B]  导出 Pi JSONL
  celagent rm <id> [--yes]         删除会话（v2 + 旧对象）
  celagent import-v0 <id> [--yes]  旧 .jsonl/.json 一次性导入 v2
  celagent status [id]             持久化状态（durable seq / 租约 / outbox / 工作区）
  celagent workspace checkpoint <id> [--cwd dir]   生成增量工作区检查点
  celagent workspace list <id>     列出检查点
  celagent workspace restore <id> [--checkpoint id] [--cwd dir] [--force]
  celagent workspace status <id>   当前检查点指针
  celagent trigger add <id> --prompt ... [--cwd dir] [--every 60]
  celagent trigger list|rm|run <id> | daemon [--start|--stop|--status]
  celagent doctor                  自检（配置/凭证/aws CLI/tmux/CAS/版本）
  celagent cas-probe               存储条件写探测（RPO 门禁）
  celagent config get [key]        读取配置
  celagent config set <key> <val>  写入配置（如 model deepseek-v4-flash）
  celagent version                 显示版本
  celagent help                    显示帮助

2.0 保证:
  客户端断开 / 终端关闭，supervisor 继续执行；
  工具结果后自动生成增量工作区检查点；
  崩溃后从最后一个检查点和 manifest 恢复；未完成工具最多一个 unknown；
  有幂等键的副作用不重复；没有幂等键的副作用绝不静默重放。

3.0 保证:
  子 agent 是完整 session：独立 worktree / lease / journal / checkpoint；
  父进程退出或崩溃，子 agent 继续跑；结果可再取回；
  kill 在 turn 中间只标记 interrupted，不重放；
  子 agent 不能再 delegate，也不会自动 push；合并由人决定。

4.1 保证:
  goal 只有验收命令 exit 0 才算完成；
  goal runner 是独立进程；agent 是它每次拉起的 headless 子进程；
  预算 / 墙钟到点杀 attempt/check 进程组；
  尝试数 / 同一失败 ×3 / 改测试 → 停或问人；
  goal 是 headless（用 logs/answer），默认独立 worktree，不自动 merge/push。

示例:
  celagent up fix-auth --cwd ~/repo --attach
  # 关闭终端后，在另一台机器（同一 bucket）：
  celagent up fix-auth --cwd ~/repo --restore --attach
  celagent workspace list fix-auth
  celagent run --prompt "修复这个测试" --session fix-1
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
  const tmux = await tmuxAvailable();
  console.log(`[6/6] 2.0 supervisor: tmux ${tmux ? "✓ " + tmux : "✗ 未安装（up/attach 需要）"}；celld ${celldUp ? "✓ 在线" : "⚠ 离线（可选）"}；加密 ${enc ? "✓ 开启" : "⚠ 未开启"}`);
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

function parseUpArgs(args) {
  let id = null, cwd = process.cwd(), attach = false, restore = false;
  for (let i = 0; i < args.length; i++) {
    const a = args[i];
    if (a === "--cwd") { cwd = args[++i] || cwd; continue; }
    if (a === "--attach") { attach = true; continue; }
    if (a === "--no-attach") { attach = false; continue; }
    if (a === "--restore") { restore = true; continue; }
    if (a.startsWith("-")) throw new Error(`未知选项: ${a}`);
    if (!id) id = a;
    else throw new Error(`多余的参数: ${a}`);
  }
  return { id, cwd: resolve(cwd), attach, restore };
}

async function restoreLatestBeforeUp(id, cwd) {
  const v2 = openV2();
  await v2.lease.acquire(id);
  try {
    const cp = await readWorkspaceCheckpoint(v2.store, id);
    if (!cp) {
      const m = await readManifest(v2.store, id);
      if (m?.workspace?.key && m.workspace.format !== "cas-v1") {
        const r = await restoreLegacyWorkspace({ store: v2.store, sessionId: id, cwd, force: true });
        console.log(`✓ 已恢复 1.0 工作区 tar ${r.snapshotId}（${(r.bytes / 1024).toFixed(1)} KB）`);
      } else {
        warn("  (没有工作区检查点，跳过 --restore)");
      }
      return;
    }
    const r = await restoreWorkspaceCheckpoint({
      store: v2.store, sessionId: id, checkpointId: cp.id, cwd, force: true,
    });
    console.log(`✓ 已恢复工作区检查点 ${r.checkpointId}（seq ${r.seq}，${r.files} 文件）`);
  } finally {
    await v2.lease.release(id).catch(() => {});
  }
}

async function upCommand(args) {
  const { id: rawId, cwd, attach, restore } = parseUpArgs(args);
  const id = rawId || `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  assertSafeSessionId(id);
  const tmux = await tmuxAvailable();
  if (!tmux) throw new Error("2.0 supervisor 需要 tmux；安装 tmux 后重试");
  if (restore) await restoreLatestBeforeUp(id, cwd);
  const { node, bin } = launcher();
  const r = await startSupervisor({ configDir: CONFIG_DIR, id, cwd, bin, node });
  // 给 TUI 一个启动窗口；如果它在 900ms 内退出（配置/租约/密钥错误），把日志带出来。
  await new Promise((res) => setTimeout(res, 900));
  if (!(await tmuxHasSession(id))) {
    let tail = "";
    try { tail = readFileSync(logPath(CONFIG_DIR, id), "utf8").split("\n").slice(-20).join("\n"); } catch (e) { /* ignore */ }
    await stopSupervisor({ configDir: CONFIG_DIR, id }).catch(() => {});
    throw new Error(`supervisor 启动后立即退出${tail ? `:\n${tail}` : ""}`);
  }
  console.log(`✓ supervisor ${id} ${r.already ? "已在运行" : "已启动"}（tmux ${"celagent-" + id}）`);
  console.log(`  cwd: ${cwd}`);
  console.log(`  日志: ${logPath(CONFIG_DIR, id)}`);
  console.log(`  attach: celagent attach ${id}`);
  console.log(`  detach: Ctrl-b d；停止: celagent down ${id}`);
  if (attach) await attachTmux(id);
}

async function attachCommand(args) {
  const id = args.find((a) => !a.startsWith("-"));
  if (!id) throw new Error("用法: celagent attach <id>");
  assertSafeSessionId(id);
  const st = await supervisorStatus({ configDir: CONFIG_DIR, id }).catch(() => null);
  if (st?.state?.kind === "goal") {
    throw new Error(`${id} 是 goal headless runner，没有 TUI；用 celagent logs ${id} / goal status ${id}，停止后再 up ${id} 接管 session`);
  }
  await attachTmux(id);
}

async function downCommand(args) {
  const id = args.find((a) => !a.startsWith("-"));
  if (!id) throw new Error("用法: celagent down <id>");
  assertSafeSessionId(id);
  const st = await supervisorStatus({ configDir: CONFIG_DIR, id });
  if (!st.running && !st.watchdog?.alive && !st.state) {
    console.log(`(会话 ${id} 没有运行中的 supervisor)`);
    return;
  }
  await stopSupervisor({ configDir: CONFIG_DIR, id, gracefulMs: 5000 });
  // tmux kill 可能只发 SIGHUP；显式清理本机已死 owner 的 lease，避免等 15 秒。
  try {
    const v2 = openV2();
    await v2.lease.releaseIfLocalDead(id);
  } catch (e) { /* 加密 key 缺失等场景留给 TTL */ }
  console.log(`✓ 已停止 ${id}（会话数据与工作区检查点保留）`);
}

function stopPidFile(path) {
  try {
    const pid = Number(readFileSync(path, "utf8").trim());
    if (pid > 0) {
      try { process.kill(-pid, "SIGKILL"); }
      catch (e) { try { process.kill(pid, "SIGKILL"); } catch (e2) { /* ignore */ } }
    }
  } catch (e) { /* no pidfile */ }
  try { unlinkSync(path); } catch (e) { /* ignore */ }
}

function stopGoalChildren(id) {
  stopPidFile(attemptPidPath(CONFIG_DIR, id));
  stopPidFile(checkPidPath(CONFIG_DIR, id));
}

async function goalCommand(args) {
  const sub = args[0] || "list";
  const opt = (name) => { const i = args.indexOf(name); return i > 0 ? args[i + 1] : null; };
  const { node, bin } = launcher();

  if (sub === "start") {
    const positional = [];
    for (let i = 1; i < args.length; i++) {
      const a = args[i];
      if (["--accept", "--cwd", "--budget", "--max-attempts", "--max-hours", "--protect", "--objective"].includes(a)) { i += 1; continue; }
      if (a.startsWith("--")) continue;
      positional.push(a);
    }
    const objective = (opt("--objective") || positional.join(" ")).trim();
    const accept = (opt("--accept") || "").trim();
    if (!objective || !accept) throw new Error('用法: celagent goal start "<objective>" --accept "npm test" [--cwd dir]');
    const cwd = opt("--cwd") ? resolve(opt("--cwd")) : process.cwd();
    if (!loadConfig().persistence?.bucket) throw new Error("未配置 persistence.bucket（goal 需要持久化会话）");
    if (!(await tmuxAvailable())) throw new Error("goal 需要 tmux");
    const inPlace = args.includes("--in-place");
    const repo = repoRoot(cwd);
    if (!inPlace && !repo) throw new Error(`${cwd} 不是 git 仓库；用 --in-place 在当前目录跑`);
    if (!inPlace && resolve(repo) !== resolve(cwd)) {
      throw new Error(`goal 默认从 repo 根开始；请在 ${repo} 启动，或用 --in-place 在当前子目录跑`);
    }
    const budgetUsd = Number(opt("--budget")) > 0 ? Number(opt("--budget")) : DEFAULT_BUDGET_USD;
    const maxAttempts = Number(opt("--max-attempts")) > 0 ? Math.floor(Number(opt("--max-attempts"))) : DEFAULT_MAX_ATTEMPTS;
    const maxHours = Number(opt("--max-hours")) > 0 ? Number(opt("--max-hours")) : DEFAULT_MAX_HOURS;
    const protectArg = opt("--protect");
    const protect = protectArg === "none" ? [] : (protectArg ? protectArg.split(",").map((x) => x.trim()).filter(Boolean) : [...DEFAULT_PROTECT]);
    const dirty = repo ? repoDirty(repo) : { dirty: false, files: [] };
    const id = newGoalId();
    const worktree = inPlace ? cwd : worktreePathFor({ configDir: CONFIG_DIR, child: id });
    const branch = inPlace ? null : branchName("goal", id);
    const baseCommit = repo ? repoHead(repo) : null;
    const now = Date.now();
    const goal = {
      id, objective, accept, cwd, worktree, branch, baseCommit,
      createdAt: new Date(now).toISOString(), updatedAt: new Date(now).toISOString(),
      deadline: new Date(now + maxHours * 3600 * 1000).toISOString(),
      status: "running", phase: "starting", attempts: 0, checks: 0, cost: 0, tokens: 0,
      budgetUsd, maxAttempts, maxHours, checkTimeoutMs: 10 * 60 * 1000,
      protect, allowProtected: false, failureStreak: 0,
      sessionId: id, inPlace,
    };
    writeGoalDoc(CONFIG_DIR, id, goal);
    writeGoal(CONFIG_DIR, id, goal);
    try {
      if (!inPlace) createWorktree({ repo, path: worktree, branch, base: baseCommit });
      await startSupervisor({ configDir: CONFIG_DIR, id, cwd: worktree, bin, node, kind: "goal" });
    } catch (e) {
      if (!inPlace) { try { removeWorktree({ repo, path: worktree, branch }); } catch (e2) { /* ignore */ } }
      patchGoal(CONFIG_DIR, id, { status: "error", error: e.message });
      throw e;
    }
    console.log(`✓ goal ${id} 已启动`);
    console.log(`  objective: ${objective}`);
    console.log(`  accept:    ${accept}`);
    console.log(`  worktree:  ${worktree}${branch ? ` (${branch})` : " (in-place)"}`);
    if (dirty.dirty) console.log(`  注意: 仓库有 ${dirty.files.length} 个未提交文件${inPlace ? "，会参与 in-place 验收" : "，未带入 worktree"}`);
    console.log(`  进度:      celagent goal status ${id}`);
    console.log(`  日志:      celagent logs ${id}`);
    return;
  }

  if (sub === "list" || (sub === "status" && !args[1])) {
    const goals = listGoals(CONFIG_DIR);
    if (goals.length === 0) { console.log("(没有 goal)"); return; }
    console.log("ID\t状态\t阶段\t尝试\t成本\t目标");
    for (const g of goals) {
      console.log(`${g.id}\t${g.status}\t${g.phase || "-"}\t${g.attempts || 0}/${g.maxAttempts}\t$${Number(g.cost || 0).toFixed(4)}\t${String(g.objective).slice(0, 60)}`);
    }
    return;
  }

  if (sub === "status") {
    const id = args[1];
    if (!validGoalId(id)) throw new Error(`无效 goal ID: ${id}`);
    const g = readGoal(CONFIG_DIR, id);
    if (!g) throw new Error(`找不到 goal: ${id}`);
    const st = await supervisorStatus({ configDir: CONFIG_DIR, id }).catch(() => null);
    console.log(`goal ${g.id}  ${g.status}  ${g.phase || "-"}  attempt ${g.attempts || 0}/${g.maxAttempts}  $${Number(g.cost || 0).toFixed(4)}/$${g.budgetUsd}  deadline ${g.deadline}`);
    console.log(`objective: ${g.objective}`);
    console.log(`accept:    ${g.accept}`);
    console.log(`worktree:  ${g.worktree}${g.branch ? ` (${g.branch})` : " (in-place)"}`);
    if (g.blockedQuestion) console.log(`question:  ${g.blockedQuestion}`);
    if (st) console.log(`supervisor: ${st.running ? "running" : "stopped"}`);
    if (g.lastCheck) {
      console.log(`last check: exit ${g.lastCheck.exitCode}${g.lastCheck.timedOut ? " (timeout)" : ""} in ${g.lastCheck.durationMs}ms  ${g.lastCheckLog || ""}`);
      const tail = String(g.lastCheck.output || "").trim().split("\n").slice(-6).join("\n");
      if (tail) console.log(tail.split("\n").map((l) => `  ${l}`).join("\n"));
    }
    if (g.summary) console.log(`diff:      ${g.summary}`);
    console.log(`logs:      celagent logs ${g.sessionId}`);
    console.log(`session:   ${g.sessionId}  (headless; 停止后用 celagent up ${g.sessionId} --cwd "${g.worktree || g.cwd}" 接管)`);
    return;
  }

  if (sub === "answer") {
    const id = args[1];
    const text = (opt("--text") || args.slice(2).filter((a) => !a.startsWith("--")).join(" ")).trim();
    if (!id || !text) throw new Error('用法: celagent goal answer <id> "<answer>"');
    if (!validGoalId(id)) throw new Error(`无效 goal ID: ${id}`);
    const g = readGoal(CONFIG_DIR, id);
    if (!g) throw new Error(`找不到 goal: ${id}`);
    if (g.status !== "blocked") throw new Error(`goal 当前是 ${g.status}，只在 blocked 时回答`);
    const st = await supervisorStatus({ configDir: CONFIG_DIR, id }).catch(() => null);
    if (!st?.running) throw new Error(`goal ${id} 的 supervisor 没在运行；先 celagent goal resume ${id}`);
    if (st.state?.kind && st.state.kind !== "goal") throw new Error(`session ${id} 正由 ${st.state.kind} supervisor 占用；先 celagent down ${id}`);
    const allowProtected = g.blockedReason === "protected"
      && /允许|可以|同意|allow|yes/i.test(text)
      && !/不允许|不可以|不要|禁止|\bno\b/i.test(text);
    writeAnswer(CONFIG_DIR, id, text, { allowProtected });
    console.log(`✓ 已回答 ${id}${allowProtected ? "（允许保护路径改动）" : ""}；goal 会继续`);
    return;
  }

  if (sub === "resume") {
    const id = args[1];
    if (!id) throw new Error("用法: celagent goal resume <id>");
    if (!validGoalId(id)) throw new Error(`无效 goal ID: ${id}`);
    const g = readGoal(CONFIG_DIR, id);
    if (!g) throw new Error(`找不到 goal: ${id}`);
    if (g.status === "done") throw new Error(`goal ${id} 已经完成，不需要 resume`);
    const st = await supervisorStatus({ configDir: CONFIG_DIR, id }).catch(() => null);
    if (st?.running && st.state?.kind && st.state.kind !== "goal") {
      throw new Error(`session ${id} 正由 ${st.state.kind} supervisor 占用；先 celagent down ${id}`);
    }
    clearGoalStop(CONFIG_DIR, id);
    clearSupervisorStop(CONFIG_DIR, id);
    stopGoalChildren(id);
    const nextStatus = g.status === "blocked" ? "blocked" : "running";
    patchGoal(CONFIG_DIR, id, { status: nextStatus, phase: nextStatus, stoppedReason: null, error: null });
    await startSupervisor({ configDir: CONFIG_DIR, id, cwd: g.worktree || g.cwd, bin, node, kind: "goal" });
    console.log(`✓ goal ${id} 已恢复（${nextStatus}）`);
    return;
  }

  if (sub === "stop") {
    const id = args[1];
    if (!id) throw new Error("用法: celagent goal stop <id>");
    if (!validGoalId(id)) throw new Error(`无效 goal ID: ${id}`);
    const g = readGoal(CONFIG_DIR, id);
    if (!g) throw new Error(`找不到 goal: ${id}`);
    writeGoalStop(CONFIG_DIR, id, "manual");
    if (g.status !== "done") patchGoal(CONFIG_DIR, id, { status: "stopped", phase: "stopped", stoppedReason: "manual", finishedAt: new Date().toISOString() });
    stopGoalChildren(id);
    await stopSupervisor({ configDir: CONFIG_DIR, id, gracefulMs: 3000 }).catch(() => {});
    console.log(`✓ 已停止 ${id}`);
    return;
  }

  throw new Error("用法: celagent goal start|list|status|answer|stop ...");
}

async function delegateCommand(args) {
  const sub = args[0] || "list";
  const { node, bin } = launcher();
  const delegator = createDelegator({
    configDir: CONFIG_DIR, startSupervisor, stopSupervisor, bin, node,
    maxChildren: delegateMaxChildren(),
  });
  const opt = (name) => { const i = args.indexOf(name); return i > 0 ? args[i + 1] : null; };
  const parent = opt("--parent") || "cli";
  if (sub === "spawn") {
    const positional = [];
    for (let i = 1; i < args.length; i++) {
      const a = args[i];
      if (a === "--cwd" || a === "--parent" || a === "--task") { i += 1; continue; }
      if (a.startsWith("--")) continue;
      positional.push(a);
    }
    const task = opt("--task") || positional.join(" ");
    if (!task) throw new Error('用法: celagent delegate spawn "<task>" [--cwd dir] [--no-isolate]');
    const cwd = opt("--cwd") ? resolve(opt("--cwd")) : process.cwd();
    const r = await delegator.spawn({ parent, cwd, task, isolate: !args.includes("--no-isolate") });
    console.log(`✓ 子 agent ${r.session}`);
    if (r.branch) console.log(`  branch:  ${r.branch}`);
    console.log(`  worktree: ${r.worktree}`);
    if (r.parentDirty) console.log(`  注意: 父仓库有 ${r.parentDirtyFiles} 个未提交文件，未带入 worktree`);
    console.log(`  取结果: celagent delegate wait ${r.session}`);
    return;
  }
  if (sub === "list") {
    const rows = delegator.list({ parent: args.includes("--all") ? null : parent });
    if (rows.length === 0) { console.log("(没有子 agent)"); return; }
    console.log("ID\t状态\t父\t分支\t任务");
    for (const r of rows) console.log(`${r.session}\t${r.status}\t${r.parent}\t${r.branch || "-"}\t${r.task}`);
    return;
  }
  if (sub === "wait") {
    const id = args[1];
    if (!id) throw new Error("用法: celagent delegate wait <id> [--timeout ms]");
    const r = await delegator.wait({ child: id, timeoutMs: Number(opt("--timeout")) || 600000 });
    console.log(JSON.stringify(r, null, 2));
    return;
  }
  if (sub === "send") {
    const id = args[1];
    const message = opt("--message") || args.slice(2).filter((a) => !a.startsWith("--")).join(" ");
    if (!id || !message) throw new Error('用法: celagent delegate send <id> "<message>"');
    const r = await delegator.send({ child: id, message });
    console.log(`✓ ${r.session}: ${r.status}`);
    return;
  }
  throw new Error("用法: celagent delegate spawn|list|wait|send ...");
}

async function psCommand(args = []) {
  const states = listStates(CONFIG_DIR);
  const sessions = await tmuxListSessions();
  const byName = new Map(sessions.map((s) => [s.name, s]));
  if (states.length === 0 && sessions.filter((s) => s.name.startsWith("celagent-")).length === 0) {
    console.log("(没有 supervisor)");
    return;
  }
  const running = (s) => Boolean(byName.get(`celagent-${s.id}`));
  if (args.includes("--tree")) {
    const children = new Map();
    for (const s of states) {
      const parent = s.parent && states.some((x) => x.id === s.parent) ? s.parent : null;
      if (!children.has(parent)) children.set(parent, []);
      children.get(parent).push(s);
    }
    const print = (parent, depth) => {
      for (const s of children.get(parent) || []) {
        const mark = depth === 0 ? "" : `${"  ".repeat(depth - 1)}└ `;
        console.log(`${mark}${s.id}\t${s.kind || "tui"}\t${running(s) ? "running" : "stopped"}\t${s.cwd}`);
        print(s.id, depth + 1);
      }
    };
    print(null, 0);
    return;
  }
  console.log("ID\t类型\t状态\ttmux\twatchdog\tcwd");
  for (const s of states) {
    const pid = s.watchdogPid;
    let alive = false;
    try { process.kill(pid, 0); alive = true; } catch (e) { alive = false; }
    console.log(`${s.id}\t${s.kind || "tui"}\t${running(s) ? "running" : "stopped"}\t${running(s) ? "yes" : "no"}\t${alive ? "yes" : "no"}\t${s.cwd}`);
  }
  for (const s of sessions) {
    if (!s.name.startsWith("celagent-")) continue;
    if (states.some((x) => `celagent-${x.id}` === s.name)) continue;
    console.log(`${s.name.slice("celagent-".length)}\t?\torphan-tmux\tyes\t-\t-`);
  }
}

function logsCommand(args) {
  const id = args.find((a) => !a.startsWith("-"));
  if (!id) throw new Error("用法: celagent logs <id>");
  assertSafeSessionId(id);
  // 子 agent / goal 有可读输出就优先看它；否则看 supervisor/watchdog 日志。
  const childLog = outputLogPath(CONFIG_DIR, id);
  const goalLog = goalOutputLogPath(CONFIG_DIR, id);
  const file = existsSync(goalLog) ? goalLog : (existsSync(childLog) ? childLog : logPath(CONFIG_DIR, id));
  if (!existsSync(file)) { console.log("(没有日志)"); return; }
  const lines = readFileSync(file, "utf8").split("\n");
  process.stdout.write(lines.slice(-200).join("\n"));
}

async function triggerCommand(args) {
  const [op, id] = args;
  if (!op || !["add", "list", "rm", "run", "daemon"].includes(op)) {
    console.error("用法: celagent trigger add <id> --prompt ... [--cwd dir] [--every 60] | list | rm <id> | run <id> | daemon [--start|--stop|--status]");
    process.exit(1);
  }
  if (op === "list") {
    const list = listTriggers(CONFIG_DIR);
    if (list.length === 0) { console.log("(没有 trigger)"); return; }
    console.log("ID\tevery\tlastRun\tlastSession\tprompt");
    for (const t of list) {
      console.log(`${t.id}\t${t.everyMinutes}m\t${t.lastRunAt ? new Date(t.lastRunAt).toISOString().slice(0, 19) : "-"}\t${t.lastSession || "-"}\t${String(t.prompt).replace(/\s+/g, " ").slice(0, 80)}`);
    }
    return;
  }
  if (op === "daemon") {
    const mode = args.includes("--stop") ? "stop" : args.includes("--status") ? "status" : "start";
    if (mode === "status") {
      const st = triggerDaemonStatus(CONFIG_DIR);
      console.log(JSON.stringify(st, null, 2));
      return;
    }
    if (mode === "stop") {
      await stopTriggerDaemon(CONFIG_DIR);
      console.log("✓ trigger daemon 已停止");
      return;
    }
    const idx = args.indexOf("--interval");
    const intervalMs = idx > 0 ? Math.max(1000, Number(args[idx + 1]) || 30000) : 30000;
    const { node, bin } = launcher();
    const r = startTriggerDaemon({ configDir: CONFIG_DIR, bin, node, intervalMs });
    console.log(r.started ? `✓ trigger daemon 已启动 (pid ${r.pid})` : `trigger daemon 已在运行 (pid ${r.pid})`);
    console.log(`  日志: ${triggerDaemonStatus(CONFIG_DIR).log}`);
    console.log("  只支持 --every 分钟；cron/webhook/审批在 2.1");
    return;
  }
  if (!id) throw new Error(`trigger ${op} 需要 id`);
  assertSafeSessionId(id);
  if (op === "add") {
    let prompt = "", cwd = process.cwd(), every = 60, timeout = 1800;
    for (let i = 2; i < args.length; i++) {
      const a = args[i];
      if (a === "--prompt") { prompt = args[++i] || ""; continue; }
      if (a === "--cwd") { cwd = resolve(args[++i] || cwd); continue; }
      if (a === "--every") { every = Number(args[++i]); continue; }
      if (a === "--timeout") { timeout = Number(args[++i]); continue; }
      throw new Error(`trigger add 未知选项: ${a}`);
    }
    const t = addTrigger(CONFIG_DIR, { id, prompt, cwd, everyMinutes: every, timeoutSeconds: timeout });
    console.log(`✓ trigger ${t.id}: every ${t.everyMinutes}m, cwd=${t.cwd}`);
    console.log(`  首次运行: celagent trigger run ${t.id}`);
    console.log(`  后台调度: celagent trigger daemon --start`);
    return;
  }
  if (op === "rm") {
    if (!removeTrigger(CONFIG_DIR, id)) throw new Error(`trigger 不存在: ${id}`);
    console.log(`✓ 已删除 trigger ${id}`);
    return;
  }
  if (op === "run") {
    const { node, bin } = launcher();
    const r = await runTriggerForeground({ configDir: CONFIG_DIR, id, bin, node });
    if (r.code !== 0) process.exitCode = r.code;
    return;
  }
}

async function workspaceCommand(args) {
  const [op, id] = args;
  const valid = ["snapshot", "checkpoint", "list", "restore", "status"];
  if (!op || !id || !valid.includes(op)) {
    console.error("用法: celagent workspace checkpoint|snapshot <id> [--cwd dir] | list <id> | restore <id> [--checkpoint id] [--cwd dir] [--force] | status <id>");
    process.exit(1);
  }
  assertSafeSessionId(id);
  const cwdIdx = process.argv.indexOf("--cwd");
  const cwd = cwdIdx > 0 ? resolve(process.argv[cwdIdx + 1]) : process.cwd();
  const cpIdx = process.argv.indexOf("--checkpoint");
  const checkpointId = cpIdx > 0 ? process.argv[cpIdx + 1] : undefined;
  const v2 = openV2();
  if (op === "list") {
    const r = await listWorkspaceCheckpoints(v2.store, id);
    if (r.checkpoints.length === 0) { console.log("(没有工作区检查点)"); return; }
    console.log(`工作区检查点 ${id}（latest=${r.latest || "-"}）\n`);
    console.log("ID                seq    文件   大小       创建时间");
    for (const c of r.checkpoints) {
      console.log(`${c.id.padEnd(16)} ${String(c.seq).padStart(5)} ${String(c.files).padStart(6)} ${((c.bytes || 0) / 1024).toFixed(1).padStart(8)} KB  ${String(c.createdAt || "").slice(0, 19)}${c.current ? "  *" : ""}`);
    }
    return;
  }
  if (op === "status") {
    const m = await readManifest(v2.store, id);
    console.log(JSON.stringify({ session: id, workspace: m?.workspace || null }, null, 2));
    return;
  }
  await v2.lease.acquire(id);
  try {
    if (op === "snapshot" || op === "checkpoint") {
      const opts = workspaceOpts();
      const r = await checkpointWorkspace({
        store: v2.store, sessionId: id, cwd, lease: v2.lease,
        excludes: opts.excludes, maxBytes: opts.maxBytes, maxFileBytes: opts.maxFileBytes,
      });
      console.log(`✓ 工作区检查点 ${r.checkpointId}（seq ${r.seq}，${r.files} 文件，${(r.bytes / 1024).toFixed(1)} KB，上传 ${r.uploaded} blob${r.reused ? "，复用" : ""}）`);
      if (r.warnings?.length) for (const w of r.warnings) warn(`  ⚠ ${w}`);
    } else {
      if (!checkpointId) {
        const m = await readManifest(v2.store, id);
        if (m?.workspace?.key && m.workspace.format !== "cas-v1") {
          const r = await restoreLegacyWorkspace({ store: v2.store, sessionId: id, cwd, force: process.argv.includes("--force") });
          console.log(`✓ 已恢复 1.0 工作区 tar ${r.snapshotId}（${(r.bytes / 1024).toFixed(1)} KB）`);
          return;
        }
      }
      const r = await restoreWorkspaceCheckpoint({
        store: v2.store, sessionId: id, checkpointId, cwd,
        force: process.argv.includes("--force"),
      });
      console.log(`✓ 已恢复工作区检查点 ${r.checkpointId}（seq ${r.seq}，${r.files} 文件，${(r.bytes / 1024).toFixed(1)} KB，删除 ${r.removed} 项）`);
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
    onToolResult: (info) => holder.onToolResult?.(info),
  });
  return pi.createAgentSessionServices({
    cwd,
    agentDir: AGENT_DIR,
    settingsManager,
    modelRuntimeSignal: AbortSignal.timeout(15000),
    resourceLoaderOptions: { extensionFactories: [ext] },
  });
}

function customToolsFor(cwd, holder, { allowDelegate = true } = {}) {
  const tools = [history_search, session_snapshot];
  if (allowDelegate) {
    const { node, bin } = launcher();
    tools.push(createDelegateTool({
      configDir: CONFIG_DIR,
      getParent: () => holder.id,
      getCwd: () => cwd,
      startSupervisor, stopSupervisor, bin, node,
      maxChildren: delegateMaxChildren(),
    }));
  }
  return tools;
}

function delegateMaxChildren() {
  const n = Number(loadConfig()?.delegate?.maxChildren);
  return Number.isFinite(n) && n > 0 ? Math.min(8, Math.floor(n)) : 4;
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

function writeRunResult(resultFile, sessionId, payload) {
  if (!resultFile) return;
  try {
    mkdirSync(dirname(resultFile), { recursive: true });
    writeFileSync(resultFile, JSON.stringify({ sessionId, at: new Date().toISOString(), ...payload }, null, 2) + "\n", { encoding: "utf8", mode: 0o600 });
  } catch (e) { warn(`  (result 文件写入失败: ${e.message})`); }
}

async function runHeadless(args) {
  const promptIdx = args.indexOf("--prompt");
  const prompt = promptIdx >= 0 ? args[promptIdx + 1] : "";
  if (!prompt) { console.error('用法: celagent run --prompt "..." [--session id] [--json] [--cwd dir] [--result-file path] [--no-delegate]'); process.exit(1); }
  const sidIdx = args.indexOf("--session");
  const cwdIdx = args.indexOf("--cwd");
  const cwd = cwdIdx >= 0 ? args[cwdIdx + 1] : process.cwd();
  const sessionId = sidIdx >= 0 ? args[sidIdx + 1] : `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  assertSafeSessionId(sessionId);
  const json = args.includes("--json");
  const noDelegate = args.includes("--no-delegate");
  const resultIdx = args.indexOf("--result-file");
  const resultFile = resultIdx >= 0 ? resolve(args[resultIdx + 1]) : null;
  const dir = sessionDirFor(cwd);
  const localFile = join(dir, `${sessionId}.jsonl`);
  mkdirSync(dir, { recursive: true });

  const v2 = openV2();
  const holder = { id: sessionId };
  let session = null;
  let release = null;
  let stopHeartbeat = null;
  try {
    const prepared = await prepareSession(v2, sessionId, localFile);
    const mat = prepared.mat;
    stopHeartbeat = prepared.stopHeartbeat;
    const sm = mat
      ? pi.SessionManager.open(localFile, dir, cwd)
      : pi.SessionManager.create(cwd, dir);
    if (!mat) bindSessionFile(sm, localFile);
    const services = await buildServices(cwd, holder, v2);
    attachSnapshotTurns(v2, holder);
    const created = await pi.createAgentSessionFromServices({
      services, sessionManager: sm,
      sessionStartEvent: { type: "session_start", reason: "startup" },
      customTools: customToolsFor(cwd, holder, { allowDelegate: !noDelegate }),
    });
    session = created.session;
    const checkpoint = makeCheckpointRunner(v2, sessionId, cwd, { quiet: true });
    const usage = createUsageRecorder({ store: v2.store, sessionId, lease: v2.lease, onError: (e) => { if (e.code !== "no-manifest") warn(`  (usage 警告: ${e.message})`); } });
    const app = makeAppender({
      v2, sessionId, localFile,
      onError: (e) => warn(`  (持久化警告: ${e.message})`),
      onAppended: () => { checkpoint?.request("append"); usage?.flush(); },
    });
    holder.onToolResult = () => checkpoint?.request("tool");
    activeRuntime = { v2, sessionId, app, checkpoint, usage, stopHeartbeat };
    if (mat?.needsAppend || v2.outbox.get(sessionId)) app.append();
    release = async () => {
      await app.flush(10000);
      checkpoint?.request("release");
      await checkpoint?.flush(15000).catch(() => {});
      await usage.flush().catch(() => {});
      await usage.close().catch(() => {});
      stopHeartbeat?.();
      await v2.lease.release(sessionId).catch(() => {});
    };
    session.subscribe((event) => {
      if (["message_end", "turn_end", "session_compact"].includes(event?.type)) {
        app.append();
      }
      if (event?.type === "message_end" && event.message?.role === "assistant" && event.message.usage) {
        usage.add(event.message.usage);
      }
      if (json && event?.type === "message_end" && event.message?.role === "assistant") {
        const text = Array.isArray(event.message.content)
          ? event.message.content.filter((b) => b.type === "text").map((b) => b.text).join("\n")
          : String(event.message.content || "");
        if (text) process.stdout.write(JSON.stringify({ type: "assistant", text }) + "\n");
      }
    });
    await session.prompt(prompt);
    let text = session.getLastAssistantText?.() || "";
    try { text = lastAssistantTextFromTurns(turnsFromJsonl(readFileSync(localFile, "utf8"))) || text; } catch (e) { /* ignore */ }
    writeRunResult(resultFile, sessionId, { exitCode: 0, text });
    if (!json && text) process.stdout.write(text + "\n");
  } catch (e) {
    writeRunResult(resultFile, sessionId, { exitCode: 1, error: e.message, text: "" });
    console.error(`✗ run 失败: ${e.message}`);
    process.exitCode = 1;
  } finally {
    if (release) await release();
    activeRuntime = null;
    try { session?.dispose?.(); } catch (e) { /* ignore */ }
  }
}

/**
 * __child：可恢复的 headless 子 agent。
 * 只跑 task 一次；send 追加的消息在 turn 边界消费；崩在 turn 中间标记 interrupted，不重放。
 */
async function runGoalMode(args) {
  const id = args[0];
  if (!id) { console.error("__goal 缺少 goal ID"); process.exit(1); }
  assertSafeSessionId(id);
  if (!validGoalId(id)) { console.error("✗ 无效 goal ID"); process.exit(1); }
  const cwdIdx = args.indexOf("--cwd");
  const cwd = cwdIdx > 0 ? resolve(args[cwdIdx + 1]) : process.cwd();
  process.chdir(cwd);
  // watchdog 重启时上一轮的子进程可能还活着；先清掉，避免两轮同时跑。
  try { stopGoalChildren(id); } catch (e) { /* ignore */ }
  const { node, bin } = launcher();
  let statsV2 = null;
  const getStats = async () => {
    if (!statsV2) statsV2 = openV2();
    const manifest = await readManifest(statsV2.store, id);
    return manifest?.stats || { cost: 0, tokens: 0 };
  };
  const attempt = makeAttemptRunner({
    configDir: CONFIG_DIR, goalId: id, cwd, bin, node, getStats,
    log: (m) => console.error(`[goal] ${m}`),
  });
  const abort = new AbortController();
  const check = makeCheckRunner({ configDir: CONFIG_DIR, goalId: id, cwd, signal: abort.signal });
  let stopping = false;
  const onStop = () => {
    if (stopping) return;
    stopping = true;
    abort.abort();
    attempt.stop();
  };
  process.on("SIGTERM", onStop);
  process.on("SIGINT", onStop);
  process.on("SIGHUP", onStop);
  try {
    await runGoal({
      configDir: CONFIG_DIR, goalId: id, cwd,
      runCheck: check, runAttempt: attempt.run, getStats,
      isStopping: () => stopping,
      log: (m) => console.error(`[goal] ${m}`),
    });
  } catch (e) {
    console.error(`✗ goal 失败: ${e.message}`);
    try { patchGoal(CONFIG_DIR, id, { status: "error", phase: "error", error: e.message }); } catch (e2) { /* ignore */ }
    process.exitCode = 1;
  } finally {
    // runGoal 正常结束时 attempt 已退出；异常时这里兜底，避免 runner 退出而 agent 还在跑。
    attempt.stop();
    abort.abort();
  }
}

async function runChild(args) {
  const id = args[0];
  if (!id) { console.error("__child 缺少会话 ID"); process.exit(1); }
  assertSafeSessionId(id);
  const cwdIdx = args.indexOf("--cwd");
  const cwd = cwdIdx > 0 ? resolve(args[cwdIdx + 1]) : process.cwd();
  const taskIdx = args.indexOf("--task");
  const taskFile = taskIdx > 0 ? resolve(args[taskIdx + 1]) : null;
  process.chdir(cwd);
  const dir = sessionDirFor(cwd);
  const localFile = join(dir, `${id}.jsonl`);
  mkdirSync(dir, { recursive: true });

  const state0 = readChildState(CONFIG_DIR, id);
  if (!state0) { console.error(`✗ 找不到子会话状态: ${id}`); process.exit(1); }
  const inbox0 = listInbox(CONFIG_DIR, id);
  if (readChildResult(CONFIG_DIR, id) && inbox0.length === 0) return;
  const task = taskFile ? readFileSync(taskFile, "utf8").trim() : "";
  appendOutput(CONFIG_DIR, id, `▶ 开始: ${(task.split("\n")[0] || id).slice(0, 120)}`);

  const holder = { id };
  let session = null;
  let release = null;
  let usage = null;

  async function finalize(status, error = null) {
    await usage?.flush().catch(() => {});
    let commit = null;
    let diffStat = "";
    let dirtyFiles = 0;
    let worktreeError = null;
    if (state0.isolated) {
      try {
        const firstLine = (task.split("\n")[0] || id).slice(0, 72);
        commit = commitWorktree({ path: cwd, message: `celagent: ${firstLine}` });
      } catch (e) { worktreeError = e.message; }
    }
    try {
      const sum = worktreeSummary({ path: cwd, base: state0.baseCommit });
      diffStat = sum.diffStat;
      dirtyFiles = sum.dirtyFiles;
    } catch (e) { /* ignore */ }
    let text = "";
    try { text = lastAssistantTextFromTurns(turnsFromJsonl(readFileSync(localFile, "utf8"))); } catch (e) { /* ignore */ }
    const cur = readChildState(CONFIG_DIR, id) || {};
    writeChildResult(CONFIG_DIR, id, {
      status, text, commit, diffStat, dirtyFiles,
      tokens: usage?.tokens || 0, cost: usage?.cost || 0,
      turns: cur.turns || 0,
      worktree: cwd, branch: state0.branch || null, baseCommit: state0.baseCommit || null,
      error: error || worktreeError,
    });
    patchChildState(CONFIG_DIR, id, { status, promptDone: true, commit, error: error || worktreeError, finishedAt: new Date().toISOString() });
    appendOutput(CONFIG_DIR, id, `■ ${status}${(error || worktreeError) ? `: ${error || worktreeError}` : ""}`);
    writeStop(CONFIG_DIR, id, `child-${status}`);
  }

  try {
    const v2 = openV2();
    const prepared = await prepareSession(v2, id, localFile);
    const mat = prepared.mat;
    const stopHeartbeat = prepared.stopHeartbeat;
    const sm = mat
      ? pi.SessionManager.open(localFile, dir, cwd)
      : pi.SessionManager.create(cwd, dir);
    if (!mat) bindSessionFile(sm, localFile);
    const services = await buildServices(cwd, holder, v2);
    attachSnapshotTurns(v2, holder);
    const created = await pi.createAgentSessionFromServices({
      services, sessionManager: sm,
      sessionStartEvent: { type: "session_start", reason: "startup" },
      customTools: customToolsFor(cwd, holder, { allowDelegate: false }),
    });
    session = created.session;
    const checkpoint = makeCheckpointRunner(v2, id, cwd, { quiet: true });
    usage = createUsageRecorder({
      store: v2.store, sessionId: id, lease: v2.lease,
      onError: (e) => { if (e.code !== "no-manifest") warn(`  (usage 警告: ${e.message})`); },
    });
    const app = makeAppender({
      v2, sessionId: id, localFile,
      onError: (e) => warn(`  (持久化警告: ${e.message})`),
      onAppended: () => { checkpoint?.request("append"); usage?.flush(); },
    });
    holder.onToolResult = () => checkpoint?.request("tool");
    activeRuntime = { v2, sessionId: id, app, checkpoint, usage, stopHeartbeat };
    if (mat?.needsAppend || v2.outbox.get(id)) app.append();
    release = async () => {
      await app.flush(10000);
      checkpoint?.request("release");
      await checkpoint?.flush(15000).catch(() => {});
      await usage.flush().catch(() => {});
      await usage.close().catch(() => {});
      stopHeartbeat?.();
      await v2.lease.release(id).catch(() => {});
    };
    session.subscribe((event) => {
      if (["message_end", "turn_end", "session_compact"].includes(event?.type)) app.append();
      if (event?.type === "message_end" && event.message?.role === "assistant") {
        if (event.message.usage) usage.add(event.message.usage);
        const text = Array.isArray(event.message.content)
          ? event.message.content.filter((b) => b.type === "text").map((b) => b.text).join("\n")
          : String(event.message.content || "");
        if (text) appendOutput(CONFIG_DIR, id, text);
      }
    });

    let turns = 0;
    for (;;) {
      const cur = readChildState(CONFIG_DIR, id) || {};
      const inbox = listInbox(CONFIG_DIR, id);
      const step = takeNextPrompt({ state: cur, task, inbox });
      if (step.kind === "prompt") {
        if (step.inboxName) consumeInbox(CONFIG_DIR, id, step.inboxName);
        patchChildState(CONFIG_DIR, id, {
          status: "running", prompted: true, promptDone: false,
          error: null, turns: (cur.turns || 0) + 1,
        });
        await session.prompt(step.text);
        patchChildState(CONFIG_DIR, id, { promptDone: true });
        turns += 1;
        if (turns > 20) { await finalize("error", "超过 20 轮，停止"); break; }
        continue;
      }
      if (step.kind === "finalize") { await finalize("done"); break; }
      if (step.kind === "interrupted") { await finalize("interrupted", step.reason); break; }
      break;
    }
  } catch (e) {
    console.error(`✗ 子 agent 失败: ${e.message}`);
    appendOutput(CONFIG_DIR, id, `✗ error: ${e.message}`);
    try {
      writeChildResult(CONFIG_DIR, id, {
        status: "error", text: "", error: e.message,
        worktree: cwd, branch: state0.branch || null, baseCommit: state0.baseCommit || null,
      });
      patchChildState(CONFIG_DIR, id, { status: "error", error: e.message, finishedAt: new Date().toISOString() });
      writeStop(CONFIG_DIR, id, "child-error");
    } catch (e2) { /* ignore */ }
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
      activeRuntime.checkpoint?.request("exit");
      await activeRuntime.checkpoint?.flush(15000).catch(() => {});
      await activeRuntime.usage?.flush().catch(() => {});
      await activeRuntime.usage?.close().catch(() => {});
      activeRuntime.stopHeartbeat?.();
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
process.on("SIGHUP", () => { scheduleSignalExit(129); void flushOnExit(); });

// Pi 的 /quit 会直接 process.exit(0)，绕过 main 的 finally；拦截一次，先 flush + release lease。
const realExit = process.exit.bind(process);
let exitFlushStarted = false;
process.exit = (code) => {
  if (exitFlushStarted) return realExit(code);
  exitFlushStarted = true;
  if (process.env.CELAGENT_SUPERVISED === "1" && process.env.CELAGENT_SESSION_ID) {
    try { writeStop(CONFIG_DIR, process.env.CELAGENT_SESSION_ID, "exit"); } catch (e) { /* ignore */ }
  }
  Promise.race([flushOnExit(), sleep(12000)]).finally(() => realExit(code));
};

async function main() {
  if (process.env.CELAGENT_ENV_FILE) {
    try {
      const env = JSON.parse(readFileSync(process.env.CELAGENT_ENV_FILE, "utf8"));
      for (const [k, v] of Object.entries(env)) {
        if (typeof v === "string") process.env[k] = v;
      }
    } catch (e) {
      console.error(`无法读取 supervisor env 文件: ${e.message}`);
    }
  }
  if (process.argv[2] === "__triggerd") {
    const { node, bin } = launcher();
    const idx = process.argv.indexOf("--interval");
    const intervalMs = idx > 0 ? Math.max(1000, Number(process.argv[idx + 1]) || 30000) : 30000;
    await runTriggerDaemon({ configDir: CONFIG_DIR, bin, node, intervalMs });
    return;
  }
  if (process.argv[2] === "__goal") {
    await runGoalMode(process.argv.slice(3));
    return;
  }
  if (process.argv[2] === "__child") {
    await runChild(process.argv.slice(3));
    return;
  }
  if (process.argv[2] === "__watch") {
    const id = process.argv[3];
    const cwdIdx = process.argv.indexOf("--cwd");
    const cwd = cwdIdx > 0 ? resolve(process.argv[cwdIdx + 1]) : process.cwd();
    const envIdx = process.argv.indexOf("--env-file");
    const envFile = envIdx > 0 ? process.argv[envIdx + 1] : null;
    const kindIdx = process.argv.indexOf("--kind");
    const kind = kindIdx > 0 ? process.argv[kindIdx + 1] : "tui";
    const taskIdx = process.argv.indexOf("--task");
    const taskFile = taskIdx > 0 ? process.argv[taskIdx + 1] : null;
    const parentIdx = process.argv.indexOf("--parent");
    const parent = parentIdx > 0 ? process.argv[parentIdx + 1] : null;
    if (!id) { console.error("__watch 缺少会话 ID"); process.exit(1); }
    assertSafeSessionId(id);
    const { node, bin } = launcher();
    await watchSupervisor({ configDir: CONFIG_DIR, id, cwd, bin, node, envFile, kind, taskFile, parent });
    return;
  }

  const forceForeground = process.argv.includes("--foreground");
  if (forceForeground) process.argv = process.argv.filter((a) => a !== "--foreground");

  if (process.argv[2] === "__tui") {
    const id = process.argv[3];
    const cwdIdx = process.argv.indexOf("--cwd");
    const cwd = cwdIdx > 0 ? resolve(process.argv[cwdIdx + 1]) : process.cwd();
    if (!id) { console.error("__tui 缺少会话 ID"); process.exit(1); }
    assertSafeSessionId(id);
    process.chdir(cwd);
    process.argv = [process.argv[0], process.argv[1], id];
  }

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
  if (cmd === "trigger") { await triggerCommand(process.argv.slice(3)); return; }
  if (cmd === "up") { await upCommand(process.argv.slice(3)); return; }
  if (cmd === "attach") { await attachCommand(process.argv.slice(3)); return; }
  if (cmd === "down" || cmd === "stop") { await downCommand(process.argv.slice(3)); return; }
  if (cmd === "goal") { await goalCommand(process.argv.slice(3)); return; }
  if (cmd === "delegate") { await delegateCommand(process.argv.slice(3)); return; }
  if (cmd === "ps" || cmd === "supervisors") { await psCommand(process.argv.slice(3)); return; }
  if (cmd === "logs") { logsCommand(process.argv.slice(3)); return; }
  if (cmd && cmd.startsWith("-")) { console.error(`未知选项: ${cmd}（celagent help）`); process.exit(1); }

  const known = new Set([
    "help", "version", "config", "doctor", "cas-probe", "list", "export", "rm",
    "import-v0", "status", "workspace", "run", "trigger", "up", "attach", "down", "stop",
    "ps", "supervisors", "logs", "delegate", "goal",
  ]);
  const shouldSupervise = !forceForeground && !process.env.CELAGENT_SUPERVISED
    && process.env.CELAGENT_NO_SUPERVISOR !== "1"
    && (!cmd || (!known.has(cmd) && !cmd.startsWith("-")));
  if (shouldSupervise) {
    if (await tmuxAvailable()) {
      const args = process.argv.slice(2);
      if (process.stdin.isTTY) args.push("--attach");
      await upCommand(args);
      return;
    }
    warn("  (tmux 不可用，退回进程内 TUI；无 detach/自动重启)");
  }

  if (process.argv[2]) assertSafeSessionId(process.argv[2]);
  const sessionId = process.argv[2] || `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  const dir = sessionDirFor(cwd);
  const localFile = join(dir, `${sessionId}.jsonl`);
  mkdirSync(dir, { recursive: true });

  let v2;
  try { v2 = openV2(); }
  catch (e) { console.error(`✗ ${e.message}`); process.exit(1); }

  let mat;
  let stopHeartbeat;
  try {
    const prepared = await prepareSession(v2, sessionId, localFile);
    mat = prepared.mat;
    stopHeartbeat = prepared.stopHeartbeat;
  } catch (e) {
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
  const checkpoint = makeCheckpointRunner(v2, sessionId, cwd);
  const usage = createUsageRecorder({ store: v2.store, sessionId, lease: v2.lease, onError: (e) => { if (e.code !== "no-manifest") warn(`  (usage 警告: ${e.message})`); } });
  const app = makeAppender({ v2, sessionId, localFile, onError: (e) => warn(`  (持久化警告: ${e.message})`), onAppended: () => { checkpoint?.request("append"); usage?.flush(); } });
  holder.onToolResult = () => checkpoint?.request("tool");
  activeRuntime = { v2, sessionId, app, checkpoint, usage, stopHeartbeat };
  if (mat?.needsAppend || v2.outbox.get(sessionId)) app.append();

  let services;
  try { services = await buildServices(cwd, holder, v2); }
  catch (e) { console.error(`services 失败: ${e.message}`); process.exit(1); }

  let current = { sessionId, localFile, manager, app, checkpoint, usage, stopHeartbeat };

  const switchSession = async (nextId, nextFile) => {
    if (current) {
      await current.app.flush(5000);
      current.checkpoint?.request("switch");
      await current.checkpoint?.flush(10000).catch(() => {});
      current.checkpoint?.close();
      await current.usage?.flush().catch(() => {});
      await current.usage?.close().catch(() => {});
      current.stopHeartbeat?.();
      await v2.lease.release(current.sessionId).catch(() => {});
      current.app.stop();
    }
    await v2.lease.acquire(nextId);
    const nextStopHeartbeat = v2.lease.startHeartbeat(nextId, (e) => warn(`  (租约警告: ${e.message})`));
    holder.id = nextId;
    globalThis.__celagentPersistId = nextId;
    const nextCheckpoint = makeCheckpointRunner(v2, nextId, cwd);
    const nextUsage = createUsageRecorder({ store: v2.store, sessionId: nextId, lease: v2.lease, onError: (e) => { if (e.code !== "no-manifest") warn(`  (usage 警告: ${e.message})`); } });
    const nextApp = makeAppender({ v2, sessionId: nextId, localFile: nextFile, onError: (e) => warn(`  (持久化警告: ${e.message})`), onAppended: () => { nextCheckpoint?.request("append"); nextUsage?.flush(); } });
    holder.onToolResult = () => nextCheckpoint?.request("tool");
    current = { sessionId: nextId, localFile: nextFile, manager: manager, app: nextApp, checkpoint: nextCheckpoint, usage: nextUsage, stopHeartbeat: nextStopHeartbeat };
    activeRuntime = { v2, sessionId: nextId, app: nextApp, checkpoint: nextCheckpoint, usage: nextUsage, stopHeartbeat: nextStopHeartbeat };
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
        customTools: customToolsFor(cwd, holder),
      });
      result.session.subscribe((event) => {
        if (["message_end", "turn_end", "session_compact"].includes(event?.type)) {
          current.app.append();
        }
        if (event?.type === "message_end" && event.message?.role === "assistant" && event.message.usage) {
          current.usage?.add(event.message.usage);
        }
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
    if (process.env.CELAGENT_SUPERVISED === "1" && process.env.CELAGENT_SESSION_ID) {
      writeStop(CONFIG_DIR, process.env.CELAGENT_SESSION_ID, "quit");
    }
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
