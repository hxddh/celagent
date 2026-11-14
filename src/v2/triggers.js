// v2/triggers.js — 本地 Always-On 触发器（2.0 preview）
// 配置在 ~/.config/celagent/triggers；daemon 按 everyMinutes 启动一次 headless run。
// 不做 cron/webhook/审批；这些在 2.1。每次 trigger 运行使用新 session，避免抢用户 TUI 的租约。
import {
  mkdirSync, readdirSync, readFileSync, writeFileSync, renameSync, unlinkSync,
  openSync, closeSync, existsSync,
} from "node:fs";
import { join } from "node:path";
import { spawn } from "node:child_process";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export function triggersDir(configDir) {
  return join(configDir, "triggers");
}
export function triggerPath(configDir, id) {
  return join(triggersDir(configDir), `${id}.json`);
}
export function daemonPidPath(configDir) {
  return join(triggersDir(configDir), "daemon.pid");
}
export function daemonLogPath(configDir) {
  return join(triggersDir(configDir), "daemon.log");
}
function ensureDir(configDir) {
  mkdirSync(triggersDir(configDir), { recursive: true, mode: 0o700 });
}
function atomicWrite(path, text) {
  mkdirSync(join(path, ".."), { recursive: true, mode: 0o700 });
  const tmp = `${path}.tmp-${process.pid}-${Date.now()}`;
  writeFileSync(tmp, text, { encoding: "utf8", mode: 0o600 });
  renameSync(tmp, path);
}
function readJson(path) {
  try { return JSON.parse(readFileSync(path, "utf8")); } catch (e) { return null; }
}
function pidAlive(pid) {
  if (!Number.isInteger(Number(pid)) || Number(pid) <= 0) return false;
  try { process.kill(Number(pid), 0); return true; } catch (e) { return e.code === "EPERM"; }
}

export function listTriggers(configDir) {
  ensureDir(configDir);
  const out = [];
  for (const f of readdirSync(triggersDir(configDir))) {
    if (!f.endsWith(".json")) continue;
    const t = readJson(join(triggersDir(configDir), f));
    if (t?.id) out.push(t);
  }
  return out.sort((a, b) => String(a.id).localeCompare(String(b.id)));
}

export function readTrigger(configDir, id) {
  const t = readJson(triggerPath(configDir, id));
  if (!t || t.id !== id) return null;
  return t;
}

export function saveTrigger(configDir, trigger) {
  if (!trigger?.id) throw new Error("trigger 缺少 id");
  if (!/^[A-Za-z0-9._-]{1,64}$/.test(trigger.id)) throw new Error("trigger id 只允许字母数字/点/下划线/连字符");
  ensureDir(configDir);
  atomicWrite(triggerPath(configDir, trigger.id), JSON.stringify(trigger, null, 2) + "\n");
  return trigger;
}

export function removeTrigger(configDir, id) {
  try { unlinkSync(triggerPath(configDir, id)); return true; } catch (e) { return false; }
}

export function addTrigger(configDir, { id, prompt, cwd, everyMinutes = 60, timeoutSeconds = 1800, now = Date.now }) {
  if (!id) throw new Error("缺少 trigger id");
  if (!prompt || !String(prompt).trim()) throw new Error("trigger 需要 --prompt");
  const every = Number(everyMinutes);
  if (!Number.isFinite(every) || every < 1) throw new Error("--every 必须是 >= 1 的分钟数");
  if (!cwd) throw new Error("trigger 需要 --cwd");
  const trigger = {
    v: 1, id, prompt: String(prompt), cwd: String(cwd),
    everyMinutes: every, timeoutSeconds: Number(timeoutSeconds) || 1800,
    enabled: true, createdAt: new Date(now()).toISOString(), lastRunAt: now(),
  };
  return saveTrigger(configDir, trigger);
}

export function dueTriggers(configDir, now = Date.now()) {
  return listTriggers(configDir).filter((t) => {
    if (t.enabled === false) return false;
    if (pidAlive(t.lastPid)) return false;
    const last = Number(t.lastRunAt) || 0;
    return now - last >= Number(t.everyMinutes || 0) * 60_000;
  });
}

function markRun(configDir, id, patch) {
  const t = readTrigger(configDir, id);
  if (!t) return null;
  const next = { ...t, ...patch, lastRunAt: patch.lastRunAt ?? Date.now() };
  saveTrigger(configDir, next);
  return next;
}

function cliArgs(bin, args) {
  return bin ? [bin, ...args] : args;
}

