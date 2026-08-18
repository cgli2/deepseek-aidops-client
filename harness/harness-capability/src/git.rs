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

/// Git 能力（Definition）。借鉴 Codex 的 git 集成：
/// 代理可读仓库状态、生成 diff、提交，并在隔离 worktree 中并行工作。
pub trait Git: Any + Send + Sync + 'static {
    fn status(&self) -> Result<GitStatus>;
    fn diff(&self) -> Result<String>;
    fn commit(&self, message: &str) -> Result<String>;
    fn current_branch(&self) -> Result<String>;
    /// 创建隔离 worktree（基于 `base`，如 `main` 或某 commit / 分支）。
    fn create_worktree(&self, name: &str, base: &str) -> Result<Worktree>;
    /// 移除 worktree（保留分支）。
    fn remove_worktree(&self, w: &Worktree) -> Result<()>;
}
