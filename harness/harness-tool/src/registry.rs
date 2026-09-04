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

    /// 为子 Agent 创建隔离的工具快照。被排除的编排类工具不会递归委派，
    /// 其余工具共享无状态/线程安全实现，避免重复构造 Provider。
    pub fn snapshot_excluding(&self, excluded: &[&str]) -> Arc<Self> {
        let next = Self::new();
        if let Ok(tools) = self.tools.read() {
            if let Ok(mut target) = next.tools.write() {
                for (name, tool) in tools.iter() {
                    if !excluded.iter().any(|excluded| name == excluded) {
                        target.insert(name.clone(), tool.clone());
                    }
                }
            }
        }
        next
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
        let max_chars = match call.name.as_str() {
            // 文件正文需要较大窗口；Shell/测试日志通常高重复，优先节省上下文。
            "fs" if call.args.get("op").and_then(|v| v.as_str()) == Some("read") => 16_000,
            "shell" | "bash" => 6_000,
            _ => 8_000,
        };
        result.content = sanitize_and_limit(&result.content, max_chars);
        Ok(result)
    }
}

fn sanitize_and_limit(input: &str, max_chars: usize) -> String {
    let mut output = String::with_capacity(input.len().min(max_chars * 2));
    let mut previous = String::new();
    let mut repeated = 0usize;
    for line in input.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.contains("api_key") || lower.contains("apikey") || lower.contains("authorization:")
        {
            append_collapsed_line(
                &mut output,
                &mut previous,
                &mut repeated,
                "[REDACTED SECRET-BEARING LINE]",
            );
            continue;
        }
        let mut rest = line;
        let mut sanitized = String::new();
        while let Some(pos) = rest.find("sk-") {
            sanitized.push_str(&rest[..pos]);
            sanitized.push_str("[REDACTED_API_KEY]");
            let tail = &rest[pos + 3..];
            let consumed = tail
                .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';'))
                .unwrap_or(tail.len());
            rest = &tail[consumed..];
        }
        sanitized.push_str(rest);
        append_collapsed_line(&mut output, &mut previous, &mut repeated, &sanitized);
    }
    flush_repeated(&mut output, &previous, repeated);
    let count = output.chars().count();
    if count > max_chars {
        let head_chars = max_chars * 3 / 5;
        let tail_chars = max_chars.saturating_sub(head_chars);
        let head: String = output.chars().take(head_chars).collect();
        let tail: String = output.chars().skip(count - tail_chars).collect();
        output = format!("{head}\n[中间输出已压缩：原始 {count} 字符，保留开头和结尾]\n{tail}");
    }
    output
}

fn append_collapsed_line(
    output: &mut String,
    previous: &mut String,
    repeated: &mut usize,
    line: &str,
) {
    if previous == line {
        *repeated += 1;
        return;
    }
    flush_repeated(output, previous, *repeated);
    previous.clear();
    previous.push_str(line);
    *repeated = 1;
}

fn flush_repeated(output: &mut String, line: &str, repeated: usize) {
    if repeated == 0 {
        return;
    }
    output.push_str(line);
    output.push('\n');
    if repeated > 1 {
        output.push_str(&format!("[上一行重复 {} 次，已折叠]\n", repeated - 1));
    }
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

    #[test]
    fn long_output_keeps_head_and_tail_and_collapses_repeats() {
        let input = format!("HEAD\n{}\nTAIL", "same\n".repeat(100));
        let output = sanitize_and_limit(&input, 80);
        assert!(output.contains("HEAD"));
        assert!(output.contains("TAIL"));
        assert!(output.contains("重复"));
    }
}
