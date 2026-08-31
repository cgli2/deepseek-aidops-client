use std::sync::Arc;

use async_trait::async_trait;

use harness_capability::shell::{Shell, ShellRequest};
use harness_core::error::Result;
use harness_llm::{ToolCall, ToolResult};

use crate::registry::DynTool;

/// Bash 工具（Consumer）：仅依赖 `Arc<dyn Shell>`，不知 Provider 是谁。
pub struct BashTool {
    shell: Arc<dyn Shell>,
    timeout_ms: u64,
}

impl BashTool {
    pub fn new(shell: Arc<dyn Shell>) -> Arc<dyn DynTool> {
        // 命令超时可配：此前硬编码 30s 总在外层工具门禁（默认 300s）之前触发，
        // build/test 等长命令被误杀；默认 120s，可用 HARNESS_BASH_TIMEOUT_MS 覆盖。
        let timeout_ms: u64 = std::env::var("HARNESS_BASH_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120_000)
            .clamp(1_000, 3_600_000);
        Arc::new(Self { shell, timeout_ms })
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
                timeout_ms: self.timeout_ms,
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
