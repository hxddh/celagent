// review-logic-proofs.test.mjs — P0 正确性回归 (修复后语义)
// 不依赖 celld/BOS/settings; 固化队列/seq/fork/BOS-first/parse/steer
// 运行: node --test tests/review-logic-proofs.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const tui = readFileSync(join(root, "bin/celagent-tui.mjs"), "utf8");

// ---- 队列超限丢最旧、保留最新 ----
test("队列超限丢最旧保留最新", () => {
  const MAX = 50;
  const q = [];
  for (let op = 1; op <= 60; op++) {
    if (q.length >= MAX) q.shift();
    q.push(op);
  }
  assert.equal(q.length, 50);
  assert.equal(q[0], 11);
  assert.equal(q[q.length - 1], 60);
});

test("源码锚定: 队列超限 shift 最旧而非 return", () => {
  assert.match(tui, /if \(bosPending\.length >= BOS_QUEUE_MAX\)/);
  const start = tui.indexOf("if (bosPending.length >= BOS_QUEUE_MAX)");
  const block = tui.slice(start, start + 180);
  assert.match(block, /bosPending\.shift\(\)/);
  assert.doesNotMatch(block, /return;/);
  assert.match(block, /丢弃最旧任务/);
});

// ---- seq 用 max(turn) ----
test("seq 用 max(turn) 而非 length", () => {
  const turns = [{ turn: 1 }, { turn: 2 }, { turn: 5 }];
  const fromLength = turns.length;
  const nums = turns.map((t) => Number(t.turn)).filter((n) => Number.isFinite(n));
  const fromMax = nums.length ? Math.max(...nums) : 0;
  assert.equal(fromLength, 3);
  assert.equal(fromMax, 5);
  assert.notEqual(fromLength, fromMax);
});

test("源码锚定: seq = maxTurn(persistHistory)", () => {
  assert.match(tui, /let seq = maxTurn\(persistHistory\)/);
  assert.doesNotMatch(tui, /let seq = \(persistHistory && persistHistory\.length\) \|\| 0/);
});

// ---- 同 turn replace 保留已有 content ----
test("同 turn replace: 缺 content 的新写入不抹掉已有 content", () => {
  const session = {
    turns: [
      { turn: 1, msg: "A".repeat(500), content: [{ type: "text", text: "A".repeat(500) }] },
      { turn: 2, msg: "B".repeat(500), content: [{ type: "text", text: "B".repeat(500) }] },
    ],
  };
  const seq = 2;
  const fullContent = null;
  const entry = { turn: 2, msg: "x".repeat(200) };
  const idx = session.turns.findIndex((t) => t.turn === seq);
  const prev = session.turns[idx];
  if (prev?.content && !(fullContent && fullContent.length)) entry.content = prev.content;
  session.turns[idx] = entry;
  assert.equal(session.turns[1].msg.length, 200);
  assert.ok(Array.isArray(session.turns[1].content));
  assert.equal(session.turns[1].content[0].text.length, 500);
});

test("源码锚定: mergeTurn 保留已有 content/toolResults", () => {
  assert.match(tui, /if \(prev\?\.content && !\(fullContent && fullContent\.length\)\) entry\.content = prev\.content/);
  assert.match(tui, /if \(prev\?\.toolResults && !\(fullToolResults && fullToolResults\.length\)\) entry\.toolResults = prev\.toolResults/);
});

// ---- ensureLock finally 释放 ----
test("ensureLock: finally 在健康早退后释放", async () => {
  let ensureLock = null;
  async function ensureFixed() {
    if (ensureLock) return ensureLock;
    const run = async () => {
      try {
        await Promise.resolve(); // 模拟 await fetch 后再 return
        return;
      } finally {
        ensureLock = null;
      }
    };
    ensureLock = run();
    return ensureLock;
  }
  await ensureFixed();
  assert.equal(ensureLock, null);
});

test("源码锚定: ensureCelld 外层 finally 清 ensureLock", () => {
  const fn = tui.slice(tui.indexOf("async function ensureCelld"), tui.indexOf("// ---- BOS 直写队列"));
  assert.match(fn, /} finally \{\s*ensureLock = null/);
});

// ---- /fork 独立 persistId ----
test("源码锚定: persistId 处理 new 与 fork", () => {
  assert.match(tui, /startReason === "new" \|\| startReason === "fork"/);
});

// ---- JSON parse 失败不覆盖 ----
test("源码锚定: BOS JSON parse 失败 return 不覆盖", () => {
  assert.match(tui, /JSON\.parse\(existing\.body\)[\s\S]{0,200}catch[\s\S]{0,250}return;/);
  assert.doesNotMatch(tui, /JSON\.parse\(existing\.body\).*catch[\s\S]{0,40}覆盖/);
});

// ---- steer 用 content + role ----
test("源码锚定: steer 注入用 t.content 与 t.role", () => {
  assert.match(tui, /Array\.isArray\(t\.content\)/);
  assert.match(tui, /t\.role \|\| "assistant"/);
  assert.match(tui, /fromContent \|\| t\.msg/);
});

// ---- BOS-first 恢复 ----
test("源码锚定: loadHistoryFromBos 先 bosGet 再 worker resume", () => {
  const fn = tui.slice(tui.indexOf("async function loadHistoryFromBos"), tui.indexOf("async function listSessions"));
  const bosIdx = fn.indexOf("bosGet");
  const workerIdx = fn.indexOf("action=resume");
  assert.ok(bosIdx >= 0 && workerIdx > bosIdx, "bosGet 出现在 resume 之前");
});

test("源码锚定: 会话 ID 白名单", () => {
  assert.match(tui, /function assertSafeSessionId/);
  assert.match(tui, /A-Za-z0-9\._-\]\{1,128\}/);
});
