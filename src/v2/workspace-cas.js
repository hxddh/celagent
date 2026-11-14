// v2/workspace-cas.js — 内容寻址的增量工作区检查点
// 不依赖 restic/tar：按文件 SHA-256 去重，只上传变化 blob；checkpoint 只写元数据。
// 适用于「代码 + 配置 + 小产物」；ignored 依赖由 .celagent/services.yaml 重新安装。
import { createHash } from "node:crypto";
import {
  existsSync, mkdirSync, readdirSync, lstatSync, readFileSync, writeFileSync,
  renameSync, chmodSync, unlinkSync, rmdirSync, readlinkSync, symlinkSync,
} from "node:fs";
import { join, resolve, sep, posix } from "node:path";
import { readManifest, writeManifest } from "./manifest.js";
import { workspaceCheckpointKey, workspaceCheckpointPrefix, workspaceBlobKey } from "./keys.js";

export const CHECKPOINT_VERSION = 1;
export const DEFAULT_EXCLUDES = Object.freeze([
  ".git", "node_modules", ".venv", "dist", "build", ".cache", "target", "vendor", ".amp",
]);
const DEFAULT_MAX_BYTES = 1024 * 1024 * 1024;
const DEFAULT_MAX_FILE_BYTES = 128 * 1024 * 1024;

function sha256Buffer(buf) {
  return createHash("sha256").update(buf).digest("hex");
}
function sha256Text(text) {
  return createHash("sha256").update(String(text)).digest("hex");
}
function nowIso(now) {
  return new Date(now()).toISOString();
}
function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}
function safeJoin(root, rel) {
  const base = resolve(root);
  const abs = resolve(base, rel);
  if (abs !== base && !abs.startsWith(base + sep)) throw new Error(`工作区路径越界: ${rel}`);
  return abs;
}
function isSafeSymlinkTarget(target) {
  if (!target || target.startsWith("/") || target.startsWith("\\")) return false;
  const n = posix.normalize(String(target).replace(/\\/g, "/"));
  return !n.startsWith("../") && n !== "..";
}
function excluded(name, excludes) {
  return excludes.has(name);
}

/** 走一遍工作区；不跟随 symlink。返回文件/目录/symlink 记录。 */
function scanWorkspace(cwd, excludes, maxFileBytes, previous, previousAt = 0) {
  const files = [];
  const dirs = [];
  const symlinks = [];
  const warnings = [];
  let totalBytes = 0;

  function walk(dir, rel) {
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); }
    catch (e) { throw new Error(`读取目录失败 ${dir}: ${e.message}`); }
    entries.sort((a, b) => a.name.localeCompare(b.name));
    for (const ent of entries) {
      if (excluded(ent.name, excludes)) continue;
      const childRel = rel ? `${rel}/${ent.name}` : ent.name;
      const abs = join(dir, ent.name);
      let st;
      try { st = lstatSync(abs, { bigint: true }); }
      catch (e) { throw new Error(`读取 ${childRel} 失败: ${e.message}`); }
      if (st.isDirectory()) {
        dirs.push(childRel);
        walk(abs, childRel);
        continue;
      }
      if (st.isSymbolicLink()) {
        let target;
        try { target = readlinkSync(abs); } catch (e) { throw new Error(`读取 symlink ${childRel} 失败: ${e.message}`); }
        if (!isSafeSymlinkTarget(target)) {
          warnings.push(`${childRel}: 跳过指向工作区外的 symlink -> ${target}`);
          continue;
        }
        symlinks.push({ path: childRel, type: "symlink", target });
        continue;
      }
      if (!st.isFile()) {
        warnings.push(`${childRel}: 跳过非普通文件 (socket/fifo/device)`);
        continue;
      }
      const size = Number(st.size);
      if (size > maxFileBytes) {
        throw new Error(`文件 ${childRel} 为 ${(size / 1048576).toFixed(1)} MB，超过单文件上限 ${(maxFileBytes / 1048576).toFixed(0)} MB；把它加入 workspace.excludes 或调大 workspace.maxFileBytes`);
      }
      const prev = previous.get(childRel);
      // 时间戳可能在粗粒度文件系统上相同；上一检查点 5 秒内一律重新 hash，避免漏掉同尺寸快速覆写。
      const trustCache = prev && previousAt > 0 && Date.now() - previousAt > 5000;
      const mtimeNs = st.mtimeNs.toString();
      const ctimeNs = st.ctimeNs.toString();
      const ino = st.ino.toString();
      let hash;
      if (trustCache && prev.type === "file" && prev.size === size && prev.mtimeNs === mtimeNs && prev.ctimeNs === ctimeNs && prev.ino === ino) {
        hash = prev.hash;
      } else {
        const buf = readFileSync(abs);
        if (buf.length !== size) throw new Error(`文件 ${childRel} 在扫描时发生变化，请重试检查点`);
        hash = sha256Buffer(buf);
      }
      const rec = {
        path: childRel, type: "file", hash, size,
        mode: Number(st.mode & 0o777n), mtimeNs, ctimeNs, ino,
      };
      files.push(rec);
      totalBytes += size;
    }
  }

  walk(cwd, "");
  files.sort((a, b) => a.path.localeCompare(b.path));
  dirs.sort((a, b) => a.localeCompare(b));
  symlinks.sort((a, b) => a.path.localeCompare(b.path));
  return { files, dirs, symlinks, warnings, totalBytes };
}

