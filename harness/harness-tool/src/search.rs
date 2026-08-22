use std::sync::Arc;

use async_trait::async_trait;

use harness_capability::search::{Search, SearchRequest};
use harness_core::error::Result;
use harness_llm::{ToolCall, ToolResult};

use crate::registry::DynTool;

/// 搜索工具（Consumer）：仅依赖 `Arc<dyn Search>`。
///
/// 存在动机（取证）：缺少专用搜索通道时，模型用 shell findstr + 自造临时脚本
/// 定位代码（单回合 758 次 shell、100+ 个 `_probe_*.py`），是步数失控主因。
/// 本工具一次调用即可拿到“文件:行号:内容”的有界结果，直接替代上述模式。
pub struct SearchTool {
    search: Arc<dyn Search>,
}

/// 结果正文上限：防止大量命中撑爆上下文（超过则截断并提示细化条件）。
const MAX_OUTPUT_CHARS: usize = 4_000;

impl SearchTool {
    pub fn new(search: Arc<dyn Search>) -> Arc<dyn DynTool> {
        Arc::new(Self { search })
    }
}

#[async_trait]
impl DynTool for SearchTool {
    fn name(&self) -> &'static str {
        "search"
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let pattern = call
            .args
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if pattern.trim().is_empty() {
            return Ok(ToolResult {
                call_id: call.id.clone(),
                ok: false,
                content: "search.pattern 不能为空".into(),
                continuation_debt: 0,
            });
        }
        let dir = call
            .args
            .get("dir")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(std::path::PathBuf::from);
        let max_results = call
            .args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(60);

        let req = SearchRequest {
            pattern: pattern.clone(),
            dir: dir.clone(),
            max_results,
        };
        let hits = match self.search.grep(req).await {
            Ok(hits) => hits,
            Err(e) => {
                return Ok(ToolResult {
                    call_id: call.id.clone(),
                    ok: false,
                    content: format!("search 失败: {e}"),
                    continuation_debt: 0,
                });
            }
        };

        if hits.is_empty() {
            return Ok(ToolResult {
                call_id: call.id.clone(),
                ok: true,
                content: format!(
                    "未找到匹配（pattern=\"{pattern}\"{}）。建议：更换/缩短关键词，或去掉 dir 限定扩大范围；不要为此编写临时扫描脚本。",
                    dir.as_ref()
                        .map(|d| format!("，dir=\"{}\"", d.display()))
                        .unwrap_or_default()
                ),
                continuation_debt: 0,
            });
        }

        let mut out = format!("共 {} 条命中（格式：相对路径:行号: 内容）：\n", hits.len());
        let mut truncated = false;
        for hit in &hits {
            let line = format!("{}:{}: {}\n", hit.path.display(), hit.line, hit.text);
            if out.chars().count() + line.chars().count() > MAX_OUTPUT_CHARS {
                truncated = true;
                break;
            }
            out.push_str(&line);
        }
        if truncated {
            out.push_str("…（结果已截断：请补充 dir 或更精确的 pattern 缩小范围）");
        }
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content: out,
            continuation_debt: 0,
        })
    }
}
