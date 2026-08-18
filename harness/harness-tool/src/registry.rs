use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use harness_core::error::Result;
use harness_llm::{ToolCall, ToolResult};

/// 模型可见工具的统一接口（Consumer 侧）。名称即工具标识，供 `ToolRegistry::dispatch` 路由。
#[async_trait]
pub trait DynTool: Send + Sync {
    fn name(&self) -> &'static str;
    async fn call(&self, call: &ToolCall) -> Result<ToolResult>;
}

/// 工具注册表 + 调度器。模型产出的 `ToolCall` 经 `dispatch` 路由到对应 `DynTool`。
pub struct ToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn DynTool>>>,
}

impl ToolRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            tools: RwLock::new(HashMap::new()),
        })
    }

    pub fn register(&self, t: Arc<dyn DynTool>) {
        self.tools.write().unwrap().insert(t.name().to_string(), t);
    }

    /// 按名称分发工具调用；未知工具返回 ok=false 的冻结结果（不变量 4：结果不可变）。
    pub async fn dispatch(&self, call: &ToolCall) -> Result<ToolResult> {
        let t = self.tools.read().unwrap().get(&call.name).cloned();
        let mut result = match t {
            Some(t) => t.call(call).await,
            None => Ok(ToolResult {
                call_id: call.id.clone(),
                ok: false,
                content: format!("unknown tool: {}", call.name),
                continuation_debt: 0,
            }),
        }?;
        result.content = sanitize_and_limit(&result.content, 32_000);
        Ok(result)
    }
}

fn sanitize_and_limit(input: &str, max_chars: usize) -> String {
    let mut output = String::with_capacity(input.len().min(max_chars));
    for line in input.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("api_key") || lower.contains("apikey") || lower.contains("authorization:")
        {
            output.push_str("[REDACTED SECRET-BEARING LINE]\n");
            continue;
        }
        let mut rest = line;
        while let Some(pos) = rest.find("sk-") {
            output.push_str(&rest[..pos]);
            output.push_str("[REDACTED_API_KEY]");
            let tail = &rest[pos + 3..];
            let consumed = tail
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';'))
                .unwrap_or(tail.len());
            rest = &tail[consumed..];
        }
        output.push_str(rest);
        output.push('\n');
        if output.chars().count() >= max_chars {
            break;
        }
    }
    if output.chars().count() > max_chars {
        output = output.chars().take(max_chars).collect();
    }
    if input.chars().count() > max_chars {
        output.push_str("\n[输出已截断：超过 32000 字符]");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::sanitize_and_limit;

    #[test]
    fn redacts_keys_and_limits_large_outputs() {
        let input = "api_key = \"secret\"\nnormal sk-abcdefghijklmnopqrstuvwxyz value\n";
        let output = sanitize_and_limit(input, 40);
        assert!(!output.contains("secret"));
        assert!(!output.contains("sk-abcdefghijklmnopqrstuvwxyz"));
        assert!(output.contains("REDACTED"));
    }
}
