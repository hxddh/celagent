// p0-p2-bugs.test.mjs — 会话 region / CAS 粘滞 / 配置错误语义 (无 aws/celld)
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { awsEnv, casGateSticky, probeStoreCas } from "../src/bos.js";
import { persistenceFromCfg } from "../src/bos-tools.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

test("awsEnv: extra.AWS_REGION 进入子进程环境", () => {
  const env = awsEnv({ AWS_PROFILE: "bos", AWS_REGION: "auto" });
  assert.equal(env.AWS_REGION, "auto");
  assert.equal(env.AWS_EC2_METADATA_DISABLED, "true");
});

test("casGateSticky: 仅 ok 与 cas-ignored 粘滞", () => {
  assert.equal(casGateSticky({ ok: true }), true);
  assert.equal(casGateSticky({ ok: false, error: "cas-ignored" }), true);
  assert.equal(casGateSticky({ ok: false, error: "create-failed" }), false);
  assert.equal(casGateSticky({ ok: false, error: "no-bucket" }), false);
  assert.equal(casGateSticky(null), false);
});

test("persistenceFromCfg: 非法 endpoint 不伪装成未配置 bucket", () => {
  const missing = persistenceFromCfg({ persistence: {} });
  assert.equal(missing.error, "no-bucket");
  const bad = persistenceFromCfg({
    persistence: { bucket: "celagent-x", endpoint: "https://evil.example", region: "us-east-1" },
  });
  assert.equal(bad.error, "endpoint-not-allowed");
  assert.match(bad.message, /不允许/);
  const ok = persistenceFromCfg({
    persistence: { bucket: "celagent-x", endpoint: "https://abc.r2.cloudflarestorage.com", region: "auto" },
  });
  assert.equal(ok.error, undefined);
  assert.equal(ok.bucket, "celagent-x");
  assert.equal(ok.region, "auto");
  assert.equal(ok.endpoint, "https://abc.r2.cloudflarestorage.com");
});

test("probeStoreCas: region 传入 ops", async () => {
  const objects = new Map();
  let n = 0;
  const seen = [];
  const ops = {
    async put(key, content, extra = {}) {
      seen.push(extra.region);
      const body = typeof content === "string" ? content : JSON.stringify(content);
      const cur = objects.get(key);
      if (extra.ifNoneMatch && cur) return { ok: false, conflict: true, error: "conflict" };
      if (extra.ifMatch && (!cur || cur.etag !== extra.ifMatch)) {
        return { ok: false, conflict: true, error: "conflict" };
      }
      n += 1;
      const etag = `"etag-${n}"`;
      objects.set(key, { body, etag });
      return { ok: true, result: { ETag: etag } };
    },
    async get(key, extra = {}) {
      seen.push(extra.region);
      const cur = objects.get(key);
      if (!cur) return { ok: false, error: "not-found" };
      return { ok: true, body: cur.body, etag: cur.etag };
    },
    async del(key, extra = {}) {
      seen.push(extra.region);
      objects.delete(key);
      return { ok: true };
    },
  };
  const r = await probeStoreCas({
    bucket: "t",
    endpoint: "http://127.0.0.1:9",
    region: "auto",
    ops,
  });
  assert.equal(r.ok, true);
  assert.ok(seen.length > 0);
  assert.ok(seen.every((x) => x === "auto"), `region 应贯穿: ${JSON.stringify(seen)}`);
});

test("store_env: 合格 host 放行, 非法 endpoint fail-closed", () => {
  const script = `
    set -e
    . "${root}/scripts/store_env.sh"
    celagent_is_allowed_endpoint "https://s3.bj.bcebos.com" || exit 11
    celagent_is_allowed_endpoint "https://abc.r2.cloudflarestorage.com" || exit 12
    celagent_is_allowed_endpoint "http://127.0.0.1:9000" || exit 13
    celagent_is_allowed_endpoint "https://evil.example" && exit 14
    CELAGENT_ALLOW_ENDPOINT=1
    celagent_is_allowed_endpoint "https://evil.example" || exit 15
    unset CELAGENT_ALLOW_ENDPOINT
    HOME=$(mktemp -d)
    mkdir -p "$HOME/.config/celagent"
    printf '%s\\n' '{"persistence":{"bucket":"x","endpoint":"https://evil.example","region":"us-east-1"}}' > "$HOME/.config/celagent/settings.json"
    if command -v jq >/dev/null 2>&1; then
      if celagent_load_store; then exit 16; fi
    fi
    echo ok
  `;
  const out = execFileSync("bash", ["-lc", script], { encoding: "utf8" });
  assert.match(out, /ok/);
});
