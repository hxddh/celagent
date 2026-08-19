// review-logic-proofs.test.mjs — P0 正确性回归 (修复后语义)
// 不依赖 celld/BOS/settings; 固化队列/seq/fork/BOS-first/parse/steer
// 运行: node --test tests/review-logic-proofs.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const tui = readFileSync(join(root, "bin/celagent-tui.mjs"), "utf8");
const persist = readFileSync(join(root, "src/persist.js"), "utf8");

// ---- 队列超限丢最旧、保留最新 ----
test("队列超限丢最旧保留最新", () => {
  const MAX = 50;
  const q = [];
  for (let op = 1; op <= 60; op++) {
    if (q.length >= MAX) q.shift();
    q.push(op);
  }
  assert.equal(q.length, 50);
  assert.equal(q[0], 11);
  assert.equal(q[q.length - 1], 60);
});

test("源码锚定: 队列超限 shift 最旧而非 return", () => {
  assert.match(persist, /if \(bosPending\.length >= BOS_QUEUE_MAX\)/);
  const start = persist.indexOf("if (bosPending.length >= BOS_QUEUE_MAX)");
  const block = persist.slice(start, start + 180);
  assert.match(block, /bosPending\.shift\(\)/);
  assert.doesNotMatch(block, /return;/);
  assert.match(block, /丢弃最旧任务/);
});

// ---- seq 用 max(turn) ----
test("seq 用 max(turn) 而非 length", () => {
  const turns = [{ turn: 1 }, { turn: 2 }, { turn: 5 }];
  const fromLength = turns.length;
  const nums = turns.map((t) => Number(t.turn)).filter((n) => Number.isFinite(n));
  const fromMax = nums.length ? Math.max(...nums) : 0;
  assert.equal(fromLength, 3);
  assert.equal(fromMax, 5);
  assert.notEqual(fromLength, fromMax);
});

test("源码锚定: seq = maxTurn(persistHistory)", () => {
  assert.match(tui, /let seq = maxTurn\(persistHistory\)/);
  assert.doesNotMatch(tui, /let seq = \(persistHistory && persistHistory\.length\) \|\| 0/);
});

// ---- 同 turn replace 保留已有 content ----
test("同 turn replace: 缺 content 的新写入不抹掉已有 content", () => {
  const session = {
    turns: [
      { turn: 1, msg: "A".repeat(500), content: [{ type: "text", text: "A".repeat(500) }] },
      { turn: 2, msg: "B".repeat(500), content: [{ type: "text", text: "B".repeat(500) }] },
    ],
  };
  const seq = 2;
  const fullContent = null;
  const entry = { turn: 2, msg: "x".repeat(200) };
  const idx = session.turns.findIndex((t) => t.turn === seq);
  const prev = session.turns[idx];
  if (prev?.content && !(fullContent && fullContent.length)) entry.content = prev.content;
  session.turns[idx] = entry;
  assert.equal(session.turns[1].msg.length, 200);
  assert.ok(Array.isArray(session.turns[1].content));
  assert.equal(session.turns[1].content[0].text.length, 500);
});

test("源码锚定: mergeTurn 保留已有 content/toolResults", () => {
  assert.match(persist, /if \(prev\?\.content && !\(fullContent && fullContent\.length\)\) entry\.content = prev\.content/);
  assert.match(persist, /if \(prev\?\.toolResults && !\(fullToolResults && fullToolResults\.length\)\) entry\.toolResults = prev\.toolResults/);
});

// ---- ensureLock finally 释放 ----
test("ensureLock: finally 在健康早退后释放", async () => {
  let ensureLock = null;
  async function ensureFixed() {
    if (ensureLock) return ensureLock;
    const run = async () => {
      try {
        await Promise.resolve(); // 模拟 await fetch 后再 return
        return;
      } finally {
        ensureLock = null;
      }
    };
    ensureLock = run();
    return ensureLock;
  }
  await ensureFixed();
  assert.equal(ensureLock, null);
});

