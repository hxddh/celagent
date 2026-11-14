// v2/keys.js — 对象布局的唯一来源
export const SESSIONS_PREFIX = "sessions/";

export function manifestKey(sessionId) {
  return `${SESSIONS_PREFIX}${sessionId}/manifest.json`;
}
export function metaKey(sessionId) {
  return `${SESSIONS_PREFIX}${sessionId}/meta.json`;
}
export function indexKey(sessionId) {
  return `${SESSIONS_PREFIX}${sessionId}/index.json`;
}
export function leaseKey(sessionId) {
  return `${SESSIONS_PREFIX}${sessionId}/lease.json`;
}
export function segmentKey(sessionId, fromSeq, toSeq, hash) {
  const h = String(hash).replace(/^sha256:/, "").slice(0, 16);
  return `${SESSIONS_PREFIX}${sessionId}/segments/${String(fromSeq).padStart(6, "0")}-${String(toSeq).padStart(6, "0")}-${h}.jsonl`;
}
export function journalKey(sessionId, toolCallId) {
  const h = String(toolCallId).replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 96);
  return `${SESSIONS_PREFIX}${sessionId}/journal/${h}.json`;
}
export function workspaceKey(sessionId, snapshotId) {
  return `${SESSIONS_PREFIX}${sessionId}/workspace/${snapshotId}.tar.gz`;
}
export function workspaceCheckpointPrefix(sessionId) {
  return `${SESSIONS_PREFIX}${sessionId}/workspace/checkpoints/`;
}
export function workspaceCheckpointKey(sessionId, checkpointId) {
  return `${workspaceCheckpointPrefix(sessionId)}${checkpointId}.json`;
}
export function workspaceBlobKey(sessionId, hash) {
  return `${SESSIONS_PREFIX}${sessionId}/workspace/blobs/${String(hash).replace(/^sha256:/, "")}`;
}
export function sessionPrefix(sessionId) {
  return `${SESSIONS_PREFIX}${sessionId}/`;
}
export function legacyJsonlKey(sessionId) {
  return `${SESSIONS_PREFIX}${sessionId}.jsonl`;
}
export function legacyTurnsKey(sessionId) {
  return `${SESSIONS_PREFIX}${sessionId}.json`;
}

export function sessionIdFromManifestKey(key) {
  const m = /^sessions\/([^/]+)\/manifest\.json$/.exec(String(key || ""));
  return m ? m[1] : null;
}
