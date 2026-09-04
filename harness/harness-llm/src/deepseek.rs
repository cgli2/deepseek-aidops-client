use std::sync::Arc;

use serde_json::json;

use crate::openai_compat;
use crate::{ChunkStream, LlmProvider, Message, RequestOptions, Role, ToolSchema};

/// DeepSeek Provider（首批一等公民，完成文档 §1）。
/// 已接入真实 HTTP SSE（OpenAI 兼容 `/chat/completions`，`stream: true` 真流式）。
pub struct DeepSeek {
    base_url: String,
    api_key: String,
    model: String,
    /// 思考档位 / 努力度（对齐 cc-switch thinkingLevelMap）。`None` 表示不设置、由模型默认；
    /// `Some("...")` 原样作为 `reasoning_effort` 发送上游（如 low/medium/high/xhigh/max）。
    reasoning_effort: Option<String>,
}

impl DeepSeek {
    /// 用解析好的 base_url / api_key / model 与可选思考档位构造 Provider。
    pub fn new(
        base_url: String,
        api_key: String,
        model: String,
        reasoning_effort: Option<String>,
    ) -> Arc<dyn LlmProvider> {
        Arc::new(Self {
            base_url,
            api_key,
            model,
            reasoning_effort,
        })
    }

    fn request_body(&self, msgs: &[Message], options: &RequestOptions) -> serde_json::Value {
        // 恒传 tools（coding agent 定位）：关键词门控曾导致模型拿不到工具 schema，
        // 转而「自创」原生 DSML 格式工具调用，既不解析也不执行；现恒传 + dsml 过滤兜底。
        // `stream_options.include_usage` 让上游在流末尾回传 token 用量（AIOps 计量）。
        let effort = options
            .reasoning_effort
            .as_ref()
            .or(self.reasoning_effort.as_ref())
            .and_then(|effort| normalize_reasoning_effort(effort));
        // A pre-fix persisted session can contain a tool-call assistant record
        // whose reasoning trace was never stored. Passing that otherwise-valid
        // OpenAI transcript to DeepSeek thinking mode is a hard 400. Preserve the
        // tool evidence as a normal historical note instead of sending a broken
        // assistant/tool protocol pair.
        let messages = if thinking_mode_enabled(effort, &self.model) {
            recover_incomplete_thinking_history(msgs)
        } else {
            msgs.to_vec()
        };
        let mut body = json!({
            "model": self.model,
            "messages": openai_compat::messages_json(&messages),
            "stream": true,
            "max_tokens": options
                .max_output_tokens
                .unwrap_or_else(crate::max_output_tokens),
            "tools": openai_compat::tools_json(&crate::allowed_coding_tools(options.allowed_tools.as_deref())),
            "tool_choice": "auto",
            "stream_options": { "include_usage": true },
        });
        // 原子任务的请求级覆盖优先；其他任务继续使用用户持久化的思考档位。
        if let Some(effort) = effort {
            body["reasoning_effort"] = json!(effort);
        }
        body
    }
}

fn thinking_mode_enabled(effort: Option<&str>, model: &str) -> bool {
    match effort {
        Some("none") => false,
        Some(_) => true,
        // `deepseek-reasoner` enables thinking even when a gateway does not
        // expose a reasoning_effort control. Chat models without an explicit
        // thinking setting retain their ordinary tool transcript unchanged.
        None => model.to_ascii_lowercase().contains("reasoner"),
    }
}

/// Rewrites only legacy assistant tool-call turns which have no recoverable
/// `reasoning_content`. Their associated tool results remain available as a
/// compact system note, while the invalid protocol pair is omitted.
fn recover_incomplete_thinking_history(msgs: &[Message]) -> Vec<Message> {
    let mut recovered = Vec::with_capacity(msgs.len());
    let mut index = 0;
    while index < msgs.len() {
        let message = &msgs[index];
        let incomplete_tool_turn = message.role == Role::Assistant
            && !message.tool_calls.is_empty()
            && message
                .reasoning_content
                .as_deref()
                .is_none_or(str::is_empty);
        if !incomplete_tool_turn {
            recovered.push(message.clone());
            index += 1;
            continue;
        }

        let ids: std::collections::HashSet<&str> = message
            .tool_calls
            .iter()
            .map(|call| call.id.as_str())
            .collect();
        let mut summary = String::from(
            "[历史工具回合摘要：原始 reasoning_content 未被旧版本保留；以下结果仍可作为证据使用]\n",
        );
        for call in &message.tool_calls {
            summary.push_str(&format!("- 工具 {}\n", call.name));
        }
        index += 1;
        while index < msgs.len()
            && msgs[index].role == Role::Tool
            && msgs[index]
                .tool_call_id
                .as_deref()
                .is_some_and(|id| ids.contains(id))
        {
            let result = &msgs[index];
            let id = result.tool_call_id.as_deref().unwrap_or("unknown");
            let excerpt: String = result.content.chars().take(2_000).collect();
            summary.push_str(&format!("  {id} 返回：{excerpt}\n"));
            if result.content.chars().count() > 2_000 {
                summary.push_str("  [历史工具输出已节选]\n");
            }
            index += 1;
        }
        recovered.push(Message::system(summary));
    }
    recovered
}

