// v2/workspace.js — 工作区快照（tar.gz，内容寻址）
import { createHash } from "node:crypto";
import { mkdtempSync, readFileSync, writeFileSync, statSync, rmSync, readdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { execFileSync } from "node:child_process";
import { workspaceKey } from "./keys.js";
import { readManifest, writeManifest } from "./manifest.js";

const DEFAULT_MAX_BYTES = 500 * 1024 * 1024;

export async function snapshotWorkspace({ store, sessionId, cwd, maxBytes = DEFAULT_MAX_BYTES, now = Date.now }) {
  const manifest = await readManifest(store, sessionId);
  if (!manifest) throw new Error(`会话 ${sessionId} 没有 manifest，无法关联工作区`);
  const dir = mkdtempSync(join(tmpdir(), "celagent-ws-"));
  const file = join(dir, "workspace.tar.gz");
  try {
    execFileSync("tar", [
      "-czf", file,
      "--exclude=.git",
      "--exclude=node_modules",
      "--exclude=.venv",
      "--exclude=dist",
      "--exclude=build",
      "-C", cwd, ".",
    ], { stdio: "ignore", timeout: 120000 });
    const size = statSync(file).size;
    if (size > maxBytes) throw new Error(`工作区快照 ${(size / 1048576).toFixed(1)} MB 超过上限 ${(maxBytes / 1048576).toFixed(0)} MB`);
    const bytes = readFileSync(file);
    const hash = createHash("sha256").update(bytes).digest("hex");
    const snapshotId = hash.slice(0, 16);
    const key = workspaceKey(sessionId, snapshotId);
    const put = await store.put(key, bytes.toString("base64"), { ifNoneMatch: true });
    if (!put.ok && !put.conflict) throw new Error(`上传工作区失败: ${put.error}`);
    const next = {
      ...manifest,
      workspace: { latest: snapshotId, key, bytes: size, sha256: hash, createdAt: new Date(now()).toISOString() },
      updatedAt: new Date(now()).toISOString(),
    };
    const written = await writeManifest(store, next, manifest.etag);
    if (!written.ok) {
      if (written.conflict) {
        const cur = await readManifest(store, sessionId);
        if (!cur) throw new Error("manifest 在快照提交时消失");
        const retry = {
          ...cur,
          workspace: { latest: snapshotId, key, bytes: size, sha256: hash, createdAt: new Date(now()).toISOString() },
          updatedAt: new Date(now()).toISOString(),
        };
        const w2 = await writeManifest(store, retry, cur.etag);
        if (!w2.ok) throw new Error(`提交工作区快照失败: ${w2.error || "conflict"}`);
        return { snapshotId, key, bytes: size, sha256: hash };
      }
      throw new Error(`提交工作区快照失败: ${written.error || "unknown"}`);
    }
    return { snapshotId, key, bytes: size, sha256: hash };
  } finally {
    try { rmSync(dir, { recursive: true, force: true }); } catch (e) { /* ignore */ }
  }
}

export async function restoreWorkspace({ store, sessionId, cwd, force = false }) {
  const manifest = await readManifest(store, sessionId);
  if (!manifest) throw new Error(`会话 ${sessionId} 没有 manifest`);
  const ws = manifest.workspace;
  if (!ws?.key) throw new Error(`会话 ${sessionId} 没有工作区快照`);
  if (!force) {
    let entries = [];
    try { entries = readdirSync(cwd).filter((n) => n !== ".git"); } catch (e) { /* dir may not exist */ }
    if (entries.length > 0) throw new Error(`目标目录非空（${entries.length} 项）；加 --force 覆盖`);
  }
  const r = await store.get(ws.key);
  if (!r.ok) throw new Error(`下载工作区失败: ${r.error}`);
  const dir = mkdtempSync(join(tmpdir(), "celagent-ws-"));
  const file = join(dir, "workspace.tar.gz");
  try {
    writeFileSync(file, Buffer.from(r.body, "base64"), { mode: 0o600 });
    execFileSync("tar", ["-xzf", file, "-C", cwd], { stdio: "ignore", timeout: 120000 });
    return { snapshotId: ws.latest, bytes: ws.bytes };
  } finally {
    try { rmSync(dir, { recursive: true, force: true }); } catch (e) { /* ignore */ }
  }
}
