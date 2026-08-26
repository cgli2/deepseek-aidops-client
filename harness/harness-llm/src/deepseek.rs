use std::sync::Arc;

use serde_json::json;

use crate::openai_compat;
use crate::{ChunkStream, LlmProvider, Message, RequestOptions, ToolSchema};

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
        let mut body = json!({
            "model": self.model,
            "messages": openai_compat::messages_json(msgs),
            "stream": true,
            "max_tokens": options
                .max_output_tokens
                .unwrap_or_else(crate::max_output_tokens),
            "tools": openai_compat::tools_json(&crate::allowed_coding_tools(options.allowed_tools.as_deref())),
            "tool_choice": "auto",
            "stream_options": { "include_usage": true },
        });
        // 原子任务的请求级覆盖优先；其他任务继续使用用户持久化的思考档位。
        if let Some(effort) = options
            .reasoning_effort
            .as_ref()
            .or(self.reasoning_effort.as_ref())
        {
            if !effort.trim().is_empty() {
                body["reasoning_effort"] = json!(effort);
            }
        }
        body
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
        assert_eq!(body["reasoning_effort"], "off");
    }
}
