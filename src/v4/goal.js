// v4/goal.js — 目标循环的持久状态、验收命令、守卫
// 一个 goal 只有一个状态文件；判断仍由模型做，这里只回答“过没过 / 该不该停”。
import { spawn, execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  existsSync, mkdirSync, readFileSync, writeFileSync, renameSync, readdirSync, unlinkSync, statSync, appendFileSync,
} from "node:fs";
import { join, basename, dirname } from "node:path";

export const DEFAULT_MAX_ATTEMPTS = 8;
export const DEFAULT_BUDGET_USD = 5;
export const DEFAULT_MAX_HOURS = 2;
export const DEFAULT_CHECK_TIMEOUT_MS = 10 * 60 * 1000;
export const MAX_OUTPUT_CHARS = 6000;
export const DEFAULT_PROTECT = ["tests/**", "test/**", "**/*.test.*", "**/*.spec.*"];

const SESSION_ID_RE = /^[A-Za-z0-9._-]{1,128}$/;

export function validGoalId(id) {
  return typeof id === "string" && SESSION_ID_RE.test(id) && !id.includes("..");
}
export function newGoalId() {
  return `goal-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
}
export function goalDir(configDir, id) {
  return join(configDir, "goals", id);
}
export function goalFilePath(configDir, id) { return join(goalDir(configDir, id), "goal.json"); }
export function goalDocPath(configDir, id) { return join(goalDir(configDir, id), "GOAL.md"); }
export function checksDir(configDir, id) { return join(goalDir(configDir, id), "checks"); }
export function answerPath(configDir, id) { return join(goalDir(configDir, id), "answer.json"); }
export function stopPath(configDir, id) { return join(goalDir(configDir, id), "STOP"); }
export function outputLogPath(configDir, id) { return join(goalDir(configDir, id), "output.log"); }
export function attemptResultPath(configDir, id) { return join(goalDir(configDir, id), "attempt.json"); }
export function attemptPidPath(configDir, id) { return join(goalDir(configDir, id), "attempt.pid"); }
export function checkPidPath(configDir, id) { return join(goalDir(configDir, id), "check.pid"); }
export function hasStop(configDir, id) { return existsSync(stopPath(configDir, id)); }
export function writeStop(configDir, id, reason = "stop") {
  atomicWrite(stopPath(configDir, id), JSON.stringify({ reason, at: new Date().toISOString() }) + "\n");
}
export function clearStop(configDir, id) {
  try { unlinkSync(stopPath(configDir, id)); } catch (e) { /* ignore */ }
}
export function appendOutput(configDir, id, text) {
  const body = String(text || "");
  if (!body) return;
  ensureDir(goalDir(configDir, id));
  appendFileSync(outputLogPath(configDir, id), body, { mode: 0o600 });
}

function ensureDir(path) { mkdirSync(path, { recursive: true, mode: 0o700 }); }
function atomicWrite(path, text) {
  ensureDir(dirname(path));
  const tmp = `${path}.tmp-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
  writeFileSync(tmp, text, { encoding: "utf8", mode: 0o600 });
  renameSync(tmp, path);
}
function readJson(path) {
  try { return JSON.parse(readFileSync(path, "utf8")); } catch (e) { return null; }
}

export function readGoal(configDir, id) { return readJson(goalFilePath(configDir, id)); }
export function writeGoal(configDir, id, goal) {
  ensureDir(goalDir(configDir, id));
  atomicWrite(goalFilePath(configDir, id), JSON.stringify(goal, null, 2) + "\n");
  return goal;
}
export function patchGoal(configDir, id, patch) {
  const cur = readGoal(configDir, id) || { id, status: "running" };
  return writeGoal(configDir, id, { ...cur, ...patch, id, updatedAt: new Date().toISOString() });
}

export function writeGoalDoc(configDir, id, goal) {
  ensureDir(goalDir(configDir, id));
  const protect = goal.protect?.length ? goal.protect.join(", ") : "(none)";
  atomicWrite(goalDocPath(configDir, id), [
    `# Goal ${id}`,
    "",
    goal.objective,
    "",
    "## 验收",
    "",
    "```bash",
    goal.accept,
    "```",
    "",
    "只有这条命令 exit 0 才算完成。runtime 会在每一轮之后运行它。",
    "",
    "## 约束",
    "",
    `- 工作目录：${goal.worktree || goal.cwd}`,
    `- 保护路径：${protect}`,
    "- 不要修改保护路径，除非人明确允许。",
    "- 不要 push、不要 merge；只在自己的分支上提交。",
    "- 每轮结束就停；下一轮会带着验收输出继续。",
    "",
  ].join("\n"));
}