function contentHashOf({ files, dirs, symlinks }) {
  const canonical = {
    files: files.map((f) => ({ path: f.path, type: "file", hash: f.hash, size: f.size, mode: f.mode })),
    dirs,
    symlinks: symlinks.map((s) => ({ path: s.path, type: "symlink", target: s.target })),
  };
  return sha256Text(JSON.stringify(canonical));
}

async function mapLimit(items, limit, fn) {
  const out = new Array(items.length);
  let next = 0;
  async function worker() {
    while (true) {
      const i = next++;
      if (i >= items.length) return;
      out[i] = await fn(items[i], i);
    }
  }
  const workers = Array.from({ length: Math.min(limit, items.length) }, worker);
  await Promise.all(workers);
  return out;
}

export async function readWorkspaceCheckpoint(store, sessionId, checkpointId) {
  let id = checkpointId;
  if (!id) {
    const manifest = await readManifest(store, sessionId);
    id = manifest?.workspace?.latest;
  }
  if (!id) return null;
  if (!/^[A-Za-z0-9._-]{1,64}$/.test(String(id))) throw new Error(`非法工作区检查点 ID: ${id}`);
  const r = await store.get(workspaceCheckpointKey(sessionId, id));
  if (!r.ok) {
    if (/not-found|NoSuchKey|\b404\b/i.test(String(r.error))) return null;
    throw new Error(`读取工作区检查点失败: ${r.error}`);
  }
  let cp;
  try { cp = JSON.parse(r.body); }
  catch (e) { throw new Error(`工作区检查点损坏 (${id}): ${e.message}`); }
  if (cp?.v !== CHECKPOINT_VERSION || cp?.session !== sessionId) {
    throw new Error(`工作区检查点格式不支持: ${id}`);
  }
  return cp;
}

/**
 * 生成（或复用）一个工作区检查点，并把 manifest.workspace 指向它。
 * lease 由调用方持有；这里会在提交前 assert。
 */
