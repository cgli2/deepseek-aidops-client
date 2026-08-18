//! OpenAI 兼容 `/chat/completions` SSE 流式调用（DeepSeek / OpenAI / llama.cpp 本地共用）。
//!
//! 请求体 `stream: true`；响应为 SSE，`choices[0].delta.content` 为文本增量，
//! `choices[0].delta.tool_calls` 为按 `index` 分片累积的工具调用（id/name/arguments
//! 均为增量字符串）。文本增量逐帧产出 `Chunk`；工具调用在流结束后一次性组装产出，
//! 保证 agent 循环拿到的是完整可解析的 `ToolCall`。

use std::collections::BTreeMap;

use async_stream::stream;
use futures::StreamExt;
use harness_core::error::Error;
use serde_json::{json, Value};

use crate::{Chunk, ChunkStream, Message, Role, ToolCall, ToolSchema, Usage};

/// 发起一次 OpenAI 兼容 SSE 流式调用并返回 Chunk 流。
///
/// `api_key` 为空时不携带 Authorization（llama.cpp 本地服务不需要鉴权）。
/// 不设整体超时（流式生成可能持续数分钟），仅设连接超时。
pub fn stream_chat(
    provider_label: &'static str,
    base_url: String,
    api_key: String,
    body: Value,
) -> ChunkStream {
    // 出口统一 DSML 过滤：DeepSeek-v4 原生 DSML 工具调用文本会被解析为 ToolCall，
    // 不再裸显进回复正文，且工具真正得到执行。
    crate::dsml::filter_stream(inner_stream_chat(
        provider_label, base_url, api_key, body,
    ))
}

fn inner_stream_chat(
    provider_label: &'static str,
    base_url: String,
    api_key: String,
    body: Value,
) -> ChunkStream {
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

        let request = {
            let mut req = client
                .post(format!("{}/chat/completions", base_url.trim_end_matches('/')))
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .json(&body);
            if !api_key.trim().is_empty() {
                req = req.bearer_auth(api_key.trim());
            }
            req
        };

        let resp = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                yield Err(Error::Llm(format!(
                    "{provider_label} 请求失败: {}",
                    friendly_error(&e)
                )));
                return;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            yield Err(Error::Llm(format!("{provider_label} HTTP {status}: {txt}")));
            return;
        }

        // index -> (id, name, arguments)；name/arguments 均为增量拼接。
        let mut tools: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
        let mut saw_text = false;
        // 末尾 token 用量（stream_options.include_usage 触发）：累计最新一份，流结束后单独成帧。
        let mut last_usage: Option<Usage> = None;

        // idle 超时：长时间无新数据视为半开连接 / 模型停滞，立即报错结束回合，
        // 避免 turn 永不结束、UI busy 常真的「30 分钟假死」。
        let idle_secs: u64 = std::env::var("HARNESS_STREAM_IDLE_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120);
        let idle = std::time::Duration::from_secs(idle_secs);

        let mut events = Box::pin(crate::sse::sse_events(resp));
        loop {
            let item = match tokio::time::timeout(idle, events.next()).await {
                Err(_) => {
                    yield Err(Error::Llm(format!(
                        "{provider_label} 模型服务长时间无响应（超过 {idle_secs} 秒无新数据），已中止以避免无限等待，请重试或检查网络/模型状态"
                    )));
                    return;
                }
                Ok(item) => item,
            };
            let Some(item) = item else {
                break;
            };
            let data = match item {
                Ok(d) => d,
                Err(e) => {
                    yield Err(e);
                    return;
                }
            };
            if data == "[DONE]" {
                break;
            }
            let v: Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(err) = v.get("error") {
                yield Err(Error::Llm(format!("{provider_label} 流内错误: {err}")));
                return;
            }
            // 末尾用量（include_usage 触发）：取 prompt/completion/total_tokens 累计。
            if let Some(u) = v.get("usage") {
                if let Some(p) = u.get("prompt_tokens").and_then(|x| x.as_u64()) {
                    last_usage = Some(Usage {
                        prompt_tokens: p,
                        completion_tokens: u
                            .get("completion_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0),
                        total_tokens: u
                            .get("total_tokens")
                            .and_then(|x| x.as_u64())
                            .unwrap_or(0),
                    });
                }
            }
            let Some(delta) = v.pointer("/choices/0/delta") else {
                continue;
            };
            if let Some(text) = delta.get("content").and_then(Value::as_str) {
                if !text.is_empty() {
                    saw_text = true;
                    yield Ok(Chunk {
                        text: Some(text.to_string()),
                        ..Default::default()
                    });
                }
            }
            // 思考链增量（DeepSeek v4 reasoning_content）：仅 UI「思考中」反馈，
            // 不并入回复文本、不进模型上下文。
            if let Some(r) = delta.get("reasoning_content").and_then(Value::as_str) {
                if !r.is_empty() {
                    yield Ok(Chunk {
                        reasoning: Some(r.to_string()),
                        ..Default::default()
                    });
                }
            }
            if let Some(arr) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in arr {
                    let idx = tc.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let entry = tools.entry(idx).or_insert_with(|| {
                        (
                            tc.get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("call")
                                .to_string(),
                            String::new(),
                            String::new(),
                        )
                    });
                    if let Some(id) = tc.get("id").and_then(Value::as_str) {
                        if !id.is_empty() {
                            entry.0 = id.to_string();
                        }
                    }
                    if let Some(f) = tc.get("function") {
                        if let Some(name) = f.get("name").and_then(Value::as_str) {
                            entry.1.push_str(name);
                        }
                        if let Some(args) = f.get("arguments").and_then(Value::as_str) {
                            entry.2.push_str(args);
                        }
                    }
                }
            }
        }

        let tool_calls: Vec<ToolCall> = tools
            .into_values()
            .filter_map(|(id, name, args)| {
                if name.is_empty() {
                    return None;
                }
                let args = serde_json::from_str(&args).unwrap_or_else(|_| json!({}));
                Some(ToolCall { id, name, args })
            })
            .collect();
        // 用量单独成帧（位于工具调用之后），供 agent 循环记入会话日志做成本计量。
        if let Some(usage) = last_usage {
            yield Ok(Chunk {
                usage: Some(usage),
                ..Default::default()
            });
        }
        if !tool_calls.is_empty() {
            yield Ok(Chunk {
                text: None,
                tool_calls,
                ..Default::default()
            });
        } else if !saw_text {
            yield Ok(Chunk {
                text: Some(format!("[{provider_label} 返回了空内容]")),
                ..Default::default()
            });
        }
    })
}

