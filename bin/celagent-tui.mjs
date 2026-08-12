#!/usr/bin/env node
// celagent — Pi TUI 版 (完全复刻 pi 交互 + Celld RPO=0 持久化)
// 组装: createAgentSessionServices → createAgentSessionFromServices
//       → createAgentSessionRuntime → InteractiveMode (完整 TUI)
//       + turn_end 钩子 → Celld 镜像
import { homedir } from "node:os";
import { join, basename } from "node:path";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
// Bug 84: 静态 import pi 包 — Bun 编译可静态分析并打包全部依赖 (2983 modules),
// 动态 import(file://路径) 无法被 Bun 打包, 单二进制运行时找不到 chalk 等嵌套依赖。
// 开发模式 (源码目录有 node_modules) 时仍从本地解析; 编译时由 Bun 内联。
import * as pi from "@earendil-works/pi-coding-agent";

const AGENT_DIR = join(homedir(), ".config", "celagent", "pi-runtime");
const CELD_NODES = ["http://127.0.0.1:18090", "http://127.0.0.1:18091", "http://127.0.0.1:19000"];

// ---- Celld 自动启动 (检测无节点 → 从配置拉起 BOS 节点) ----
let ensureRan = false;
let ensureTime = 0;
const ENSURE_COOLDOWN_MS = 30000;  // 30s 冷却, 避免频繁尝试
let ensureLock = null;  // Bug 53: 进程内互斥 — 并发调用 ensureCelld 只执行一次自动启动
async function ensureCelld() {
  // 冷却期内不重复检查 (Bug B: 但允许周期性重试, 而非只跑一次)
  const now = Date.now();
  if (ensureRan && now - ensureTime < ENSURE_COOLDOWN_MS) return;
  // 并发调用串行化: 第一个调用拿锁, 后续等待
  if (ensureLock) return ensureLock;
  ensureRan = true;
  ensureTime = now;
  const run = async () => {
    for (const base of CELD_NODES) {
      try {
        const r = await fetch(`${base}/__celld/health`, { signal: AbortSignal.timeout(2000) });
        if (r.ok) return;
      } catch (e) { /* down */ }
    }
  // 自动拉起 BOS 模式节点
  const cfgFile = join(homedir(), ".config", "celagent", "settings.json");
  if (existsSync(cfgFile)) {
    try {
      const cfg = JSON.parse(readFileSync(cfgFile, "utf8"));
      const bucket = cfg.persistence?.bucket;
      // 凭证: 优先完整环境变量; 否则 AWS_PROFILE=bos (不把 SK 读进 Node 堆)
      // Bug 77: 与 bos.js awsEnv 同策略 — 要么全用 env, 要么全用 profile,
      // 绝不混用 (env 只有部分凭证时, 用 profile 的会签名失败且难排查)
      const hasFullEnv = !!(process.env.AWS_ACCESS_KEY_ID && process.env.AWS_SECRET_ACCESS_KEY);
      let hasBosProfile = false;
      if (!hasFullEnv && existsSync(join(homedir(), ".aws", "credentials"))) {
        try {
          const creds = readFileSync(join(homedir(), ".aws", "credentials"), "utf8");
          const section = creds.split(/\[bos\]/)[1]?.split(/\[/)[0] || "";
          hasBosProfile = !!(section.match(/aws_access_key_id\s*=\s*\S+/) && section.match(/aws_secret_access_key\s*=\s*\S+/));
        } catch (e) { /* 读取失败 */ }
      }
      const bosChildEnv = () => {
        const env = { ...process.env, AWS_REGION: cfg.persistence?.region || "bj", AWS_EC2_METADATA_DISABLED: "true" };
        if (hasFullEnv) {
          delete env.AWS_PROFILE;
        } else {
          delete env.AWS_ACCESS_KEY_ID;
          delete env.AWS_SECRET_ACCESS_KEY;
          delete env.AWS_SESSION_TOKEN;
          env.AWS_PROFILE = "bos";
        }
        return env;
      };
      if (bucket && (hasFullEnv || hasBosProfile)) {
        console.log("  (自动启动 Celld 节点, bucket=" + bucket + ")...");
        const { spawn } = await import("node:child_process");
        const { mkdirSync } = await import("node:fs");
        const stateDir = join(homedir(), ".local", "celagent", "state");
        mkdirSync(stateDir, { recursive: true });
        let celldBin = null;
        for (const cand of [join(homedir(), ".local", "bin", "celld"), "/usr/local/bin/celld"]) {
          if (existsSync(cand)) { celldBin = cand; break; }
        }
        if (celldBin) {
          // Bug 50: 先清理残留 own.json (旧节点被强杀后, own.json 指向已死节点,
          // 会阻塞新节点接管导致 RestoreFailed) — 必须在启动节点之前清理
          // (之前顺序是"启动→等待→清理", 已失败的节点不会自动重试接管)
          try {
            const { execFileSync } = await import("node:child_process");
            const keys = execFileSync("aws", [
              "s3api", "list-objects-v2",
              "--bucket", bucket,
              "--prefix", "cells/",
              "--endpoint-url", cfg.persistence?.endpoint || "https://s3.bj.bcebos.com",
              "--query", "Contents[?ends_with(Key, `own.json`)].Key",
              "--output", "json",
            ], { env: bosChildEnv(), encoding: "utf8" });
            const ownKeys = JSON.parse(keys || "[]");
            if (ownKeys.length > 0) {
              for (const k of ownKeys) {
                execFileSync("aws", [
                  "s3api", "delete-object",
                  "--bucket", bucket,
                  "--key", k,
                  "--endpoint-url", cfg.persistence?.endpoint || "https://s3.bj.bcebos.com",
                ], { env: bosChildEnv(), stdio: "ignore" });
              }
              console.log(`  (已清理 ${ownKeys.length} 个残留 ownership)`);
            }
          } catch (e) { /* 清理失败不阻塞 */ }

          for (const port of [18090, 18091]) {
            // Bug 53: 端口预检 — 另一进程可能已启动节点 (跨进程双启动竞态),
            // 端口已被监听则跳过, 避免重复 spawn 竞争
            try {
              const probe = await fetch(`http://127.0.0.1:${port}/__celld/health`, { signal: AbortSignal.timeout(800) });
              if (probe.ok) continue;
            } catch (e) { /* 端口空闲, 启动 */ }
            // Bug 57: spawn 必须挂 error handler — 否则 celld 二进制缺失/损坏/
            // 无权限/端口冲突时, unhandled 'error' 事件直接炸掉整个 celagent 进程
            const child = spawn(celldBin, [
              "--bucket", `s3://${bucket}`,
              "--endpoint", cfg.persistence?.endpoint || "https://s3.bj.bcebos.com",
              "--region", cfg.persistence?.region || "bj",
              "--listen", `127.0.0.1:${port}`,
              "--advertise", `127.0.0.1:${port}`,
            ], {
              env: { ...bosChildEnv(), CELLD_WATCH: join(stateDir, `node${port}`) },
              stdio: "ignore",
              detached: true,
            });
            child.on("error", (err) => {
              if (!bosWarned) { console.warn(`  (警告: Celld 节点 ${port} 启动失败: ${err.message})`); bosWarned = true; }
            });
            // Bug 62: celld 自身端口冲突/启动即崩时, spawn 不报 error (进程已起来),
            // 但子进程会很快非零退出 — 监听 exit 事件给出明确诊断
            child.on("exit", (code, sig) => {
              if (code !== 0 && code !== null && !bosWarned) {
                console.warn(`  (警告: Celld 节点 ${port} 启动后异常退出 code=${code} sig=${sig ?? ""}, 请检查端口占用或 node log)`);
                bosWarned = true;
              }
            });
            child.unref();
          }
          // Bug 61: 等待两个节点中任意一个就绪即可 (18090 失败不应空等 10s)
          let anyReady = false;
          for (let i = 0; i < 20 && !anyReady; i++) {
            for (const port of [18090, 18091]) {
              try {
                const r = await fetch(`http://127.0.0.1:${port}/__celld/health`, { signal: AbortSignal.timeout(1000) });
                if (r.ok) { anyReady = true; break; }
              } catch (e) { /* wait */ }
            }
            if (!anyReady) await new Promise(r => setTimeout(r, 500));
          }
          if (anyReady) { console.log("  (Celld 节点已启动)"); }
          else { console.warn("  (警告: Celld 节点未能就绪, 请检查 node log)"); }
          return;
        }
      }
      } catch (e) { /* 配置解析失败, 跳过自动启动 */ }
      finally {
        ensureLock = null;  // 释放锁, 允许下次冷却后重试
      }
    }
  };
  ensureLock = run();
  return ensureLock;
}

// ---- BOS 直写队列 (串行, 不阻塞对话; 每次对话后异步落盘) ----
let bosQueue = Promise.resolve();
let bosQueueLen = 0;
let bosWarned = false;  // 只警告一次, 避免刷屏
const BOS_QUEUE_MAX = 50;  // Bug E: 队列限长, 防内存泄漏
function queueBosWrite(sessionId, seq, role, msg, opts = {}) {
  // 完整记忆 (方案 A): fullContent 完整内容块 + fullToolResults 完整工具结果
  const { fullContent = null, fullToolResults = null } = opts || {};
  // 队列过长时丢弃最旧任务 (防堆积)
  if (bosQueueLen >= BOS_QUEUE_MAX) {
    if (!bosWarned) { console.warn("  (警告: BOS 写队列过长, 丢弃旧任务)"); bosWarned = true; }
    return;
  }
  bosQueueLen++;
  bosQueue = bosQueue.then(async () => {
    try {
      const { bosPut, bosGet } = await import("../src/bos.js");
      const cfg = JSON.parse(readFileSync(join(homedir(), ".config", "celagent", "settings.json"), "utf8"));
      const bucket = cfg.persistence?.bucket;
      const endpoint = cfg.persistence?.endpoint; // Bug 70: 透传自定义 endpoint
      if (!bucket) {
        if (!bosWarned) { console.warn("  (警告: 未配置 persistence.bucket, 会话不会持久化)"); bosWarned = true; }
        return;
      }
      const key = `sessions/${sessionId}.json`;
      // 乐观锁 CAS: 读 ETag → If-Match 写 → 冲突重试 (防并发覆盖)
      // Bug 75: 冲突重试必须重新读 ETag — 旧实现 3 次都用循环外读的同一个
      // etag, 并发写入时必 412 重试耗尽 → 该轮数据丢失
      for (let attempt = 0; attempt < 3; attempt++) {
        let session = { id: sessionId, turns: [] };
        let etag = undefined;
        // 每次尝试都重新读 (冲突后对方已写, 必须拿到新 ETag 才能继续)
        const existing = await bosGet(key, { bucket, endpoint });
        if (existing.ok) {
          try { session = JSON.parse(existing.body); } catch (e) { /* 覆盖 */ }
          etag = existing.etag;
        } else if (existing.error === "not-found") {
          // Bug 76: 首写也条件化 (If-None-Match) — 并发冷启动同 ID 时,
          // 双方都读 not-found 会互相无条件覆盖丢首轮; 条件写保证只有一个成功
          // Bug 97: 首写必须包含当前轮次 — 旧实现建空对象后 return, 首轮数据丢失
          const entry = { turn: seq, role, msg, ts: Date.now() };
          if (fullContent && fullContent.length > 0) entry.content = fullContent;
          if (fullToolResults && fullToolResults.length > 0) entry.toolResults = fullToolResults;
          session.turns.push(entry);
          session.updatedAt = Date.now();
          const put = await bosPut(key, session, { bucket, endpoint, ifNoneMatch: true });
          if (put.ok) return;
          if (put.conflict) { await new Promise(r => setTimeout(r, 100)); continue; } // 对方已建, 重读合并
          if (!bosWarned) { console.warn(`  (警告: BOS 首写失败: ${put.error || "未知错误"})`); bosWarned = true; }
          return;
        } else {
          // Bug 49: 读失败(网络/限流)时状态未知 — 绝不写, 防止覆盖已有历史
          if (!bosWarned) { console.warn(`  (警告: BOS 读取失败, 跳过本轮持久化: ${existing.error})`); bosWarned = true; }
          return;
        }
        // Bug 修复: turn 序号基于 BOS 实际历史, 防读失败时覆盖旧数据
        // - 新会话(无历史): 用传入 seq (从 1 开始)
        // - 有历史: 若 seq 已存在则替换, 否则追加为历史长度+1 (续写不覆盖)
        let finalSeq = seq;
        if (session.turns.length > 0) {
          const exists = session.turns.some(t => t.turn === seq);
          if (!exists) {
            // 续写: 追加到历史末尾 (序号 = 最大 turn + 1), 绝不覆盖旧数据
            const maxTurn = Math.max(...session.turns.map(t => t.turn));
            finalSeq = maxTurn + 1;
          }
        }
        const idx = session.turns.findIndex(t => t.turn === finalSeq);
        // 完整记忆: 同轮替换时也保留完整字段; 新轮直接存
        const entry = { turn: finalSeq, role, msg, ts: Date.now() };
        if (fullContent && fullContent.length > 0) entry.content = fullContent;
        if (fullToolResults && fullToolResults.length > 0) entry.toolResults = fullToolResults;
        if (idx >= 0) session.turns[idx] = entry;
        else session.turns.push(entry);
        session.updatedAt = Date.now();
        const put = await bosPut(key, session, { bucket, ifMatch: etag, endpoint });
        if (put.ok) return;
        if (put.conflict) { await new Promise(r => setTimeout(r, 100)); continue; } // 冲突重试
        // 其他错误: 警告一次
        if (!bosWarned) { console.warn(`  (警告: BOS 持久化失败: ${put.error || "未知错误"})`); bosWarned = true; }
        return;
      }
    } catch (e) {
      if (!bosWarned) { console.warn(`  (警告: BOS 持久化异常: ${e.message})`); bosWarned = true; }
    } finally {
      bosQueueLen--;  // 任务完成, 释放队列槽
    }
  }).catch(() => { /* 队列错误不阻塞 */ });
  return bosQueue;
}

// ---- Celld 镜像 (worker 同步 + BOS 异步, 不阻塞对话) ----
async function celldCheckpoint(sessionId, seq, role, content, opts = {}) {
  const msg = typeof content === "string" ? content : JSON.stringify(content ?? "");
  // 完整记忆 (方案 A): fullContent/fullToolResults 全量存 BOS (权威源),
  // worker 缓存只存摘要 msg (URL 截断 200 字符)
  const { fullContent = null, fullToolResults = null } = opts;

  // 1. worker SQLite (即时缓存 — Bug 52: 异步 fire-and-forget, 不阻塞对话;
  //    超时 2s (worker 只是缓存, BOS 是权威源, 丢了可重建))
  ensureCelld();
  void (async () => {
    let workerOk = false;
    for (const base of CELD_NODES) {
      try {
        // Bug D: sessionId 和 msg 都编码
        const url = `${base}/agent/celagent?action=checkpoint&session=${encodeURIComponent(sessionId)}&turn=${seq}&role=${encodeURIComponent(role)}&msg=${encodeURIComponent(msg.slice(0, 200))}`;
        const resp = await fetch(url, { signal: AbortSignal.timeout(2000) });
        const data = await resp.json();
        if (data.ok) { workerOk = true; break; }
      } catch (e) { /* try next */ }
    }
    // Bug C: worker 全失败时提示一次 (BOS 兜底仍会写)
    if (!workerOk && !bosWarned) {
      console.warn("  (警告: Celld worker 写入失败, 仅 BOS 持久化)");
      bosWarned = true;
    }
  })();

  // 2. BOS 直写 (异步队列, 完整 msg + 完整记忆 — 方案 A)
  queueBosWrite(sessionId, seq, role, msg, { fullContent, fullToolResults });

  return { worker: "async", bos: "queued" };
}

function extractText(content) {
  if (typeof content === "string") return content;
  if (Array.isArray(content)) {
    return content.filter(b => b.type === "text").map(b => b.text).join(" ").trim();
  }
  return "";
}

// ---- 从 BOS 读历史 (权威源, 重启后恢复) ----
async function loadHistoryFromBos(sessionId) {
  // P0: 恢复读路径 — 优先 worker 缓存 (快, ~100ms), miss 回 BOS (权威, ~1.3s)
  // 让 Celld worker 从"只写不读"变为真正的快路径
  try {
    // 1. 先试 worker 缓存 (任一节点)
    for (const base of CELD_NODES) {
      try {
        const url = `${base}/agent/celagent?action=resume&session=${encodeURIComponent(sessionId)}`;
        const resp = await fetch(url, { signal: AbortSignal.timeout(1500) });
        const data = await resp.json();
        if (data.ok && data.session && data.session.turns && data.session.turns.length > 0) {
          return data.session.turns;  // 快路径: worker 缓存命中
        }
        break;  // 节点活着但无此会话 → 直接回 BOS
      } catch (e) { /* try next node */ }
    }
    // 2. worker 全 miss → BOS 权威源
    const { bosGet } = await import("../src/bos.js");
    const cfgFile = join(homedir(), ".config", "celagent", "settings.json");
    if (!existsSync(cfgFile)) return null;
    const cfg = JSON.parse(readFileSync(cfgFile, "utf8"));
    const bucket = cfg.persistence?.bucket;
    const endpoint = cfg.persistence?.endpoint;
    if (!bucket) return null;
    const existing = await bosGet(`sessions/${sessionId}.json`, { bucket, endpoint });
    if (existing.ok) {
      try {
        const session = JSON.parse(existing.body);
        return session.turns || [];
      } catch (e) {
        // 损坏的 JSON: 提示但不崩溃 (Bug 38)
        console.warn(`  (警告: 会话 ${sessionId} 的 BOS 历史数据损坏, 跳过恢复)`);
        return null;
      }
    }
  } catch (e) { /* 忽略 */ }
  return null;
}

async function listSessions() {
  // 从 BOS 列出所有会话 (sessions/<id>.json), 显示 id/轮数/更新时间
  // 降级链 (Bug 65): settings.json 丢失时自动发现账号下所有含会话的 bucket,
  // 保证“本地数据全丢, 只要凭证还在”仍能找回会话
  try {
    const { execFile } = await import("node:child_process");
    const cfgFile = join(homedir(), ".config", "celagent", "settings.json");
    let bucket = null;
    let endpoint = "https://s3.bj.bcebos.com";
    // 1) 命令行显式指定 (最高优先): celagent list --bucket <name>
    const argvIdx = process.argv.indexOf("--bucket");
    if (argvIdx > 0 && process.argv[argvIdx + 1]) {
      bucket = process.argv[argvIdx + 1];
    } else if (existsSync(cfgFile)) {
      // 2) settings.json (正常路径)
      try {
        const cfg = JSON.parse(readFileSync(cfgFile, "utf8"));
        bucket = cfg.persistence?.bucket || null;
        endpoint = cfg.persistence?.endpoint || endpoint;
      } catch (e) { /* 损坏则走降级 */ }
    }
    const runAws = (args) => new Promise((resolve) => {
      execFile("aws", args, { env: { ...process.env, AWS_PROFILE: "bos" }, timeout: 20000, encoding: "utf8" }, (err, stdout) => {
        try { resolve(JSON.parse(stdout || "[]")); }
        catch (e) { resolve([]); }
      });
    });
    // 3) 降级: 扫描账号下所有 bucket, 找含 sessions/ 会话的 (Bug 65)
    // 并发扫描 (Bug 83): bucket 多时串行扫描慢 (每个 ~200ms), 并发限 6
    if (!bucket) {
      console.log("(未找到 settings.json 配置, 自动扫描账号下所有 bucket...)");
      const buckets = await runAws(["s3api", "list-buckets", "--query", "Buckets[].Name", "--output", "json"]);
      const all = Array.isArray(buckets) ? buckets : [];
      const candidates = [];
      const worker = async (b) => {
        const hit = await runAws(["s3api", "list-objects-v2", "--bucket", b, "--prefix", "sessions/", "--max-items", "1", "--endpoint-url", endpoint, "--query", "Contents[].Key", "--output", "json"]);
        if (Array.isArray(hit) && hit.length > 0) candidates.push(b);
      };
      for (let i = 0; i < all.length; i += 6) {
        await Promise.all(all.slice(i, i + 6).map(worker));
      }
      if (candidates.length === 0) { console.log("(账号下没有找到含会话的 bucket)"); return; }
      if (candidates.length === 1) { bucket = candidates[0]; }
      else {
        console.log(`找到多个含会话的 bucket, 请指定: celagent list --bucket <name>`);
        for (const c of candidates) console.log(`  - ${c}`);
        return;
      }
    }
    const list = await runAws(["s3api", "list-objects-v2", "--bucket", bucket, "--prefix", "sessions/", "--endpoint-url", endpoint, "--query", "Contents[].{k:Key,s:Size,l:LastModified}", "--output", "json"]);
    const sessions = (Array.isArray(list) ? list : [])
      .filter(i => i.k?.endsWith(".json") && !/\/verify\/|bugtest-|stress-|takeover-|async-/.test(i.k) && !/-(test|verify|check|fix|tmp|temp)$/i.test(i.k.replace("sessions/", "").replace(/\.json$/, "")) && !/^(bug\d|cred-|iso-|race|dup-|ifmatch|queue-|nooverwrite|conc-|degrade|direct|corrupt|long-msg|bos-|aws-|default|debug|seq-|switch-|final-|full-|restore-e2e)/.test(i.k.replace("sessions/", "").replace(/\.json$/, "")))
      .map(i => ({ id: i.k.replace("sessions/", "").replace(/\.json$/, ""), size: i.s || 0, modified: (i.l || "").slice(0, 16) }))
      .sort((a, b) => b.modified.localeCompare(a.modified));
    if (sessions.length === 0) { console.log("(BOS 暂无会话)"); return; }
    console.log(`celagent — BOS 会话列表 (${sessions.length} 个, bucket=${bucket})\n`);
    console.log("  ID (用于 celagent <id> 续写)                                           大小    更新");
    console.log("  " + "-".repeat(95));
    for (const s of sessions) {
      const id = s.id.length > 52 ? s.id.slice(0, 49) + "..." : s.id;
      console.log(`  ${id.padEnd(52)} ${String(s.size).padStart(8)}B  ${s.modified}`);
    }
    console.log("\n  续写: celagent <id>    新会话: celagent (不带参数)");
  } catch (e) {
    console.error(`列表失败: ${e.message}`);
  }
}

// ---- 版本/帮助 ----
const CELAGENT_VERSION = "0.3.0";
function printVersion() {
  console.log(`celagent v${CELAGENT_VERSION} — Pi TUI + Celld/BOS RPO=0 持久化`);
}
function printHelp() {
  printVersion();
  console.log(`
用法:
  celagent                     启动 TUI (自动生成唯一会话 ID)
  celagent <id>                续写指定会话 (从 BOS 恢复历史)
  celagent list [--bucket B]   列出 BOS 里所有可恢复会话 (settings 丢失时自动扫描 bucket)
  celagent export <id> [--bucket B]  导出会话到 JSON (stdout)
  celagent rm <id> [--bucket B]      删除 BOS 里的会话 (需确认)
  celagent config get <key>   读取配置 (如 persistence.bucket)
  celagent config set <key> <value>  写入配置 (如 model deepseek-v4-flash)
  celagent doctor             自检: 配置/凭证/节点/BOS 连通性
  celagent task submit <type> [steps]  提交分布式任务 (celld 状态机)
  celagent task status [taskId]        任务状态 (断点续跑)
  celagent task ledger                 幂等 ledger (exactly-once)
  celagent version            显示版本
  celagent help               显示帮助

示例:
  celagent list
  celagent sess-demo-secondary
  celagent export sess-demo-secondary > backup.json
`);
}

// ---- 配置管理 (config get/set) ----
function configFile() { return join(homedir(), ".config", "celagent", "settings.json"); }
function loadConfig() {
  const f = configFile();
  if (!existsSync(f)) return {};
  try { return JSON.parse(readFileSync(f, "utf8")); } catch (e) { return {}; }
}
function saveConfig(cfg) {
  const { mkdirSync, writeFileSync } = require("node:fs");
  mkdirSync(join(homedir(), ".config", "celagent"), { recursive: true });
  writeFileSync(configFile(), JSON.stringify(cfg, null, 2) + "\n", "utf8");
}
async function configCommand(args) {
  const [op, key, value] = args;
  const cfg = loadConfig();
  if (op === "get") {
    if (!key) { console.log(JSON.stringify(cfg, null, 2)); return; }
    const v = key.split(".").reduce((o, k) => o?.[k], cfg);
    console.log(v === undefined ? "(未设置)" : typeof v === "object" ? JSON.stringify(v) : String(v));
  } else if (op === "set") {
    if (!key || value === undefined) { console.error("用法: celagent config set <key> <value>"); process.exit(1); }
    const parts = key.split(".");
    let o = cfg;
    for (let i = 0; i < parts.length - 1; i++) { o[parts[i]] ??= {}; o = o[parts[i]]; }
    if (value === "" || value === "null") {
      // 空值 = 删除 key (避免残留空配置)
      delete o[parts[parts.length - 1]];
      saveConfig(cfg);
      console.log(`✓ 已删除配置项 ${key}`);
    } else {
      o[parts[parts.length - 1]] = value;
      saveConfig(cfg);
      // Bug 79: model/provider 必须同步到 pi-runtime 配置 — TUI 实际模型由
      // pi-runtime/settings.json (defaultModel) + models.json 决定,
      // 只写 celagent settings.json 会让 `config set model` 静默不生效
      if (key === "model" || key === "provider") {
        const piSettingsFile = join(homedir(), ".config", "celagent", "pi-runtime", "settings.json");
        if (existsSync(piSettingsFile)) {
          try {
            const piSettings = JSON.parse(readFileSync(piSettingsFile, "utf8"));
            if (key === "model") piSettings.defaultModel = value;
            if (key === "provider") piSettings.defaultProvider = value;
            const { writeFileSync } = require("node:fs");
            writeFileSync(piSettingsFile, JSON.stringify(piSettings, null, 2) + "\n", "utf8");
            console.log(`  ↳ 已同步 pi-runtime 配置 (${piSettingsFile})`);
          } catch (e) {
            console.warn(`  (警告: pi-runtime 配置同步失败: ${e.message})`);
          }
        }
      }
      console.log(`✓ 已设置 ${key} = ${value} (${configFile()})`);
    }
  } else {
    console.error("用法: celagent config get [key] | config set <key> <value>");
    process.exit(1);
  }
}

// ---- doctor 自检 ----
async function doctorCommand() {
  console.log("celagent doctor — 自检\n");
  let ok = true;
  // 0. pi-runtime 前置依赖 (Bug 89: models.json/auth.json/settings.json 是 TUI 启动前提,
  //    缺失/损坏时 TUI 直接崩, 但旧 doctor 不检查 → 误报"全部正常")
  const piDir = join(homedir(), ".config", "celagent", "pi-runtime");
  const piFiles = ["settings.json", "models.json", "auth.json"];
  const piStates = piFiles.map(f => {
    const p = join(piDir, f);
    if (!existsSync(p)) return { f, state: "缺缺失" };
    try { JSON.parse(readFileSync(p, "utf8")); return { f, state: "✓" }; }
    catch (e) { return { f, state: "损坏" }; }
  });
  const piOk = piStates.every(s => s.state === "✓");
  console.log(`[0/5] pi-runtime: ${piStates.map(s => `${s.f}${s.state === "✓" ? "" : " " + s.state}`).join(", ")} ${piOk ? "" : "✗ (TUI 无法启动!)"}`);
  if (!piOk) ok = false;
  // 1. 配置
  const cfg = loadConfig();
  const bucket = cfg.persistence?.bucket;
  console.log(`[1/5] 配置: ${bucket ? "✓ bucket=" + bucket : "✗ 缺 persistence.bucket (运行 setup.sh 或 config set)"}`);
  if (!bucket) ok = false;
  // 2. BOS 凭证
  const { execFile } = await import("node:child_process");
  const cred = await new Promise((resolve) => {
    execFile("aws", ["configure", "get", "aws_access_key_id", "--profile", "bos"], { timeout: 10000, encoding: "utf8" }, (err, stdout) => resolve(err ? null : (stdout || "").trim()));
  });
  const hasEnv = process.env.AWS_ACCESS_KEY_ID && process.env.AWS_SECRET_ACCESS_KEY;
  console.log(`[2/5] 凭证: ${(cred || hasEnv) ? "✓ [bos] profile" + (hasEnv ? " (env)" : "") : "✗ 无凭证 (需 ~/.aws/credentials [bos] 或环境变量)"}`);
  if (!cred && !hasEnv) ok = false;
  // 3. Celld 节点
  const nodes = [];
  for (const base of CELD_NODES) {
    try {
      const r = await fetch(`${base}/__celld/health`, { signal: AbortSignal.timeout(2000) });
      if (r.ok) nodes.push(base);
    } catch (e) { /* down */ }
  }
  console.log(`[3/5] Celld 节点: ${nodes.length > 0 ? "✓ " + nodes.join(", ") : "✗ 全部离线 (celagent 会自动拉起, 或 node_mgr.sh start)"}`);
  // 4. BOS 连通
  if (bucket) {
    const probe = await new Promise((resolve) => {
      execFile("aws", ["s3api", "list-objects-v2", "--bucket", bucket, "--prefix", "sessions/", "--max-items", "1", "--endpoint-url", cfg.persistence?.endpoint || "https://s3.bj.bcebos.com", "--query", "Contents[].Key", "--output", "json"], { env: { ...process.env, AWS_PROFILE: "bos" }, timeout: 15000, encoding: "utf8" }, (err, stdout) => resolve(!err));
    });
    console.log(`[4/5] BOS 连通: ${probe ? "✓ 可读写 bucket=" + bucket : "✗ 访问失败 (检查凭证/endpoint/网络)"}`);
    if (!probe) ok = false;
  }
  console.log(`\n结论: ${ok ? "✓ 全部正常" : "✗ 存在异常, 按上面 ✗ 项处理"}`);
  process.exit(ok ? 0 : 1);
}

// ---- 导出/删除会话 ----
async function getBucketArg() {
  // 返回 { bucket, endpoint } — 显式 --bucket > settings.json > 自动扫描 (复用 listSessions 逻辑简化版)
  const argvIdx = process.argv.indexOf("--bucket");
  if (argvIdx > 0 && process.argv[argvIdx + 1]) {
    const cfg = loadConfig();
    return { bucket: process.argv[argvIdx + 1], endpoint: cfg.persistence?.endpoint || "https://s3.bj.bcebos.com" };
  }
  const cfg = loadConfig();
  if (cfg.persistence?.bucket) return { bucket: cfg.persistence.bucket, endpoint: cfg.persistence.endpoint || "https://s3.bj.bcebos.com" };
  return { bucket: null, endpoint: "https://s3.bj.bcebos.com" };
}
// ---- P1: agent 任务化 — 任务状态机 (celld submit/status/ledger) ----
async function taskCommand(args) {
  const [op, ...rest] = args;
  // celagent task submit <type> [steps]  |  task status [taskId]  |  task ledger
  if (op === "submit") {
    const type = rest[0] || "short";
    const steps = rest[1] || (type === "long" ? "15" : "3");
    let lastErr = null;
    for (const base of CELD_NODES) {
      try {
        const r = await fetch(`${base}/agent/celagent?action=submit&type=${type}&steps=${steps}`, { signal: AbortSignal.timeout(3000) });
        const j = await r.json();
        if (j.taskId) {
          console.log(`✓ 任务已提交: ${j.taskId} (type=${j.type || type}, steps=${j.steps})`);
          console.log(`  查看: celagent task status ${j.taskId}`);
          return;
        }
        lastErr = JSON.stringify(j);
      } catch (e) { lastErr = e.message; }
    }
    console.error(`✗ 任务提交失败: ${lastErr}`);
    process.exit(1);
  }
  if (op === "status") {
    const taskId = rest[0] || "";
    const q = taskId ? `&task=${encodeURIComponent(taskId)}` : "";
    let lastErr = null;
    for (const base of CELD_NODES) {
      try {
        const r = await fetch(`${base}/agent/celagent?action=status${q}`, { signal: AbortSignal.timeout(3000) });
        const j = await r.json();
        if (Array.isArray(j)) {
          console.log(`任务列表 (${j.length}):`);
          for (const t of j) console.log(`  ${t.id} [${t.status}] step ${t.step}/${t.steps} ${t.type}`);
        } else {
          console.log(JSON.stringify(j, null, 1));
        }
        return;
      } catch (e) { lastErr = e.message; }
    }
    console.error(`✗ 查询失败: ${lastErr}`);
    process.exit(1);
  }
  if (op === "ledger") {
    // 幂等 ledger — exactly-once 验证
    let lastErr = null;
    for (const base of CELD_NODES) {
      try {
        const r = await fetch(`${base}/agent/celagent?action=ledger`, { signal: AbortSignal.timeout(3000) });
        const j = await r.json();
        if (Array.isArray(j)) {
          console.log(`execution ledger (${j.length} 条):`);
          const dedup = j.filter(e => e.deduped).length;
          const byTool = {};
          for (const e of j) byTool[e.tool] = (byTool[e.tool] || 0) + 1;
          console.log(`  工具调用: ${JSON.stringify(byTool)}`);
          console.log(`  去重命中: ${dedup} 次 (exactly-once 保护)`);
          return;
        }
        lastErr = JSON.stringify(j);
      } catch (e) { lastErr = e.message; }
    }
    console.error(`✗ 查询失败: ${lastErr}`);
    process.exit(1);
  }
  console.error(`用法: celagent task submit <short|long> [steps] | task status [taskId] | task ledger`);
  process.exit(1);
}

async function exportCommand(id) {
  if (!id || id.startsWith("-")) { console.error("用法: celagent export <会话ID> [--bucket B] (ID 可用 celagent list 查看)"); process.exit(1); }
  const { bucket, endpoint } = await getBucketArg();
  if (!bucket) { console.error("✗ 未找到 bucket (用 --bucket 指定)"); process.exit(1); }
  const { bosGet } = await import("../src/bos.js");
  const r = await bosGet(`sessions/${id}.json`, { bucket, endpoint });
  if (!r.ok) { console.error(`✗ 会话不存在或读取失败: ${r.error}`); process.exit(1); }
  const session = JSON.parse(r.body);
  console.log(JSON.stringify({ id, exportedAt: new Date().toISOString(), turns: session.turns || [] }, null, 2));
}
async function rmCommand(id) {
  if (!id || id.startsWith("-")) { console.error("用法: celagent rm <会话ID> [--bucket B] (ID 可用 celagent list 查看)"); process.exit(1); }
  const { bucket, endpoint } = await getBucketArg();
  if (!bucket) { console.error("✗ 未找到 bucket (用 --bucket 指定)"); process.exit(1); }
  const { execFile } = await import("node:child_process");
  const confirm = await new Promise((resolve) => {
    const readline = require("node:readline").createInterface({ input: process.stdin, output: process.stdout });
    readline.question(`确定删除会话 "${id}" (BOS 永久删除, 不可恢复)? [y/N] `, (a) => { readline.close(); resolve(/^y/i.test(a.trim())); });
  });
  if (!confirm) { console.log("已取消"); return; }
  await new Promise((resolve) => {
    execFile("aws", ["s3api", "delete-object", "--bucket", bucket, "--key", `sessions/${id}.json`, "--endpoint-url", endpoint], { env: { ...process.env, AWS_PROFILE: "bos" }, timeout: 15000 }, (err) => resolve());
  });
  console.log(`✓ 已删除会话 ${id}`);
}

async function main() {
  const cwd = process.cwd();
  const cmd = process.argv[2];
  // 非交互子命令
  if (cmd === "list" || cmd === "--list") { await listSessions(); return; }
  if (cmd === "help" || cmd === "--help" || cmd === "-h") { printHelp(); return; }
  if (cmd === "version" || cmd === "--version" || cmd === "-v") { printVersion(); return; }
  if (cmd === "config") { await configCommand(process.argv.slice(3)); return; }
  if (cmd === "doctor") { await doctorCommand(); return; }
  if (cmd === "export") { await exportCommand(process.argv[3]); return; }
  if (cmd === "rm") { await rmCommand(process.argv[3]); return; }
  if (cmd === "task") { await taskCommand(process.argv.slice(3)); return; }
  // Bug 80: 未知的 - 开头参数 (拼错的 --xxx) 不应被静默当 sessionId 进 TUI
  if (cmd && cmd.startsWith("-")) {
    console.error(`未知选项: ${cmd} (用 celagent help 查看用法)`);
    process.exit(1);
  }
  // Bug 46: 无参数时不再共用 "default" — 生成唯一会话名 (时间戳+随机),
  // 避免多实例/多用户写入同一 key 造成串扰覆盖
  // 显式传 sessionId 时保留原名 (用户明确想续写该会话)
  const sessionId = process.argv[2] || `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  console.log(`celagent — Pi TUI, 会话 ${sessionId}, Celld RPO=0 持久化\n`);

  // 0. Bug 87: celagent settings.json 的 provider/model 是单源配置 —
  // 启动时同步到 pi-runtime (手改 celagent settings 也生效, 不再有死配置)
  try {
    const cfg = loadConfig();
    const piSettingsFile = join(homedir(), ".config", "celagent", "pi-runtime", "settings.json");
    if (cfg.model && existsSync(piSettingsFile)) {
      const piSettings = JSON.parse(readFileSync(piSettingsFile, "utf8"));
      let changed = false;
      if (cfg.model && piSettings.defaultModel !== cfg.model) { piSettings.defaultModel = cfg.model; changed = true; }
      if (cfg.provider && piSettings.defaultProvider !== cfg.provider) { piSettings.defaultProvider = cfg.provider; changed = true; }
      if (changed) {
        const { writeFileSync } = require("node:fs");
        writeFileSync(piSettingsFile, JSON.stringify(piSettings, null, 2) + "\n", "utf8");
        console.log(`  (已同步模型配置: ${cfg.provider || "deepseek"}/${cfg.model})`);
      }
    }
  } catch (e) { /* 同步失败不阻塞 */ }

  // 0. 确保 Celld 节点在跑 (自动启动)
  await ensureCelld();

  // 0.5 从 BOS 恢复历史 (权威源, 重启不丢) — 只读一次, 传给会话注入用
  const savedHistory = await loadHistoryFromBos(sessionId);
  if (savedHistory && savedHistory.length > 0) {
    console.log(`  (已从 BOS 恢复 ${savedHistory.length} 轮历史)`);
  }

  // P0: 冷启动对齐 — 把 BOS 权威状态同步到 worker 缓存 (sync),
  // 使 worker 与 BOS 一致 (节点迁移/重启后, 后续读 worker = 完整历史)
  if (savedHistory && savedHistory.length > 0) {
    void (async () => {
      for (const base of CELD_NODES) {
        try {
          await fetch(`${base}/agent/celagent?action=sync&session=${encodeURIComponent(sessionId)}`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ turns: savedHistory }),
            signal: AbortSignal.timeout(2000),
          });
          break;
        } catch (e) { /* try next */ }
      }
    })();
  }

  // 1. 组装 services (独立 agentDir)
  let services;
  try {
    const settingsManager = pi.SettingsManager.create(cwd, AGENT_DIR, { projectTrusted: true });
    services = await pi.createAgentSessionServices({
      cwd,
      agentDir: AGENT_DIR,
      settingsManager,
      modelRuntimeSignal: AbortSignal.timeout(15000),
    });
    console.log("✓ services 就绪");
  } catch (e) {
    console.error(`services 失败: ${e.message}`);
    process.exit(1);
  }

  // 2. 创建会话 (含 Celld 镜像钩子)
  let session, runtime;
  try {
    // Bug 修复: SessionManager 用独立会话目录, 绝不碰 ~/.pi/agent/sessions (本机 Pi 数据)
    const sessionDir = join(AGENT_DIR, "sessions", encodeURIComponent(cwd.replace(/\//g, "-")));
    const sessionManager = pi.SessionManager.create(cwd, sessionDir);
    const createRuntime = async (opts = {}) => {
      // Bug: 接受 pi 传入的 sessionManager/sessionStartEvent (newSession/resume 会传)
      // 并返回完整契约: {session, services, diagnostics, ...} 防止 /new 崩溃退出
      const effSessionManager = opts?.sessionManager || sessionManager;
      const effStartEvent = opts?.sessionStartEvent || { type: "session_start", reason: "startup" };
      // P1 记忆增强: 注入 BOS 记忆工具 (history_search / session_snapshot)
      const { history_search, session_snapshot } = await import("../src/bos-tools.js");
      const result = await pi.createAgentSessionFromServices({
        cwd, agentDir: AGENT_DIR, services,
        sessionManager: effSessionManager,
        sessionStartEvent: effStartEvent,
        // P1: agent 可主动检索历史记忆 / 打显式快照
        customTools: [history_search, session_snapshot],
      });
      // 恢复历史注入: 仅首次启动(startup)注入 argv 会话的历史
      // 注: /resume /new /fork 不注入 — resume 可能是别的会话(无法匹配),
      //     new/fork 是新上下文 (Bug 修复)
      const startReason = effStartEvent?.reason;
      if (startReason === "startup" && savedHistory && savedHistory.length > 0) {
        try {
          // Bug 78: 注入历史有长度上限 — 超长会话 (几百轮) 全量拼进一条 steer
          // 会直接撑爆模型上下文窗口。只注入最近 MAX_INJECT_TURNS 轮 + 提示省略。
          const MAX_INJECT_TURNS = 50;
          const recent = savedHistory.slice(-MAX_INJECT_TURNS);
          const omitted = savedHistory.length - recent.length;
          // 合并为一条清晰的历史上下文, 明确标注是之前的 assistant 回复
          // (避免逐条 steer 成 user 消息误导 LLM — Bug 24)
          const historySummary = recent
            .map(t => `[第${t.turn}轮(assistant)] ${t.msg}`)
            .join("\n");
          result.session.steer(
            `以下是本会话之前的对话历史(均为 assistant 的回复内容, 请以此作为继续对话的上下文, 不要重复回答这些内容):\n${historySummary}` +
            (omitted > 0 ? `\n\n(注: 较早的 ${omitted} 轮历史已省略, 完整历史在 BOS 中)` : "")
          );
        } catch (e) { /* 注入失败不阻塞 */ }
      }
      // 挂 Celld 镜像钩子
      // 持久化 id: startup 用 argv sessionId (续写原会话); /new 生成独立 key (Bug 47 修复)
      let persistId = sessionId;
      let persistHistory = savedHistory;
      if (startReason === "new") {
        // /new: 新会话独立存储, 不混入旧历史
        persistId = `sess-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
        persistHistory = [];
        // 提示用户新会话的持久化 ID (下次可 `celagent <id>` 续写)
        console.log(`  ↳ 新会话持久化 ID: ${persistId} (下次: celagent ${persistId})`);
      } else if (startReason === "resume" && effSessionManager?.getSessionFile) {
        // Bug 64: /resume 切到别的本地会话后, persistId 必须跟着切换 —
        // 否则被恢复会话的续写会串写到 argv sessionId 的 key 下 (数据串写污染)
        const resumedFile = effSessionManager.getSessionFile();
        if (resumedFile) {
          persistId = `sess-${basename(resumedFile).replace(/\.jsonl$/, "")}`;
          // 从 BOS 读该会话的镜像历史 (若有) — 决定 seq 续写起点, 避免二次 resume 覆盖旧轮
          try {
            const resumedHistory = await loadHistoryFromBos(persistId);
            persistHistory = (resumedHistory && resumedHistory.length) ? resumedHistory : [];
          } catch (e) { persistHistory = []; }
          console.log(`  ↳ 已恢复本地会话, 持久化 ID: ${persistId} (续写 ${persistHistory.length} 轮)`);
        }
      }
      // seq 从当前会话历史长度继续 (resume 续写; new 从 0)
      let seq = (persistHistory && persistHistory.length) || 0;
      // P1: 维护当前会话 turns 快照缓存 (供 session_snapshot 工具保存用)
      let snapshotTurns = (persistHistory || []).map(t => ({ turn: t.turn, role: t.role || "assistant", msg: t.msg, ts: t.ts }));
      globalThis.__celagentSnapshotTurns = () => snapshotTurns;
      result.session.subscribe(async (event) => {
        if (event?.type === "turn_end") {
          seq++;
          const text = extractText(event.message?.content);
          // 完整记忆 (方案 A): 完整 message 内容块 (text/thinking/toolCall 全量)
          // + 完整 toolResults (含文本结果), 存 BOS 权威源
          const fullContent = Array.isArray(event.message?.content) ? event.message.content : [];
          const fullToolResults = (event.toolResults || []).map(tr => ({
            toolName: tr.toolName,
            content: Array.isArray(tr.content) ? tr.content : null,
          }));
          // 摘要 msg (兼容: worker 缓存 URL 截断 + 恢复注入用, 不占大体积)
          const toolCalls = fullContent.filter(b => b.type === "toolCall").map(b => `${b.name}(${JSON.stringify(b.arguments)})`);
          const toolResults = fullToolResults.map(tr => {
            const resultText = Array.isArray(tr.content)
              ? tr.content.filter(b => b.type === "text").map(b => b.text).join(" ").trim()
              : "";
            return `${tr.toolName}: ${(resultText || "(无文本结果)").slice(0, 120)}`;
          });
          let msg = text || "(无文本)";
          if (toolCalls.length > 0) msg += ` [工具调用: ${toolCalls.join(", ").slice(0, 100)}]`;
          if (toolResults.length > 0) msg += ` [工具结果: ${toolResults.join(" | ").slice(0, 300)}]`;
          // Bug 52: 不 await — checkpoint 全异步 (worker fire-and-forget + BOS 队列), 绝不阻塞对话
          void celldCheckpoint(persistId, seq, "assistant", msg, { fullContent, fullToolResults });
          // P1: 同步快照缓存
          snapshotTurns.push({ turn: seq, role: "assistant", msg, ts: Date.now() });
        }
      });
      return { ...result, services, diagnostics: [] };  // 完整契约 (Bug: /new 需 services+diagnostics)
    };
    runtime = await pi.createAgentSessionRuntime(createRuntime, {
      cwd,
      agentDir: AGENT_DIR,
      sessionManager,
      sessionStartEvent: { type: "session_start", reason: "startup" },
    });
    session = runtime.session;
    console.log("✓ 会话就绪\n");
  } catch (e) {
    console.error(`会话失败: ${e.message}`);
    process.exit(1);
  }

  // 3. 启动完整 TUI
  const interactive = new pi.InteractiveMode(runtime, {
    verbose: true,
    tuiMode: undefined,
  });
  await interactive.init();
  await interactive.run();

  // 4. 退出前 flush BOS 队列 (Bug 17 修复: 避免丢最后几轮)
  try {
    await bosQueue;
  } catch (e) { /* 忽略 */ }
}

