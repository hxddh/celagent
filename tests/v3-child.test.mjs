// tests/v3-child.test.mjs — 子 agent 状态机：task / inbox / interrupted / finalize
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  appendInbox, consumeInbox, lastAssistantTextFromTurns, listInbox,
  patchChildState, readChildResult, readChildState, resultPath, takeNextPrompt,
  truncateText, writeChildResult, writeTask,
} from "../src/v3/child.js";

function temp() {
  return mkdtempSync(join(tmpdir(), "celagent-v3-child-"));
}

test("takeNextPrompt: 第一次跑 task；turn 没结束标记 interrupted；跑完 finalize", () => {
  assert.deepEqual(
    takeNextPrompt({ state: { prompted: false }, task: "修复测试" }),
    { kind: "prompt", text: "修复测试", source: "task" },
  );
  assert.equal(
    takeNextPrompt({ state: { prompted: true, promptDone: false }, task: "x" }).kind,
    "interrupted",
  );
  assert.equal(
    takeNextPrompt({ state: { prompted: true, promptDone: true }, task: "x" }).kind,
    "finalize",
  );
  assert.equal(
    takeNextPrompt({ state: { prompted: false }, task: "" }).kind,
    "interrupted",
  );
});

test("takeNextPrompt: inbox 优先于 task，且不会被重复消费", () => {
  const dir = temp();
  const child = "dlg-inbox-1";
  try {
    writeTask(dir, child, "原始任务");
    appendInbox(dir, child, "先改 A");
    appendInbox(dir, child, "再改 B");
    const inbox = listInbox(dir, child);
    assert.equal(inbox.length, 2);
    const step = takeNextPrompt({ state: { prompted: false }, task: "原始任务", inbox });
    assert.equal(step.kind, "prompt");
    assert.equal(step.text, "先改 A");

    consumeInbox(dir, child, step.inboxName);
    const next = takeNextPrompt({ state: { prompted: true, promptDone: true }, task: "原始任务", inbox: listInbox(dir, child) });
    assert.equal(next.text, "再改 B");
    consumeInbox(dir, child, next.inboxName);
    assert.equal(
      takeNextPrompt({ state: { prompted: true, promptDone: true }, task: "原始任务", inbox: listInbox(dir, child) }).kind,
      "finalize",
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("state/result: 原子写、字段合并、result 可读", () => {
  const dir = temp();
  const child = "dlg-state-1";
  try {
    writeTask(dir, child, "任务\n第二行");
    patchChildState(dir, child, { status: "running", prompted: false, turns: 0 });
    patchChildState(dir, child, { prompted: true, turns: 1 });
    const st = readChildState(dir, child);
    assert.equal(st.status, "running");
    assert.equal(st.prompted, true);
    assert.equal(st.turns, 1);
    assert.equal(st.child, child);

    writeChildResult(dir, child, { status: "done", text: "ok", commit: "abc" });
    assert.equal(readChildResult(dir, child).text, "ok");
    assert.equal(readChildResult(dir, child).child, child);
    assert.match(readFileSync(resultPath(dir, child), "utf8"), /"commit": "abc"/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("lastAssistantTextFromTurns: 取最后一条 assistant 文本块", () => {
  const turns = [
    { role: "user", content: [{ type: "text", text: "q" }] },
    { role: "assistant", content: [{ type: "text", text: "第一版" }] },
    { role: "assistant", content: [{ type: "toolCall", name: "bash" }, { type: "text", text: "最终结果" }] },
  ];
  assert.equal(lastAssistantTextFromTurns(turns), "最终结果");
  assert.equal(lastAssistantTextFromTurns([]), "");
});

test("truncateText: 超限截断并标记", () => {
  const short = truncateText("abc", 10);
  assert.equal(short.truncated, false);
  const long = truncateText("x".repeat(20), 10);
  assert.equal(long.truncated, true);
  assert.match(long.text, /截断/);
});