export async function checkpointWorkspace({
  store, sessionId, cwd, lease, excludes = DEFAULT_EXCLUDES,
  maxBytes = DEFAULT_MAX_BYTES, maxFileBytes = DEFAULT_MAX_FILE_BYTES,
  now = Date.now, force = false,
} = {}) {
  if (!store || !sessionId) throw new Error("checkpointWorkspace 需要 store/sessionId");
  if (!cwd || !existsSync(cwd)) throw new Error(`工作区不存在: ${cwd}`);
  if (lease) await lease.assert(sessionId);

  const manifest = await readManifest(store, sessionId);
  if (!manifest) {
    const e = new Error(`会话 ${sessionId} 没有 manifest，无法关联工作区`);
    e.code = "no-manifest";
    throw e;
  }
  const seq = manifest.head.seq;
  const excludeSet = new Set(excludes);

  let previous = new Map();
  const prevCp = manifest.workspace?.latest
    ? await readWorkspaceCheckpoint(store, sessionId, manifest.workspace.latest).catch(() => null)
    : null;
  if (prevCp?.format === "cas-v1" && Array.isArray(prevCp.files)) {
    for (const f of prevCp.files) previous.set(f.path, f);
  }

  const previousAt = prevCp?.createdAt ? Date.parse(prevCp.createdAt) : 0;
  const scanned = scanWorkspace(cwd, excludeSet, maxFileBytes, previous, previousAt);
  if (scanned.totalBytes > maxBytes) {
    throw new Error(`工作区 ${(scanned.totalBytes / 1048576).toFixed(1)} MB 超过上限 ${(maxBytes / 1048576).toFixed(0)} MB；调整 excludes 或 workspace.maxBytes`);
  }
  const contentHash = contentHashOf(scanned);
  const checkpointId = contentHash.slice(0, 16);
  const key = workspaceCheckpointKey(sessionId, checkpointId);
  const createdAt = nowIso(now);

  // 先检查同内容检查点是否已存在；存在则复用，不重复写 metadata。
  const existing = await store.get(key);
  if (existing.ok && !force) {
    const cp = JSON.parse(existing.body);
    await commitWorkspacePointer(store, sessionId, {
      latest: checkpointId, key, seq, format: "cas-v1",
      files: cp.files?.length ?? scanned.files.length,
      dirs: cp.dirs?.length ?? scanned.dirs.length,
      symlinks: cp.symlinks?.length ?? scanned.symlinks.length,
      bytes: cp.totalBytes ?? scanned.totalBytes,
      sha256: cp.sha256 ?? contentHash, createdAt: cp.createdAt || createdAt,
    }, lease);
    return {
      checkpointId, key, seq, bytes: cp.totalBytes ?? scanned.totalBytes,
      files: cp.files?.length ?? scanned.files.length, uploaded: 0, reused: true,
      contentHash, warnings: scanned.warnings,
    };
  }

  const knownHashes = new Set((prevCp?.files || []).map((f) => f.hash));
  const toUpload = scanned.files.filter((f) => !knownHashes.has(f.hash));
  const uploadResults = await mapLimit(toUpload, 8, async (file) => {
    const abs = safeJoin(cwd, file.path);
    const buf = readFileSync(abs);
    const actual = sha256Buffer(buf);
    if (actual !== file.hash) {
      throw new Error(`文件 ${file.path} 在检查点期间发生变化，请重试`);
    }
    const put = await store.put(workspaceBlobKey(sessionId, file.hash), buf.toString("base64"), { ifNoneMatch: true });
    if (!put.ok && !put.conflict) throw new Error(`上传工作区 blob ${file.hash} 失败: ${put.error}`);
    knownHashes.add(file.hash);
    return 1;
  });
  const uploaded = uploadResults.length;

  const cp = {
    v: CHECKPOINT_VERSION, session: sessionId, id: checkpointId, seq,
    format: "cas-v1", createdAt, cwd,
    excludes: [...excludeSet], totalBytes: scanned.totalBytes,
    files: scanned.files, dirs: scanned.dirs, symlinks: scanned.symlinks,
    warnings: scanned.warnings, sha256: contentHash,
  };
  const body = JSON.stringify(cp, null, 2);
  const put = await store.put(key, body, { ifNoneMatch: true });
  if (!put.ok && !put.conflict) throw new Error(`写工作区检查点失败: ${put.error}`);
  if (put.conflict) {
    const got = await store.get(key);
    if (!got.ok) throw new Error(`检查点冲突后读取失败: ${got.error}`);
    const old = JSON.parse(got.body);
    if (old.sha256 !== contentHash) throw new Error(`检查点 ${checkpointId} 冲突且内容不一致`);
  }

  await commitWorkspacePointer(store, sessionId, {
    latest: checkpointId, key, seq, format: "cas-v1",
    files: scanned.files.length, dirs: scanned.dirs.length,
    symlinks: scanned.symlinks.length, bytes: scanned.totalBytes,
    sha256: contentHash, createdAt,
  }, lease);

  return {
    checkpointId, key, seq, bytes: scanned.totalBytes,
    files: scanned.files.length, uploaded, reused: false,
    contentHash, warnings: scanned.warnings,
  };
}

async function commitWorkspacePointer(store, sessionId, workspace, lease) {
  if (lease) await lease.assert(sessionId);
  for (let attempt = 0; attempt < 8; attempt++) {
    const cur = await readManifest(store, sessionId);
    if (!cur) throw new Error(`提交工作区时会话 ${sessionId} 消失`);
    if (cur.workspace?.latest === workspace.latest && cur.workspace?.sha256 === workspace.sha256 && cur.workspace?.seq === workspace.seq) return cur;
    const next = {
      ...cur,
      workspace,
      updatedAt: new Date().toISOString(),
    };
    const written = await writeManifest(store, next, cur.etag);
    if (written.ok) return written.manifest;
    if (!written.conflict) throw new Error(`提交工作区指针失败: ${written.error || "unknown"}`);
    await sleep(100 * (attempt + 1));
  }
  const e = new Error("提交工作区指针失败（CAS 重试耗尽）");
  e.code = "manifest-conflict";
  throw e;
}

