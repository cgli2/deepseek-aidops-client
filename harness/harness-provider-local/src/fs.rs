use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use harness_capability::fs::Fs;
use harness_core::error::Result;
use harness_core::Workspace;

/// 本地 FS Provider（实现 `Fs`）。返回 `Arc<dyn Fs>`。
/// 根目录共享 `Arc<Workspace>`：项目切换后操作立即落在新工作区。
pub struct LocalFs {
    ws: Arc<Workspace>,
}

/// 规范化路径用于沙箱边界比较。
///
/// Windows 上 `Path::canonicalize()` 返回带 `\\?\` 长路径前缀的绝对路径，
/// 而对「不存在的文件」会失败。若直接拿未规范化的候选路径去和规范化后的 root
/// 做 `starts_with`，前缀不一致会导致误报 `path is outside workspace`（例如点击
/// 助手回复里指向不存在文件的相对路径时）。
///
/// 方案：对候选路径的「最近存在的祖先」做 canonicalize（拿到与 root 一致的规范化
/// 前缀），再拼接回剩余不存在的部分，使两者前缀对齐后比较。文件实际不存在时，
/// 该路径仍应落在工作区内（返回 file-not-found），而不是被误判为越界。
pub(crate) fn normalize_for_sandbox(p: &Path) -> PathBuf {
    // 已存在：直接 canonicalize（与 root 前缀一致）。
    if p.exists() {
        return p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    }
    // 不存在：先把候选自身的文件名计入 suffix，再沿父目录向上找最近存在的祖先，
    // canonicalize 祖先后拼回剩余路径（含自身文件名），使前缀与 root 对齐。
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    if let Some(name) = p.file_name() {
        suffix.push(name.to_os_string());
    }
    let mut cur = p;
    loop {
        match cur.parent() {
            Some(parent) if parent != cur => {
                if parent.exists() {
                    let mut out = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
                    for part in suffix.iter().rev() {
                        out.push(part);
                    }
                    return out;
                }
                if let Some(name) = cur.file_name() {
                    suffix.push(name.to_os_string());
                }
                cur = parent;
            }
            _ => {
                // 一路追到根都不存在：退回原始候选（比较仍可能判定越界，但不会更糟）。
                return p.to_path_buf();
            }
        }
    }
}

/// 去掉 Windows `\\?\` 长路径前缀，返回可直接用于文件 IO 的干净绝对路径。
///
/// `canonicalize()` 在 Windows 上返回 `\\?\C:\...` 形式；带此前缀的路径
/// 对「不存在的文件」做 `read_to_string` 会返回 error 5（拒绝访问）而非 error 2
/// （找不到），导致预览报错信息失真。前缀仅用于沙箱边界比较，IO 用干净路径。
#[cfg(windows)]
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    let trimmed = s
        .strip_prefix(r"\\?\")
        .or_else(|| s.strip_prefix(r"\\?\UNC\"))
        .unwrap_or(&s);
    PathBuf::from(trimmed)
}

#[cfg(not(windows))]
fn strip_verbatim(p: &Path) -> PathBuf {
    p.to_path_buf()
}

impl LocalFs {
    pub fn new(root: std::path::PathBuf) -> Arc<dyn Fs> {
        Self::with_workspace(Workspace::new(root))
    }

    /// 共享外部 Workspace（GUI 项目切换可动态换根）。
    pub fn with_workspace(ws: Arc<Workspace>) -> Arc<dyn Fs> {
        Arc::new(Self { ws })
    }

    fn root(&self) -> std::path::PathBuf {
        let root = self.ws.root();
        root.canonicalize().unwrap_or(root)
    }

    fn path(&self, path: &Path) -> Result<std::path::PathBuf> {
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(harness_core::error::Error::SandboxDenied(format!(
                "parent traversal is not allowed: {}",
                path.display()
            )));
        }
        let root = self.root();
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        // 关键：用「最近存在祖先」规范化候选，避免 Windows `\\?\` 前缀不一致
        // 把工作区内不存在的文件误判为 outside workspace。
        let normalized = normalize_for_sandbox(&candidate);
        if !normalized.starts_with(&root) {
            return Err(harness_core::error::Error::SandboxDenied(format!(
                "path is outside workspace: {}",
                path.display()
            )));
        }
        // 返回无 `\\?\` 前缀的干净路径：保证对不存在文件的 IO 报 not-found
        // 而非 error 5（Windows 上 `\\?\` + 不存在文件的诡异行为）。
        // 返回无 `\\?\` 前缀的干净路径：保证对不存在文件的 IO 报 not-found
        // 而非 error 5（Windows 上 `\\?\` + 不存在文件的诡异行为）。
        Ok(strip_verbatim(&normalized))
    }
}

#[async_trait]
impl Fs for LocalFs {
    async fn read(&self, path: &Path) -> Result<String> {
        Ok(tokio::fs::read_to_string(self.path(path)?).await?)
    }

    async fn write(&self, path: &Path, content: &str) -> Result<()> {
        let path = self.path(path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        Ok(tokio::fs::write(path, content).await?)
    }

    async fn list(&self, path: &Path) -> Result<Vec<std::path::PathBuf>> {
        let mut out = vec![];
        let mut rd = tokio::fs::read_dir(self.path(path)?).await?;
        while let Some(e) = rd.next_entry().await? {
            out.push(e.path());
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_parent_traversal() {
        let fs = LocalFs::new(std::env::temp_dir());
        assert!(fs
            .read(std::path::Path::new("../outside.txt"))
            .await
            .is_err());
    }

    /// 回归守卫：工作区内「不存在的文件」应返回 file-not-found，
    /// 而不是被 Windows `\\?\` 前缀不对齐误判为 outside workspace。
    #[tokio::test]
    async fn missing_file_inside_workspace_is_not_sandbox_denied() {
        let root = std::env::temp_dir().join(format!("harness-fs-miss-{}", std::process::id()));
        // 用独立子目录作为工作区根：避免直接拿系统 Temp 根当沙箱根时
        // 触达其特殊权限（Windows Temp 根上 io error 5 与沙箱逻辑无关）。
        let ws_root = root.join("ws");
        std::fs::create_dir_all(&ws_root).unwrap();
        let fs = LocalFs::new(ws_root.clone());

        // 不存在的文件，但在工作区根内：应为 file-not-found（IO 错误），
        // 且绝不能是沙箱越界误判（这是本次修复的核心）。
        let err = fs.read(std::path::Path::new("missing.txt")).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("outside workspace"),
            "should be file-not-found, got: {msg}"
        );
        assert!(
            msg.contains("os error 2"),
            "expected not-found (os error 2), got: {msg}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 回归守卫：子目录内不存在的文件同样不应误判越界。
    #[tokio::test]
    async fn missing_file_in_subdir_inside_workspace_is_not_sandbox_denied() {
        let root = std::env::temp_dir().join(format!("harness-fs-miss2-{}", std::process::id()));
        let sub = root.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        let fs = LocalFs::new(root.clone());

        let err = fs.read(std::path::Path::new("a/b/missing.rs")).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            !msg.contains("outside workspace"),
            "should be file-not-found, got: {msg}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
