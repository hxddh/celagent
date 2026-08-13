// review-logic-proofs.test.mjs — 评估报告 Critical 逻辑的可执行证据
// 不依赖 celld/BOS/settings; 固化 R3-C3 / R3-M1 / R3-H1 /fork 的最小复现
// 运行: node --test tests/review-logic-proofs.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";

// ---- R3-C3: 队列门闩丢最新(注释写丢最旧) ----
// 对应 bin/celagent-tui.mjs queueBosWrite: if (bosQueueLen >= MAX) return;
test("R3-C3: 队列超限丢弃的是最新任务, 不是最旧", () => {
  const MAX = 50;
  let len = 0;
  const accepted = [];
  const dropped = [];
  for (let op = 1; op <= 60; op++) {
    if (len >= MAX) { dropped.push(op); continue; }
    len++;
    accepted.push(op);
  }
  assert.equal(accepted.length, 50);
  assert.deepEqual(dropped, [51, 52, 53, 54, 55, 56, 57, 58, 59, 60]);
  assert.ok(dropped.every((d) => d > MAX), "被丢弃的序号全部大于已接受集合 → 最新");
});

// ---- R3-M1: seq 用 length 而非 max(turn) ----
test("R3-M1: turns.length 在有 gap 时小于 max(turn)", () => {
  const turns = [{ turn: 1 }, { turn: 2 }, { turn: 5 }];
  const fromLength = turns.length;
  const fromMax = Math.max(...turns.map((t) => t.turn));
  assert.equal(fromLength, 3);
  assert.equal(fromMax, 5);
  assert.notEqual(fromLength, fromMax);
});

// ---- 同 turn replace 可抹掉完整 content ----
test("同 turn replace: 截断 entry 覆盖完整 BOS 轮", () => {
  const session = {
    turns: [
      { turn: 1, msg: "A".repeat(500), content: [{ type: "text", text: "A".repeat(500) }] },
      { turn: 2, msg: "B".repeat(500), content: [{ type: "text", text: "B".repeat(500) }] },
    ],
  };
  const seq = 2;
  const entry = { turn: 2, msg: "x".repeat(200) }; // 模拟截断续写
  const idx = session.turns.findIndex((t) => t.turn === seq);
  assert.ok(idx >= 0);
  session.turns[idx] = entry;
  assert.equal(session.turns[1].msg.length, 200);
  assert.equal(session.turns[1].content, undefined);
});

// ---- R3-H1: ensureLock 早退不释放的控制流模型 ----
test("R3-H1: 健康早退后 ensureLock 仍被持有", async () => {
  let ensureLock = null;
  async function ensureBroken() {
    if (ensureLock) return ensureLock;
    const run = async () => {
      // 模拟 if (r.ok) return; — finally 只挂在更深 try 上, 到不了
      return;
    };
    ensureLock = run();
    return ensureLock;
  }
  await ensureBroken();
  assert.ok(ensureLock !== null);
  await ensureLock;
  assert.ok(ensureLock !== null, "锁未清 → 后续调用 if (ensureLock) return ensureLock 跳过拉起");
});

// ---- /fork 未切换 persistId 的分支覆盖缺口 ----
// 源码锚定: 生产文件仍包含已知缺陷形态时这些断言成立;
// 修复对应缺陷后应同步改/删本文件, 避免「模型测试」与实现脱节。
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const tui = readFileSync(join(root, "bin/celagent-tui.mjs"), "utf8");

test("源码锚定: 队列超限是 return 而非丢最旧", () => {
  assert.match(tui, /if \(bosQueueLen >= BOS_QUEUE_MAX\)/);
  const block = tui.slice(tui.indexOf("if (bosQueueLen >= BOS_QUEUE_MAX)"), tui.indexOf("bosQueueLen++;"));
  assert.match(block, /return;/);
  assert.doesNotMatch(block, /shift\(|丢弃最旧任务[^]*bosQueue\s*=/);
});

test("源码锚定: persistId 只处理 new/resume, 无 fork 分支", () => {
  assert.match(tui, /startReason === "new"/);
  assert.match(tui, /startReason === "resume"/);
  assert.doesNotMatch(tui, /startReason === "fork"/);
});

test("源码锚定: BOS JSON parse 失败走覆盖", () => {
  assert.match(tui, /JSON\.parse\(existing\.body\).*catch[\s\S]{0,40}覆盖/);
});

test("源码锚定: steer 注入只用 t.msg", () => {
  assert.match(tui, /\$\{t\.msg\}/);
  assert.doesNotMatch(tui, /t\.content/);
});

test("源码锚定: loadHistoryFromBos 先 resume worker", () => {
  const fn = tui.slice(tui.indexOf("async function loadHistoryFromBos"), tui.indexOf("async function listSessions"));
  const workerIdx = fn.indexOf("action=resume");
  const bosIdx = fn.indexOf("bosGet");
  assert.ok(workerIdx >= 0 && bosIdx > workerIdx, "resume 出现在 bosGet 之前");
});
