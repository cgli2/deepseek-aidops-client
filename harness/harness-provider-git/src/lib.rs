//! 基于 git CLI 的 Provider（借鉴 Codex 的 git 集成）。
//!
//! 所有操作通过 `git -C <repo>` 子进程执行，零 C 绑定。worktree 方法配合 `WorktreeGuard`
//! （Drop 自动移除）实现可逆的隔离工作副本——呼应 dsh 的 `effect()` 回滚哲学：进入即承诺，离开即清理。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use harness_capability::git::{Git, GitStatus, Worktree};
use harness_core::error::{Error, Result};

/// git CLI Provider。
pub struct GitCli {
    repo: PathBuf,
}

impl GitCli {
    pub fn new(repo: impl AsRef<Path>) -> Arc<dyn Git> {
        Arc::new(Self {
            repo: repo.as_ref().to_path_buf(),
        })
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(args)
            .output()
            .map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Git(format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

impl Git for GitCli {
    fn status(&self) -> Result<GitStatus> {
        let branch = self
            .run(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string();
        let porcelain = self.run(&["status", "--porcelain"])?;
        let dirty = !porcelain.trim().is_empty();
        let ab = self
            .run(&["rev-list", "--left-right", "--count", "@{upstream}...HEAD"])
            .unwrap_or_default();
        let (ahead, behind) = parse_ahead_behind(&ab);
        Ok(GitStatus {
            branch,
            dirty,
            ahead,
            behind,
        })
    }

    fn diff(&self) -> Result<String> {
        self.run(&["diff"])
    }

    fn commit(&self, message: &str) -> Result<String> {
        self.run(&["add", "-A"])?;
        self.run(&["commit", "-m", message])?;
        Ok(self.run(&["rev-parse", "HEAD"])?.trim().to_string())
    }

    fn current_branch(&self) -> Result<String> {
        Ok(self
            .run(&["rev-parse", "--abbrev-ref", "HEAD"])?
            .trim()
            .to_string())
    }

    fn create_worktree(&self, name: &str, base: &str) -> Result<Worktree> {
        let parent = self.repo.parent().unwrap_or(&self.repo).to_path_buf();
        let path = parent.join(format!(".wt-{}", name));
        self.run(&["worktree", "add", path.to_str().unwrap_or(""), base])?;
        Ok(Worktree {
            name: name.to_string(),
            path,
            base: base.to_string(),
        })
    }

    fn remove_worktree(&self, w: &Worktree) -> Result<()> {
        self.run(&[
            "worktree",
            "remove",
            w.path.to_str().unwrap_or(""),
            "--force",
        ])?;
        Ok(())
    }
}

fn parse_ahead_behind(s: &str) -> (usize, usize) {
    let mut it = s.split('\t');
    let ahead = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    let behind = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    (ahead, behind)
}

/// Worktree 守卫：RAII 封装，`Drop` 时自动移除 worktree（可逆副作用）。
///
/// 用法：`let _wt = WorktreeGuard::new(git, "task-7", "main")?;` —— 离开作用域即清理，
/// 即使中途 panic 也不会残留孤儿 worktree。
pub struct WorktreeGuard {
    git: Arc<dyn Git>,
    wt: Worktree,
}

impl WorktreeGuard {
    pub fn new(git: Arc<dyn Git>, name: &str, base: &str) -> Result<Self> {
        let wt = git.create_worktree(name, base)?;
        Ok(Self { git, wt })
    }

    pub fn path(&self) -> &Path {
        &self.wt.path
    }
}

impl Drop for WorktreeGuard {
    fn drop(&mut self) {
        let _ = self.git.remove_worktree(&self.wt);
    }
}