/// 内部消息 → OpenAI 兼容 messages 数组。
pub fn messages_json(msgs: &[Message]) -> Vec<Value> {
    msgs.iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let mut value = json!({ "role": role, "content": m.content });
            if !m.tool_calls.is_empty() {
                value["tool_calls"] = json!(m
                    .tool_calls
                    .iter()
                    .map(|t| json!({
                        "id": t.id, "type": "function",
                        "function": { "name": t.name, "arguments": t.args.to_string() }
                    }))
                    .collect::<Vec<_>>());
            }
            if let Some(id) = &m.tool_call_id {
                value["tool_call_id"] = json!(id);
            }
            value
        })
        .collect()
}

/// ToolSchema → OpenAI 兼容 tools 数组。
pub fn tools_json(tools: &[ToolSchema]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.json_schema
                }
            })
        })
        .collect()
}

/// 编码代理默认可提供的模型可见工具（fs / edit / shell）。
pub fn coding_tools() -> Vec<ToolSchema> {
    vec![
        ToolSchema {
            name: "fs".into(),
            description: "Read, write, or list files inside the workspace".into(),
            json_schema: json!({"type":"object","properties":{"op":{"type":"string","enum":["read","write","list"]},"path":{"type":"string"},"content":{"type":"string"}},"required":["op","path"]}),
        },
        ToolSchema {
            name: "edit".into(),
            description: "Replace an exact text fragment in a workspace file".into(),
            json_schema: json!({"type":"object","properties":{"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}},"required":["path","old_text","new_text"]}),
        },
        ToolSchema {
            name: "shell".into(),
            description: if cfg!(windows) {
                "Run a Windows cmd.exe command in the workspace".into()
            } else {
                "Run a POSIX shell command in the workspace".into()
            },
            json_schema: json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"]}),
        },
        ToolSchema {
            name: "plan".into(),
            description: "Publish or update a structured task plan for complex multi-step work; each item has text and status (pending/doing/done)".into(),
            json_schema: json!({"type":"object","properties":{"items":{"type":"array","items":{"type":"object","properties":{"text":{"type":"string"},"status":{"type":"string","enum":["pending","doing","done"]}},"required":["text"]}}},"required":["items"]}),
        },
        ToolSchema {
            name: "delegate".into(),
            description: "Delegate a time-consuming or independent subtask to a sub-agent and receive its final answer".into(),
            json_schema: json!({"type":"object","properties":{"task":{"type":"string","description":"complete, self-contained subtask description"}},"required":["task"]}),
        },
    ]
}

fn friendly_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "请求超时，请检查网络、代理和 API 地址".to_string()
    } else if e.is_connect() {
        "无法连接模型服务，请检查网络、代理、防火墙和 API 地址".to_string()
    } else {
        e.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_json_maps_roles_and_tool_fields() {
        let msgs = vec![
            Message::system("sys"),
            Message::user("hi"),
            Message::assistant_with_tools(
                "",
                vec![ToolCall {
                    id: "c1".into(),
                    name: "fs".into(),
                    args: json!({"op":"read"}),
                }],
            ),
            Message::tool("c1", "file content"),
        ];
        let v = messages_json(&msgs);
        assert_eq!(v[0]["role"], "system");
        assert_eq!(v[1]["role"], "user");
        assert_eq!(v[2]["tool_calls"][0]["function"]["name"], "fs");
        assert_eq!(v[3]["role"], "tool");
        assert_eq!(v[3]["tool_call_id"], "c1");
    }

    #[test]
    fn tools_json_wraps_function_schema() {
        let v = tools_json(&coding_tools());
        assert_eq!(v.len(), 5);
        assert_eq!(v[0]["type"], "function");
        assert_eq!(v[0]["function"]["name"], "fs");
    }
}
