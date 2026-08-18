//! DeepSeek 原生 DSML（DeepSeek Markup Language）工具调用解析器。
//!
//! DeepSeek-v4 系列模型在部分网关/模式下会把工具调用以 DSML 文本写进 `delta.content`：
//!
//! ```text
//! <｜DSML｜tool_calls>
//! <｜DSML｜invoke name="exec_command">
//! <｜DSML｜parameter name="cmd" string="true">dir</｜DSML｜parameter>
//! </｜DSML｜invoke>
//! </｜DSML｜tool_calls>
//! ```
//!
//! 若 harness 不解析，这些标记会被当作回复原文裸显且工具永不执行（会话质量事故根因）。
//! 本模块提供：
//! - [`DsmlFilter`]：增量状态机，跨 SSE 分片安全地切分「普通文本 / DSML 工具调用」；
//! - [`filter_stream`]：包装任意 Provider 的 `ChunkStream`，DSML 块转为 `ToolCall`；
//! - [`strip_dsml`]：渲染防御，移除完整 DSML 块与尾部不完整标记（旧日志回放用）。

use futures::StreamExt;
use serde_json::{json, Value};

use crate::{Chunk, ChunkStream, ToolCall};

/// 全宽竖线变体为规范形；ASCII 变体 `<|DSML|` 在入口归一化。
const OPEN: &str = "<｜DSML｜tool_calls>";
const CLOSE: &str = "</｜DSML｜tool_calls>";
const INVOKE_OPEN: &str = "<｜DSML｜invoke";
const INVOKE_CLOSE: &str = "</｜DSML｜invoke>";
const PARAM_OPEN: &str = "<｜DSML｜parameter";
const PARAM_CLOSE: &str = "</｜DSML｜parameter>";

/// 过滤器产出项：普通文本增量，或一个已解析的工具调用。
#[derive(Debug, Clone, PartialEq)]
pub enum DsmlItem {
    Text(String),
    Call(ToolCall),
}

/// DSML 增量解析状态机（Normal / Capturing 两态）。
#[derive(Default)]
pub struct DsmlFilter {
    /// Normal 态保留的「可能是标记前缀」的尾巴。
    buf: String,
    /// Capturing 态累积区（见到 OPEN 后直到 CLOSE）。
    capture: Option<String>,
    counter: u64,
}

impl DsmlFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂入一段文本增量，产出已确定的文本/工具调用序列。
    pub fn push(&mut self, text: &str) -> Vec<DsmlItem> {
        let mut out = Vec::new();
        let mut pending = normalize(text);
        loop {
            if self.capture.is_some() {
                let cap = self.capture.as_mut().unwrap();
                cap.push_str(&pending);
                drain_invokes(cap, &mut self.counter, &mut out);
                if let Some(pos) = cap.find(CLOSE) {
                    let tail = cap[pos + CLOSE.len()..].to_string();
                    self.capture = None;
                    pending = tail;
                    continue;
                }
                return out;
            }

            let mut s = std::mem::take(&mut self.buf);
            s.push_str(&pending);
            if let Some(pos) = s.find(OPEN) {
                if pos > 0 {
                    out.push(DsmlItem::Text(s[..pos].to_string()));
                }
                self.capture = Some(String::new());
                pending = s[pos + OPEN.len()..].to_string();
                continue;
            }
            let hold = hold_suffix(&s, OPEN);
            let emit_len = s.len() - hold;
            if emit_len > 0 {
                out.push(DsmlItem::Text(s[..emit_len].to_string()));
            }
            self.buf = s[emit_len..].to_string();
            return out;
        }
    }

    /// 流结束冲刷：闭合的 invoke 仍解析为工具调用；未闭合的 DSML 残块丢弃（不裸显）。
    pub fn finish(&mut self) -> Vec<DsmlItem> {
        let mut out = Vec::new();
        if let Some(mut cap) = self.capture.take() {
            drain_invokes(&mut cap, &mut self.counter, &mut out);
        }
        let buf = std::mem::take(&mut self.buf);
        let cleaned = strip_dsml(&buf);
        if !cleaned.is_empty() {
            out.push(DsmlItem::Text(cleaned));
        }
        out
    }
}

/// 包装任意 Provider 流：文本增量过 [`DsmlFilter`]，DSML 块转 `Chunk.tool_calls`；
/// 原生 `tool_calls` / `reasoning` 分片原样透传。
pub fn filter_stream(inner: ChunkStream) -> ChunkStream {
    Box::pin(async_stream::stream! {
        let mut filter = DsmlFilter::new();
        let mut inner = inner;
        while let Some(item) = inner.next().await {
            match item {
                Ok(chunk) => {
                    if let Some(text) = &chunk.text {
                        for out in filter.push(text) {
                            yield item_from(out);
                        }
                    }
                    if !chunk.tool_calls.is_empty() {
                        yield Ok(Chunk { tool_calls: chunk.tool_calls.clone(), ..Default::default() });
                    }
                    if chunk.reasoning.is_some() {
                        yield Ok(Chunk { reasoning: chunk.reasoning.clone(), ..Default::default() });
                    }
                    // 用量帧（只带 usage）必须透传，否则 AIOps 计量数据被静默丢弃。
                    if chunk.usage.is_some() {
                        yield Ok(Chunk { usage: chunk.usage.clone(), ..Default::default() });
                    }
                }
                Err(e) => yield Err(e),
            }
        }
        for out in filter.finish() {
            yield item_from(out);
        }
    })
}

