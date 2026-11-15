// v4 — goal loop
// 目标 + 验收命令；runner 只跑验收、喂回失败、执行硬停。
export {
  DEFAULT_MAX_ATTEMPTS, DEFAULT_BUDGET_USD, DEFAULT_MAX_HOURS, DEFAULT_CHECK_TIMEOUT_MS,
  DEFAULT_PROTECT, validGoalId, newGoalId, goalDir,
  goalFilePath, goalDocPath, checksDir, answerPath, stopPath, outputLogPath,
  attemptResultPath, attemptPidPath, checkPidPath,
  readGoal, writeGoal, patchGoal, writeGoalDoc,
  writeCheckLog, readAnswer, writeAnswer, hasStop, writeStop, clearStop, appendOutput,
  runAccept, changedPaths, protectedChanges, gitSummary,
  nextAction, truncate, renderPrompt, listGoals,
} from "./goal.js";

export { runGoal, makeCheckRunner, makeAttemptRunner } from "./runner.js";