export function spawnTriggerRun({ configDir, id, bin, node = process.execPath, detached = true }) {
  const t = readTrigger(configDir, id);
  if (!t) throw new Error(`trigger 不存在: ${id}`);
  const sessionId = `trg-${id}-${Date.now().toString(36)}`;
  const args = [
    "run", "--prompt", t.prompt, "--session", sessionId,
    "--cwd", t.cwd, "--json",
  ];
  const stamp = new Date().toISOString().replace(/[:.]/g, "-");
  const log = join(triggersDir(configDir), "runs", `${id}-${stamp}.log`);
  mkdirSync(join(triggersDir(configDir), "runs"), { recursive: true, mode: 0o700 });
  const fd = openSync(log, "a", 0o600);
  let child;
  try {
    child = spawn(node, cliArgs(bin, args), {
      detached,
      stdio: ["ignore", fd, fd],
      env: { ...process.env, CELAGENT_TRIGGER_ID: id, CELAGENT_TRIGGER_SESSION: sessionId },
    });
    child.unref?.();
  } finally {
    try { closeSync(fd); } catch (e) { /* ignore */ }
  }
  markRun(configDir, id, { lastPid: child.pid, lastSession: sessionId, lastLog: log });
  return { id, sessionId, pid: child.pid, log };
}

export async function runTriggerForeground({ configDir, id, bin, node = process.execPath }) {
  const t = readTrigger(configDir, id);
  if (!t) throw new Error(`trigger 不存在: ${id}`);
  const sessionId = `trg-${id}-${Date.now().toString(36)}`;
  const args = ["run", "--prompt", t.prompt, "--session", sessionId, "--cwd", t.cwd, "--json"];
  markRun(configDir, id, { lastPid: null, lastSession: sessionId, lastLog: null });
  const code = await new Promise((resolve) => {
    const child = spawn(node, cliArgs(bin, args), {
      stdio: "inherit",
      env: { ...process.env, CELAGENT_TRIGGER_ID: id, CELAGENT_TRIGGER_SESSION: sessionId },
    });
    child.on("exit", (c, signal) => resolve(c ?? (signal ? 1 : 0)));
    child.on("error", () => resolve(1));
  });
  const after = readTrigger(configDir, id);
  if (after) saveTrigger(configDir, { ...after, lastExitCode: code, lastFinishedAt: Date.now() });
  return { id, sessionId, code };
}

export function daemonPid(configDir) {
  try { return Number(readFileSync(daemonPidPath(configDir), "utf8").trim()); }
  catch (e) { return null; }
}

export function startTriggerDaemon({ configDir, bin, node = process.execPath, intervalMs = 30000 }) {
  ensureDir(configDir);
  const old = daemonPid(configDir);
  if (pidAlive(old)) return { started: false, pid: old };
  const fd = openSync(daemonLogPath(configDir), "a", 0o600);
  let child;
  try {
    child = spawn(node, cliArgs(bin, ["__triggerd", "--interval", String(intervalMs)]), {
      detached: true,
      stdio: ["ignore", fd, fd],
      env: { ...process.env, CELAGENT_TRIGGERD: "1" },
    });
    child.unref?.();
  } finally {
    try { closeSync(fd); } catch (e) { /* ignore */ }
  }
  atomicWrite(daemonPidPath(configDir), `${child.pid}\n`);
  return { started: true, pid: child.pid };
}

export async function stopTriggerDaemon(configDir) {
  const pid = daemonPid(configDir);
  if (pidAlive(pid)) {
    try { process.kill(pid, "SIGTERM"); } catch (e) { /* ignore */ }
    for (let i = 0; i < 10 && pidAlive(pid); i++) await sleep(100);
    if (pidAlive(pid)) { try { process.kill(pid, "SIGKILL"); } catch (e) { /* ignore */ } }
  }
  try { unlinkSync(daemonPidPath(configDir)); } catch (e) { /* ignore */ }
  return { stopped: true };
}

export function triggerDaemonStatus(configDir) {
  const pid = daemonPid(configDir);
  return { pid, alive: pidAlive(pid), log: daemonLogPath(configDir), triggers: listTriggers(configDir).length };
}

/**
 * triggerd 主循环：每 30 秒检查 dueTriggers；每个 trigger 只拉起一次，lastPid 存活时跳过。
 * 不做并发池；同一时间最多一个 trigger run，避免本地 workspace 互相踩。
 */
export async function runTriggerDaemon({ configDir, bin, node = process.execPath, intervalMs = 30000 }) {
  ensureDir(configDir);
  atomicWrite(daemonPidPath(configDir), `${process.pid}\n`);
  let stopping = false;
  const stop = () => { stopping = true; };
  process.on("SIGTERM", stop);
  process.on("SIGINT", stop);
  while (!stopping) {
    for (const t of dueTriggers(configDir)) {
      try {
        const r = spawnTriggerRun({ configDir, id: t.id, bin, node, detached: true });
        console.error(`[triggerd] ${new Date().toISOString()} ${t.id} -> ${r.sessionId} pid=${r.pid}`);
        break; // 本地 workspace 串行；下一个 trigger 下一轮再跑
      } catch (e) {
        console.error(`[triggerd] ${t.id} 启动失败: ${e.message}`);
      }
    }
    await sleep(intervalMs);
  }
  try { unlinkSync(daemonPidPath(configDir)); } catch (e) { /* ignore */ }
}
