use std::sync::Arc;

use async_trait::async_trait;

use harness_capability::editor::Editor;
use harness_core::error::Result;
use harness_llm::{ToolCall, ToolResult};

use crate::registry::DynTool;

/// Edit 工具（Consumer）：仅依赖 `Arc<dyn Editor>`。
pub struct EditTool {
    editor: Arc<dyn Editor>,
}

impl EditTool {
    pub fn new(editor: Arc<dyn Editor>) -> Arc<dyn DynTool> {
        Arc::new(Self { editor })
    }
}

#[async_trait]
impl DynTool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let path = call
            .args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if path.trim().is_empty() {
            return Ok(ToolResult {
                call_id: call.id.clone(),
                ok: false,
                content: "edit.path 不能为空".into(),
                continuation_debt: 0,
            });
        }
        let old_text = call
            .args
            .get("old_text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let new_text = call
            .args
            .get("new_text")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let patch = serde_json::json!({"old_text": old_text, "new_text": new_text}).to_string();
        self.editor
            .apply(std::path::Path::new(&path), &patch)
            .await?;
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content: String::new(),
            continuation_debt: 0,
        })
    }
}
