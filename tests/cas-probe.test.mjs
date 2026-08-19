// cas-probe.test.mjs — 无 aws CLI:内存 store 验证 CAS 门禁
import { test } from "node:test";
import assert from "node:assert/strict";
import { probeStoreCas, evaluateCasChecks } from "../src/bos.js";

function makeMemoryStore({ ignoreCas = false } = {}) {
  const objects = new Map();
  let n = 0;
  return {
    async put(key, content, extra = {}) {
      const body = typeof content === "string" ? content : JSON.stringify(content);
      const cur = objects.get(key);
      if (extra.ifNoneMatch && cur && !ignoreCas) {
        return { ok: false, conflict: true, error: "conflict" };
      }
      if (extra.ifMatch) {
        if (!ignoreCas && (!cur || cur.etag !== extra.ifMatch)) {
          return { ok: false, conflict: true, error: "conflict" };
        }
      }
      n += 1;
      const etag = `"etag-${n}"`;
      objects.set(key, { body, etag });
      return { ok: true, result: { ETag: etag } };
    },
    async get(key) {
      const cur = objects.get(key);
      if (!cur) return { ok: false, error: "not-found" };
      return { ok: true, body: cur.body, etag: cur.etag };
    },
    async del(key) {
      objects.delete(key);
      return { ok: true };
    },
  };
}

test("probeStoreCas: 遵守条件写则通过", async () => {
  const ops = makeMemoryStore({ ignoreCas: false });
  const r = await probeStoreCas({ bucket: "t", endpoint: "http://127.0.0.1:9", ops });
  assert.equal(r.ok, true);
  assert.match(r.message, /CAS 探针通过/);
});

test("probeStoreCas: 忽略 If-Match 则 cas-ignored 且文案含 RPO", async () => {
  const ops = makeMemoryStore({ ignoreCas: true });
  const r = await probeStoreCas({ bucket: "t", endpoint: "http://127.0.0.1:9", ops });
  assert.equal(r.ok, false);
  assert.equal(r.error, "cas-ignored");
  assert.match(r.message, /RPO=0/);
});

test("probeStoreCas: 无 bucket 失败且不粘滞", async () => {
  const r = await probeStoreCas({});
  assert.equal(r.ok, false);
  assert.equal(r.error, "no-bucket");
  assert.equal(r.transient, true);
});

// ---- evaluateCasChecks: transient (无法判定) 与结论性能力失败必须分开 ----
const okPut = (etag) => ({ ok: true, result: { ETag: etag } });
const conflict = () => ({ ok: false, conflict: true, error: "conflict" });
const netErr = (msg = "Could not connect to the endpoint URL") => ({ ok: false, error: msg });
const baseSteps = () => ({
  create: okPut('"e1"'),
  got: { ok: true, body: "b1", etag: '"e1"' },
  ifNoneMatchExisting: conflict(),
  ifMatchWrong: conflict(),
  ifMatchRight: okPut('"e2"'),
  gotAfter: { ok: true, body: "b2" },
  expectedBody1: "b1",
  expectedBody2: "b2",
});

test("evaluateCasChecks: 探针写入网络失败 → transient, 不判存储不合格", () => {
  const r = evaluateCasChecks({ ...baseSteps(), create: netErr() });
  assert.equal(r.ok, false);
  assert.equal(r.transient, true);
  assert.equal(r.error, "create-failed");
  assert.doesNotMatch(r.message, /不能保证 RPO=0/);
});

test("evaluateCasChecks: 条件写步骤网络失败 → transient, 不判存储不合格", () => {
  for (const step of ["ifNoneMatchExisting", "ifMatchWrong", "ifMatchRight"]) {
    const r = evaluateCasChecks({ ...baseSteps(), [step]: netErr() });
    assert.equal(r.ok, false, step);
    assert.equal(r.transient, true, step);
    assert.doesNotMatch(r.message, /不能保证 RPO=0/, step);
  }
  const rr = evaluateCasChecks({ ...baseSteps(), gotAfter: netErr() });
  assert.equal(rr.transient, true);
});

test("evaluateCasChecks: NotImplemented → 结论性失败 (存储不支持条件写)", () => {
  const r = evaluateCasChecks({ ...baseSteps(), ifNoneMatchExisting: { ok: false, error: "An error occurred (NotImplemented)" } });
  assert.equal(r.ok, false);
  assert.equal(r.transient, undefined);
  assert.equal(r.error, "if-none-match");
  assert.match(r.message, /不能保证 RPO=0/);
});

test("evaluateCasChecks: 无 ETag / 忽略 If-Match / 写后读不一致 → 结论性失败", () => {
  const noEtag = evaluateCasChecks({ ...baseSteps(), got: { ok: true, body: "b1" } });
  assert.equal(noEtag.error, "no-etag");
  assert.equal(noEtag.transient, undefined);
  const ignored = evaluateCasChecks({ ...baseSteps(), ifMatchWrong: okPut('"x"') });
  assert.equal(ignored.error, "cas-ignored");
  assert.equal(ignored.transient, undefined);
  const raw = evaluateCasChecks({ ...baseSteps(), gotAfter: { ok: true, body: "stale" } });
  assert.equal(raw.error, "read-after-write");
  assert.equal(raw.transient, undefined);
});
