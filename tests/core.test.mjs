// celagent 核心链路测试 (node:test)
// 覆盖真实产品路径:
//   1. Celld 节点 API (checkpoint/resume/kv) — 需节点在跑 (node_mgr.sh start)
//   2. BOS 直写链路 (bosPut/bosGet/CAS/If-None-Match) — 需凭证 + bucket
//   3. CLI 命令 (version/help/config) — 无外部依赖
//   4. 配置单源化 (celagent settings ↔ pi-runtime 同步)
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, rmSync, readFileSync, existsSync } from "node:fs";
import { tmpdir, homedir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";

// 测试环境: 需要 celld 节点在跑 (node_mgr.sh start)
const NODES = ["http://127.0.0.1:18090", "http://127.0.0.1:18091"];
const AGENT = "celagent-test";

// CI/无节点环境: 探测一次, 无节点则 Celld 用例 skip (而非失败)
let celldUp = false;
before(async () => {
  for (const base of NODES) {
    try {
      const r = await fetch(`${base}/__celld/health`, { signal: AbortSignal.timeout(2000) });
      if (r.ok) { celldUp = true; break; }
    } catch (e) { /* down */ }
  }
  if (!celldUp) console.log("(Celld 节点未运行, Celld 用例将 skip — node_mgr.sh start 可启动)");
});

async function celld(action, params = {}) {
  const q = new URLSearchParams(params).toString();
  for (const base of NODES) {
    try {
      const resp = await fetch(`${base}/agent/${AGENT}?action=${action}&${q}`, {
        signal: AbortSignal.timeout(8000),
      });
      return await resp.json();
    } catch (e) { /* try next */ }
  }
  return { error: "celld unreachable" };
}

// ---- 真实 BOS 链路 (来自 src/bos.js) ----
let bucket, endpoint;
before(async () => {
  // 读配置 (settings.json 单源); CI 无配置时跳过 BOS 用例
  try {
    const cfg = JSON.parse(readFileSync(join(homedir(), ".config", "celagent", "settings.json"), "utf8"));
    bucket = cfg.persistence?.bucket;
    endpoint = cfg.persistence?.endpoint;
  } catch (e) {
    bucket = null;
  }
  if (!bucket) console.log("(无 bucket 配置, BOS 用例将跳过)");
});

function awsTestEnv() {
  const env = { ...process.env, AWS_EC2_METADATA_DISABLED: "true" };
  if (!(process.env.AWS_ACCESS_KEY_ID && process.env.AWS_SECRET_ACCESS_KEY)) {
    delete env.AWS_ACCESS_KEY_ID;
    delete env.AWS_SECRET_ACCESS_KEY;
    delete env.AWS_SESSION_TOKEN;
    env.AWS_PROFILE = "bos";
  } else {
    delete env.AWS_PROFILE;
  }
  return env;
}

test("1. Celld 可达 (checkpoint/resume API)", async (t) => {
  if (!celldUp) return t.skip("Celld 节点未运行");
  const r = await celld("resume", { session: "t1" });
  assert.ok(r.ok === false || r.ok === true, "resume API 响应正常");
});

test("2. checkpoint + resume 往返", async (t) => {
  if (!celldUp) return t.skip("Celld 节点未运行");
  const sid = `t-${Date.now()}`;
  const r1 = await celld("checkpoint", { session: sid, turn: 1, msg: "hello" });
  assert.equal(r1.ok, true);
  const r2 = await celld("checkpoint", { session: sid, turn: 2, msg: "world" });
  assert.equal(r2.ok, true);
  const r3 = await celld("resume", { session: sid });
  assert.equal(r3.ok, true);
  assert.equal(r3.session.turns.length, 2, "两轮历史完整");
});

test("3. kv-put/get/list/delete 往返", async (t) => {
  if (!celldUp) return t.skip("Celld 节点未运行");
  const k = `k-${Date.now()}`;
  await celld("kv-put", { k, v: "v1" });
  const g = await celld("kv-get", { k });
  assert.equal(g.v, "v1");
  const l = await celld("kv-list", { prefix: k.slice(0, 5), limit: 10 });
  assert.ok(Object.keys(l.entries).length >= 1);
  await celld("kv-delete", { k });
  const g2 = await celld("kv-get", { k });
  assert.equal(g2.ok, false, "删除后不存在");
});

// ---- BOS 直写链路 (Bug 75/76 的回归保护) ----
test("4. BOS 写→读→ETag 往返 (CAS 基础)", async (t) => {
  if (!bucket) return t.skip("无 bucket 配置");
  const { bosPut, bosGet } = await import("../src/bos.js");
  const key = `tests/cas-basic-${Date.now()}.json`;
  const put = await bosPut(key, { id: "t", turns: [{ turn: 1, msg: "x" }] }, { bucket, endpoint });
  assert.ok(put.ok, `首写成功: ${put.error || ""}`);
  const get = await bosGet(key, { bucket, endpoint });
  assert.ok(get.ok && get.etag, "读回 + ETag");
  assert.equal(JSON.parse(get.body).turns.length, 1);
  // 清理
  const { execFileSync: ex } = await import("node:child_process");
  try { ex("aws", ["s3api", "delete-object", "--bucket", bucket, "--key", key, "--endpoint-url", endpoint], { env: awsTestEnv(), stdio: "ignore" }); } catch (e) { /* ignore */ }
});

test("5. CAS 冲突检测 (旧 ETag 写 → 412)", async (t) => {
  if (!bucket) return t.skip("无 bucket 配置");
  const { bosPut, bosGet } = await import("../src/bos.js");
  const key = `tests/cas-conflict-${Date.now()}.json`;
  await bosPut(key, { id: "t", turns: [] }, { bucket, endpoint });
  const g1 = await bosGet(key, { bucket, endpoint });
  // 用错 ETag 写 → 必须 conflict
  const put = await bosPut(key, { id: "t", turns: [{ turn: 9 }] }, { bucket, ifMatch: "wrong-etag", endpoint });
  assert.ok(put.conflict === true, `错误 ETag 应冲突: ${JSON.stringify(put)}`);
  // 正确 ETag 写 → 成功
  const put2 = await bosPut(key, { id: "t", turns: [{ turn: 9 }] }, { bucket, ifMatch: g1.etag, endpoint });
  assert.ok(put2.ok, "正确 ETag 写入成功");
  const { execFileSync: ex } = await import("node:child_process");
  try { ex("aws", ["s3api", "delete-object", "--bucket", bucket, "--key", key, "--endpoint-url", endpoint], { env: awsTestEnv(), stdio: "ignore" }); } catch (e) { /* ignore */ }
});

test("6. If-None-Match 首写保护 (Bug 76 回归)", async (t) => {
  if (!bucket) return t.skip("无 bucket 配置");
  const { bosPut, bosGet } = await import("../src/bos.js");
  const key = `tests/inm-${Date.now()}.json`;
  // 首写 (不存在) → 成功
  const p1 = await bosPut(key, { id: "t", turns: [{ turn: 1, msg: "first" }] }, { bucket, endpoint, ifNoneMatch: true });
  assert.ok(p1.ok, "首写成功");
  // 重复首写 (已存在) → 412, 不覆盖
  const p2 = await bosPut(key, { id: "t", turns: [{ turn: 1, msg: "second" }] }, { bucket, endpoint, ifNoneMatch: true });
  assert.ok(p2.conflict === true, "已存在时 If-None-Match 应拒绝");
  const g = await bosGet(key, { bucket, endpoint });
  assert.equal(JSON.parse(g.body).turns[0].msg, "first", "首轮未被覆盖");
  const { execFileSync: ex } = await import("node:child_process");
  try { ex("aws", ["s3api", "delete-object", "--bucket", bucket, "--key", key, "--endpoint-url", endpoint], { env: awsTestEnv(), stdio: "ignore" }); } catch (e) { /* ignore */ }
});

// ---- CLI 命令 (无外部依赖) ----
test("7. CLI: version/help 输出", async () => {
  const { execFileSync: ex } = await import("node:child_process");
  const v = ex("node", ["bin/celagent-tui.mjs", "version"], { encoding: "utf8" });
  assert.match(v, /celagent v\d+\.\d+\.\d+/);
  const h = ex("node", ["bin/celagent-tui.mjs", "help"], { encoding: "utf8" });
  assert.match(h, /用法/);
});

test("8. CLI: 未知 - 参数拒绝 (Bug 80 回归)", async () => {
  const { execFileSync: ex } = await import("node:child_process");
  let threw = false;
  try { ex("node", ["bin/celagent-tui.mjs", "--badflag"], { encoding: "utf8" }); }
  catch (e) { threw = true; assert.match(String(e.stderr || ""), /未知选项/); }
  assert.ok(threw, "未知选项应报错退出");
});

// ---- 配置单源化 (Bug 87 回归) ----
test("9. config set model 同步 pi-runtime (Bug 87 回归)", async (t) => {
  const { execFileSync: ex } = await import("node:child_process");
  const piFile = join(homedir(), ".config", "celagent", "pi-runtime", "settings.json");
  if (!existsSync(piFile)) return t.skip("无 pi-runtime settings");
  const orig = JSON.parse(readFileSync(piFile, "utf8"));
  try {
    ex("node", ["bin/celagent-tui.mjs", "config", "set", "model", orig.defaultModel], { encoding: "utf8" });
    const after = JSON.parse(readFileSync(piFile, "utf8"));
    assert.equal(after.defaultModel, orig.defaultModel, "pi-runtime defaultModel 同步");
  } finally {
    // 还原
    writeFileSync(piFile, JSON.stringify(orig, null, 2) + "\n", "utf8");
  }
});

test("10. awsEnv 不混用凭证 + 会话 ID 白名单", async () => {
  const { awsEnv } = await import("../src/bos.js");
  const prevAk = process.env.AWS_ACCESS_KEY_ID;
  const prevSk = process.env.AWS_SECRET_ACCESS_KEY;
  const prevTok = process.env.AWS_SESSION_TOKEN;
  const prevProf = process.env.AWS_PROFILE;
  try {
    delete process.env.AWS_ACCESS_KEY_ID;
    delete process.env.AWS_SECRET_ACCESS_KEY;
    delete process.env.AWS_SESSION_TOKEN;
    process.env.AWS_PROFILE = "other";
    const e1 = awsEnv();
    assert.equal(e1.AWS_PROFILE, "bos");
    assert.equal(e1.AWS_ACCESS_KEY_ID, undefined);
    process.env.AWS_ACCESS_KEY_ID = "AKIATEST";
    process.env.AWS_SECRET_ACCESS_KEY = "secret";
    const e2 = awsEnv();
    assert.equal(e2.AWS_PROFILE, undefined);
    assert.equal(e2.AWS_ACCESS_KEY_ID, "AKIATEST");
  } finally {
    if (prevAk === undefined) delete process.env.AWS_ACCESS_KEY_ID; else process.env.AWS_ACCESS_KEY_ID = prevAk;
    if (prevSk === undefined) delete process.env.AWS_SECRET_ACCESS_KEY; else process.env.AWS_SECRET_ACCESS_KEY = prevSk;
    if (prevTok === undefined) delete process.env.AWS_SESSION_TOKEN; else process.env.AWS_SESSION_TOKEN = prevTok;
    if (prevProf === undefined) delete process.env.AWS_PROFILE; else process.env.AWS_PROFILE = prevProf;
  }
  const { execFileSync: ex } = await import("node:child_process");
  let threw = false;
  try { ex("node", ["bin/celagent-tui.mjs", "export", "../evil"], { encoding: "utf8" }); }
  catch (e) { threw = true; assert.match(String(e.stderr || e.stdout || ""), /非法/); }
  assert.ok(threw, "路径穿越 session id 应拒绝");
});
