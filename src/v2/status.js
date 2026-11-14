// v2/status.js — 会话持久化状态
import { readManifest } from "./manifest.js";
import { leaseKey } from "./keys.js";

export async function sessionStatus({ store, outbox, journal, sessionId }) {
  const manifest = await readManifest(store, sessionId);
  const lease = await store.get(leaseKey(sessionId));
  let leaseBody = null;
  if (lease.ok) { try { leaseBody = JSON.parse(lease.body); } catch (e) { leaseBody = { corrupt: true }; } }
  const pending = outbox ? outbox.get(sessionId) : null;
  let journalInfo = null;
  if (journal) {
    try {
      const entries = await journal.list(sessionId);
      journalInfo = {
        total: entries.length,
        finished: entries.filter((e) => e.status === "finished").length,
        unknown: entries.filter((e) => e.status !== "finished").length,
      };
    } catch (e) { journalInfo = { error: String(e.message || e) }; }
  }
  return {
    session: sessionId,
    durableSeq: manifest?.head?.seq ?? null,
    segments: manifest?.segments?.length ?? 0,
    updatedAt: manifest?.updatedAt ?? null,
    lease: leaseBody,
    outbox: pending ? { pending: true, at: pending.at, localFile: pending.localFile } : { pending: false },
    workspace: manifest?.workspace || null,
    journal: journalInfo,
    stats: manifest?.stats || null,
  };
}
