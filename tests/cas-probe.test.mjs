// cas-probe.test.mjs — 无 aws CLI:内存 store 验证 CAS 门禁
import { test } from "node:test";
import assert from "node:assert/strict";
import { probeStoreCas } from "../src/bos.js";

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

test("probeStoreCas: 无 bucket 失败", async () => {
  const r = await probeStoreCas({});
  assert.equal(r.ok, false);
  assert.equal(r.error, "no-bucket");
});
