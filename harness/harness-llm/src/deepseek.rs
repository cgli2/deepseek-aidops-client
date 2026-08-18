use std::sync::Arc;

use serde_json::json;

use crate::openai_compat;
use crate::{ChunkStream, LlmProvider, Message, ToolSchema};

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
        // 恒传 tools（coding agent 定位）：关键词门控曾导致模型拿不到工具 schema，
        // 转而「自创」原生 DSML 格式工具调用，既不解析也不执行；现恒传 + dsml 过滤兜底。
        // `stream_options.include_usage` 让上游在流末尾回传 token 用量（AIOps 计量）。
        let mut body = json!({
            "model": self.model,
            "messages": openai_compat::messages_json(&msgs),
            "stream": true,
            "tools": openai_compat::tools_json(&openai_compat::coding_tools()),
            "tool_choice": "auto",
            "stream_options": { "include_usage": true },
        });
        // 仅在用户显式设置时发送：避免 `reasoning_effort: null` 被上游拒绝。
        if let Some(effort) = &self.reasoning_effort {
            if !effort.trim().is_empty() {
                body["reasoning_effort"] = json!(effort);
            }
        }
        openai_compat::stream_chat(
            "DeepSeek",
            self.base_url.clone(),
            self.api_key.clone(),
            body,
        )
    }
}
