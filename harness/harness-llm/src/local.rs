use std::sync::Arc;

use serde_json::json;

use crate::openai_compat;
use crate::{ChunkStream, LlmProvider, Message, RequestOptions, ToolSchema};

/// 本地 LLM Provider（llama.cpp HTTP server，OpenAI 兼容协议）。feature `local` 启用。
///
/// llama.cpp server 默认无鉴权，`api_key` 留空即不携带 Authorization。
pub struct LocalLlm {
    base_url: String,
}

impl LocalLlm {
    pub fn new(base_url: impl Into<String>) -> Arc<dyn LlmProvider> {
        Arc::new(Self {
            base_url: base_url.into(),
        })
    }
}

#[async_trait::async_trait]
impl LlmProvider for LocalLlm {
    fn name(&self) -> &'static str {
        "local"
    }

    fn tools(&self) -> Vec<ToolSchema> {
        openai_compat::coding_tools()
    }

    fn stream(&self, msgs: Vec<Message>) -> ChunkStream {
        self.stream_with_options(msgs, RequestOptions::default())
    }

    fn stream_with_options(&self, msgs: Vec<Message>, options: RequestOptions) -> ChunkStream {
        let body = json!({
            "model": "local",
            "messages": openai_compat::messages_json(&msgs),
            "stream": true,
            "stream_options": { "include_usage": true },
            "tools": openai_compat::tools_json(&crate::allowed_coding_tools(options.allowed_tools.as_deref())),
            "tool_choice": "auto",
        });
        openai_compat::stream_chat("Local", self.base_url.clone(), String::new(), body)
    }
}
