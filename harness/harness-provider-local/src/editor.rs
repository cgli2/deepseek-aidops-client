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
                mismatch_report(&content, old, count),
            )));
        }
        std::fs::write(&path, content.replacen(old, new, 1))?;
        Ok(())
    }
}

/// old_text 失配（0 次或多次）时的自愈报告：工具自读磁盘，给出候选区域的
/// 精确原文与行号，让模型以磁盘事实重发，禁止凭记忆重构（spec §4.6）。
/// 已知边界：锚点用子串匹配、有意宽松，报告的是「候选区域」需模型核对；
/// 且按字面字节匹配——CRLF 文件遇到仅 \n 的 old_text 会失配（历史限制，另行治理）。
fn mismatch_report(content: &str, old: &str, count: usize) -> String {
    if count > 1 {
        let lines: Vec<usize> = content
            .match_indices(old)
            .map(|(off, _)| content[..off].matches('\n').count() + 1)
            .collect();
        return format!(
            "old_text must match exactly once (matched {count}). 命中行号: {}。请扩大 old_text 的上下文使其唯一后重发。",
            lines
                .iter()
                .map(|l| l.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // count == 0：文件已变化。用 old_text 的首个非空行作锚点，回读候选区域。
    let old_lines: Vec<&str> = old.lines().collect();
    let anchor = old_lines
        .iter()
        .find(|l| !l.trim().is_empty())
        .copied()
        .unwrap_or("");
    let file_lines: Vec<&str> = content.lines().collect();
    if !anchor.trim().is_empty() {
        if let Some(idx) = file_lines.iter().position(|l| l.contains(anchor.trim())) {
            let start = idx.saturating_sub(3);
            let end = (idx + old_lines.len() + 3).min(file_lines.len());
            let region: String = file_lines[start..end]
                .iter()
                .enumerate()
                .map(|(i, l)| format!("{}|{}\n", start + i + 1, l))
                .collect();
            return format!(
                "old_text must match exactly once (matched 0). 文件已变化；以下是磁盘当前候选区域（行号|内容），请用磁盘原文重发，禁止凭记忆重构：\n{region}"
            );
        }
    }
    format!(
        "old_text must match exactly once (matched 0). old_text 在文件中无任何锚点（文件共 {} 行）。请先用 fs 工具 read 该文件获取最新内容，再基于磁盘原文重发。",
        file_lines.len()
    )
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

    #[tokio::test]
    async fn edit_zero_match_returns_disk_region_with_line_numbers() {
        let root = std::env::temp_dir().join(format!("harness-editor-mismatch-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("target.rs");
        // 磁盘现状是 v2；模型凭记忆发来 v1 的 old_text
        std::fs::write(&path, "fn main() {\n    println!(\"v2\");\n}\n").unwrap();
        let editor = LocalEditor::new(root.clone());
        let err = editor
            .apply(
                std::path::Path::new("target.rs"),
                r#"{"old_text":"fn main() {\n    println!(\"v1\");","new_text":"x"}"#,
            )
            .await
            .expect_err("matched 0 必须报错");
        let msg = format!("{err}");
        assert!(msg.contains("matched 0"), "{msg}");
        assert!(msg.contains("println!(\"v2\")"), "报告必须回读磁盘现状: {msg}");
        assert!(msg.contains("2|"), "报告必须带行号: {msg}");
        // 文件未被修改
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "fn main() {\n    println!(\"v2\");\n}\n"
        );
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(root);
    }

    #[tokio::test]
    async fn edit_ambiguous_match_lists_hit_line_numbers() {
        let root = std::env::temp_dir().join(format!("harness-editor-amb-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("dup.txt");
        std::fs::write(&path, "dup\nmiddle\ndup\n").unwrap();
        let editor = LocalEditor::new(root.clone());
        let err = editor
            .apply(
                std::path::Path::new("dup.txt"),
                r#"{"old_text":"dup","new_text":"y"}"#,
            )
            .await
            .expect_err("matched 2 必须报错");
        let msg = format!("{err}");
        assert!(msg.contains("matched 2"), "{msg}");
        assert!(msg.contains("1") && msg.contains("3"), "报告必须给出命中行号: {msg}");
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(root);
    }

    #[tokio::test]
    async fn edit_zero_match_without_anchor_suggests_reread() {
        let root = std::env::temp_dir().join(format!("harness-editor-noanchor-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("a.txt");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let editor = LocalEditor::new(root.clone());
        let err = editor
            .apply(
                std::path::Path::new("a.txt"),
                r#"{"old_text":"zzz qqq","new_text":"y"}"#,
            )
            .await
            .expect_err("matched 0 必须报错");
        let msg = format!("{err}");
        assert!(msg.contains("无任何锚点"), "{msg}");
        assert!(msg.contains("read"), "必须引导模型先重新读取文件: {msg}");
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(root);
    }
}
