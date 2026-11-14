// v2/supervisor.js — tmux 托管的本机 Agent Computer
// tmux 只负责 PTY/多客户端；lease、journal、checkpoint 仍由 TUI 进程和 v2 引擎负责。
import { spawn, spawnSync, execFile } from "node:child_process";
import { promisify } from "node:util";
import {
  existsSync, mkdirSync, readFileSync, writeFileSync, renameSync, unlinkSync,
  openSync, closeSync, readdirSync,
} from "node:fs";
import { join, dirname } from "node:path";

const execFileAsync = promisify(execFile);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export function supervisorDir(configDir) {
  return join(configDir, "supervisor");
}
export function statePath(configDir, id) {
  return join(supervisorDir(configDir), `${id}.json`);
}
export function stopPath(configDir, id) {
  return join(supervisorDir(configDir), `${id}.stop`);
}
export function pidPath(configDir, id) {
  return join(supervisorDir(configDir), `${id}.watch.pid`);
}
export function logPath(configDir, id) {
  return join(supervisorDir(configDir), `${id}.log`);
}
export function envPath(configDir, id) {
  return join(supervisorDir(configDir), `${id}.env.json`);
}
export function tmuxName(id) {
  return `celagent-${id}`;
}

function ensureDir(configDir) {
  mkdirSync(supervisorDir(configDir), { recursive: true, mode: 0o700 });
}
function atomicWrite(path, text) {
  mkdirSync(dirname(path), { recursive: true, mode: 0o700 });
  const tmp = `${path}.tmp-${process.pid}-${Date.now()}`;
  writeFileSync(tmp, text, { encoding: "utf8", mode: 0o600 });
  renameSync(tmp, path);
}
function readJson(path) {
  try { return JSON.parse(readFileSync(path, "utf8")); } catch (e) { return null; }
}
function pidAlive(pid) {
  if (!Number.isInteger(Number(pid)) || Number(pid) <= 0) return false;
  try { process.kill(Number(pid), 0); return true; }
  catch (e) { return e.code === "EPERM"; }
}

export async function tmuxAvailable() {
  try {
    const r = await execFileAsync("tmux", ["-V"], { timeout: 5000, encoding: "utf8" });
    return String(r.stdout || "").trim() || "tmux";
  } catch (e) { return null; }
}

export async function tmuxHasSession(id) {
  try {
    await execFileAsync("tmux", ["has-session", "-t", tmuxName(id)], { timeout: 5000 });
    return true;
  } catch (e) { return false; }
}

export async function tmuxListSessions() {
  try {
    const r = await execFileAsync("tmux", [
      "list-sessions", "-F", "#{session_name}\t#{session_created}\t#{session_attached}",
    ], { timeout: 5000, encoding: "utf8" });
    return String(r.stdout || "").split("\n").filter(Boolean).map((line) => {
      const [name, created, attached] = line.split("\t");
      return { name, created: Number(created) * 1000 || null, attached: Number(attached) || 0 };
    });
  } catch (e) { return []; }
}

export async function startTmux({ id, cwd, bin, node = process.execPath, envFile = null, kind = "tui", taskFile = null, parent = null, goalId = null }) {
  if (!existsSync(cwd)) throw new Error(`工作目录不存在: ${cwd}`);
  let childArgs;
  if (kind === "child") {
    if (!taskFile) throw new Error("child supervisor 需要 taskFile");
    childArgs = bin
      ? [bin, "__child", id, "--cwd", cwd, "--task", taskFile, "--parent", parent || ""]
      : ["__child", id, "--cwd", cwd, "--task", taskFile, "--parent", parent || ""];
  } else {
    childArgs = bin ? [bin, "__tui", id, "--cwd", cwd] : ["__tui", id, "--cwd", cwd];
    if (goalId) childArgs.push("--goal", goalId);
  }
  const envArgs = ["env", "CELAGENT_SUPERVISED=1", `CELAGENT_SESSION_ID=${id}`];
  if (envFile) envArgs.push(`CELAGENT_ENV_FILE=${envFile}`);
  const args = [
    "new-session", "-d", "-s", tmuxName(id), "-c", cwd,
    ...envArgs, node, ...childArgs,
  ];
  try {
    await execFileAsync("tmux", args, { timeout: 15000, encoding: "utf8" });
  } catch (e) {
    throw new Error(`启动 tmux 会话失败: ${String(e.stderr || e.message || e).trim()}`);
  }
}

