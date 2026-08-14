// bos.js — celagent 对象存储直写 (绕过 celld LTX 缺陷)
// 用 aws CLI 完成直写 (签名由 aws 处理); 默认 BOS, 合格 S3 兼容 endpoint 可配置
import { join } from "node:path";
import { execFile } from "node:child_process";
import { randomUUID } from "node:crypto";
import { writeFile, readFile, chmod, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";

export const DEFAULT_ENDPOINT = "https://s3.bj.bcebos.com";
const AWS_TIMEOUT_MS = 20000;

function normalizeEndpoint(raw) {
  return String(raw || "").trim().replace(/\/$/, "");
}

function isAllowedHost(hostname) {
  const h = String(hostname || "").toLowerCase();
  if (h === "127.0.0.1" || h === "localhost" || h === "::1") return true;
  if (/^s3(\.[a-z0-9-]+)?\.bcebos\.com$/i.test(h)) return true;
  if (h === "s3.amazonaws.com" || /^s3(\.[a-z0-9-]+)?\.amazonaws\.com$/i.test(h)) return true;
  if (h.endsWith(".r2.cloudflarestorage.com")) return true;
  if (h === "fly.storage.tigris.dev" || h.endsWith(".tigris.dev")) return true;
  if (h === "t3.storage.dev" || h.endsWith(".t3.storage.dev")) return true;
  return false;
}

/** 合格 https host / 本机; 其他需 CELAGENT_ALLOW_ENDPOINT=1 */
export function isAllowedEndpoint(raw) {
  const ep = normalizeEndpoint(raw);
  if (!ep) return false;
  if (process.env.CELAGENT_ALLOW_ENDPOINT === "1" || process.env.CELAGENT_ALLOW_ENDPOINT === "true") {
    try { return Boolean(new URL(ep)); } catch (e) { return false; }
  }
  try {
    const u = new URL(ep);
    if (u.hostname === "127.0.0.1" || u.hostname === "localhost" || u.hostname === "::1") {
      return u.protocol === "http:" || u.protocol === "https:";
    }
    if (u.protocol !== "https:") return false;
    return isAllowedHost(u.hostname);
  } catch (e) {
    return false;
  }
}

/** 无 override → 默认 BOS。有值但不允许 → 抛错,绝不改写成 BOS */
export function resolveEndpoint(override) {
  const ep = normalizeEndpoint(override);
  if (!ep) return DEFAULT_ENDPOINT;
  if (!isAllowedEndpoint(ep)) {
    const err = new Error(`persistence.endpoint 不允许: ${ep} (仅 https 合格 host 或本机; 或设 CELAGENT_ALLOW_ENDPOINT=1)`);
    err.code = "endpoint-not-allowed";
    throw err;
  }
  return ep;
}

/** host 为 *.bcebos.com 时默认 bj; 其它须显式 persistence.region */
export function defaultRegion(endpoint) {
  const ep = normalizeEndpoint(endpoint) || DEFAULT_ENDPOINT;
  try {
    const u = new URL(ep);
    if (/\.bcebos\.com$/i.test(u.hostname)) return "bj";
  } catch (e) { /* ignore */ }
  return undefined;
}

export function resolveRegion(endpoint, configured) {
  const explicit = String(configured ?? "").trim();
  if (explicit) return explicit;
  return defaultRegion(endpoint);
}

/** 凭证要么全用 env, 要么全用 profile — 绝不混用 */
export function awsEnv(extra = {}) {
  const env = { ...process.env, AWS_EC2_METADATA_DISABLED: "true", ...extra };
  const hasFullEnvCreds = process.env.AWS_ACCESS_KEY_ID && process.env.AWS_SECRET_ACCESS_KEY;
  if (hasFullEnvCreds) {
    delete env.AWS_PROFILE;
  } else {
    delete env.AWS_ACCESS_KEY_ID;
    delete env.AWS_SECRET_ACCESS_KEY;
    delete env.AWS_SESSION_TOKEN;
    env.AWS_PROFILE = extra.AWS_PROFILE || "bos";
  }
  return env;
}

/** 0700 私有临时目录, 避免可预测 /tmp/celagent-* 路径 */
async function privateTmp(name = "body.json") {
  const dir = await mkdtemp(join(tmpdir(), "celagent-"));
  try { await chmod(dir, 0o700); } catch (e) { /* ignore */ }
  return {
    dir,
    path: join(dir, name),
    async cleanup() {
      try { await rm(dir, { recursive: true, force: true }); } catch (e) { /* ignore */ }
    },
  };
}

function runAws(args, { timeout = AWS_TIMEOUT_MS, profile, region } = {}) {
  const extra = {};
  if (profile) extra.AWS_PROFILE = profile;
  const rg = String(region ?? "").trim();
  if (rg) extra.AWS_REGION = rg;
  return new Promise((resolve) => {
    execFile("aws", args, { env: awsEnv(extra), timeout, encoding: "utf8" }, (err, stdout, stderr) => {
      if (err) {
        const msg = String(err.message || err) + (stderr ? " " + String(stderr).slice(0, 200) : "");
        resolve({ ok: false, error: msg });
        return;
      }
      resolve({ ok: true, stdout });
    });
  });
}

/** aws CLI JSON 输出; 失败不伪装成空数组 */
export async function awsJson(args, opts = {}) {
  const r = await runAws(args, opts);
  if (!r.ok) return { ok: false, error: r.error };
  try {
    return { ok: true, data: JSON.parse(r.stdout || "[]") };
  } catch (e) {
    return { ok: false, error: "invalid-json" };
  }
}

/** 进程内 CAS 门禁: 通过或 cas-ignored 才粘滞; 网络/凭证失败下次重试 */
export function casGateSticky(result) {
  return Boolean(result && (result.ok || result.error === "cas-ignored"));
}

function etagFromGetStdout(stdout) {
  if (!stdout) return undefined;
  try {
    const j = JSON.parse(stdout);
    const e = j.ETag || j.etag;
    return e ? String(e).trim() : undefined;
  } catch (e) {
    return undefined;
  }
}

function resolvePutGetEndpoint(endpoint) {
  try {
    return { ok: true, ep: resolveEndpoint(endpoint) };
  } catch (e) {
    return { ok: false, error: e.code === "endpoint-not-allowed" ? "endpoint-not-allowed" : String(e.message || e) };
  }
}

export async function bosPut(key, content, { bucket, ifMatch, ifNoneMatch, maxRetries = 3, endpoint, profile, region } = {}) {
  if (!bucket) return { ok: false, error: "no-bucket" };
  const resolved = resolvePutGetEndpoint(endpoint);
  if (!resolved.ok) return { ok: false, error: resolved.error };
  const ep = resolved.ep;
  const tmp = await privateTmp("put.json");
  const body = typeof content === "string" ? content : JSON.stringify(content);
  await writeFile(tmp.path, body, { encoding: "utf8", mode: 0o600 });
  try {
    const args = [
      "s3api", "put-object",
      "--bucket", bucket,
      "--key", key,
      "--body", tmp.path,
      "--endpoint-url", ep,
      "--output", "json",
    ];
    if (ifMatch) args.push("--if-match", ifMatch);
    if (ifNoneMatch) args.push("--if-none-match", "*");
    for (let attempt = 0; ; attempt++) {
      const r = await runAws(args, { profile, region });
      if (r.ok) {
        try { return { ok: true, result: JSON.parse(r.stdout) }; }
        catch (e) { return { ok: true, result: {} }; }
      }
      const msg = r.error || "";
      if (msg.includes("PreconditionFailed") || msg.includes("412")) {
        return { ok: false, conflict: true, error: "conflict" };
      }
      if (attempt < maxRetries - 1) {
        await new Promise(res => setTimeout(res, 300 * 2 ** attempt));
        continue;
      }
      return { ok: false, error: msg.split("\n").slice(-2).join(" ") };
    }
  } finally {
    await tmp.cleanup();
  }
}

export async function bosGet(key, { bucket, endpoint, profile, region } = {}) {
  if (!bucket) return { ok: false, error: "no-bucket" };
  const resolved = resolvePutGetEndpoint(endpoint);
  if (!resolved.ok) return { ok: false, error: resolved.error };
  const ep = resolved.ep;
  const tmp = await privateTmp("get.json");
  try {
    const dl = await runAws([
      "s3api", "get-object",
      "--bucket", bucket,
      "--key", key,
      "--endpoint-url", ep,
      tmp.path,
    ], { profile, region });
    if (!dl.ok) {
      const msg = dl.error || "";
      return { ok: false, error: (msg.includes("404") || msg.includes("NoSuchKey")) ? "not-found" : msg };
    }
    try { await chmod(tmp.path, 0o600); } catch (e) { /* ignore */ }
    // get-object 的 stdout 即对象元数据(含 ETag), 避免再 head 造成 TOCTOU
    let etag = etagFromGetStdout(dl.stdout);
    if (!etag) {
      const head = await runAws([
        "s3api", "head-object",
        "--bucket", bucket,
        "--key", key,
        "--endpoint-url", ep,
        "--query", "ETag",
        "--output", "text",
      ], { profile, region });
      if (head.ok) etag = head.stdout.trim() || undefined;
    }
    const body = await readFile(tmp.path, "utf8");
    return { ok: true, body, etag };
  } catch (e) {
    const msg = String(e.message || e);
    return { ok: false, error: (msg.includes("404") || msg.includes("NoSuchKey")) ? "not-found" : msg };
  } finally {
    await tmp.cleanup();
  }
}

export async function bosDelete(key, { bucket, endpoint, profile, region } = {}) {
  if (!bucket) return { ok: false, error: "no-bucket" };
  const resolved = resolvePutGetEndpoint(endpoint);
  if (!resolved.ok) return { ok: false, error: resolved.error };
  const r = await runAws([
    "s3api", "delete-object",
    "--bucket", bucket,
    "--key", key,
    "--endpoint-url", resolved.ep,
  ], { profile, region });
  if (!r.ok) return { ok: false, error: r.error };
  return { ok: true };
}

/** 根据探针各步结果判定存储是否真的执行条件写。忽略 If-Match → cas-ignored */
export function evaluateCasChecks({
  create,
  got,
  ifNoneMatchExisting,
  ifMatchWrong,
  ifMatchRight,
  gotAfter,
  expectedBody1,
  expectedBody2,
} = {}) {
  const checks = [];
  const fail = (error, message) => ({ ok: false, error, message, checks });
  if (!create?.ok) {
    checks.push({ name: "create", ok: false });
    return fail("create-failed", create?.error || "CAS 探针无法写入");
  }
  checks.push({ name: "create", ok: true });
  if (!got?.ok || String(got.body) !== String(expectedBody1)) {
    checks.push({ name: "read-after-write", ok: false });
    return fail("read-after-write", "PUT 成功后 GET 看不到新内容,此存储不能保证 RPO=0");
  }
  checks.push({ name: "read-after-write", ok: true });
  if (!got.etag) {
    checks.push({ name: "etag", ok: false });
    return fail("no-etag", "对象没有稳定 ETag,无法做条件覆盖,此存储不能保证 RPO=0");
  }
  checks.push({ name: "etag", ok: true });
  if (ifNoneMatchExisting?.ok) {
    checks.push({ name: "if-none-match-existing", ok: false, ignored: true });
    return fail("cas-ignored", "此存储不能保证 RPO=0: If-None-Match 被忽略");
  }
  if (!ifNoneMatchExisting?.conflict) {
    checks.push({ name: "if-none-match-existing", ok: false });
    return fail("if-none-match", "已存在对象上 If-None-Match:* 未返回冲突,此存储不能保证 RPO=0");
  }
  checks.push({ name: "if-none-match-existing", ok: true });
  if (ifMatchWrong?.ok) {
    checks.push({ name: "if-match-wrong", ok: false, ignored: true });
    return fail("cas-ignored", "此存储不能保证 RPO=0: If-Match 被忽略");
  }
  if (!ifMatchWrong?.conflict) {
    checks.push({ name: "if-match-wrong", ok: false });
    return fail("if-match-wrong", "错误 ETag 的 If-Match 未返回冲突,此存储不能保证 RPO=0");
  }
  checks.push({ name: "if-match-wrong", ok: true });
  if (!ifMatchRight?.ok) {
    checks.push({ name: "if-match-right", ok: false });
    return fail("if-match-right", ifMatchRight?.error || "正确 ETag 的 If-Match 失败");
  }
  checks.push({ name: "if-match-right", ok: true });
  if (!gotAfter?.ok || String(gotAfter.body) !== String(expectedBody2)) {
    checks.push({ name: "read-after-cas", ok: false });
    return fail("read-after-write", "条件覆盖后 GET 看不到新内容,此存储不能保证 RPO=0");
  }
  checks.push({ name: "read-after-cas", ok: true });
  return { ok: true, message: "CAS 探针通过 (If-Match / If-None-Match / 写后读)", checks };
}

/** 对当前 bucket 做会话路径 CAS 门禁。ops 可注入,供无 aws CLI 的测试 */
export async function probeStoreCas({ bucket, endpoint, profile, region, ops } = {}) {
  if (!bucket) return { ok: false, error: "no-bucket", message: "未配置 bucket", checks: [] };
  const put = ops?.put || ((key, content, extra) => bosPut(key, content, extra));
  const get = ops?.get || ((key, extra) => bosGet(key, extra));
  const del = ops?.del || ((key, extra) => bosDelete(key, extra));
  const id = randomUUID();
  const key = `celagent-cas-probe/${id}.json`;
  const expectedBody1 = JSON.stringify({ probe: 1, id });
  const expectedBody2 = JSON.stringify({ probe: 2, id });
  const common = { bucket, endpoint, profile, region };
  try {
    const create = await put(key, expectedBody1, common);
    const got = create.ok ? await get(key, common) : {};
    let ifNoneMatchExisting = {};
    let ifMatchWrong = {};
    let ifMatchRight = {};
    let gotAfter = {};
    if (create.ok && got.ok && got.etag) {
      ifNoneMatchExisting = await put(key, expectedBody2, { ...common, ifNoneMatch: true });
      ifMatchWrong = await put(key, expectedBody2, { ...common, ifMatch: '"bogus-etag-celagent"' });
      ifMatchRight = await put(key, expectedBody2, { ...common, ifMatch: got.etag });
      gotAfter = await get(key, common);
    }
    return evaluateCasChecks({
      create, got, ifNoneMatchExisting, ifMatchWrong, ifMatchRight, gotAfter,
      expectedBody1, expectedBody2,
    });
  } finally {
    try { await del(key, common); } catch (e) { /* 探针残留不挡结论 */ }
  }
}