// 退出时 flush 队列 — Bug 48/59: 信号处理策略
// pi 的 InteractiveMode 用 prependListener 注册 SIGTERM 优雅关闭 (dispose → 恢复终端 → exit(0)),
// 我们不能直接 process.exit 抢占它 (会导致终端未恢复), 也不能不注册 (注册 handler 会取消
// 信号的默认终止行为, 若 pi 尚未接管则进程永久挂死 — Bug 60 实测)。
// 方案: 收到信号 → 启动 flush + 启动一个 unref 兜底退出定时器。
//   - pi 正常接管时, 其 process.exit(0) 先发生, 定时器随进程退出消失 (unref 不阻塞);
//   - pi 未接管 (TUI 未初始化/失败) 时, 定时器 (12s > flush 10s 上限) 触发兜底退出, 不挂死。
// 正常退出路径 (run() 返回) 的 flush 在 main() 中 await bosQueue, 这里仅信号兜底。
let flushedOnExit = false;
async function flushOnExit() {
  if (flushedOnExit) return;
  flushedOnExit = true;
  // 超时保护: 队列最多等 10s (BOS 写失败不阻塞退出)
  await Promise.race([bosQueue, new Promise(r => setTimeout(r, 10000))]).catch(() => {});
}
function scheduleSignalExit(code) {
  // unref: 若 pi 的 process.exit 先执行, 此定时器不会阻止进程退出
  setTimeout(() => process.exit(code), 12000).unref();
}
process.on("SIGINT", () => { scheduleSignalExit(130); void flushOnExit(); });
process.on("SIGTERM", () => { scheduleSignalExit(143); void flushOnExit(); });

main().catch(e => { console.error(`错误: ${e.message}`); process.exit(1); });
