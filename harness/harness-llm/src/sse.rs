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
/// - 产出 `Ok(Some(data))` = 携带负载的数据事件；`Ok(None)` = **心跳/保活帧**
///   （注释行、`event: ping`、无 `data:` 的空事件）。心跳不含模型内容，但它证明
///   “服务端仍在处理本请求、连接仍然存活”，调用方的空闲看门狗必须用它重置计时；
///   否则在长推理期间持续发注释心跳的服务端（如 DeepSeek）会被误判为无响应而中断。
pub fn sse_events(resp: reqwest::Response) -> impl Stream<Item = crate::Result<Option<String>>> {
    async_stream::stream! {
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(bytes) => {
                    for frame in push_chunk(&mut buf, &bytes) {
                        yield Ok(frame);
                    }
                }
                Err(e) => {
                    yield Err(Error::Llm(format!("SSE 读取失败: {e}")));
                    return;
                }
            }
        }
        // 冲刷残余（有的服务端关流时不带结尾空行）。
        if let Some(data) = flush_tail(&buf) {
            yield Ok(Some(data));
        }
    }
}

/// 把一个响应体 chunk 追加进缓冲区，并产出其中已完整的事件帧。
///
/// 返回项 `Some(data)` = 数据帧；`None` = **心跳帧**（注释行、`event: ping`、
/// 无 `data:` 的空事件）。心跳不含模型内容，但证明“服务端仍在处理本请求、连接
/// 仍然存活”，调用方的空闲看门狗必须据此重置计时；否则长推理期间持续发注释心跳
/// 的服务端（如 DeepSeek）会被误判为无响应而中断。
fn push_chunk(buf: &mut String, bytes: &[u8]) -> Vec<Option<String>> {
    // 跨 chunk 的 `\r\n` 处理：上一块以 `\r` 结尾时延迟到本块判定。
    if buf.ends_with('\r') {
        if bytes.first() == Some(&b'\n') {
            buf.pop();
        } else {
            buf.push('\n');
        }
    }
    buf.push_str(&String::from_utf8_lossy(bytes).replace("\r\n", "\n"));
    drain_events(buf)
        .into_iter()
        .map(|raw| {
            let data = event_data(&raw);
            if data.is_empty() { None } else { Some(data) }
        })
        .collect()
}

/// 流结束时的残余：只有真正带负载的残帧才产出，末尾不补心跳。
fn flush_tail(buf: &str) -> Option<String> {
    let data = event_data(buf);
    if data.is_empty() { None } else { Some(data) }
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

    /// 心跳帧必须真的产出 `None`：调用方的空闲看门狗靠它重置计时。
    #[test]
    fn comment_ping_and_empty_events_are_heartbeat_frames() {
        let mut buf = String::new();
        assert_eq!(push_chunk(&mut buf, b": keep-alive\n\n"), vec![None]);
        assert_eq!(push_chunk(&mut buf, b"event: ping\n\n"), vec![None]);
        assert_eq!(push_chunk(&mut buf, b"\n\n"), vec![None]);
        assert_eq!(buf, "", "完整事件应已全部从缓冲区取走");
    }

    /// 数据帧与心跳帧可以同批产出，且未闭合的事件不得提前产出。
    #[test]
    fn incomplete_events_wait_and_then_data_and_heartbeat_coexist() {
        let mut buf = String::new();
        assert!(push_chunk(&mut buf, b"data: {\"a\"").is_empty());
        assert_eq!(
            push_chunk(&mut buf, b":1}\n\n: ping\n\n"),
            vec![Some("{\"a\":1}".to_string()), None]
        );
    }

    /// CRLF 跨 chunk 切开时仍要归一化成一个事件边界。
    #[test]
    fn crlf_split_across_chunks_is_normalized() {
        let mut buf = String::new();
        assert!(push_chunk(&mut buf, b"data: a\r").is_empty());
        assert_eq!(push_chunk(&mut buf, b"\n\r\n"), vec![Some("a".to_string())]);
    }

    /// 关流时的残余冲刷只补数据帧，末尾不补心跳。
    #[test]
    fn tail_flush_never_emits_a_heartbeat() {
        assert_eq!(flush_tail(": keep-alive\n"), None);
        assert_eq!(flush_tail("data: {\"a\":1}"), Some("{\"a\":1}".to_string()));
    }
}