fn item_from(item: DsmlItem) -> crate::Result<Chunk> {
    Ok(match item {
        DsmlItem::Text(t) => Chunk {
            text: Some(t),
            ..Default::default()
        },
        DsmlItem::Call(c) => Chunk {
            tool_calls: vec![c],
            ..Default::default()
        },
    })
}

/// 渲染防御：移除完整 DSML 块；未闭合的 OPEN 及其尾部不完整标记前缀截断丢弃。
pub fn strip_dsml(text: &str) -> String {
    let mut s = normalize(text);
    loop {
        if let Some(start) = s.find(OPEN) {
            if let Some(rel) = s[start..].find(CLOSE) {
                let end = start + rel + CLOSE.len();
                s.replace_range(start..end, "");
                continue;
            }
            s.truncate(start);
        }
        break;
    }
    let hold = hold_suffix(&s, OPEN);
    s.truncate(s.len() - hold);
    s
}

/// ASCII 变体归一化为全宽竖线规范形（开/闭标记都要覆盖，否则混合变体的闭合标签无法匹配）。
fn normalize(text: &str) -> String {
    text.replace("</|DSML|", "</｜DSML｜")
        .replace("<|DSML|", "<｜DSML｜")
}

/// 返回 `s` 尾部「是 marker 真前缀」的最长后缀字节长度（按字符边界枚举，防 UTF-8  panic）。
fn hold_suffix(s: &str, marker: &str) -> usize {
    let mut best = 0;
    for (i, _) in s.char_indices() {
        let suffix = &s[i..];
        if suffix.len() < marker.len() && marker.starts_with(suffix) {
            best = suffix.len();
        }
    }
    best
}

/// 从累积区头部反复取走完整的 invoke 块并解析产出。
fn drain_invokes(cap: &mut String, counter: &mut u64, out: &mut Vec<DsmlItem>) {
    loop {
        let Some(start) = cap.find(INVOKE_OPEN) else {
            return;
        };
        let Some(rel_end) = cap[start..].find(INVOKE_CLOSE) else {
            return;
        };
        let end = start + rel_end + INVOKE_CLOSE.len();
        let block = cap[start..end].to_string();
        if let Some(call) = parse_invoke(&block, counter) {
            out.push(DsmlItem::Call(call));
        }
        cap.drain(..end);
    }
}

/// 解析单个 invoke 块：`name="X"` + 若干 parameter 标签 → 映射后的 [`ToolCall`]。
fn parse_invoke(block: &str, counter: &mut u64) -> Option<ToolCall> {
    let rest = block.strip_prefix(INVOKE_OPEN)?;
    let name = attr_value(rest, "name")?;
    let mut map = serde_json::Map::new();

    let mut idx = 0;
    while let Some(rel) = rest[idx..].find(PARAM_OPEN) {
        let tag_start = idx + rel;
        let Some(gt_rel) = rest[tag_start..].find('>') else { break };
        let tag = &rest[tag_start..tag_start + gt_rel + 1];
        let val_start = tag_start + gt_rel + 1;
        let Some(close_rel) = rest[val_start..].find(PARAM_CLOSE) else { break };
        let value = &rest[val_start..val_start + close_rel];
        idx = val_start + close_rel + PARAM_CLOSE.len();

        let Some(pname) = attr_value(tag, "name") else {
            continue;
        };
        let is_str = attr_value(tag, "string").unwrap_or_else(|| "true".into()) == "true";
        let v = if is_str {
            Value::String(value.to_string())
        } else {
            serde_json::from_str(value.trim()).unwrap_or_else(|_| Value::String(value.to_string()))
        };
        map.insert(pname, v);
    }

    *counter += 1;
    Some(map_native(&name, Value::Object(map), format!("dsml-{counter}")))
}

