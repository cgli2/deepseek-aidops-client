use std::collections::BTreeMap;
use std::sync::Arc;

use async_stream::stream;
use futures::StreamExt;
use harness_core::error::Error;
use serde_json::{Value, json};

use crate::openai_compat;
use crate::{Chunk, ChunkStream, LlmProvider, Message, Role, ToolCall, ToolSchema};

/// Anthropic Provider（原生 Messages API + HTTP/SSE）。
///
/// 事件模型：`content_block_delta`（text_delta 增量 / input_json_delta 工具入参增量）、
/// `content_block_start`（tool_use 块的 id/name）、`message_stop`（结束）。
pub struct Anthropic {
    base_url: String,
    api_key: String,
    model: String,
    /// 思考档位 / 努力度（对齐 cc-switch thinkingLevelMap）。`None` 不开启扩展思考；
    /// `Some(...)` 原样视为「已启用扩展思考」并附带预算（见 `request_body`）。
    reasoning_effort: Option<String>,
}

impl Anthropic {
    /// 用环境变量 `ANTHROPIC_API_KEY` 与官方端点构造（保持旧构造签名）。
    pub fn new(model: impl Into<String>) -> Arc<dyn LlmProvider> {
        Self::with_endpoint(
            "https://api.anthropic.com",
            std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            model,
            None,
        )
    }

    pub fn with_endpoint(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        reasoning_effort: Option<String>,
    ) -> Arc<dyn LlmProvider> {
        Arc::new(Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            reasoning_effort,
        })
    }
}

#[async_trait::async_trait]
impl LlmProvider for Anthropic {
    fn name(&self) -> &'static str {
        "anthropic"
    }

    fn tools(&self) -> Vec<ToolSchema> {
        openai_compat::coding_tools()
    }

    fn stream(&self, msgs: Vec<Message>) -> ChunkStream {
        let base_url = self.base_url.clone();
        let api_key = self.api_key.clone();
        let model = self.model.clone();
        let body = request_body(&model, &msgs, self.reasoning_effort.as_deref());

        Box::pin(stream! {
            let client = match reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    yield Err(Error::Llm(format!("reqwest init: {e}")));
                    return;
                }
            };

            let resp = match client
                .post(format!("{}/v1/messages", base_url.trim_end_matches('/')))
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .header("x-api-key", api_key.trim())
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    yield Err(Error::Llm(format!("Anthropic 请求失败: {e}")));
                    return;
                }
            };

            if !resp.status().is_success() {
                let status = resp.status();
                let txt = resp.text().await.unwrap_or_default();
                yield Err(Error::Llm(format!("Anthropic HTTP {status}: {txt}")));
                return;
            }

            // index -> (id, name, 累积的 input JSON 片段)
            let mut tools: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
            let mut saw_text = false;

            let mut events = Box::pin(crate::sse::sse_events(resp));
            while let Some(item) = events.next().await {
                let data = match item {
                    Ok(d) => d,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };
                let v: Value = match serde_json::from_str(&data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                match v.get("type").and_then(Value::as_str) {
                    Some("error") => {
                        yield Err(Error::Llm(format!(
                            "Anthropic 流内错误: {}",
                            v.get("error").unwrap_or(&v)
                        )));
                        return;
                    }
                    Some("content_block_start") => {
                        if let Some(block) = v.get("content_block") {
                            if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                                let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                                let id = block
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("call")
                                    .to_string();
                                let name = block
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                tools.insert(idx, (id, name, String::new()));
                            }
                        }
                    }
                    Some("content_block_delta") => {
                        let idx = v.get("index").and_then(Value::as_u64).unwrap_or(0);
                        if let Some(delta) = v.get("delta") {
                            match delta.get("type").and_then(Value::as_str) {
                                Some("text_delta") => {
                                    if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                        if !text.is_empty() {
                                            saw_text = true;
                                            yield Ok(Chunk {
                                                text: Some(text.to_string()),
                                                ..Default::default()
                                            });
                                        }
                                    }
                                }
                                Some("input_json_delta") => {
                                    if let Some(part) =
                                        delta.get("partial_json").and_then(Value::as_str)
                                    {
                                        if let Some(entry) = tools.get_mut(&idx) {
                                            entry.2.push_str(part);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    Some("message_stop") => break,
                    _ => {}
                }
            }

            let tool_calls: Vec<ToolCall> = tools
                .into_values()
                .filter_map(|(id, name, raw)| {
                    if name.is_empty() {
                        return None;
                    }
                    let args = serde_json::from_str(&raw).unwrap_or_else(|_| json!({}));
                    Some(ToolCall { id, name, args })
                })
                .collect();
            if !tool_calls.is_empty() {
                yield Ok(Chunk {
                    text: None,
                    tool_calls,
                    ..Default::default()
                });
            } else if !saw_text {
                yield Ok(Chunk {
                    empty_response: true,
                    finish_reason: Some("unknown".into()),
                    ..Default::default()
                });
            }
        })
    }
}

/// 内部消息 → Anthropic Messages API 请求体（system 提取为顶层字段，工具结果转
/// `tool_result` 块，assistant 工具调用转 `tool_use` 块）。
fn request_body(model: &str, msgs: &[Message], reasoning_effort: Option<&str>) -> Value {
    let system: Vec<&str> = msgs
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.content.as_str())
        .collect();
    let mut messages = Vec::new();
    for m in msgs {
        match m.role {
            Role::System => {}
            Role::Tool => messages.push(json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.content,
                }],
            })),
            Role::Assistant if !m.tool_calls.is_empty() => {
                let mut content: Vec<Value> = Vec::new();
                if !m.content.is_empty() {
                    content.push(json!({ "type": "text", "text": m.content }));
                }
                for tc in &m.tool_calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.args,
                    }));
                }
                messages.push(json!({ "role": "assistant", "content": content }));
            }
            _ => messages.push(json!({ "role": role_name(m.role), "content": m.content })),
        }
    }
    let tools: Vec<Value> = openai_compat::coding_tools()
        .into_iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.json_schema,
            })
        })
        .collect();
    let mut body = json!({
        "model": model,
        "max_tokens": 4096,
        "stream": true,
        "system": system.join("\n\n"),
        "messages": messages,
        "tools": tools,
    });
    // 仅当用户显式设置思考档位时开启扩展思考；预算固定为 max_tokens 的一半（< max_tokens 才合法）。
    // 不自动推断能力：是否支持由模型/预设决定，未知 effort 字符串原样视为「启用」。
    if reasoning_effort.is_some_and(|e| !e.trim().is_empty()) {
        body["thinking"] = json!({ "type": "enabled", "budget_tokens": 2048 });
    }
    body
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "user",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_maps_system_tools_and_tool_use() {
        let msgs = vec![
            Message::system("be helpful"),
            Message::user("read a.txt"),
            Message::assistant_with_tools(
                "reading",
                vec![ToolCall {
                    id: "c1".into(),
                    name: "fs".into(),
                    args: json!({"op":"read","path":"a.txt"}),
                }],
            ),
            Message::tool("c1", "file content"),
        ];
        let body = request_body("claude-test", &msgs, None);
        assert_eq!(body["system"], "be helpful");
        assert_eq!(body["stream"], true);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[1]["content"][0]["type"], "text");
        assert_eq!(messages[1]["content"][1]["type"], "tool_use");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "c1");
    }
}