export function writeCheckLog(configDir, id, check) {
  const dir = checksDir(configDir, id);
  ensureDir(dir);
  const name = `${String(check.seq || 0).padStart(4, "0")}-${Date.now().toString(36)}.log`;
  const path = join(dir, name);
  atomicWrite(path, [
    `# ${check.at}`,
    `exit: ${check.exitCode}`,
    `timedOut: ${check.timedOut}`,
    `durationMs: ${check.durationMs}`,
    `hash: ${check.hash}`,
    "",
    check.output,
    "",
  ].join("\n"));
  return path;
}

export function readAnswer(configDir, id) { return readJson(answerPath(configDir, id)); }
export function writeAnswer(configDir, id, text, { allowProtected = false } = {}) {
  const body = { text: String(text), at: new Date().toISOString(), allowProtected: Boolean(allowProtected) };
  atomicWrite(answerPath(configDir, id), JSON.stringify(body, null, 2) + "\n");
  return body;
}

/** 在目标 worktree 里跑验收；超时杀进程组，输出截断。 */
export async function runAccept({ cwd, command, timeoutMs = DEFAULT_CHECK_TIMEOUT_MS, maxOutput = MAX_OUTPUT_CHARS * 4, signal = null, pidFile = null } = {}) {
  if (!command) throw new Error("accept 命令不能为空");
  if (!existsSync(cwd)) throw new Error(`goal 工作目录不存在: ${cwd}`);
  const started = Date.now();
  const isWin = process.platform === "win32";
  const env = { ...process.env };
  delete env.CELAGENT_SUPERVISED;
  delete env.CELAGENT_SESSION_ID;
  const child = spawn(isWin ? "cmd" : "/bin/sh", isWin ? ["/c", command] : ["-c", command], {
    cwd,
    detached: !isWin,
    stdio: ["ignore", "pipe", "pipe"],
    env,
  });
  if (pidFile && child.pid) {
    try { writeFileSync(pidFile, `${child.pid}\n`, { encoding: "utf8", mode: 0o600 }); } catch (e) { /* ignore */ }
  }
  let stdout = "";
  let stderr = "";
  let truncated = false;
  const append = (target, chunk) => {
    if (target.length >= maxOutput) { truncated = true; return target; }
    const text = chunk.toString();
    const room = maxOutput - target.length;
    if (text.length > room) { truncated = true; return target + text.slice(0, room); }
    return target + text;
  };
  child.stdout.on("data", (d) => { stdout = append(stdout, d); });
  child.stderr.on("data", (d) => { stderr = append(stderr, d); });
  let timedOut = false;
  let aborted = false;
  const kill = () => {
    try {
      if (!isWin && child.pid) process.kill(-child.pid, "SIGKILL");
      else child.kill("SIGKILL");
    } catch (e) { /* already gone */ }
  };
  const onAbort = () => { aborted = true; kill(); };
  if (signal) {
    if (signal.aborted) onAbort();
    else signal.addEventListener("abort", onAbort, { once: true });
  }
  const timer = setTimeout(() => {
    timedOut = true;
    kill();
  }, Math.max(1000, timeoutMs));
  const exitCode = await new Promise((resolve) => {
    child.on("close", (code, signal) => resolve(Number.isFinite(code) ? code : (signal ? 1 : 0)));
    child.on("error", () => resolve(1));
  });
  clearTimeout(timer);
  if (signal) signal.removeEventListener("abort", onAbort);
  if (pidFile) { try { unlinkSync(pidFile); } catch (e) { /* ignore */ } }
  const output = [stdout, stderr].filter(Boolean).join("\n").trim();
  const normalized = output.replace(/\d+(\.\d+)?(ms|s|m)\b/g, "<time>");
  const hash = createHash("sha256").update(`${exitCode}\n${normalized}`).digest("hex").slice(0, 16);
  return {
    exitCode, output, truncated, timedOut, aborted, hash,
    durationMs: Date.now() - started,
    at: new Date().toISOString(),
  };
}

function globToRe(glob) {
  let re = "";
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i];
    if (c === "*") {
      if (glob[i + 1] === "*") {
        re += ".*";
        i += 1;
        if (glob[i + 1] === "/") i += 1;
      } else re += "[^/]*";
    } else if ("\\^$+?.()|{}[]".includes(c)) re += `\\${c}`;
    else re += c;
  }
  return new RegExp(`^${re}$`);
}

function matchAny(path, patterns) {
  for (const p of patterns) {
    if (globToRe(p).test(path)) return true;
    if (!p.includes("/") && globToRe(p).test(basename(path))) return true;
  }
  return false;
}

