// v2/index.js — v2 运行时装配
import { homedir } from "node:os";
import { join } from "node:path";
import { createBosAdapter } from "./adapter.js";
import { createOutbox } from "./outbox.js";
import { createLeaseManager } from "./lease.js";
import { createJournal } from "./journal.js";
import { createCrypto } from "./crypto.js";

export function createV2({ storeCfg, configDir = join(homedir(), ".config", "celagent"), owner, now } = {}) {
  if (!storeCfg?.bucket) throw new Error("未配置 persistence.bucket");
  const crypto = createCrypto();
  const store = createBosAdapter(storeCfg, { crypto });
  const outbox = createOutbox(join(configDir, "outbox"));
  const lease = createLeaseManager(store, { owner, now });
  const journal = createJournal({ store, localDir: join(configDir, "journal"), now });
  return { store, outbox, lease, journal, crypto, configDir };
}

export { createBosAdapter, createMemoryAdapter } from "./adapter.js";
export { createOutbox } from "./outbox.js";
export { createLeaseManager, SessionLockedError, LeaseLostError } from "./lease.js";
export { createJournal } from "./journal.js";
export { createCrypto } from "./crypto.js";
export {
  appendFromFile, materialize, readSession, listSessions, deleteSession, importV0,
  parseLocalJsonl, renderJsonl, LineageMismatchError, InvalidJsonlError,
} from "./session.js";
export { readManifest, writeManifest, createManifest, ManifestCorruptError } from "./manifest.js";
export { durabilityExtension } from "./extension.js";
export * from "./keys.js";
