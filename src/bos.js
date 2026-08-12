// bos.js — celagent 直接写 BOS (绕过 celld LTX 缺陷)
// 用 aws CLI 完成直写 (签名由 aws 处理, 可靠); 凭证从 ~/.aws [bos] profile 自动读取
// 异步实现: execFile (promise) 而非 execFileSync — Bug 59 修复:
//   同步阻塞会冻结整个事件循环 (~780ms/次), 即使调用方在异步队列里,
//   TUI 渲染/输入仍会被卡住; 改异步后队列执行不再阻塞交互
import { homedir } from "node:os";
import { join } from "node:path";
import { readFileSync, existsSync } from "node:fs";
import { execFile } from "node:child_process";
import { writeFile, readFile, chmod, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";

const EP = "https://s3.bj.bcebos.com";
// aws CLI 调用超时 (Bug 59: 防网络黑洞永久挂起队列, 卡死退出路径)
const AWS_TIMEOUT_MS = 20000;

function resolveEndpoint(override) {
  // Bug 70: endpoint 支持从调用方传入 — settings.json 配了自定义 endpoint
  // (OSS/minio/其他 S3 兼容) 时不再硬编码 BOS, 避免读写路径不一致
  return override || EP;
}

/** 凭证要么全用 env, 要么全用 profile — 绝不混用 (env 部分覆盖会签名失败) */
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

/** 0700 私有临时目录 + 0600 文件 — 避免 /tmp/celagent-* 可预测路径 */
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
        // 超时 (ETIMEDOUT) / 网络错误统一包装
        const msg = String(err.message || err) + (stderr ? " " + String(stderr).slice(0, 200) : "");
        resolve({ ok: false, error: msg });
        return;
      }
      resolve({ ok: true, stdout });
    });
  });
}

// ---- 直写 BOS (PUT, 支持 If-Match 条件写; 网络错误自动重试) ----
export async function bosPut(key, content, { bucket, ifMatch, ifNoneMatch, maxRetries = 3, endpoint } = {}) {
  if (!bucket) return { ok: false, error: "no-bucket" };
  const ep = resolveEndpoint(endpoint);
  const tmp = await privateTmp("put.json");
  const body = typeof content === "string" ? content : JSON.stringify(content);
  // 会话内容可能含敏感对话 — 临时文件权限收紧为 owner-only
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
    // 条件写: If-Match 乐观锁 (防并发覆盖)
    if (ifMatch) {
      args.push("--if-match", ifMatch);
    }
    // Bug 76: If-None-Match 首写条件化 (仅对象不存在时写, 防并发冷启动覆盖)
    if (ifNoneMatch) {
      args.push("--if-none-match", "*");
    }
    for (let attempt = 0; ; attempt++) {
      const r = await runAws(args);
      if (r.ok) {
        try { return { ok: true, result: JSON.parse(r.stdout) }; }
        catch (e) { return { ok: true, result: {} }; }
      }
      const msg = r.error || "";
      // 412 = 条件不匹配 (并发冲突) — 不重试, 交给调用方重读
      if (msg.includes("PreconditionFailed") || msg.includes("412")) {
        return { ok: false, conflict: true, error: "conflict" };
      }
      // 网络/瞬时错误 (超时/限流/5xx) — 指数退避重试 (Bug 49 修复)
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

// ---- 直读 BOS (GET, 返回 ETag 用于条件写) ----
export async function bosGet(key, { bucket, endpoint } = {}) {
  if (!bucket) return { ok: false, error: "no-bucket" };
  const ep = resolveEndpoint(endpoint);
  const tmp = await privateTmp("get.json");
  try {
    // 1. 下载文件
    const dl = await runAws([
      "s3api", "get-object",
      "--bucket", bucket,
      "--key", key,
      "--endpoint-url", ep,
      tmp.path,
    ]);
    if (!dl.ok) {
      // Bug G: 404 检测更准确 (NoSuchKey / 404)
      const msg = dl.error || "";
      return { ok: false, error: (msg.includes("404") || msg.includes("NoSuchKey")) ? "not-found" : msg };
    }
    // aws CLI 写出的文件可能过宽权限 — 收紧后再读
    try { await chmod(tmp.path, 0o600); } catch (e) { /* ignore */ }
    // 2. 单独 head-object 取 ETag (get-object 的 --query 不适用)
    let etag;
    const head = await runAws([
      "s3api", "head-object",
      "--bucket", bucket,
      "--key", key,
      "--endpoint-url", ep,
      "--query", "ETag",
      "--output", "text",
    ]);
    if (head.ok) etag = head.stdout.trim() || undefined;
    const body = await readFile(tmp.path, "utf8");
    return { ok: true, body, etag };
  } catch (e) {
    const msg = String(e.message || e);
    return { ok: false, error: (msg.includes("404") || msg.includes("NoSuchKey")) ? "not-found" : msg };
  } finally {
    await tmp.cleanup();
  }
}
