use std::sync::Arc;

use async_trait::async_trait;

use harness_capability::shell::{Shell, ShellRequest};
use harness_core::error::Result;
use harness_llm::{ToolCall, ToolResult};

use crate::registry::DynTool;

/// Bash 工具（Consumer）：仅依赖 `Arc<dyn Shell>`，不知 Provider 是谁。
pub struct BashTool {
    shell: Arc<dyn Shell>,
}

impl BashTool {
    pub fn new(shell: Arc<dyn Shell>) -> Arc<dyn DynTool> {
        Arc::new(Self { shell })
    }
}

#[async_trait]
impl DynTool for BashTool {
    fn name(&self) -> &'static str {
        "shell"
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let cmd = call
            .args
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if cmd.trim().is_empty() {
            return Ok(ToolResult {
                call_id: call.id.clone(),
                ok: false,
                content: "bash.command 不能为空".into(),
                continuation_debt: 0,
            });
        }
        let out = self
            .shell
            .run(ShellRequest {
                cmd,
                cwd: None,
                timeout_ms: 30_000,
            })
            .await?;
        let mut content = out.stdout;
        if !out.stderr.is_empty() {
            content.push_str(&format!("\n[stderr]\n{}", out.stderr));
        }
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: out.exit_code == 0,
            content,
            continuation_debt: 0,
        })
    }
}
