// tests/v2-usage.test.mjs — 2.0 usage → manifest.stats
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createMemoryAdapter, appendFromFile, readManifest } from "../src/v2/index.js";
import { createUsageRecorder } from "../src/v2/usage.js";

test("usage: 累加 tokens/cost 到 manifest.stats", async () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-usage-"));
  const local = join(dir, "s.jsonl");
  const header = { type: "session", version: 3, id: "s", timestamp: new Date().toISOString(), cwd: dir };
  const entry = { type: "message", id: "a", parentId: null, timestamp: new Date().toISOString(), message: { role: "user", content: [], timestamp: Date.now() } };
  writeFileSync(local, [header, entry].map(JSON.stringify).join("\n") + "\n");
  const store = createMemoryAdapter();
  await appendFromFile(store, "s", local);
  const rec = createUsageRecorder({ store, sessionId: "s" });
  rec.add({ totalTokens: 10, cost: { total: 0.01 } });
  rec.add({ totalTokens: 5, cost: { total: 0.02 } });
  await rec.flush();
  const m = await readManifest(store, "s");
  assert.equal(m.stats.tokens, 15);
  assert.ok(Math.abs(m.stats.cost - 0.03) < 1e-9);
  await rec.close();
  rmSync(dir, { recursive: true, force: true });
});

test("usage: 非法 usage 忽略", async () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-usage2-"));
  const store = createMemoryAdapter();
  const rec = createUsageRecorder({ store, sessionId: "s" });
  rec.add(null);
  rec.add({ totalTokens: -1 });
  rec.add({ totalTokens: "x" });
  assert.equal(rec.tokens, 0);
  await rec.close();
  rmSync(dir, { recursive: true, force: true });
});
