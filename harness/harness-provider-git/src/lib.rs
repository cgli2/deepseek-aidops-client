//! 基于 git CLI 的 Provider（借鉴 Codex 的 git 集成）。
//!
//! 所有操作通过 `git -C <repo>` 子进程执行，零 C 绑定。worktree 方法配合 `WorktreeGuard`
//! （Drop 自动移除）实现可逆的隔离工作副本——呼应 dsh 的 `effect()` 回滚哲学：进入即承诺，离开即清理。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use harness_capability::git::{Git, GitChange, GitStatus, Worktree};
use harness_core::Workspace;
use harness_core::error::{Error, Result};

/// git CLI Provider。
pub struct GitCli {
    root: GitRoot,
}

enum GitRoot {
    Fixed(PathBuf),
    Workspace(Arc<Workspace>),
}

impl GitCli {
    pub fn new(repo: impl AsRef<Path>) -> Arc<dyn Git> {
        Arc::new(Self {
            root: GitRoot::Fixed(repo.as_ref().to_path_buf()),
        })
    }

    /// 与文件/搜索/编辑 Provider 共用可切换的 Workspace，项目切换后每次 Git
    /// 命令都会读取新根目录，不能保留启动时的旧仓库路径。
    pub fn with_workspace(workspace: Arc<Workspace>) -> Arc<dyn Git> {
        Arc::new(Self {
            root: GitRoot::Workspace(workspace),
        })
    }

    fn repo(&self) -> PathBuf {
        match &self.root {
            GitRoot::Fixed(path) => path.clone(),
            GitRoot::Workspace(workspace) => workspace.root(),
        }
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        let out = git_command(&self.repo(), args)
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

/// 统一的 git 子进程构造：Windows GUI 进程下必须带 CREATE_NO_WINDOW，
/// 否则每次调用都会闪一帧黑色控制台（会话 7ba3370f turn 3–14 的体验根因）。
fn git_command(repo: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd
}

impl Git for GitCli {
    fn repository_root(&self) -> Result<PathBuf> {
        let root = self.run(&["rev-parse", "--show-toplevel"])?;
        Ok(PathBuf::from(root.trim()))
    }

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
        let out = git_command(&self.repo(), &["status", "--porcelain", "-z"])
            .output()
            .map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Git(format!(
                "git status --porcelain failed in {}: {}",
                self.repo().display(),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
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
        let repo = self.repo();
        let parent = repo.parent().unwrap_or(&repo).to_path_buf();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        let mut cmd = git_command(path, &["init", "-q"]);
        cmd.current_dir(path);
        let output = cmd.output().unwrap();
        assert!(output.status.success());
    }

    #[test]
    fn workspace_backed_git_follows_project_switches() {
        let root = std::env::temp_dir().join(format!("harness-git-workspace-{}", uuid()));
        let project_a = root.join("a");
        let project_b = root.join("b");
        init_repo(&project_a);
        init_repo(&project_b);
        std::fs::write(project_b.join("uncommitted.txt"), "pending").unwrap();

        let workspace = Workspace::new(project_a.clone());
        let git = GitCli::with_workspace(workspace.clone());
        assert!(git.changed_files().unwrap().is_empty());
        assert_eq!(
            git.repository_root()
                .unwrap()
                .file_name()
                .and_then(|name| name.to_str()),
            Some("a")
        );

        workspace.set_root(project_b.clone());
        let changes = git.changed_files().unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "uncommitted.txt");
        assert_eq!(
            git.repository_root()
                .unwrap()
                .file_name()
                .and_then(|name| name.to_str()),
            Some("b")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn non_repository_is_an_error_not_an_empty_change_list() {
        let path = std::env::temp_dir().join(format!("harness-git-nonrepo-{}", uuid()));
        std::fs::create_dir_all(&path).unwrap();
        let git = GitCli::new(&path);
        assert!(git.changed_files().is_err());
        assert!(git.repository_root().is_err());
        let _ = std::fs::remove_dir_all(path);
    }

    fn uuid() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
            .to_string()
    }
}
