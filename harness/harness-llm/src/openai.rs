use std::sync::Arc;

use serde_json::json;

use crate::openai_compat;
use crate::{ChunkStream, LlmProvider, Message, RequestOptions, ToolSchema};

/// OpenAI Provider（HTTP+SSE，OpenAI 兼容 `/chat/completions`）。
///
/// 默认指向官方端点；`with_endpoint` 可指向任意 OpenAI 兼容服务（Azure / 网关 / 代理）。
pub struct OpenAI {
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAI {
    /// 用环境变量 `OPENAI_API_KEY` 与官方端点构造（保持旧构造签名）。
    pub fn new(model: impl Into<String>) -> Arc<dyn LlmProvider> {
        Self::with_endpoint(
            "https://api.openai.com/v1",
            std::env::var("OPENAI_API_KEY").unwrap_or_default(),
            model,
        )
    }

    pub fn with_endpoint(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Arc<dyn LlmProvider> {
        Arc::new(Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
        })
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAI {
    fn name(&self) -> &'static str {
        "openai"
    }

    fn tools(&self) -> Vec<ToolSchema> {
        openai_compat::coding_tools()
    }

    fn supports_vision(&self) -> bool {
        supports_vision_model(&self.model)
    }

    fn stream(&self, msgs: Vec<Message>) -> ChunkStream {
        self.stream_with_options(msgs, RequestOptions::default())
    }

    fn stream_with_options(&self, msgs: Vec<Message>, options: RequestOptions) -> ChunkStream {
        let body = json!({
            "model": self.model,
            "messages": openai_compat::messages_json(&msgs),
            "stream": true,
            "max_tokens": options.max_output_tokens.unwrap_or_else(crate::max_output_tokens),
            "tools": openai_compat::tools_json(&crate::allowed_coding_tools(options.allowed_tools.as_deref())),
            "tool_choice": "auto",
            "stream_options": { "include_usage": true },
        });
        openai_compat::stream_chat("OpenAI", self.base_url.clone(), self.api_key.clone(), body)
    }
}

/// OpenAI 兼容端点没有统一的 capabilities 接口；只对常见视觉模型启用图片内容块，
/// 避免将图片意外发给纯文本模型。其它模型仍能收到附件文字摘录。
fn supports_vision_model(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    [
        "gpt-4o", "gpt-4.1", "gpt-5", "o1", "o3", "vision", "qwen-vl", "qwen2-vl",
        "qwen2.5-vl", "qvq", "llava", "minicpm-v", "gemma-3",
    ]
    .iter()
    .any(|name| model.contains(name))
}

#[cfg(test)]
mod tests {
    use super::supports_vision_model;

    #[test]
    fn recognizes_common_vision_model_names() {
        assert!(supports_vision_model("gpt-4o"));
        assert!(supports_vision_model("Qwen2.5-VL-72B-Instruct"));
        assert!(!supports_vision_model("deepseek-chat"));
    }
}
