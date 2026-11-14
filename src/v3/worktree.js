// v3/worktree.js — 每个子 agent 一个独立 git worktree
// 只做 git 原语：repo 探测、worktree 创建、结果快照、清理。
// 不持有 mission/task 状态；调用方决定什么时候创建、什么时候停。
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";

function run(cwd, args, { timeout = 30000 } = {}) {
  try {
    const stdout = execFileSync("git", ["-C", cwd, ...args], {
      encoding: "utf8",
      timeout,
      stdio: ["ignore", "pipe", "pipe"],
      maxBuffer: 16 * 1024 * 1024,
    });
    return { ok: true, stdout: String(stdout).trim(), stderr: "", status: 0 };
  } catch (e) {
    return {
      ok: false,
      stdout: String(e.stdout || "").trim(),
      stderr: String(e.stderr || e.message || e).trim(),
      status: typeof e.status === "number" ? e.status : 1,
    };
  }
}

export function repoRoot(cwd) {
  const r = run(cwd, ["rev-parse", "--show-toplevel"]);
  return r.ok && r.stdout ? r.stdout : null;
}

export function repoHead(cwd) {
  const r = run(cwd, ["rev-parse", "HEAD"]);
  return r.ok ? r.stdout : null;
}

export function repoDirty(cwd) {
  const r = run(cwd, ["status", "--porcelain"]);
  if (!r.ok) return { ok: false, dirty: false, files: [] };
  const files = r.stdout ? r.stdout.split("\n").filter(Boolean) : [];
  return { ok: true, dirty: files.length > 0, files };
}

export function branchName(parent, child) {
  return `celagent/${parent}/${child}`;
}

/** 在 repo 的 HEAD 上创建子 worktree；返回 base commit 与分支。 */
export function createWorktree({ repo, path, branch, base = null }) {
  if (!repo) throw new Error("不是 git 仓库，无法创建 worktree");
  if (existsSync(path)) throw new Error(`worktree 路径已存在: ${path}`);
  const start = base || repoHead(repo);
  if (!start) throw new Error("仓库没有 HEAD，无法创建 worktree");
  mkdirSync(dirname(path), { recursive: true });
  const r = run(repo, ["worktree", "add", "-b", branch, path, start]);
  if (!r.ok) throw new Error(`git worktree add 失败: ${r.stderr || r.stdout}`);
  return { path, branch, base: start };
}

/** 把 worktree 里的改动提交到子分支；没有改动返回 null。 */
export function commitWorktree({ path, message }) {
  const st = run(path, ["status", "--porcelain"]);
  if (!st.ok) throw new Error(`git status 失败: ${st.stderr}`);
  if (!st.stdout) return null;
  const add = run(path, ["add", "-A"]);
  if (!add.ok) throw new Error(`git add 失败: ${add.stderr}`);
  // 子分支是本地草稿分支；用 celagent 身份提交，避免冒充用户。
  const commit = run(path, [
    "-c", "user.name=celagent",
    "-c", "user.email=celagent@localhost",
    "commit", "--no-verify", "-m", message,
  ]);
  if (!commit.ok) throw new Error(`git commit 失败: ${commit.stderr || commit.stdout}`);
  const head = run(path, ["rev-parse", "HEAD"]);
  return head.ok ? head.stdout : null;
}

/** 结果快照：HEAD、相对 base 的 diffstat、未提交文件数。 */
export function worktreeSummary({ path, base }) {
  const out = { ok: false, commit: null, diffStat: "", dirtyFiles: 0, error: null };
  if (!existsSync(path)) { out.error = "worktree 不存在"; return out; }
  const head = run(path, ["rev-parse", "HEAD"]);
  if (head.ok) out.commit = head.stdout;
  if (base) {
    const diff = run(path, ["diff", "--stat", `${base}..HEAD`]);
    if (diff.ok) out.diffStat = diff.stdout;
  }
  const st = run(path, ["status", "--porcelain"]);
  if (st.ok) out.dirtyFiles = st.stdout ? st.stdout.split("\n").filter(Boolean).length : 0;
  out.ok = true;
  return out;
}

export function removeWorktree({ repo, path, branch = null }) {
  if (!repo) return { removed: false, error: "不是 git 仓库" };
  const r = run(repo, ["worktree", "remove", "--force", path]);
  let branchRemoved = false;
  if (branch) {
    const b = run(repo, ["branch", "-D", branch]);
    branchRemoved = b.ok;
  }
  return { removed: r.ok, branchRemoved, error: r.ok ? null : (r.stderr || r.stdout) };
}

export function worktreePathFor({ configDir, child }) {
  return join(configDir, "worktrees", child);
}
