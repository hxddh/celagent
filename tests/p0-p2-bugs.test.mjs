// p0-p2-bugs.test.mjs — 会话 region / CAS 粘滞 / 配置错误语义 (无 aws/celld)
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { awsEnv, casGateSticky, probeStoreCas } from "../src/bos.js";
import { persistenceFromCfg, collectHitsFromTurns } from "../src/bos-tools.js";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

test("awsEnv: extra.AWS_REGION 进入子进程环境", () => {
  const env = awsEnv({ AWS_PROFILE: "bos", AWS_REGION: "auto" });
  assert.equal(env.AWS_REGION, "auto");
  assert.equal(env.AWS_EC2_METADATA_DISABLED, "true");
});

test("casGateSticky: 结论性结果粘滞, transient 不粘滞", () => {
  assert.equal(casGateSticky({ ok: true }), true);
  // 结论性能力失败也必须粘滞 — 否则每一轮都重跑探针再丢轮 (P0)
  assert.equal(casGateSticky({ ok: false, error: "cas-ignored" }), true);
  assert.equal(casGateSticky({ ok: false, error: "no-etag" }), true);
  assert.equal(casGateSticky({ ok: false, error: "if-none-match" }), true);
  assert.equal(casGateSticky({ ok: false, error: "read-after-write" }), true);
  // transient (网络等) 不粘滞, 下次重试
  assert.equal(casGateSticky({ ok: false, transient: true, error: "create-failed" }), false);
  assert.equal(casGateSticky({ ok: false, transient: true, error: "probe-put-failed" }), false);
  assert.equal(casGateSticky({ ok: false, transient: true, error: "no-bucket" }), false);
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

test("collectHitsFromTurns: 命中在 toolResults 深处时片段必须含命中文本", () => {
  const turns = [{
    turn: 3, role: "assistant", ts: 1,
    msg: "本轮摘要与查询完全无关的一段开头文字",
    toolResults: [{
      toolName: "bash",
      content: [{ type: "text", text: "x".repeat(500) + " 独特命中标记词 " + "y".repeat(100) }],
    }],
  }];
  const hits = [];
  collectHitsFromTurns(turns, { query: "独特命中标记词", sessionId: "s", source: "session", hits, limit: 5 });
  assert.equal(hits.length, 1);
  assert.match(hits[0].snippet, /独特命中标记词/);
});

test("store_env: 合格 host 放行, 非法 endpoint fail-closed", () => {
  const script = `
    set -e
    . "${root}/scripts/store_env.sh"
    celagent_is_allowed_endpoint "https://s3.bj.bcebos.com" || exit 11
    celagent_is_allowed_endpoint "https://abc.r2.cloudflarestorage.com" || exit 12
    celagent_is_allowed_endpoint "http://127.0.0.1:9000" || exit 13
    celagent_is_allowed_endpoint "https://evil.example" && exit 14
    celagent_is_allowed_endpoint "http://[::1]:9000" || exit 17
    celagent_is_allowed_endpoint "http://[::1]" || exit 18
    celagent_is_allowed_endpoint "https://s3.a.b.bcebos.com" && exit 19
    celagent_is_allowed_endpoint "https://s3.a.b.amazonaws.com" && exit 20
    celagent_is_allowed_endpoint "https://s3.us-east-1.amazonaws.com" || exit 21
    celagent_is_allowed_endpoint "http://[::1]@evil.example" && exit 22
    celagent_is_allowed_endpoint "http://[::1].evil.example" && exit 23
    celagent_is_allowed_endpoint "http://[::1]:9000@evil.example" && exit 24
    celagent_is_allowed_endpoint "https://s3.bj.bcebos.com@evil.example" && exit 25
    celagent_is_allowed_endpoint "https://evil.example?x=.r2.cloudflarestorage.com" && exit 26
    celagent_is_allowed_endpoint "http://[::1]:abc" && exit 27
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