test("源码锚定: ensureCelld 外层 finally 清 ensureLock", () => {
  const fn = tui.slice(tui.indexOf("async function ensureCelld"), tui.indexOf("// ---- Celld 镜像"));
  assert.match(fn, /} finally \{\s*ensureLock = null/);
  assert.match(fn, /releaseEnsureFileLock/);
});

// ---- /fork 独立 persistId ----
test("源码锚定: persistId 处理 new 与 fork", () => {
  assert.match(tui, /startReason === "new" \|\| startReason === "fork"/);
});

// ---- JSON parse 失败不覆盖 ----
test("源码锚定: BOS JSON parse 失败 return 不覆盖", () => {
  assert.match(persist, /JSON\.parse\(existing\.body\)[\s\S]{0,200}catch[\s\S]{0,250}return;/);
  assert.doesNotMatch(persist, /JSON\.parse\(existing\.body\).*catch[\s\S]{0,40}覆盖/);
});

// ---- steer 用 content + role ----
test("源码锚定: steer 注入用 t.content 与 t.role", () => {
  assert.match(tui, /Array\.isArray\(t\.content\)/);
  assert.match(tui, /t\.role \|\| "assistant"/);
  assert.match(tui, /fromContent \|\| t\.msg/);
});

// ---- BOS-first 恢复 ----
test("源码锚定: loadSessionHistory 仅 miss 才 fallbackResume", () => {
  const fn = persist.slice(persist.indexOf("export async function loadSessionHistory"));
  assert.match(fn, /kind !== "not-found"/);
  assert.match(fn, /transient: kind === "transient"/);
  assert.match(tui, /未回退 worker 缓存/);
  assert.match(tui, /fallbackResume: workerResumeTurns/);
});

test("源码锚定: 会话 ID 白名单", () => {
  assert.match(tui, /function assertSafeSessionId/);
  assert.match(tui, /A-Za-z0-9\._-\]\{1,128\}/);
});

test("sync 按 turn 合并: 不覆盖更新/更完整的 worker 轮", () => {
  function mergeTurns(existingTurns, incomingTurns) {
    const byTurn = new Map();
    for (const t of existingTurns || []) {
      const n = Number(t?.turn);
      if (Number.isFinite(n)) byTurn.set(n, t);
    }
    for (const t of incomingTurns || []) {
      const n = Number(t?.turn);
      if (!Number.isFinite(n)) continue;
      const prev = byTurn.get(n);
      if (!prev) { byTurn.set(n, t); continue; }
      const merged = { ...prev, ...t, turn: n };
      if (prev.content && !t.content) merged.content = prev.content;
      if (prev.toolResults && !t.toolResults) merged.toolResults = prev.toolResults;
      if ((t.msg || "").length < (prev.msg || "").length) merged.msg = prev.msg;
      byTurn.set(n, merged);
    }
    return [...byTurn.values()].sort((a, b) => Number(a.turn) - Number(b.turn));
  }
  const existing = [
    { turn: 1, msg: "old-short" },
    { turn: 2, msg: "worker-newer-and-long", content: [{ type: "text", text: "full" }] },
  ];
  const incoming = [
    { turn: 1, msg: "bos-longer-message" },
    { turn: 2, msg: "trunc" },
    { turn: 3, msg: "new" },
  ];
  const out = mergeTurns(existing, incoming);
  assert.equal(out.length, 3);
  assert.equal(out[0].msg, "bos-longer-message");
  assert.equal(out[1].msg, "worker-newer-and-long");
  assert.ok(Array.isArray(out[1].content));
  assert.equal(out[2].msg, "new");
});

const worker = readFileSync(join(root, "worker/src/index.js"), "utf8");