/// DeepSeek 的 OpenAI 兼容网关以 `none` 关闭推理；项目历史配置/旧模型目录
/// 使用过 `off`。在唯一出网边界归一化，保证旧设置不会直接演变成 HTTP 400。
/// 空值、`auto` 与未知值均不发送，让上游使用模型默认值。
fn normalize_reasoning_effort(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" | "none" => Some("none"),
        "minimal" => Some("minimal"),
        "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "xhigh" => Some("xhigh"),
        "max" => Some("max"),
        _ => None,
    }
}

#[async_trait::async_trait]
impl LlmProvider for DeepSeek {
    fn name(&self) -> &'static str {
        "deepseek"
    }

    fn tools(&self) -> Vec<ToolSchema> {
        openai_compat::coding_tools()
    }

    fn stream(&self, msgs: Vec<Message>) -> ChunkStream {
        self.stream_with_options(msgs, RequestOptions::default())
    }

    fn stream_with_options(&self, msgs: Vec<Message>, options: RequestOptions) -> ChunkStream {
        let body = self.request_body(&msgs, &options);
        openai_compat::stream_chat(
            "DeepSeek",
            self.base_url.clone(),
            self.api_key.clone(),
            body,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_options_override_persisted_reasoning_and_output_budget() {
        let provider = DeepSeek {
            base_url: "https://example.invalid".into(),
            api_key: "test".into(),
            model: "deepseek-reasoner".into(),
            reasoning_effort: Some("high".into()),
        };
        let body = provider.request_body(
            &[Message::user("repair this regression")],
            &RequestOptions {
                max_output_tokens: Some(1_536),
                reasoning_effort: Some("off".into()),
                ..Default::default()
            },
        );
        assert_eq!(body["max_tokens"], 1_536);
        assert_eq!(body["reasoning_effort"], "none");
    }

    #[test]
    fn legacy_or_invalid_reasoning_effort_never_reaches_the_gateway() {
        assert_eq!(normalize_reasoning_effort(" OFF "), Some("none"));
        assert_eq!(normalize_reasoning_effort("auto"), None);
        assert_eq!(normalize_reasoning_effort("ultra"), None);
    }

    #[test]
    fn thinking_request_summarizes_legacy_tool_turn_without_reasoning() {
        let provider = DeepSeek {
            base_url: "https://example.invalid".into(),
            api_key: "test".into(),
            model: "deepseek-reasoner".into(),
            reasoning_effort: Some("high".into()),
        };
        let body = provider.request_body(
            &[
                Message::user("修复问题"),
                Message::assistant_with_tools(
                    "",
                    vec![crate::ToolCall {
                        id: "old-call".into(),
                        name: "fs".into(),
                        args: json!({"op": "read"}),
                    }],
                ),
                Message::tool("old-call", "旧工具结果"),
                Message::user("继续"),
            ],
            &RequestOptions::default(),
        );
        let messages = body["messages"].as_array().unwrap();
        assert!(!messages.iter().any(|message| {
            message["role"] == "assistant" && message.get("tool_calls").is_some()
        }));
        assert!(!messages.iter().any(|message| message["role"] == "tool"));
        assert!(messages.iter().any(|message| {
            message["role"] == "system"
                && message["content"]
                    .as_str()
                    .is_some_and(|text| text.contains("旧工具结果"))
        }));
    }

    #[test]
    fn thinking_request_keeps_complete_tool_turn_verbatim() {
        let mut assistant = Message::assistant_with_tools(
            "",
            vec![crate::ToolCall {
                id: "call-1".into(),
                name: "fs".into(),
                args: json!({"op": "read"}),
            }],
        );
        assistant.reasoning_content = Some("读取文件。".into());
        let messages =
            recover_incomplete_thinking_history(&[assistant, Message::tool("call-1", "ok")]);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].reasoning_content.as_deref(), Some("读取文件。"));
        assert_eq!(messages[1].role, Role::Tool);
    }
}