/** goal worktree 里相对 base 的改动路径（含未提交）。 */
export function changedPaths({ cwd, base = null } = {}) {
  const out = new Set();
  try {
    const st = execFileSync("git", ["-C", cwd, "status", "--porcelain"], { encoding: "utf8", timeout: 15000, stdio: ["ignore", "pipe", "pipe"] });
    for (const line of st.split("\n")) {
      if (!line.trim()) continue;
      let p = line.slice(3).trim();
      if (p.includes(" -> ")) p = p.split(" -> ").pop();
      p = p.replace(/^"|"$/g, "");
      out.add(p);
    }
  } catch (e) { /* not a git repo */ }
  if (base) {
    try {
      const d = execFileSync("git", ["-C", cwd, "diff", "--name-only", `${base}..HEAD`], { encoding: "utf8", timeout: 15000, stdio: ["ignore", "pipe", "pipe"] });
      for (const line of d.split("\n")) if (line.trim()) out.add(line.trim());
    } catch (e) { /* ignore */ }
  }
  return [...out];
}

export function protectedChanges({ cwd, base, patterns = DEFAULT_PROTECT } = {}) {
  if (!patterns || patterns.length === 0) return [];
  return changedPaths({ cwd, base }).filter((p) => matchAny(p, patterns));
}

export function gitSummary({ cwd, base } = {}) {
  const parts = [];
  try {
    if (base) {
      const d = execFileSync("git", ["-C", cwd, "diff", "--stat", `${base}..HEAD`], { encoding: "utf8", timeout: 15000, stdio: ["ignore", "pipe", "pipe"] }).trim();
      if (d) parts.push(d);
    }
    const st = execFileSync("git", ["-C", cwd, "status", "--porcelain"], { encoding: "utf8", timeout: 15000, stdio: ["ignore", "pipe", "pipe"] });
    const n = st.split("\n").filter(Boolean).length;
    if (n > 0) parts.push(`${n} 个未提交文件`);
  } catch (e) { /* ignore */ }
  return parts.join("; ");
}

/**
 * 一次验收之后的决定。
 * done / stopped / blocked / prompt；prompt 时 attempts 由调用方 +1。
 */
export function nextAction({ goal, check, cost = 0, protectedChanged = [], now = Date.now() } = {}) {
  if (protectedChanged.length > 0 && !goal.allowProtected) {
    return {
      action: "blocked", reason: "protected", paths: protectedChanged,
      question: `agent 改了保护路径：${protectedChanged.join(", ")}。允许吗？`,
    };
  }
  if (check.exitCode === 0) return { action: "done" };
  const attempts = Number(goal.attempts || 0);
  if (attempts >= Number(goal.maxAttempts || DEFAULT_MAX_ATTEMPTS)) {
    return { action: "stopped", reason: "max-attempts" };
  }
  const deadline = Date.parse(goal.deadline || 0);
  if (deadline && now >= deadline) return { action: "stopped", reason: "deadline" };
  if (Number(goal.budgetUsd) > 0 && cost >= Number(goal.budgetUsd)) {
    return { action: "stopped", reason: "budget" };
  }
  const same = goal.lastCheck?.hash && goal.lastCheck.hash === check.hash;
  const streak = same ? Number(goal.failureStreak || 0) + 1 : 1;
  if (streak >= 3) {
    return {
      action: "blocked", reason: "same-failure", streak,
      question: `同一个失败已经连续 ${streak} 次。要继续吗？`,
    };
  }
  return { action: "prompt", streak };
}

export function truncate(text, limit = MAX_OUTPUT_CHARS) {
  const s = String(text || "");
  return s.length > limit ? s.slice(0, limit) + "\n… (截断)" : s;
}

export function renderPrompt({ goal, check, attempt }) {
  const paths = goal.protect?.length ? goal.protect.join(", ") : "(none)";
  return [
    `继续执行 goal ${goal.id}。`,
    "",
    `目标：${goal.objective}`,
    `验收命令：${goal.accept}`,
    `尝试：第 ${attempt} / ${goal.maxAttempts} 次`,
    ...(goal.lastAnswer ? ["", "人对上一次 blocked 的回答：", truncate(goal.lastAnswer, 2000)] : []),
    "",
    "最近一次验收：",
    "```",
    `exit ${check.exitCode}${check.timedOut ? " (timeout)" : ""}`,
    truncate(check.output) || "(无输出)",
    "```",
    "",
    `约束：不要修改保护路径（${paths}），除非人明确允许；每轮结束把改动 commit 到当前分支；不要 push/merge；停下来，runtime 会再跑验收。`,
    "如果必须由人决定，最后一行只写 `BLOCKED: <问题>`，然后停下。",
  ].join("\n");
}

export function listGoals(configDir) {
  const root = join(configDir, "goals");
  if (!existsSync(root)) return [];
  const out = [];
  for (const name of readdirSync(root)) {
    if (!validGoalId(name)) continue;
    try { if (!statSync(join(root, name)).isDirectory()) continue; } catch (e) { continue; }
    const goal = readGoal(configDir, name);
    if (goal) out.push(goal);
  }
  return out.sort((a, b) => String(b.createdAt || "").localeCompare(String(a.createdAt || "")));
}
