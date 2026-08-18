use std::sync::Arc;

use async_trait::async_trait;

use harness_capability::fs::Fs;
use harness_core::error::Result;
use harness_llm::{ToolCall, ToolResult};

use crate::registry::DynTool;

/// FS 工具（Consumer）：仅依赖 `Arc<dyn Fs>`。
pub struct FsTool {
    fs: Arc<dyn Fs>,
}

impl FsTool {
    pub fn new(fs: Arc<dyn Fs>) -> Arc<dyn DynTool> {
        Arc::new(Self { fs })
    }
}

#[async_trait]
impl DynTool for FsTool {
    fn name(&self) -> &'static str {
        "fs"
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let op = call
            .args
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
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
                content: "fs.path 不能为空".into(),
                continuation_debt: 0,
            });
        }
        let p = std::path::Path::new(&path);

        let content = match op.as_str() {
            "read" => self.fs.read(p).await?,
            "write" => {
                let body = call
                    .args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.fs.write(p, &body).await?;
                String::new()
            }
            "list" => {
                let items = self.fs.list(p).await?;
                items
                    .iter()
                    .map(|x| x.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            other => {
                return Ok(ToolResult {
                    call_id: call.id.clone(),
                    ok: false,
                    content: format!("unknown fs op: {other}"),
                    continuation_debt: 0,
                });
            }
        };

        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content,
            continuation_debt: 0,
        })
    }
}
