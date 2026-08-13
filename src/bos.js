// bos.js — celagent 直接写 BOS (绕过 celld LTX 缺陷)
// 用 aws CLI 完成直写 (签名由 aws 处理, 可靠); 凭证从 ~/.aws [bos] profile 自动读取
import { join } from "node:path";
import { execFile } from "node:child_process";
import { writeFile, readFile, chmod, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";

const EP = "https://s3.bj.bcebos.com";
const AWS_TIMEOUT_MS = 20000;

function resolveEndpoint(override) {
  return override || EP;
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
    env.AWS_PROFILE = "bos";
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

function runAws(args, { timeout = AWS_TIMEOUT_MS } = {}) {
  return new Promise((resolve) => {
    execFile("aws", args, { env: awsEnv(), timeout, encoding: "utf8" }, (err, stdout, stderr) => {
      if (err) {
        const msg = String(err.message || err) + (stderr ? " " + String(stderr).slice(0, 200) : "");
        resolve({ ok: false, error: msg });
        return;
      }
      resolve({ ok: true, stdout });
    });
  });
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

export async function bosPut(key, content, { bucket, ifMatch, ifNoneMatch, maxRetries = 3, endpoint } = {}) {
  if (!bucket) return { ok: false, error: "no-bucket" };
  const ep = resolveEndpoint(endpoint);
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
      const r = await runAws(args);
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

export async function bosGet(key, { bucket, endpoint } = {}) {
  if (!bucket) return { ok: false, error: "no-bucket" };
  const ep = resolveEndpoint(endpoint);
  const tmp = await privateTmp("get.json");
  try {
    const dl = await runAws([
      "s3api", "get-object",
      "--bucket", bucket,
      "--key", key,
      "--endpoint-url", ep,
      tmp.path,
    ]);
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
      ]);
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
