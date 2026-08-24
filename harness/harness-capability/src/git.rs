use std::any::Any;
use std::path::PathBuf;

use harness_core::error::Result;

/// 仓库状态快照。
#[derive(Debug, Clone)]
pub struct GitStatus {
    pub branch: String,
    pub dirty: bool,
    pub ahead: usize,
    pub behind: usize,
}

/// 一个 git worktree（隔离的工作副本）。
#[derive(Debug, Clone)]
pub struct Worktree {
    pub name: String,
    pub path: PathBuf,
    pub base: String,
}

/// 一个未提交变更（git status --porcelain 一行）。
#[derive(Debug, Clone)]
pub struct GitChange {
    /// 相对仓库根的路径。
    pub path: String,
    /// 索引状态码（M/A/D/R/C/U），`??` 表示未跟踪。
    pub index: String,
    /// 工作区状态码（M/D/..），`??` 表示未跟踪。
    pub worktree: String,
}

impl GitChange {
    /// 展示用状态标记（M/A/D/??），优先索引码。
    pub fn marker(&self) -> &str {
        if self.index != "  " && self.index != "??" {
            self.index.trim()
        } else if self.worktree != "  " && self.worktree != "??" {
            self.worktree.trim()
        } else if self.index == "??" || self.worktree == "??" {
            "??"
        } else {
            ""
        }
    }
}

/// Git 能力（Definition）。借鉴 Codex 的 git 集成：
/// 代理可读仓库状态、生成 diff、提交，并在隔离 worktree 中并行工作。
pub trait Git: Any + Send + Sync + 'static {
    /// 当前实际查询的仓库根。UI 必须把它与当前项目路径一并展示，避免错误仓库
    /// 或 Git 查询失败被伪装成“工作区干净”。
    fn repository_root(&self) -> Result<PathBuf>;
    fn status(&self) -> Result<GitStatus>;
    fn diff(&self) -> Result<String>;
    /// 指定文件的未暂存 diff（unified 格式）。无修改返回空串。
    fn diff_path(&self, path: &str) -> Result<String>;
    /// 文件是否被 git 跟踪（`git ls-files --error-unmatch <path>` 成功即跟踪）。
    fn is_tracked(&self, path: &str) -> Result<bool>;
    /// 当前有未提交变化的文件列表（`git status --porcelain` 解析，含状态码）。
    /// 文件树据此给变化文件加色块标记；Git 变更面板据此列文件。非 git 仓库返回空列表。
    fn changed_files(&self) -> Result<Vec<GitChange>>;
    fn commit(&self, message: &str) -> Result<String>;
    fn current_branch(&self) -> Result<String>;
    /// 创建隔离 worktree（基于 `base`，如 `main` 或某 commit / 分支）。
    fn create_worktree(&self, name: &str, base: &str) -> Result<Worktree>;
    /// 移除 worktree（保留分支）。
    fn remove_worktree(&self, w: &Worktree) -> Result<()>;
}
