use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use harness_capability::editor::Editor;
use harness_core::error::Result;
use harness_core::Workspace;

/// 本地 Editor Provider（实现 `Editor`）。返回 `Arc<dyn Editor>`。
/// 根目录共享 `Arc<Workspace>`：项目切换后操作立即落在新工作区。
pub struct LocalEditor {
    ws: Arc<Workspace>,
}

impl LocalEditor {
    pub fn new(root: std::path::PathBuf) -> Arc<dyn Editor> {
        Self::with_workspace(Workspace::new(root))
    }

    /// 共享外部 Workspace（GUI 项目切换可动态换根）。
    pub fn with_workspace(ws: Arc<Workspace>) -> Arc<dyn Editor> {
        Arc::new(Self { ws })
    }
}

#[async_trait]
impl Editor for LocalEditor {
    async fn apply(&self, path: &Path, patch: &str) -> Result<()> {
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(harness_core::error::Error::SandboxDenied(
                "parent traversal is not allowed".into(),
            ));
        }
        let root = self.ws.root();
        let root = root.canonicalize().unwrap_or(root);
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        };
        let path = candidate.canonicalize()?;
        if !path.starts_with(&root) {
            return Err(harness_core::error::Error::SandboxDenied(format!(
                "path is outside workspace: {}",
                path.display()
            )));
        }
        let spec: serde_json::Value = serde_json::from_str(patch)?;
        let old = spec.get("old_text").and_then(|v| v.as_str()).unwrap_or("");
        let new = spec.get("new_text").and_then(|v| v.as_str()).unwrap_or("");
        if old.is_empty() {
            return Err(harness_core::error::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "old_text must not be empty",
            )));
        }
        let content = std::fs::read_to_string(&path)?;
        let count = content.matches(old).count();
        if count != 1 {
            return Err(harness_core::error::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("old_text must match exactly once (matched {count})"),
            )));
        }
        std::fs::write(&path, content.replacen(old, new, 1))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn edit_replaces_exactly_once_and_rejects_ambiguous_matches() {
        let root = std::env::temp_dir().join(format!("harness-editor-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("sample.txt");
        std::fs::write(&path, "alpha beta").unwrap();
        let editor = LocalEditor::new(root.clone());
        editor
            .apply(
                std::path::Path::new("sample.txt"),
                r#"{"old_text":"alpha","new_text":"gamma"}"#,
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "gamma beta");
        std::fs::write(&path, "x x").unwrap();
        assert!(editor
            .apply(
                std::path::Path::new("sample.txt"),
                r#"{"old_text":"x","new_text":"y"}"#
            )
            .await
            .is_err());
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(root);
    }
}
