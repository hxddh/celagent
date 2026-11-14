// tests/v3-worktree.test.mjs — 子 agent 的 worktree 隔离
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync, readFileSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  branchName, commitWorktree, createWorktree, removeWorktree,
  repoDirty, repoHead, repoRoot, worktreeSummary,
} from "../src/v3/worktree.js";

function git(cwd, args) {
  return execFileSync("git", ["-C", cwd, ...args], { encoding: "utf8" }).trim();
}
function repo() {
  const dir = mkdtempSync(join(tmpdir(), "celagent-v3-wt-"));
  git(dir, ["init", "-q"]);
  git(dir, ["config", "user.name", "test"]);
  git(dir, ["config", "user.email", "test@example.com"]);
  writeFileSync(join(dir, "a.txt"), "one\n");
  git(dir, ["add", "-A"]);
  git(dir, ["commit", "-q", "-m", "init"]);
  return dir;
}

test("worktree: 从 HEAD 创建独立分支，提交后 diff 只含子改动", () => {
  const root = repo();
  const cfg = mkdtempSync(join(tmpdir(), "celagent-v3-cfg-"));
  const child = "dlg-test-1";
  try {
    const base = repoHead(root);
    assert.equal(repoRoot(root), root);
    assert.equal(repoDirty(root).dirty, false);

    const wt = join(cfg, "worktrees", child);
    const branch = branchName("parent-1", child);
    createWorktree({ repo: root, path: wt, branch, base });
    assert.equal(existsSync(join(wt, "a.txt")), true);
    assert.equal(git(wt, ["rev-parse", "--abbrev-ref", "HEAD"]), branch);

    writeFileSync(join(wt, "b.txt"), "child\n");
    const commit = commitWorktree({ path: wt, message: "child work" });
    assert.ok(commit);
    const sum = worktreeSummary({ path: wt, base });
    assert.match(sum.diffStat, /b\.txt/);
    assert.doesNotMatch(sum.diffStat, /a\.txt/);
    assert.equal(sum.dirtyFiles, 0);

    // 父仓库仍在原分支，没被子 agent 改动。
    assert.equal(readFileSync(join(root, "a.txt"), "utf8"), "one\n");
    assert.equal(existsSync(join(root, "b.txt")), false);
  } finally {
    rmSync(cfg, { recursive: true, force: true });
    rmSync(root, { recursive: true, force: true });
  }
});

test("worktree: 父仓库脏时不阻塞，但记录 dirty；remove 清理分支", () => {
  const root = repo();
  const cfg = mkdtempSync(join(tmpdir(), "celagent-v3-cfg2-"));
  const child = "dlg-test-2";
  try {
    writeFileSync(join(root, "a.txt"), "dirty\n");
    assert.equal(repoDirty(root).dirty, true);

    const wt = join(cfg, "worktrees", child);
    const branch = branchName("parent-1", child);
    createWorktree({ repo: root, path: wt, branch });
    // worktree 从 HEAD 开始，不包含父会话未提交改动。
    assert.equal(readFileSync(join(wt, "a.txt"), "utf8"), "one\n");

    const r = removeWorktree({ repo: root, path: wt, branch });
    assert.equal(r.removed, true);
    assert.equal(r.branchRemoved, true);
    assert.equal(existsSync(wt), false);
  } finally {
    rmSync(cfg, { recursive: true, force: true });
    rmSync(root, { recursive: true, force: true });
  }
});

test("worktree: 非 git 目录返回 null root", () => {
  const dir = mkdtempSync(join(tmpdir(), "celagent-v3-nogit-"));
  try {
    assert.equal(repoRoot(dir), null);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