export async function attachTmux(id) {
  if (!(await tmuxHasSession(id))) throw new Error(`会话 ${id} 没有运行中的 supervisor；先 celagent up ${id}`);
  const r = spawnSync("tmux", ["attach-session", "-t", tmuxName(id)], { stdio: "inherit" });
  if (r.status !== 0) throw new Error(`attach 失败 (exit ${r.status ?? "signal"})`);
}

export async function sendTmuxQuit(id) {
  try {
    await execFileAsync("tmux", ["send-keys", "-t", tmuxName(id), "/quit", "Enter"], { timeout: 5000 });
    return true;
  } catch (e) { return false; }
}

export async function stopTmux(id) {
  try {
    await execFileAsync("tmux", ["kill-session", "-t", tmuxName(id)], { timeout: 5000 });
  } catch (e) { /* already gone */ }
}

export function writeStop(configDir, id, reason = "down") {
  ensureDir(configDir);
  atomicWrite(stopPath(configDir, id), JSON.stringify({ id, reason, at: new Date().toISOString() }) + "\n");
}
export function hasStop(configDir, id) {
  return existsSync(stopPath(configDir, id));
}
export function clearStop(configDir, id) {
  try { unlinkSync(stopPath(configDir, id)); } catch (e) { /* ignore */ }
}

export function readState(configDir, id) {
  return readJson(statePath(configDir, id));
}
export function writeState(configDir, id, state) {
  ensureDir(configDir);
  atomicWrite(statePath(configDir, id), JSON.stringify(state, null, 2) + "\n");
}
export function removeState(configDir, id) {
  try { unlinkSync(statePath(configDir, id)); } catch (e) { /* ignore */ }
}

export function listStates(configDir) {
  ensureDir(configDir);
  const out = [];
  for (const f of readdirSync(supervisorDir(configDir))) {
    if (!f.endsWith(".json")) continue;
    const s = readJson(join(supervisorDir(configDir), f));
    if (s?.id) out.push(s);
  }
  return out.sort((a, b) => String(b.createdAt).localeCompare(String(a.createdAt)));
}

export function readWatchdogPid(configDir, id) {
  try { return Number(readFileSync(pidPath(configDir, id), "utf8").trim()); }
  catch (e) { return null; }
}

function writeWatchdogPid(configDir, id, pid) {
  ensureDir(configDir);
  atomicWrite(pidPath(configDir, id), `${pid}\n`);
}

export function startWatchdog({ configDir, id, cwd, bin, node = process.execPath, envFile = null, kind = "tui", taskFile = null, parent = null, goalId = null }) {
  ensureDir(configDir);
  const existing = readWatchdogPid(configDir, id);
  if (pidAlive(existing)) return existing;
  const fd = openSync(logPath(configDir, id), "a", 0o600);
  try {
    const watchArgs = ["__watch", id, "--cwd", cwd];
    if (envFile) watchArgs.push("--env-file", envFile);
    if (kind === "child") {
      if (!taskFile) throw new Error("child watchdog 需要 taskFile");
      watchArgs.push("--kind", "child", "--task", taskFile, "--parent", parent || "");
    } else if (goalId) {
      watchArgs.push("--goal", goalId);
    }
    const child = spawn(node, bin ? [bin, ...watchArgs] : watchArgs, {
      detached: true,
      stdio: ["ignore", fd, fd],
      env: { ...process.env, CELAGENT_WATCHDOG: "1" },
    });
    child.unref();
    writeWatchdogPid(configDir, id, child.pid);
    return child.pid;
  } finally {
    try { closeSync(fd); } catch (e) { /* ignore */ }
  }
}

export async function stopWatchdog(configDir, id) {
  const pid = readWatchdogPid(configDir, id);
  if (pidAlive(pid)) {
    try { process.kill(pid, "SIGTERM"); } catch (e) { /* ignore */ }
    for (let i = 0; i < 10 && pidAlive(pid); i++) await sleep(100);
    if (pidAlive(pid)) { try { process.kill(pid, "SIGKILL"); } catch (e) { /* ignore */ } }
  }
  try { unlinkSync(pidPath(configDir, id)); } catch (e) { /* ignore */ }
}