export async function listWorkspaceCheckpoints(store, sessionId, { limit = 50 } = {}) {
  const manifest = await readManifest(store, sessionId);
  const latest = manifest?.workspace?.latest || null;
  const r = await store.list(workspaceCheckpointPrefix(sessionId));
  if (!r.ok) throw new Error(`列举工作区检查点失败: ${r.error}`);
  const ids = [];
  for (const item of r.keys) {
    const m = /workspace\/checkpoints\/([A-Za-z0-9._-]+)\.json$/.exec(item.key);
    if (m) ids.push(m[1]);
  }
  const out = [];
  for (const id of ids.slice(0, limit)) {
    const cp = await readWorkspaceCheckpoint(store, sessionId, id).catch(() => null);
    if (!cp) continue;
    out.push({
      id, seq: cp.seq, createdAt: cp.createdAt, files: cp.files?.length || 0,
      dirs: cp.dirs?.length || 0, symlinks: cp.symlinks?.length || 0,
      bytes: cp.totalBytes || 0, current: id === latest,
    });
  }
  out.sort((a, b) => String(b.createdAt).localeCompare(String(a.createdAt)));
  return { latest, checkpoints: out };
}

function assertInside(root, abs, rel) {
  const base = resolve(root);
  const p = resolve(abs);
  if (p !== base && !p.startsWith(base + sep)) throw new Error(`恢复路径越界: ${rel}`);
  return p;
}

/** 恢复一个检查点；force 时只清理检查点管理的路径，保留 .git/node_modules 等 excluded 目录。 */
export async function restoreWorkspaceCheckpoint({
  store, sessionId, checkpointId, cwd, force = false,
} = {}) {
  if (!store || !sessionId) throw new Error("restoreWorkspaceCheckpoint 需要 store/sessionId");
  if (!cwd) throw new Error("restoreWorkspaceCheckpoint 需要 cwd");
  const cp = await readWorkspaceCheckpoint(store, sessionId, checkpointId);
  if (!cp) throw new Error(`会话 ${sessionId} 没有工作区检查点`);
  if (!existsSync(cwd)) mkdirSync(cwd, { recursive: true, mode: 0o700 });
  const excludes = new Set(cp.excludes || DEFAULT_EXCLUDES);

  if (!force) {
    let entries = [];
    try { entries = readdirSync(cwd).filter((n) => !excludes.has(n)); } catch (e) { /* ignore */ }
    if (entries.length > 0) throw new Error(`目标目录非空（${entries.length} 项，已忽略 excluded）；加 --force 覆盖`);
  }

  const bufCache = new Map();
  async function blob(hash) {
    const clean = String(hash).replace(/^sha256:/, "");
    if (!/^[a-f0-9]{64}$/i.test(clean)) throw new Error(`非法工作区 blob hash: ${hash}`);
    if (bufCache.has(clean)) return bufCache.get(clean);
    const r = await store.get(workspaceBlobKey(sessionId, clean));
    if (!r.ok) throw new Error(`下载工作区 blob ${hash} 失败: ${r.error}`);
    const buf = Buffer.from(r.body, "base64");
    const actual = sha256Buffer(buf);
    if (actual !== clean) throw new Error(`工作区 blob hash 不匹配: ${hash}`);
    bufCache.set(clean, buf);
    return buf;
  }
  const uniqueHashes = [...new Set((cp.files || []).map((f) => String(f.hash).replace(/^sha256:/, "")))];
  await mapLimit(uniqueHashes, 4, blob);

  for (const d of cp.dirs || []) {
    mkdirSync(assertInside(cwd, join(cwd, ...d.split("/")), d), { recursive: true, mode: 0o700 });
  }
  for (const f of cp.files || []) {
    const abs = assertInside(cwd, join(cwd, ...f.path.split("/")), f.path);
    mkdirSync(join(abs, ".."), { recursive: true, mode: 0o700 });
    const buf = await blob(f.hash);
    const tmp = `${abs}.tmp-${process.pid}-${Date.now()}`;
    writeFileSync(tmp, buf, { mode: f.mode || 0o600 });
    try { chmodSync(tmp, f.mode || 0o600); } catch (e) { /* ignore */ }
    renameSync(tmp, abs);
  }
  for (const s of cp.symlinks || []) {
    const abs = assertInside(cwd, join(cwd, ...s.path.split("/")), s.path);
    mkdirSync(join(abs, ".."), { recursive: true, mode: 0o700 });
    try { unlinkSync(abs); } catch (e) { /* not exists */ }
    symlinkSync(s.target, abs);
  }

  let removed = 0;
  if (force) removed = pruneManaged(cwd, cp, excludes);
  return {
    checkpointId: cp.id, seq: cp.seq, bytes: cp.totalBytes || 0,
    files: cp.files?.length || 0, restored: cp.files?.length || 0, removed,
  };
}

