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

/// 进程级共享异步 HTTP client：连接池与 TLS 会话跨请求复用。
/// 此前每次流式调用都 `Client::builder().build()`，每个回合重付一次
/// TLS 握手 + 连接建立（首 token 延迟的主要网络开销之一）。
pub(crate) fn shared_client() -> Result<&'static reqwest::Client, Error> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    let entry = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| format!("reqwest init: {e}"))
    });
    match entry {
        Ok(c) => Ok(c),
        Err(e) => Err(Error::Llm(e.clone())),
    }
}

/// 进程级共享 blocking client（模型列表等低频管理请求用）。
fn shared_blocking_client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::blocking::Client, String>> =
        std::sync::OnceLock::new();
    let entry = CLIENT.get_or_init(|| {
        reqwest::blocking::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .map_err(|e| format!("reqwest init: {e}"))
    });
    entry.as_ref().map_err(|e| e.clone())
}

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
    crate::dsml::filter_stream(inner_stream_chat(provider_label, base_url, api_key, body))
}

fn inner_stream_chat(
    provider_label: &'static str,
    base_url: String,
    api_key: String,
    body: Value,
) -> ChunkStream {
    Box::pin(stream! {
        let client = match shared_client() {
            Ok(c) => c,
            Err(e) => {
                yield Err(e);
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

        // 少数 OpenAI 兼容网关会忽略 `stream: true`，却以 200 + application/json 返回完整
        // Chat Completions 响应。旧实现把它交给 SSE 解析器后会得到零帧，再误报“空内容”。
        // 对明确的 JSON 响应走完整响应回退解析；真正的 SSE 仍使用下方增量路径。
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if content_type.contains("application/json") {
            let response: Value = match resp.json().await {
                Ok(value) => value,
                Err(error) => {
                    yield Err(Error::Llm(format!(
                        "{provider_label} 返回了非 SSE JSON，但解析失败: {error}"
                    )));
                    return;
                }
            };
            match complete_response_chunks(&response) {
                Ok(chunks) => {
                    for chunk in chunks {
                        yield Ok(chunk);
                    }
                }
                Err(error) => yield Err(Error::Llm(format!("{provider_label} 非流式响应无有效内容: {error}"))),
            }
            return;
        }

        // index -> (id, name, arguments)；name/arguments 均为增量拼接。
        let mut tools: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
        let mut saw_text = false;
        let mut saw_reasoning = false;
        let mut finish_reason: Option<String> = None;
        // 末尾 token 用量（stream_options.include_usage 触发）：累计最新一份，流结束后单独成帧。
        let mut last_usage: Option<Usage> = None;

        // idle 超时：长时间无新数据视为半开连接 / 模型停滞，立即报错结束回合，
        // 避免 turn 永不结束、UI busy 常真的「30 分钟假死」。
        let idle_secs: u64 = std::env::var("HARNESS_STREAM_IDLE_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120);
        let idle = std::time::Duration::from_secs(idle_secs);

        let first_frame_secs: u64 = std::env::var("HARNESS_STREAM_FIRST_FRAME_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30)
            .clamp(5, 120);
        let first_frame = std::time::Duration::from_secs(first_frame_secs);
        let mut received_frame = false;
        let mut events = Box::pin(crate::sse::sse_events(resp));
        loop {
            let wait = if received_frame { idle } else { first_frame };
            let item = match tokio::time::timeout(wait, events.next()).await {
                Err(_) => {
                    let reason = if received_frame {
                        format!("超过 {idle_secs} 秒无新数据")
                    } else {
                        format!("连接成功后 {first_frame_secs} 秒仍未收到首帧")
                    };
                    yield Err(Error::Llm(format!(
                        "{provider_label} 模型服务长时间无响应（{reason}），已中止以避免无限等待，请重试或检查网络/模型状态"
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
            received_frame = true;
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
            if let Some(reason) = v.pointer("/choices/0/finish_reason").and_then(Value::as_str) {
                if !reason.is_empty() {
                    finish_reason = Some(reason.to_string());
                }
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
                    saw_reasoning = true;
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
                empty_response: true,
                finish_reason: Some(match finish_reason {
                    Some(reason) => reason,
                    None if saw_reasoning => "reasoning_only".into(),
                    None => "unknown".into(),
                }),
                ..Default::default()
            });
        }
    })
}

/// 解析兼容服务返回的标准（非 SSE）Chat Completions 响应。
/// 仅作为 `Content-Type: application/json` 的回退，避免把代理行为误判为模型空响应。
fn complete_response_chunks(response: &Value) -> std::result::Result<Vec<Chunk>, String> {
    if let Some(error) = response.get("error") {
        return Err(format!("上游错误: {error}"));
    }
    let choice = response
        .pointer("/choices/0")
        .ok_or_else(|| "缺少 choices[0]".to_string())?;
    let message = choice
        .get("message")
        .ok_or_else(|| "缺少 choices[0].message".to_string())?;
    let finish_reason = choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut chunks = Vec::new();
    // A few compatible gateways return a full JSON response even when stream was
    // requested. Preserve DeepSeek's protocol state on this fallback path too.
    if let Some(reasoning) = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|reasoning| !reasoning.is_empty())
    {
        chunks.push(Chunk {
            reasoning: Some(reasoning.to_string()),
            ..Default::default()
        });
    }
    if let Some(content) = message.get("content").and_then(Value::as_str) {
        if !content.is_empty() {
            chunks.push(Chunk {
                text: Some(content.to_string()),
                ..Default::default()
            });
        }
    }
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|call| {
            let name = call.pointer("/function/name").and_then(Value::as_str)?;
            if name.is_empty() {
                return None;
            }
            let raw_args = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("{}");
            Some(ToolCall {
                id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .unwrap_or("call")
                    .to_string(),
                name: name.to_string(),
                args: serde_json::from_str(raw_args).unwrap_or_else(|_| json!({})),
            })
        })
        .collect::<Vec<_>>();
    if !tool_calls.is_empty() {
        chunks.push(Chunk {
            tool_calls,
            ..Default::default()
        });
    }
    if chunks.is_empty() {
        chunks.push(Chunk {
            empty_response: true,
            finish_reason: Some(finish_reason.unwrap_or_else(|| "unknown".into())),
            ..Default::default()
        });
    }
    if let Some(usage) = response.get("usage") {
        if let Some(prompt_tokens) = usage.get("prompt_tokens").and_then(Value::as_u64) {
            chunks.push(Chunk {
                usage: Some(Usage {
                    prompt_tokens,
                    completion_tokens: usage
                        .get("completion_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    total_tokens: usage
                        .get("total_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                }),
                ..Default::default()
            });
        }
    }
    Ok(chunks)
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
            let content = if m.role == Role::User && !m.image_data_urls.is_empty() {
                let mut parts = Vec::with_capacity(m.image_data_urls.len() + 1);
                parts.push(json!({ "type": "text", "text": m.content }));
                parts.extend(m.image_data_urls.iter().map(|url| {
                    json!({ "type": "image_url", "image_url": { "url": url } })
                }));
                Value::Array(parts)
            } else {
                Value::String(m.content.clone())
            };
            let mut value = json!({ "role": role, "content": content });
            // DeepSeek thinking mode treats reasoning_content as part of the
            // assistant tool-call transcript.  It must be preserved verbatim on
            // the next request, but must never be fabricated for other roles.
            if m.role == Role::Assistant {
                if let Some(reasoning) = m
                    .reasoning_content
                    .as_deref()
                    .filter(|reasoning| !reasoning.is_empty())
                {
                    value["reasoning_content"] = json!(reasoning);
                }
            }
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
            // 明确告知相对路径以工作区根解析：避免模型在每条命令/路径里
            // 重复拼完整绝对路径（取证：单回合 411 条命令携带 cd 全路径前缀）。
            // 取证（UI 任务）：大文件整读会被上下文压缩截断，模型看不到目标
            // 区域转而自造截取脚本；提供 start_line/end_line 区间读并引导按需读取。
            description: "Read, write, or list files inside the workspace; relative paths resolve against the workspace root. For reads, large files return a head window with total line count: use start_line/end_line (1-based, inclusive) to fetch the exact range you need instead of writing temporary extraction scripts".into(),
            json_schema: json!({"type":"object","properties":{"op":{"type":"string","enum":["read","write","list"]},"path":{"type":"string","description":"relative to workspace root (preferred) or absolute"},"content":{"type":"string"},"start_line":{"type":"integer","description":"read only: first line to return (1-based)"},"end_line":{"type":"integer","description":"read only: last line to return (inclusive)"}},"required":["op","path"]}),
        },
        ToolSchema {
            name: "edit".into(),
            description: "Replace an exact text fragment in a workspace file; relative paths resolve against the workspace root".into(),
            json_schema: json!({"type":"object","properties":{"path":{"type":"string","description":"relative to workspace root (preferred) or absolute"},"old_text":{"type":"string"},"new_text":{"type":"string"}},"required":["path","old_text","new_text"]}),
        },
        ToolSchema {
            name: "shell".into(),
            // 取证：模型因不确定工作目录，每条命令都重复拼 `cd /d <全路径> &&`，
            // 既拉长命令又让每条命令签名唯一、绕过重复守卫。明确告知
            // 命令已在工作区根执行、无需 cd，相对路径直接可用。
            description: if cfg!(windows) {
                "Run a Windows cmd.exe command. It already starts in the workspace root; never prepend 'cd' to the workspace path, use relative paths directly".into()
            } else {
                "Run a POSIX shell command. It already starts in the workspace root; never prepend 'cd' to the workspace path, use relative paths directly".into()
            },
            json_schema: json!({"type":"object","properties":{"command":{"type":"string","description":"command body only; do not cd into the workspace first"}},"required":["command"]}),
        },
        ToolSchema {
            name: "search".into(),
            // 定位代码的首选工具：一次调用返回「文件:行号:内容」的有界结果。
            // 明确禁止用临时脚本/全仓命令替代，切断“自造扫描脚本”的失控模式。
            description: "Case-insensitive substring search across workspace files, returns bounded path:line:text hits. ALWAYS use this to locate code instead of findstr/dir/grep shell commands or writing temporary scan scripts".into(),
            json_schema: json!({"type":"object","properties":{"pattern":{"type":"string","description":"substring to find (case-insensitive)"},"dir":{"type":"string","description":"optional relative subdirectory to narrow the scan"},"max_results":{"type":"integer","description":"max hits to return (default 60)"}},"required":["pattern"]}),
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

/// 拉取上游模型列表（OpenAI 兼容 `GET {base_url}/models`）。
///
/// 解析响应的 `data[].id` 作为模型 id 列表。部分服务（如某些本地网关）把
/// 模型列表放在 `models[].id` 或 `data[].name`，这里做兼容解析。失败返回
/// 友好错误（网络/认证/HTTP 状态均给出可读原因）。
pub fn fetch_models(base_url: String, api_key: String) -> Result<Vec<String>, String> {
    let client = shared_blocking_client()?;

    let mut req = client
        .get(format!("{}/models", base_url.trim_end_matches('/')))
        .header("Accept", "application/json");
    if !api_key.trim().is_empty() {
        req = req.header("Authorization", format!("Bearer {}", api_key.trim()));
    }

    let resp = req.send().map_err(|e| {
        if e.is_timeout() {
            "请求超时，请检查网络、代理和 API 地址".to_string()
        } else if e.is_connect() {
            "无法连接模型服务，请检查网络、代理、防火墙和 API 地址".to_string()
        } else {
            e.to_string()
        }
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("获取模型列表失败: HTTP {status} {body}"));
    }

    let text = resp.text().map_err(|e| format!("读取响应失败: {e}"))?;
    parse_models_json(&text)
}

/// 从模型列表 JSON 文本中解析模型 id（与 `fetch_models` 相同的兼容解析）。
/// 拆出以便离线单测（fetch_models 本身是网络请求）。
pub fn parse_models_json(text: &str) -> std::result::Result<Vec<String>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("响应不是合法 JSON: {e}"))?;
    let mut ids: Vec<String> = Vec::new();
    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                ids.push(id.to_string());
            } else if let Some(name) = item.get("name").and_then(|x| x.as_str()) {
                ids.push(name.to_string());
            }
        }
    }
    if ids.is_empty() {
        if let Some(arr) = v.get("models").and_then(|d| d.as_array()) {
            for item in arr {
                if let Some(id) = item.get("id").and_then(|x| x.as_str()) {
                    ids.push(id.to_string());
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    ids.retain(|m| seen.insert(m.clone()));
    if ids.is_empty() {
        return Err(format!("上游返回了空模型列表：{text}"));
    }
    Ok(ids)
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
        let mut assistant = Message::assistant_with_tools(
            "",
            vec![ToolCall {
                id: "c1".into(),
                name: "fs".into(),
                args: json!({"op":"read"}),
            }],
        );
        assistant.reasoning_content = Some("先检查文件，再调用工具。".into());
        let msgs = vec![
            Message::system("sys"),
            Message::user("hi"),
            assistant,
            Message::tool("c1", "file content"),
        ];
        let v = messages_json(&msgs);
        assert_eq!(v[0]["role"], "system");
        assert_eq!(v[1]["role"], "user");
        assert_eq!(v[2]["tool_calls"][0]["function"]["name"], "fs");
        assert_eq!(v[2]["reasoning_content"], "先检查文件，再调用工具。");
        assert!(v[1].get("reasoning_content").is_none());
        assert_eq!(v[3]["role"], "tool");
        assert_eq!(v[3]["tool_call_id"], "c1");
    }

    #[test]
    fn messages_json_encodes_user_images_as_openai_content_parts() {
        let message = Message::user_with_images(
            "请识别图片",
            vec!["data:image/png;base64,aGVsbG8=".into()],
        );
        let value = messages_json(&[message]);
        assert_eq!(value[0]["content"][0]["type"], "text");
        assert_eq!(value[0]["content"][0]["text"], "请识别图片");
        assert_eq!(value[0]["content"][1]["type"], "image_url");
        assert_eq!(
            value[0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,aGVsbG8="
        );
    }

    #[test]
    fn tools_json_wraps_function_schema() {
        let v = tools_json(&coding_tools());
        assert_eq!(v.len(), 6);
        assert_eq!(v[0]["type"], "function");
        assert_eq!(v[0]["function"]["name"], "fs");
    }

    #[test]
    fn parse_models_json_extracts_ids() {
        // OpenAI / DeepSeek 标准：data[].id
        let ids = parse_models_json(
            r#"{"object":"list","data":[{"id":"deepseek-chat"},{"id":"deepseek-reasoner"}]}"#,
        )
        .unwrap();
        assert_eq!(
            ids,
            vec!["deepseek-chat".to_string(), "deepseek-reasoner".to_string()]
        );
    }

    #[test]
    fn parse_models_json_falls_back_to_models_array() {
        // 部分网关：models[].id
        let ids =
            parse_models_json(r#"{"models":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"}]}"#).unwrap();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"gpt-4o".to_string()));
    }

    #[test]
    fn parse_models_json_dedups_and_rejects_empty() {
        let ids = parse_models_json(r#"{"data":[{"id":"a"},{"id":"a"},{"id":"b"}]}"#).unwrap();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
        assert!(parse_models_json(r#"{"data":[]}"#).is_err());
    }

    #[test]
    fn complete_response_fallback_preserves_text_tools_usage_and_finish_reason() {
        let response = json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "reasoning_content": "need to inspect first",
                    "content": "我先检查。",
                    "tool_calls": [{
                        "id": "call-1",
                        "function": { "name": "fs", "arguments": "{\"op\":\"read\"}" }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 4, "total_tokens": 14 }
        });
        let chunks = complete_response_chunks(&response).unwrap();
        assert_eq!(chunks[0].reasoning.as_deref(), Some("need to inspect first"));
        assert_eq!(chunks[1].text.as_deref(), Some("我先检查。"));
        assert_eq!(chunks[2].tool_calls[0].name, "fs");
        assert_eq!(chunks[3].usage.unwrap().total_tokens, 14);

        let empty = complete_response_chunks(&json!({
            "choices": [{ "finish_reason": "length", "message": { "content": null } }]
        }))
        .unwrap();
        assert!(empty[0].empty_response);
        assert_eq!(empty[0].finish_reason.as_deref(), Some("length"));
    }
}