const ENV_KEYS = [
  "PATH", "HOME", "LANG", "LC_ALL",
  "AWS_PROFILE", "AWS_REGION", "AWS_DEFAULT_REGION",
  "AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN",
  "CELAGENT_ENCRYPTION_KEY", "CELAGENT_ALLOW_ENDPOINT",
  "DEEPSEEK_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY",
];

export async function startSupervisor({ configDir, id, cwd, bin, node = process.execPath, kind = "tui", taskFile = null, parent = null, goalId = null }) {
  ensureDir(configDir);
  if (await tmuxHasSession(id)) return { started: false, already: true, session: tmuxName(id) };
  clearStop(configDir, id);
  const envFile = envPath(configDir, id);
  const env = {};
  for (const k of ENV_KEYS) if (process.env[k] !== undefined) env[k] = process.env[k];
  atomicWrite(envFile, JSON.stringify(env, null, 2) + "\n");
  let watchdogPid;
  try {
    await startTmux({ id, cwd, bin, node, envFile, kind, taskFile, parent, goalId });
    watchdogPid = startWatchdog({ configDir, id, cwd, bin, node, envFile, kind, taskFile, parent, goalId });
  } catch (e) {
    try { unlinkSync(envFile); } catch (e2) { /* ignore */ }
    throw e;
  }
  const state = {
    id, cwd, session: tmuxName(id), version: 2, kind,
    parent: parent || null, taskFile: taskFile || null, goalId: goalId || null,
    createdAt: new Date().toISOString(), updatedAt: new Date().toISOString(),
    watchdogPid, node, bin, envFile,
  };
  writeState(configDir, id, state);
  return { started: true, already: false, session: tmuxName(id), watchdogPid };
}

export async function stopSupervisor({ configDir, id, gracefulMs = 0 }) {
  writeStop(configDir, id, "down");
  if (gracefulMs > 0 && await tmuxHasSession(id)) {
    await sendTmuxQuit(id);
    const until = Date.now() + gracefulMs;
    while (Date.now() < until && await tmuxHasSession(id)) await sleep(100);
  }
  await stopWatchdog(configDir, id);
  await stopTmux(id);
  removeState(configDir, id);
  try { unlinkSync(envPath(configDir, id)); } catch (e) { /* ignore */ }
  return { stopped: true };
}

export async function supervisorStatus({ configDir, id }) {
  const state = readState(configDir, id);
  const running = await tmuxHasSession(id);
  const pid = readWatchdogPid(configDir, id);
  return {
    id, state, running, session: tmuxName(id),
    watchdog: pid ? { pid, alive: pidAlive(pid) } : null,
    stopped: hasStop(configDir, id),
    log: logPath(configDir, id),
  };
}

/**
 * Watchdog：tmux session 消失且没有 stop 标记时，重新拉起 __tui。
 * - 用户 /quit 或 celagent down 会写 stop 标记；
 * - kill -9 不会写标记，watchdog 重新拉起，TUI 从 manifest/checkpoint 恢复；
 * - 10 分钟内重启超过 20 次则退出，避免坏配置死循环。
 */
export async function watchSupervisor({ configDir, id, cwd, bin, node = process.execPath, envFile = null, kind = "tui", taskFile = null, parent = null, goalId = null }) {
  ensureDir(configDir);
  writeWatchdogPid(configDir, id, process.pid);
  let stopping = false;
  const onStop = () => { stopping = true; };
  process.on("SIGTERM", onStop);
  process.on("SIGINT", onStop);
  let restarts = 0;
  let windowStart = Date.now();
  while (!stopping) {
    if (hasStop(configDir, id)) return;
    if (!(await tmuxHasSession(id))) {
      if (hasStop(configDir, id)) return;
      try {
        await startTmux({ id, cwd, bin, node, envFile, kind, taskFile, parent, goalId });
        restarts += 1;
        if (Date.now() - windowStart > 10 * 60 * 1000) { restarts = 1; windowStart = Date.now(); }
        if (restarts > 20) {
          console.error(`[celagent watchdog] ${id} 重启过于频繁，停止自动拉起`);
          writeStop(configDir, id, "watchdog-restart-limit");
          return;
        }
      } catch (e) {
        console.error(`[celagent watchdog] ${id} 拉起失败: ${e.message}`);
      }
    }
    await sleep(2000);
  }
}