function pruneManaged(cwd, cp, excludes) {
  const fileSet = new Set((cp.files || []).map((f) => f.path));
  const dirSet = new Set(cp.dirs || []);
  const symlinkSet = new Set((cp.symlinks || []).map((s) => s.path));
  let removed = 0;
  function walk(dir, rel) {
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); } catch (e) { return; }
    for (const ent of entries) {
      if (excluded(ent.name, excludes)) continue;
      const childRel = rel ? `${rel}/${ent.name}` : ent.name;
      const abs = join(dir, ent.name);
      let st;
      try { st = lstatSync(abs); } catch (e) { continue; }
      if (st.isDirectory()) {
        walk(abs, childRel);
        if (!dirSet.has(childRel)) {
          try { if (readdirSync(abs).length === 0) { rmdirSync(abs); removed += 1; } } catch (e) { /* ignore */ }
        }
        continue;
      }
      if (st.isFile() && !fileSet.has(childRel)) {
        try { unlinkSync(abs); removed += 1; } catch (e) { /* ignore */ }
      } else if (st.isSymbolicLink() && !symlinkSet.has(childRel)) {
        try { unlinkSync(abs); removed += 1; } catch (e) { /* ignore */ }
      }
    }
  }
  walk(cwd, "");
  return removed;
}

/**
 * 自动检查点队列：500ms 去抖，同一时刻只跑一个；关闭时 flush 等待最后一次。
 * 检查点失败只上报，不阻塞 Pi 工具执行；下次 request 会重试。
 */
export function createCheckpointRunner({
  store, sessionId, cwd, lease, debounceMs = 500,
  onProgress, onError,
} = {}) {
  let timer = null;
  let retryTimer = null;
  let running = null;
  let pending = false;
  let closed = false;

  function request(reason = "") {
    if (closed) return;
    pending = true;
    if (running) return;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => { timer = null; void run(); }, debounceMs);
    timer.unref?.();
  }

  async function run() {
    if (running) return running;
    running = (async () => {
      while (pending && !closed) {
        pending = false;
        try {
          const r = await checkpointWorkspace({ store, sessionId, cwd, lease });
          onProgress?.(r);
        } catch (e) {
          onError?.(e);
          if (e.code === "manifest-conflict" && !closed) {
            if (retryTimer) clearTimeout(retryTimer);
            retryTimer = setTimeout(() => { retryTimer = null; request("retry"); }, 1500);
            retryTimer.unref?.();
          }
        }
      }
    })().finally(() => { running = null; });
    return running;
  }

  async function flush(timeoutMs = 15000) {
    if (timer) { clearTimeout(timer); timer = null; }
    if (!pending && !running) return;
    await Promise.race([run(), sleep(timeoutMs)]);
  }

  function close() {
    closed = true;
    if (timer) { clearTimeout(timer); timer = null; }
    if (retryTimer) { clearTimeout(retryTimer); retryTimer = null; }
  }

  return { request, flush, close, get pending() { return pending; } };
}

export function workspaceCheckpointSize(cwd, excludes = DEFAULT_EXCLUDES) {
  const excludeSet = new Set(excludes);
  let total = 0;
  function walk(dir) {
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); } catch (e) { return; }
    for (const ent of entries) {
      if (excluded(ent.name, excludeSet)) continue;
      const abs = join(dir, ent.name);
      let st;
      try { st = lstatSync(abs); } catch (e) { continue; }
      if (st.isDirectory()) walk(abs);
      else if (st.isFile()) total += st.size;
    }
  }
  walk(cwd);
  return total;
}
