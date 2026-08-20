//! 基于 git CLI 的 Provider（借鉴 Codex 的 git 集成）。
//!
//! 所有操作通过 `git -C <repo>` 子进程执行，零 C 绑定。worktree 方法配合 `WorktreeGuard`
//! （Drop 自动移除）实现可逆的隔离工作副本——呼应 dsh 的 `effect()` 回滚哲学：进入即承诺，离开即清理。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use harness_capability::git::{Git, GitChange, GitStatus, Worktree};
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

    fn diff_path(&self, path: &str) -> Result<String> {
        self.run(&["diff", "--", path])
    }

    fn is_tracked(&self, path: &str) -> Result<bool> {
        // ls-files --error-unmatch 对未跟踪文件返回非零退出码 → run() 报错。
        // 成功即表示文件被跟踪；输出非空为双重确认。
        self.run(&["ls-files", "--error-unmatch", path])
            .map(|s| !s.trim().is_empty())
    }

    fn changed_files(&self) -> Result<Vec<GitChange>> {
        // -z 以 NUL 分隔，正确处理含空格 / 中文的文件名。
        // 格式：`XY PATH\0` —— 索引状态 + 工作树状态各 1 字符，随后一个空格 + 路径。
        // 注意：**不能 trim 条目**（trim 会把工作树修改 ` M path` 开头的状态空格
        // 吃掉，导致 bytes[2] 不再是分隔空格而误判跳过，只有 ?? 未跟踪能通过）。
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args(["status", "--porcelain", "-z"])
            .output()
            .map_err(Error::Io)?;
        if !out.status.success() {
            // 非 git 仓库 / 错误：返回空列表（UI 不崩溃，只是无标记）。
            return Ok(Vec::new());
        }
        let raw = String::from_utf8_lossy(&out.stdout).to_string();
        let mut files = Vec::new();
        for entry in raw.split('\0') {
            if entry.len() < 4 {
                // 空条目 / 重命名条目的第二个路径（纯路径，无状态码）。
                continue;
            }
            let bytes = entry.as_bytes();
            // 第 3 个字符必须是分隔空格；重命名第二个路径等无状态码条目会被跳过。
            if bytes[2] != b' ' {
                continue;
            }
            files.push(GitChange {
                path: entry[3..].to_string(),
                index: entry[..1].to_string(),
                worktree: entry[1..2].to_string(),
            });
        }
        Ok(files)
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
