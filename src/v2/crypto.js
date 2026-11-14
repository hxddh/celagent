// v2/crypto.js — 可选的会话内容加密（AES-256-GCM）
// 只加密对象 body；键名/manifest 结构保持可读，便于运维定位。
import { createCipheriv, createDecipheriv, randomBytes } from "node:crypto";

export const ENCRYPTION_ENV = "CELAGENT_ENCRYPTION_KEY";
const ALG = "aes-256-gcm";

function parseKey(raw) {
  if (!raw) return null;
  const key = Buffer.from(String(raw), "base64");
  if (key.length !== 32) {
    throw new Error(`${ENCRYPTION_ENV} 必须是 32 字节 base64（当前 ${key.length} 字节）`);
  }
  return key;
}

export function createCrypto(rawKey = process.env[ENCRYPTION_ENV]) {
  const key = parseKey(rawKey);
  return {
    enabled: Boolean(key),
    encrypt(plain) {
      if (!key) return plain;
      const iv = randomBytes(12);
      const cipher = createCipheriv(ALG, key, iv);
      const data = Buffer.concat([cipher.update(String(plain), "utf8"), cipher.final()]);
      return JSON.stringify({
        v: 1,
        alg: ALG,
        iv: iv.toString("base64"),
        tag: cipher.getAuthTag().toString("base64"),
        data: data.toString("base64"),
      });
    },
    decrypt(body) {
      if (!key) return body;
      let obj;
      try {
        obj = JSON.parse(body);
      } catch (e) {
        return body;
      }
      if (!obj || obj.v !== 1 || obj.alg !== ALG) return body;
      const iv = Buffer.from(obj.iv, "base64");
      const tag = Buffer.from(obj.tag, "base64");
      const data = Buffer.from(obj.data, "base64");
      const decipher = createDecipheriv(ALG, key, iv);
      decipher.setAuthTag(tag);
      return Buffer.concat([decipher.update(data), decipher.final()]).toString("utf8");
    },
  };
}
