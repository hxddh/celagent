// v3 — durable delegation
// 子 agent 是完整的 celagent session：独立 worktree / lease / journal / checkpoint。
export {
  MAX_TASK_CHARS, MAX_RESULT_CHARS,
  delegationDir, taskPath, statePath, resultPath, inboxDir, outputLogPath,
  readChildState, writeChildState, patchChildState,
  writeTask, readTask, writeChildResult, readChildResult,
  appendInbox, listInbox, consumeInbox, appendOutput,
  takeNextPrompt, lastAssistantTextFromTurns, truncateText,
} from "./child.js";

export {
  repoRoot, repoHead, repoDirty, branchName,
  createWorktree, commitWorktree, worktreeSummary, removeWorktree, worktreePathFor,
} from "./worktree.js";

export {
  validSessionId, newChildId, createDelegator, createDelegateTool,
} from "./delegate.js";