test("源码锚定: worker sync 调用 mergeTurns", () => {
  assert.match(worker, /function mergeTurns\(/);
  assert.match(worker, /const turns = mergeTurns\(existing\.turns, body\.turns\)/);
});

test("源码锚定: 持久化 user 轮 (message_end)", () => {
  assert.match(tui, /event\?\.type === "message_end" && event\.message\?\.role === "user"/);
  assert.match(tui, /celldCheckpoint\(persistId, seq, "user"/);
});

test("源码锚定: checkpoint 走 POST JSON 而非 URL msg", () => {
  assert.match(tui, /celldFetch\(base, "checkpoint"/);
  assert.doesNotMatch(tui, /action=checkpoint&session=.*&msg=/);
});

test("源码锚定: worker 先写 ledger pending 再 webhook", () => {
  const fn = worker.slice(worker.indexOf("async recordToolCall"), worker.indexOf("return entry;"));
  const pendingIdx = fn.indexOf("status: 'pending'");
  const webhookIdx = fn.indexOf("webhookCall");
  assert.ok(pendingIdx >= 0 && webhookIdx > pendingIdx, "pending 写入在 webhookCall 之前");
});

test("源码锚定: worker token 鉴权 + webhook 默认 loopback", () => {
  assert.match(worker, /function checkToken/);
  assert.match(worker, /CELAGENT_WORKER_TOKEN/);
  assert.match(worker, /function webhookBase/);
  assert.match(tui, /X-Celagent-Token/);
  assert.match(tui, /function ensureWorkerToken/);
  assert.match(worker, /targetStub\.fetch\(new Request/);
  assert.match(worker, /headers\['X-Celagent-Token'\]/);
});

test("源码锚定: rm 非 TTY 需要 --yes 且检查删除失败", () => {
  assert.match(tui, /非交互删除需要 --yes/);
  assert.match(tui, /process\.argv\.includes\("--yes"\)/);
  assert.match(tui, /删除失败/);
});

test("源码锚定: snapshot 全量从 BOS 重建, 进程内只留摘要", () => {
  assert.match(tui, /__celagentSnapshotTurns = async \(\) =>/);
  const start = tui.indexOf("__celagentSnapshotTurns = async");
  const block = tui.slice(start, start + 500);
  assert.match(block, /loadHistoryFromBos\(persistId\)/);
  // 内存缓存只 push 摘要对象, 不再驻留完整 content/toolResults
  assert.doesNotMatch(tui, /snapshotTurns\.push\(makeTurnEntry/);
  assert.match(tui, /snapshotTurns\.push\(\{ turn: seq, role: "user"/);
  assert.match(tui, /snapshotTurns\.push\(\{ turn: seq, role: "assistant"/);
});

test("resolveEndpoint fail-closed 且白名单扩合格 host", async () => {
  delete process.env.CELAGENT_ALLOW_ENDPOINT;
  const { resolveEndpoint, isAllowedEndpoint, defaultRegion, DEFAULT_ENDPOINT, bosPut } = await import("../src/bos.js");
  assert.equal(isAllowedEndpoint("https://s3.bj.bcebos.com"), true);
  assert.equal(isAllowedEndpoint("https://s3.gz.bcebos.com"), true);
  assert.equal(isAllowedEndpoint("https://s3.us-east-1.amazonaws.com"), true);
  assert.equal(isAllowedEndpoint("https://s3.amazonaws.com"), true);
  assert.equal(isAllowedEndpoint("https://abc.r2.cloudflarestorage.com"), true);
  assert.equal(isAllowedEndpoint("https://fly.storage.tigris.dev"), true);
  assert.equal(isAllowedEndpoint("https://t3.storage.dev"), true);
  assert.equal(isAllowedEndpoint("http://127.0.0.1:9000"), true);
  assert.equal(isAllowedEndpoint("http://[::1]:9000"), true);
  assert.equal(isAllowedEndpoint("http://[::1]"), true);
  assert.equal(isAllowedEndpoint("http://[::1]@evil.example"), false);
  assert.equal(isAllowedEndpoint("http://[::1].evil.example"), false);
  assert.equal(isAllowedEndpoint("https://s3.a.b.bcebos.com"), false);
  assert.equal(isAllowedEndpoint("https://s3.a.b.amazonaws.com"), false);
  assert.equal(isAllowedEndpoint("https://evil.example/"), false);
  assert.equal(isAllowedEndpoint("http://evil.example"), false);
  assert.equal(resolveEndpoint(), DEFAULT_ENDPOINT);
  assert.equal(resolveEndpoint(""), DEFAULT_ENDPOINT);
  assert.equal(resolveEndpoint("https://s3.gz.bcebos.com"), "https://s3.gz.bcebos.com");
  assert.throws(() => resolveEndpoint("https://evil.example"), (err) => err && err.code === "endpoint-not-allowed");
  assert.notEqual(resolveEndpoint("https://s3.gz.bcebos.com"), DEFAULT_ENDPOINT);
  assert.equal(defaultRegion("https://s3.bj.bcebos.com"), "bj");
  assert.equal(defaultRegion("https://abc.r2.cloudflarestorage.com"), undefined);
  const put = await bosPut("k", "x", { bucket: "b", endpoint: "https://evil.example" });
  assert.equal(put.ok, false);
  assert.equal(put.error, "endpoint-not-allowed");
});

test("源码锚定: list 不再隐藏 default/debug/bos-*", () => {
  assert.doesNotMatch(tui, /bos-\|aws-\|default\|debug/);
  assert.match(tui, /\[--scan\]/);
});

test("源码锚定: config set 拒绝把 persistence 写成标量", () => {
  assert.match(tui, /protectedObjs/);
  assert.match(tui, /是对象, 请用/);
  assert.match(tui, /isAllowedEndpoint\(value\)/);
});

test("源码锚定: own.json 清理受 bucket 前缀约束", () => {
  assert.match(tui, /CELAGENT_CLEAN_OWN/);
  assert.match(tui, /startsWith\("celagent-"\)/);
});

test("源码锚定: history_search 默认当前会话且读 snapshots/", () => {
  const tools = readFileSync(join(root, "src/bos-tools.js"), "utf8");
  assert.match(tools, /__celagentPersistId/);
  assert.match(tools, /rawSession === "\*"/);
  assert.match(tools, /sessions\/\$\{sessionFilter\}\.json/);
  assert.match(tools, /--max-items/);
  assert.match(tools, /snapshots\//);
  assert.match(tools, /persistenceFromCfg/);
  assert.match(tools, /endpoint-not-allowed/);
});

test("源码锚定: worker cwrite 锁 TTL + scheduleNextAlarm", () => {
  assert.match(worker, /LOCK_TTL_MS/);
  assert.match(worker, /async scheduleNextAlarm/);
  assert.match(worker, /await this\.scheduleNextAlarm\(\)/);
});

test("源码锚定: projectTrusted 默认 false", () => {
  assert.match(tui, /projectTrusted: false/);
});

test("源码锚定: ensureCelld 跨进程 ensure.lock", () => {
  assert.match(tui, /function tryEnsureFileLock/);
  assert.match(tui, /ensure\.lock/);
  assert.match(tui, /openSync\(lockPath, "wx"\)/);
  assert.match(tui, /releaseEnsureFileLock/);
  const fn = tui.slice(tui.indexOf("async function ensureCelld"), tui.indexOf("// ---- Celld 镜像"));
  assert.match(fn, /releaseEnsureFileLock\(fileLockDir\)/);
});

test("源码锚定: CI syntax check 失败即失败且覆盖 worker", () => {
  const ci = readFileSync(join(root, ".github/workflows/ci.yml"), "utf8");
  assert.match(ci, /set -euo pipefail/);
  assert.match(ci, /SYNTAX FAIL/);
  assert.match(ci, /worker\/src\/\*\.js/);
  assert.match(ci, /exit "\$fail"/);
});

test("源码锚定: failover 测试 resume 原会话且传 token", () => {
  const sh = readFileSync(join(root, "scripts/celld-bos-test.sh"), "utf8");
  assert.match(sh, /action=resume&session=\$SID/);
  assert.doesNotMatch(sh, /failover-check-\$/);
  assert.match(sh, /CELAGENT_WORKER_TOKEN=/);
  assert.match(sh, /Content-Type: application\/json/);
  assert.doesNotMatch(sh, /action=checkpoint&session=.*&msg=/);
});

test("源码锚定: cluster_mgr 传入 worker token", () => {
  const sh = readFileSync(join(root, "scripts/cluster_mgr.sh"), "utf8");
  assert.match(sh, /CELAGENT_WORKER_TOKEN=/);
});

test("源码锚定: celld v0.2 双监听 spawn", () => {
  assert.match(tui, /--internal-listen/);
  assert.match(tui, /internalPort = port \+ 2/);
  assert.match(tui, /CELLD_IDLE_EVICT_S: "30"/);
  assert.doesNotMatch(tui, /--advertise", `127\.0\.0\.1:\$\{port\}`/);
  for (const rel of [
    "scripts/node_mgr.sh",
    "scripts/cluster_mgr.sh",
    "setup.sh",
    "install.sh",
    "scripts/celld-bos-test.sh",
  ]) {
    const sh = readFileSync(join(root, rel), "utf8");
    assert.match(sh, /--internal-listen/, `${rel} 必须 --internal-listen`);
    assert.match(sh, /port \+ 2|18092/, `${rel} 内部口应为 public+2`);
  }
});

test("源码锚定: HANDOFF 不再把认证当唯一阻塞", () => {
  const handoff = readFileSync(join(root, "HANDOFF.md"), "utf8");
  assert.doesNotMatch(handoff, /卡 GitHub 认证/);
  assert.doesNotMatch(handoff, /阻塞\(唯一\)/);
  assert.doesNotMatch(handoff, /资产缺口/);
  assert.match(handoff, /SHA256SUMS/);
  assert.match(handoff, /celld-linux-x64/);
  assert.match(handoff, /release\.yml/);
  assert.match(handoff, /v0\.3\.1/);
  assert.match(handoff, /v0\.3\.2/);
  assert.match(handoff, /已发布/);
});

test("源码锚定: Release 流水线拉 denoland celld 并匿名编译", () => {
  const prep = readFileSync(join(root, "scripts/prepare-release-assets.sh"), "utf8");
  const wf = readFileSync(join(root, ".github/workflows/release.yml"), "utf8");
  assert.match(prep, /celld-x86_64-unknown-linux-gnu\.gz/);
  assert.match(prep, /celld-linux-x64/);
  assert.match(prep, /\/tmp\/anon-build/);
  assert.match(prep, /bun-windows-x64/);
  assert.match(wf, /prepare-release-assets\.sh/);
  assert.match(wf, /gh release upload/);
  assert.match(wf, /GH_REPO:/);
  assert.match(wf, /upload-artifact/);
  assert.match(wf, /pull_request:/);
});

test("源码锚定: install 支持 arm64/windows 且缺 SHA256SUMS 会警告", () => {
  const sh = readFileSync(join(root, "install.sh"), "utf8");
  assert.match(sh, /linux-arm64/);
  assert.match(sh, /windows-x64/);
  assert.match(sh, /CELAGENT_REQUIRE_CHECKSUM/);
  assert.match(sh, /verify_checksums\(\) \{/);
  assert.match(sh, /cas-probe/);
  assert.match(sh, /celagent_install_ep_ok/);
});

test("源码锚定: doctor Celld 离线不报全部正常", () => {
  assert.match(tui, /核心正常 \(Celld 离线/);
});

test("源码锚定: doctor 含 CAS 门禁且 persist 拒绝无 CAS 写入", () => {
  assert.match(tui, /\[5\/6\] CAS:/);
  assert.match(tui, /cas-probe/);
  assert.match(tui, /probeStoreCas/);
  assert.match(tui, /src\/persist\.js/);
  assert.match(persist, /async function ensureStoreCas/);
  assert.match(persist, /casGateSticky/);
  assert.match(persist, /此存储不能保证 RPO=0,拒绝权威写入/);
  assert.match(persist, /classifyStoreError/);
  assert.doesNotMatch(tui, /\[1\/5\]/);
  assert.match(tui, /bosDelete/);
  assert.match(tui, /列举会话失败/);
  assert.match(tui, /region: store\.region/);
});

test("源码锚定: worker 已删除未调用的 SigV4 bosPut", () => {
  const w = readFileSync(join(root, "worker/src/index.js"), "utf8");
  assert.doesNotMatch(w, /async function bosPut\(/);
  assert.doesNotMatch(w, /hmacSha256Raw/);
  assert.match(w, /async function bosPutProxy/);
});

test("源码锚定: 版本 0.3.7 与 release-smoke", () => {
  assert.match(tui, /CELAGENT_VERSION = "0\.3\.7"/);
  const smoke = readFileSync(join(root, "scripts/release-smoke.sh"), "utf8");
  assert.match(smoke, /sha256sum --ignore-missing/);
  assert.match(smoke, /celagent-linux-x64/);
});

test("源码锚定: 运维脚本读 store_env 而非写死 BOS endpoint", () => {
  const nm = readFileSync(join(root, "scripts/node_mgr.sh"), "utf8");
  assert.match(nm, /store_env\.sh/);
  assert.match(nm, /celagent_load_store/);
  assert.doesNotMatch(nm, /EP="https:\/\/s3\.bj\.bcebos\.com"/);
  const cm = readFileSync(join(root, "scripts/cluster_mgr.sh"), "utf8");
  assert.match(cm, /store_env\.sh/);
  assert.doesNotMatch(cm, /EP="https:\/\/s3\.bj\.bcebos\.com"/);
  const env = readFileSync(join(root, "scripts/store_env.sh"), "utf8");
  assert.match(env, /celagent_is_allowed_endpoint/);
  assert.match(env, /persistence\.endpoint 不允许/);
});

test("源码锚定: celld v0.2 token vars 与 timingSafeEqual", () => {
  assert.match(tui, /CELLD_VAR_CELAGENT_WORKER_TOKEN/);
  assert.match(tui, /CELLD_ALARM_RESIDENT_MS: "60000"/);
  assert.match(tui, /port \+ 2\}\/state/);
  const worker = readFileSync(join(root, "worker/src/index.js"), "utf8");
  assert.match(worker, /timingSafeEqual/);
  const wrangler = readFileSync(join(root, "worker/wrangler.jsonc"), "utf8");
  assert.match(wrangler, /CELAGENT_WORKER_TOKEN/);
  const nm = readFileSync(join(root, "scripts/node_mgr.sh"), "utf8");
  assert.match(nm, /handoff=preserve/);
  assert.match(nm, /CELLD_VAR_CELAGENT_WORKER_TOKEN=/);
  const cm = readFileSync(join(root, "scripts/cluster_mgr.sh"), "utf8");
  assert.match(cm, /celld diagnose/);
});

test("源码锚定: saveConfig 合并 persistence/worker", () => {
  assert.match(tui, /out\[k\] = \{ \.\.\.prev, \.\.\.cfg\[k\] \}/);
});

test("源码锚定: Windows install.ps1 与 checkout v5", () => {
  const ps = readFileSync(join(root, "install.ps1"), "utf8");
  assert.match(ps, /celagent-windows-x64\.exe/);
  const ci = readFileSync(join(root, ".github/workflows/ci.yml"), "utf8");
  assert.match(ci, /actions\/checkout@v5/);
});
