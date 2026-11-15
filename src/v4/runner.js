// v4/runner.js — goal runner：一个受 supervisor 看护的循环进程
// 每轮：跑验收 → 失败就把输出交给 `celagent run` 的一次尝试 → 再验收。
// agent 是 runner 的子进程，不是 runner 的宿主；超时/预算/停止都是进程生命周期问题。
import { spawn } from "node:child_process";
import { existsSync, readFileSync, unlinkSync, writeFileSync } from "node:fs";
import {
  DEFAULT_CHECK_TIMEOUT_MS, appendOutput, attemptPidPath, attemptResultPath,
  gitSummary, hasStop, nextAction, patchGoal, protectedChanges, readAnswer,
  readGoal, renderPrompt, runAccept, writeCheckLog, checkPidPath,
} from "./goal.js";
import { writeStop as writeSupervisorStop } from "../v2/supervisor.js";

const sleepDefault = (ms) => new Promise((r) => setTimeout(r, ms));

function killGroup(child) {
  try {
    if (process.platform !== "win32" && child.pid) process.kill(-child.pid, "SIGKILL");
    else child.kill("SIGKILL");
  } catch (e) { /* already gone */ }
}

function readJson(path) {
  try { return JSON.parse(readFileSync(path, "utf8")); } catch (e) { return null; }
}

/** 验收：在 runner 进程里跑；超时/abort 都杀进程组。 */
export function makeCheckRunner({ configDir, goalId, cwd, signal = null }) {
  return async ({ goal, seq, timeoutMs }) => {
    const limit = Math.min(Number(goal.checkTimeoutMs) || DEFAULT_CHECK_TIMEOUT_MS, timeoutMs || Infinity);
    const check = await runAccept({ cwd, command: goal.accept, timeoutMs: limit, signal, pidFile: checkPidPath(configDir, goalId) });
    check.seq = seq;
    const log = writeCheckLog(configDir, goalId, check);
    return { ...check, log };
  };
}

/** 一次 agent 尝试：headless `celagent run`，同一 session，可被 runner 杀。 */
export function makeAttemptRunner({ configDir, goalId, cwd, bin = null, node = process.execPath, getStats, log = () => {} }) {
  let current = null;
  let stopRequested = false;

  async function run({ prompt, timeoutMs, budgetUsd, attempt }) {
    if (stopRequested) return { exitCode: 1, text: "", stopped: true, reason: "stop" };
    const resultFile = attemptResultPath(configDir, goalId);
    try { unlinkSync(resultFile); } catch (e) { /* ignore */ }
    const args = [
      "run", "--session", goalId, "--cwd", cwd,
      "--prompt", prompt, "--result-file", resultFile, "--no-delegate",
    ];
    // attempt 是 goal 的子进程，不是 supervisor 本身；清掉 supervisor 标记，
    // 否则 runHeadless 里的 process.exit 会把整个 goal supervisor 停掉。
    const env = { ...process.env };
    delete env.CELAGENT_SUPERVISED;
    delete env.CELAGENT_SESSION_ID;
    const child = spawn(node, bin ? [bin, ...args] : args, {
      cwd, env,
      detached: process.platform !== "win32",
      stdio: ["ignore", "pipe", "pipe"],
    });
    current = child;
    try { writeFileSync(attemptPidPath(configDir, goalId), `${child.pid}\n`, { encoding: "utf8", mode: 0o600 }); } catch (e) { /* ignore */ }
    appendOutput(configDir, goalId, `\n▶ 第 ${attempt} 次尝试\n`);
    child.stdout.on("data", (d) => appendOutput(configDir, goalId, d.toString()));
    child.stderr.on("data", (d) => appendOutput(configDir, goalId, d.toString()));
    log(`attempt ${attempt} pid=${child.pid}`);

    let reason = null;
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      reason = "deadline";
      killGroup(child);
    }, Math.max(1000, timeoutMs || DEFAULT_CHECK_TIMEOUT_MS));
    const budgetTimer = budgetUsd > 0
      ? setInterval(async () => {
        const stats = await getStats().catch(() => null);
        if (stats && Number(stats.cost || 0) >= budgetUsd) {
          reason = "budget";
          killGroup(child);
        }
      }, 2000)
      : null;

    const exitCode = await new Promise((resolve) => {
      child.on("close", (code, signal) => resolve(Number.isFinite(code) ? code : (signal ? 1 : 0)));
      child.on("error", () => resolve(1));
    });
    clearTimeout(timer);
    if (budgetTimer) clearInterval(budgetTimer);
    current = null;
    try { unlinkSync(attemptPidPath(configDir, goalId)); } catch (e) { /* ignore */ }
    const result = readJson(resultFile) || {};
    return {
      exitCode,
      text: String(result.text || ""),
      error: result.error || null,
      timedOut,
      reason: reason || (stopRequested ? "stop" : null),
      stopped: Boolean(reason || stopRequested),
    };
  }

  function stop() {
    stopRequested = true;
    if (current) killGroup(current);
  }

  return { run, stop };
}

