// v4/extension.js — goal loop 作为 Pi 扩展运行在 TUI session 里
// agent_settled 后跑验收；失败就用 sendMessage(triggerTurn) 续跑；人只在 blocked 时回答。
import { readGoal, patchGoal, runAccept, writeCheckLog, nextAction, protectedChanges, renderPrompt, readAnswer, gitSummary } from "./goal.js";

function textOf(message) {
  if (!message) return "";
  if (Array.isArray(message.content)) {
    return message.content.filter((b) => b.type === "text").map((b) => b.text).join("\n");
  }
  return String(message.content || "");
}

export function goalExtension({ configDir, goalId, cwd }) {
  return (pi) => {
    let busy = false;
    let bootstrapped = false;
    let cost = 0;
    let tokens = 0;
    let lastText = "";
    let watcher = null;
    let lastCtx = null;

    const current = () => readGoal(configDir, goalId);

    async function checkNow(goal) {
      const seq = Number(goal.checks || 0) + 1;
      let check;
      try {
        check = await runAccept({ cwd, command: goal.accept, timeoutMs: goal.checkTimeoutMs });
      } catch (e) {
        check = { exitCode: 1, output: `验收命令无法运行: ${e.message}`, hash: "spawn-error", timedOut: false, durationMs: 0, at: new Date().toISOString() };
      }
      check.seq = seq;
      const log = writeCheckLog(configDir, goalId, check);
      patchGoal(configDir, goalId, {
        checks: seq, lastCheck: check, lastCheckLog: log, lastCheckAt: check.at,
      });
      return { ...check, log };
    }

    function decide(goal, check) {
      const changed = goal.protect?.length ? protectedChanges({ cwd, base: goal.baseCommit, patterns: goal.protect }) : [];
      return {
        decision: nextAction({ goal, check, cost, protectedChanged: changed }),
        changed,
      };
    }

    function finish(ctx, status, patch, message) {
      patchGoal(configDir, goalId, {
        status, ...patch, cost, tokens,
        finishedAt: new Date().toISOString(),
        summary: gitSummary({ cwd, base: current()?.baseCommit }),
      });
      try { ctx?.ui?.notify(message, status === "done" ? "info" : "warning"); } catch (e) { /* ignore */ }
    }

    function sendPrompt(goal, check, streak) {
      const attempt = Number(goal.attempts || 0) + 1;
      const isBaseline = Number(goal.attempts || 0) === 0;
      patchGoal(configDir, goalId, {
        status: "running", attempts: attempt, lastPromptAt: new Date().toISOString(),
        failureStreak: isBaseline ? 0 : (Number.isFinite(streak) ? streak : goal.failureStreak || 0),
        lastCheck: check, lastCheckLog: check.log, cost, tokens,
      });
      const text = renderPrompt({ goal: { ...goal, attempts: attempt }, check, attempt });
      try {
        pi.sendMessage(
          { customType: "celagent-goal", content: text, display: true },
          { triggerTurn: true },
        );
      } catch (e) {
        patchGoal(configDir, goalId, { status: "error", error: e.message });
      }
    }

    async function apply({ decision, changed }, goal, check, ctx) {
      if (decision.action === "done") {
        finish(ctx, "done", { lastCheck: check, lastCheckLog: check.log, failureStreak: 0 }, `goal ${goalId} 完成：验收通过`);
        return;
      }
      if (decision.action === "stopped") {
        const why = { "max-attempts": "尝试次数用尽", deadline: "超过墙钟", budget: "超过预算" }[decision.reason] || decision.reason;
        finish(ctx, "stopped", { lastCheck: check, lastCheckLog: check.log, stoppedReason: decision.reason }, `goal ${goalId} 停止：${why}`);
        return;
      }
      if (decision.action === "blocked") {
        patchGoal(configDir, goalId, {
          status: "blocked", blockedReason: decision.reason, blockedQuestion: decision.question,
          blockedPaths: changed, lastCheck: check, lastCheckLog: check.log,
          failureStreak: decision.streak || goal.failureStreak || 0, cost, tokens,
        });
        try { ctx?.ui?.notify(`goal ${goalId} 需要人：${decision.question}`, "warning"); } catch (e) { /* ignore */ }
        return;
      }
      sendPrompt(goal, check, decision.streak);
    }

    async function step(ctx) {
      if (busy) return;
      if (ctx?.isIdle && !ctx.isIdle()) return;
      const goal = current();
      if (!goal || goal.status !== "running") return;
      busy = true;
      try {
        // agent 主动交给人：最后一行 BLOCKED: <问题>
        if (/^\s*BLOCKED:/m.test(lastText)) {
          const question = (lastText.match(/^\s*BLOCKED:\s*(.+)$/m) || [])[1] || "agent 请求人工决定";
          patchGoal(configDir, goalId, { status: "blocked", blockedReason: "agent-question", blockedQuestion: question, cost, tokens });
          try { ctx?.ui?.notify(`goal ${goalId}：${question}`, "warning"); } catch (e) { /* ignore */ }
          return;
        }
        const check = await checkNow(goal);
        await apply(decide(goal, check), goal, check, ctx);
      } finally {
        busy = false;
      }
    }

    function startWatcher() {
      if (watcher) return;
      watcher = setInterval(() => {
        const goal = current();
        if (!goal || goal.status !== "blocked") return;
        if (lastCtx?.isIdle && !lastCtx.isIdle()) return;
        const answer = readAnswer(configDir, goalId);
        if (!answer || (goal.answeredAt && answer.at <= goal.answeredAt)) return;
        patchGoal(configDir, goalId, {
          status: "running", answeredAt: answer.at, blockedReason: null, blockedQuestion: null,
          failureStreak: 0, allowProtected: Boolean(goal.allowProtected || answer.allowProtected),
        });
        try {
          pi.sendMessage(
            { customType: "celagent-goal", content: `人的回答：${answer.text}\n\n继续 goal ${goalId}。`, display: true },
            { triggerTurn: true },
          );
        } catch (e) {
          patchGoal(configDir, goalId, { status: "error", error: e.message });
        }
      }, 1500);
      watcher.unref?.();
    }

    async function bootstrap(ctx, tries = 0) {
      if (bootstrapped) return;
      if (ctx?.isIdle && !ctx.isIdle()) {
        if (tries < 20) setTimeout(() => { void bootstrap(ctx, tries + 1); }, 1000);
        return;
      }
      bootstrapped = true;
      await step(ctx);
    }

    pi.on("session_start", async (_event, ctx) => {
      lastCtx = ctx;
      const goal = current();
      if (!goal) return;
      cost = Number(goal.cost || 0);
      tokens = Number(goal.tokens || 0);
      startWatcher();
      if (bootstrapped) return;
      // 让 TUI 先进入 idle，再跑 baseline / 恢复检查。
      setTimeout(() => { void bootstrap(ctx); }, 1200);
    });

    pi.on("message_end", (event) => {
      if (event?.message?.role !== "assistant") return;
      if (!current()) return;
      lastText = textOf(event.message);
      const usage = event.message.usage;
      if (usage) {
        const t = Number(usage.totalTokens ?? (Number(usage.input || 0) + Number(usage.output || 0) + Number(usage.cacheRead || 0) + Number(usage.cacheWrite || 0)));
        const c = Number(usage.cost?.total ?? usage.cost ?? 0);
        if (Number.isFinite(t) && t > 0) tokens += t;
        if (Number.isFinite(c) && c > 0) cost += c;
        patchGoal(configDir, goalId, { cost, tokens });
      }
    });

    pi.on("agent_settled", async (_event, ctx) => {
      lastCtx = ctx;
      if (!bootstrapped) return;
      await step(ctx);
    });
  };
}
