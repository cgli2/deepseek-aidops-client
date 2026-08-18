//! 运行时可切换的工作区根服务。
//!
//! shell / fs / editor 等 Provider 共享同一 `Arc<Workspace>`，每次操作调 `root()`
//! 取当前根：GUI 侧栏切换项目后，工具操作立即落在新工作区，无需重建 Provider 或
//! 重启进程（上下文隔离的运行时接缝）。

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// 当前工作区根（内部可变，多线程共享）。
pub struct Workspace {
    root: RwLock<PathBuf>,
}

impl Workspace {
    pub fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            root: RwLock::new(root),
        })
    }

    /// 当前根快照。
    pub fn root(&self) -> PathBuf {
        self.root.read().map(|g| g.clone()).unwrap_or_default()
    }

    /// 切换根（项目切换时调用）。
    pub fn set_root(&self, root: PathBuf) {
        if let Ok(mut g) = self.root.write() {
            *g = root;
        }
    }
}
