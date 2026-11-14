// tests/v2-triggers.test.mjs — 2.0 本地 trigger 配置/到期判断
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  addTrigger, listTriggers, readTrigger, removeTrigger, dueTriggers,
} from "../src/v2/triggers.js";

test("trigger: add/list/read/due/remove", () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-trigger-"));
  try {
    const t = addTrigger(dir, {
      id: "nightly", prompt: "triage", cwd: dir, everyMinutes: 1, now: () => 1_000,
    });
    assert.equal(t.id, "nightly");
    assert.equal(t.everyMinutes, 1);
    assert.equal(readTrigger(dir, "nightly").prompt, "triage");
    assert.equal(listTriggers(dir).length, 1);
    assert.equal(dueTriggers(dir, 1_000).length, 0, "刚添加不到期");
    assert.equal(dueTriggers(dir, 1_000 + 60_000).length, 1, "到 1 分钟应到期");
    assert.equal(removeTrigger(dir, "nightly"), true);
    assert.equal(listTriggers(dir).length, 0);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("trigger: 非法 every / 缺 prompt 拒绝", () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-trigger2-"));
  try {
    assert.throws(() => addTrigger(dir, { id: "x", prompt: "", cwd: dir }), /prompt/);
    assert.throws(() => addTrigger(dir, { id: "x", prompt: "p", cwd: dir, everyMinutes: 0 }), /every/);
    assert.throws(() => addTrigger(dir, { id: "../x", prompt: "p", cwd: dir }), /id/);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
