//! SSE（Server-Sent Events）流解析，HTTP Provider 共用。
//!
//! OpenAI 兼容（DeepSeek/OpenAI/llama.cpp）与 Anthropic 的流式响应都是 SSE：
//! 事件以空行分隔，`data:` 字段携带负载。本模块把响应体字节流解析为逐条 `data:`
//! 文本负载的流；负载 JSON 的业务解析由各 Provider 自行完成。

use futures::{Stream, StreamExt};
use harness_core::error::Error;

/// 把 SSE 响应体解析为「每个事件的 `data:` 字段内容」流。
///
/// - 事件以空行（`\n\n`，CRLF 已归一化）分隔；
/// - 多行 `data:` 字段按 `\n` 连接；注释行（`:` 开头）与 `event:`/`id:` 字段在此忽略。
pub fn sse_events(resp: reqwest::Response) -> impl Stream<Item = crate::Result<String>> {
    async_stream::stream! {
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(bytes) => {
                    // 跨 chunk 的 `\r\n` 处理：上一块以 `\r` 结尾时延迟到本块判定。
                    if buf.ends_with('\r') {
                        if bytes.first() == Some(&b'\n') {
                            buf.pop();
                        } else {
                            buf.push('\n');
                        }
                    }
                    buf.push_str(&String::from_utf8_lossy(&bytes).replace("\r\n", "\n"));
                    for raw in drain_events(&mut buf) {
                        let data = event_data(&raw);
                        if !data.is_empty() {
                            yield Ok(data);
                        }
                    }
                }
                Err(e) => {
                    yield Err(Error::Llm(format!("SSE 读取失败: {e}")));
                    return;
                }
            }
        }
        // 冲刷残余（有的服务端关流时不带结尾空行）。
        let data = event_data(&buf);
        if !data.is_empty() {
            yield Ok(data);
        }
    }
}

/// 从缓冲区头部取走所有已完整（以空行结束）的事件原文。
fn drain_events(buf: &mut String) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(pos) = buf.find("\n\n") {
        let raw: String = buf.drain(..pos + 2).collect();
        out.push(raw);
    }
    out
}

/// 提取一个 SSE 事件的 `data:` 字段（多行用 `\n` 连接，去首尾空白）。
fn event_data(raw: &str) -> String {
    let mut parts = Vec::new();
    for line in raw.lines() {
        if let Some(payload) = line.strip_prefix("data:") {
            parts.push(payload.trim());
        }
    }
    parts.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drains_complete_events_only() {
        let mut buf = String::from("data: a\n\ndata: b\n\ndata: partial");
        let events = drain_events(&mut buf);
        assert_eq!(events.len(), 2);
        assert_eq!(buf, "data: partial");
    }

    #[test]
    fn extracts_multiline_data_fields() {
        let raw = "event: message\n: comment\ndata: {\"x\":\ndata:  1}\n\n";
        assert_eq!(event_data(raw), "{\"x\":\n1}");
    }

    #[test]
    fn ignores_events_without_data() {
        assert_eq!(event_data("event: ping\n\n"), "");
    }
}