/// 取标签内 `key="..."` 属性值。
fn attr_value(tag: &str, key: &str) -> Option<String> {
    let key_pat = format!("{key}=\"");
    let start = tag.find(&key_pat)? + key_pat.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

/// 原生 DSML 工具名 → harness 注册表工具名/参数形态映射；未知名透传（registry 报错让模型自纠）。
fn map_native(name: &str, args: Value, id: String) -> ToolCall {
    let get = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| args.get(*k).cloned())
    };
    let as_text = |v: Option<Value>| match v {
        Some(Value::String(s)) => s,
        Some(other) => other.to_string(),
        None => String::new(),
    };
    match name {
        "exec_command" | "run_command" | "run_shell_command" | "bash" | "shell" => ToolCall {
            id,
            name: "shell".into(),
            args: json!({ "command": as_text(get(&["cmd", "command"])) }),
        },
        "read_file" => ToolCall {
            id,
            name: "fs".into(),
            args: json!({ "op": "read", "path": as_text(get(&["path", "file_path"])) }),
        },
        "write_file" | "create_file" => ToolCall {
            id,
            name: "fs".into(),
            args: json!({
                "op": "write",
                "path": as_text(get(&["path", "file_path"])),
                "content": get(&["content", "text"]).unwrap_or(Value::String(String::new())),
            }),
        },
        "list_directory" | "list_files" => ToolCall {
            id,
            name: "fs".into(),
            args: json!({ "op": "list", "path": as_text(get(&["path", "directory"])) }),
        },
        "edit_file" | "str_replace" | "str_replace_editor" | "replace" => ToolCall {
            id,
            name: "edit".into(),
            args: json!({
                "path": as_text(get(&["path", "file_path"])),
                "old_text": as_text(get(&["old_text", "old_str", "old_string", "original_text"])),
                "new_text": as_text(get(&["new_text", "new_str", "new_string"])),
            }),
        },
        // 已是 harness 工具名或未知名：原样透传。
        _ => ToolCall {
            id,
            name: name.to_string(),
            args,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(items: &[DsmlItem]) -> Vec<ToolCall> {
        items
            .iter()
            .filter_map(|i| match i {
                DsmlItem::Call(c) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn parses_block_split_across_frames() {
        let full = "<｜DSML｜tool_calls>\n<｜DSML｜invoke name=\"exec_command\">\n<｜DSML｜parameter name=\"cmd\" string=\"true\">dir /b</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜tool_calls>尾部文本";
        let mut f = DsmlFilter::new();
        let mut all = Vec::new();
        // 逐字符喂入，验证跨帧截断安全（标记含多字节竖线）。
        for ch in full.chars() {
            all.extend(f.push(&ch.to_string()));
        }
        all.extend(f.finish());
        let c = calls(&all);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "shell");
        assert_eq!(c[0].args["command"], "dir /b");
        let text: String = all
            .iter()
            .filter_map(|i| match i {
                DsmlItem::Text(t) => Some(t.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "尾部文本");
    }

    #[test]
    fn maps_native_names_and_json_params() {
        let block = "<｜DSML｜tool_calls><｜DSML｜invoke name=\"exec_command\"><｜DSML｜parameter name=\"cmd\" string=\"true\">ls</｜DSML｜parameter><｜DSML｜parameter name=\"max_output_tokens\" string=\"false\">20000</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>";
        let mut f = DsmlFilter::new();
        let items = f.push(block);
        let c = calls(&items);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "shell");
        assert!(c[0].id.starts_with("dsml-"));
    }

    #[test]
    fn read_file_maps_to_fs_read() {
        let block = "<｜DSML｜tool_calls><｜DSML｜invoke name=\"read_file\"><｜DSML｜parameter name=\"path\" string=\"true\">a.txt</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>";
        let mut f = DsmlFilter::new();
        let c = calls(&f.push(block));
        assert_eq!(c[0].name, "fs");
        assert_eq!(c[0].args["op"], "read");
        assert_eq!(c[0].args["path"], "a.txt");
    }

    #[test]
    fn holds_partial_marker_suffix() {
        let mut f = DsmlFilter::new();
        let items = f.push("hello <｜DS");
        assert_eq!(items, vec![DsmlItem::Text("hello ".into())]);
        // 后续帧补全标记 → 进入捕获，不再吐出标记碎片。
        let more = f.push("ML｜tool_calls><｜DSML｜invoke name=\"shell\"><｜DSML｜parameter name=\"command\" string=\"true\">pwd</｜DSML｜parameter></｜DSML｜invoke></｜DSML｜tool_calls>");
        assert_eq!(calls(&more).len(), 1);
    }

    #[test]
    fn ascii_variant_normalized() {
        let block = "<|DSML|tool_calls><|DSML|invoke name=\"exec_command\"><|DSML|parameter name=\"cmd\" string=\"true\">echo hi</|DSML|parameter></｜DSML｜invoke></｜DSML｜tool_calls>";
        let mut f = DsmlFilter::new();
        let c = calls(&f.push(block));
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].args["command"], "echo hi");
    }

    #[test]
    fn strip_dsml_removes_blocks_and_trailing_partial() {
        let s = "前文<｜DSML｜tool_calls><｜DSML｜invoke name=\"x\"></｜DSML｜invoke></｜DSML｜tool_calls>后文<｜DSML｜too";
        assert_eq!(strip_dsml(s), "前文后文");
    }

    #[test]
    fn plain_text_passes_through() {
        let mut f = DsmlFilter::new();
        let items = f.push("普通对话，没有工具。");
        assert_eq!(items, vec![DsmlItem::Text("普通对话，没有工具。".into())]);
        assert!(f.finish().is_empty());
    }
}