function blockedLine(text) {
  const lines = String(text || "").trim().split("\n").filter(Boolean);
  const last = lines[lines.length - 1] || "";
  const m = last.match(/^BLOCKED:\s*(.+)$/);
  return m ? m[1].trim() : null;
}

/**
 * 循环本身。测试注入 runCheck / runAttempt / getStats；生产由 __goal 注入。
 */
export async function runGoal({
  configDir, goalId, cwd, runCheck, runAttempt, getStats,
  isStopping = () => false, sleep = sleepDefault, now = Date.now, log = () => {},
}) {
  if (typeof runCheck !== "function") throw new Error("runGoal 需要 runCheck");
  if (typeof runAttempt !== "function") throw new Error("runGoal 需要 runAttempt");
  if (typeof getStats !== "function") getStats = async () => ({ cost: 0, tokens: 0 });

  let goal = readGoal(configDir, goalId);
  if (!goal) throw new Error(`找不到 goal: ${goalId}`);
  if (goal.status === "done" || goal.status === "stopped" || goal.status === "error") {
    // 可能是上一次在 patchGoal 之后、写 supervisor stop 之前崩了；补写 stop，
    // 否则 watchdog 看到 tmux 消失会无限拉起一个马上返回的 runner。
    try { writeSupervisorStop(configDir, goalId, `goal-${goal.status}`); } catch (e) { /* ignore */ }
    return goal;
  }

  function finish(status, patch = {}) {
    try {
      patchGoal(configDir, goalId, {
        status, phase: status,
        cost: Number(goal.cost || 0), tokens: Number(goal.tokens || 0),
        finishedAt: new Date().toISOString(),
        summary: gitSummary({ cwd, base: goal.baseCommit }),
        ...patch,
      });
    } catch (e) { /* the supervisor stop below still prevents a restart loop */ }
    try { writeSupervisorStop(configDir, goalId, `goal-${status}`); } catch (e) { /* ignore */ }
    log(`goal ${goalId} ${status}${patch.stoppedReason ? `: ${patch.stoppedReason}` : ""}`);
  }

  async function waitAnswer() {
    while (!hasStop(configDir, goalId) && !isStopping()) {
      const answer = readAnswer(configDir, goalId);
      if (answer && answer.at > (goal.answeredAt || "")) {
        const allow = goal.blockedReason === "protected"
          && Boolean(answer.allowProtected);
        goal = { ...goal, status: "running", phase: "running", answeredAt: answer.at, lastAnswer: answer.text, blockedReason: null, blockedQuestion: null, failureStreak: 0, allowProtected: goal.allowProtected || allow };
        patchGoal(configDir, goalId, {
          status: "running", phase: "running", answeredAt: answer.at,
          lastAnswer: answer.text, blockedReason: null, blockedQuestion: null,
          failureStreak: 0, allowProtected: goal.allowProtected,
        });
        appendOutput(configDir, goalId, `\n▶ 人回答：${answer.text}\n`);
        return true;
      }
      await sleep(1000);
    }
    return false;
  }

  try {
    for (;;) {
      if (hasStop(configDir, goalId) || isStopping()) {
        finish("stopped", { stoppedReason: "manual" });
        return readGoal(configDir, goalId) || goal;
      }
      if (goal.status === "blocked") {
        const answered = await waitAnswer();
        if (!answered) { finish("stopped", { stoppedReason: "manual" }); return readGoal(configDir, goalId) || goal; }
        continue;
      }
      if (goal.status !== "running") return goal;

      const stats = await getStats().catch(() => null);
      if (stats) {
        goal = { ...goal, cost: Number(stats.cost || 0), tokens: Number(stats.tokens || 0) };
        patchGoal(configDir, goalId, { cost: goal.cost, tokens: goal.tokens });
      }

      const deadline = Date.parse(goal.deadline || 0);
      if (deadline && now() >= deadline) { finish("stopped", { stoppedReason: "deadline" }); return readGoal(configDir, goalId) || goal; }
      if (Number(goal.budgetUsd) > 0 && Number(goal.cost || 0) >= Number(goal.budgetUsd)) {
        finish("stopped", { stoppedReason: "budget" });
        return readGoal(configDir, goalId) || goal;
      }
      const remaining = deadline ? Math.max(1000, deadline - now()) : undefined;

      patchGoal(configDir, goalId, { phase: "checking" });
      let check;
      try {
        check = await runCheck({ goal, seq: Number(goal.checks || 0) + 1, timeoutMs: remaining });
      } catch (e) {
        check = {
          exitCode: 1, output: `验收无法运行: ${e.message}`, hash: "check-error",
          timedOut: false, durationMs: 0, at: new Date().toISOString(), seq: Number(goal.checks || 0) + 1,
        };
      }
      if (check.aborted || isStopping() || hasStop(configDir, goalId)) {
        finish("stopped", { stoppedReason: "manual" });
        return readGoal(configDir, goalId) || goal;
      }
      // 先用“上一次” lastCheck 计算 streak，再写本次 check；否则永远和自己相等。
      const changed = goal.protect?.length
        ? protectedChanges({ cwd, base: goal.baseCommit, patterns: goal.protect })
        : [];
      const decision = nextAction({ goal, check, cost: goal.cost, protectedChanged: changed, now: now() });
      goal = {
        ...goal,
        checks: Number(goal.checks || 0) + 1,
        lastCheck: check, lastCheckLog: check.log || null, lastCheckAt: check.at,
      };
      patchGoal(configDir, goalId, {
        checks: goal.checks, lastCheck: check, lastCheckLog: check.log || null,
        lastCheckAt: check.at, cost: goal.cost, tokens: goal.tokens,
      });

      if (decision.action === "done") {
        finish("done", { lastCheck: check, lastCheckLog: check.log || null, failureStreak: 0 });
        return readGoal(configDir, goalId) || goal;
      }
      if (decision.action === "stopped") {
        finish("stopped", { stoppedReason: decision.reason, lastCheck: check, lastCheckLog: check.log || null });
        return readGoal(configDir, goalId) || goal;
      }
      if (decision.action === "blocked") {
        goal = {
          ...goal, status: "blocked", phase: "blocked",
          blockedReason: decision.reason, blockedQuestion: decision.question,
          blockedPaths: changed, failureStreak: decision.streak || goal.failureStreak || 0,
        };
        patchGoal(configDir, goalId, {
          status: "blocked", phase: "blocked",
          blockedReason: decision.reason, blockedQuestion: decision.question,
          blockedPaths: changed, failureStreak: goal.failureStreak,
        });
        appendOutput(configDir, goalId, `\n⏸ blocked: ${decision.question}\n`);
        continue;
      }

      // stop/resume 可能在验收后被另一个进程写入；起 attempt 前再确认一次，
      // 否则会把 stopped 覆盖回 running，或在 STOP 之后又拉起一个 agent。
      if (hasStop(configDir, goalId) || isStopping()) {
        finish("stopped", { stoppedReason: "manual", lastCheck: check, lastCheckLog: check.log || null });
        return readGoal(configDir, goalId) || goal;
      }
      const current = readGoal(configDir, goalId);
      if (current && current.status !== "running") {
        goal = current;
        continue;
      }

      const isBaseline = Number(goal.attempts || 0) === 0;
      const attempt = Number(goal.attempts || 0) + 1;
      const streak = isBaseline ? 0 : decision.streak;
      goal = {
        ...goal, status: "running", phase: "attempting",
        attempts: attempt, failureStreak: streak, lastPromptAt: new Date().toISOString(),
      };
      patchGoal(configDir, goalId, {
        status: "running", phase: "attempting", attempts: attempt,
        failureStreak: streak, lastPromptAt: goal.lastPromptAt,
      });
      const prompt = renderPrompt({ goal, check, attempt });
      const result = await runAttempt({
        goal, check, prompt, attempt,
        timeoutMs: remaining,
        budgetUsd: Number(goal.budgetUsd || 0),
      });
      if (result.stopped) {
        finish("stopped", { stoppedReason: result.reason || "stop", lastCheck: check, lastCheckLog: check.log || null });
        return readGoal(configDir, goalId) || goal;
      }
      if (result.error) appendOutput(configDir, goalId, `\n✗ attempt error: ${result.error}\n`);
      const blocked = blockedLine(result.text);
      if (blocked) {
        goal = { ...goal, status: "blocked", phase: "blocked", blockedReason: "agent-question", blockedQuestion: blocked };
        patchGoal(configDir, goalId, { status: "blocked", phase: "blocked", blockedReason: "agent-question", blockedQuestion: blocked });
        appendOutput(configDir, goalId, `\n⏸ blocked: ${blocked}\n`);
      }
      // 下一轮回到验收
    }
  } catch (e) {
    finish("error", { error: e.message });
    throw e;
  }
}
