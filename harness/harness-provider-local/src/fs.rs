use std::path::Path;
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
        let normalized = candidate
            .canonicalize()
            .unwrap_or_else(|_| candidate.clone());
        if !normalized.starts_with(&root) {
            return Err(harness_core::error::Error::SandboxDenied(format!(
                "path is outside workspace: {}",
                path.display()
            )));
        }
        Ok(normalized)
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
}
