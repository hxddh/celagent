// v2/outbox.js — 本地 durable 发件箱
// 每个会话一个 JSON 标记文件；先 fsync 本地，再尝试上云；失败留到下次启动重放。
import { mkdirSync, readFileSync, writeFileSync, renameSync, unlinkSync, readdirSync, openSync, fsyncSync, closeSync } from "node:fs";
import { join } from "node:path";

function atomicWrite(path, text) {
  const tmp = `${path}.tmp-${process.pid}-${Date.now()}`;
  const fd = openSync(tmp, "w", 0o600);
  try {
    writeFileSync(fd, text, "utf8");
    fsyncSync(fd);
  } finally {
    closeSync(fd);
  }
  renameSync(tmp, path);
}

function fsyncDir(dir) {
  try {
    const fd = openSync(dir, "r");
    try { fsyncSync(fd); } finally { closeSync(fd); }
  } catch (e) { /* 目录 fsync 在部分平台不可用，不阻塞 */ }
}

export function createOutbox(dir) {
  mkdirSync(dir, { recursive: true, mode: 0o700 });

  function pathOf(sessionId) {
    return join(dir, `${sessionId}.json`);
  }

  return {
    dir,
    pathOf,
    mark(sessionId, localFile, note = "") {
      const body = JSON.stringify({ sessionId, localFile, note, at: Date.now() }) + "\n";
      atomicWrite(pathOf(sessionId), body);
      fsyncDir(dir);
      return pathOf(sessionId);
    },
    clear(sessionId) {
      try { unlinkSync(pathOf(sessionId)); } catch (e) { /* already gone */ }
    },
    get(sessionId) {
      try {
        return JSON.parse(readFileSync(pathOf(sessionId), "utf8"));
      } catch (e) {
        return null;
      }
    },
    list() {
      try {
        return readdirSync(dir)
          .filter((f) => f.endsWith(".json"))
          .map((f) => {
            try { return JSON.parse(readFileSync(join(dir, f), "utf8")); } catch (e) { return null; }
          })
          .filter(Boolean);
      } catch (e) {
        return [];
      }
    },
    async flush(handler) {
      const result = { ok: 0, failed: 0, errors: [] };
      for (const item of this.list()) {
        try {
          await handler(item);
          this.clear(item.sessionId);
          result.ok += 1;
        } catch (e) {
          result.failed += 1;
          result.errors.push({ sessionId: item.sessionId, error: String(e.message || e) });
        }
      }
      return result;
    },
  };
}
