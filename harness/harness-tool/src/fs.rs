use std::sync::Arc;

use async_trait::async_trait;

use harness_capability::fs::Fs;
use harness_core::error::Result;
use harness_llm::{ToolCall, ToolResult};

use crate::registry::DynTool;

/// FS 工具（Consumer）：仅依赖 `Arc<dyn Fs>`。
pub struct FsTool {
    fs: Arc<dyn Fs>,
}

/// 无区间参数时的默认视图行数：大文件只给首部窗口 + 总行数提示，
/// 引导模型用 start_line/end_line 按需读，而不是整文件灌进上下文。
/// 取证：旧实现整读大文件被上下文压缩截断，模型看不到目标区域，
/// 转而自造临时脚本按行截取（单回合写了 7 次 _extract.py）。
const DEFAULT_VIEW_LINES: usize = 250;
/// 单次区间读取的最大行数：与上下文压缩阈值匹配，保证请求的区间基本不被二次截断。
const MAX_RANGE_LINES: usize = 250;

impl FsTool {
    pub fn new(fs: Arc<dyn Fs>) -> Arc<dyn DynTool> {
        Arc::new(Self { fs })
    }
}

/// 把整文件内容切成「带元信息头/尾的视图」：
/// - 指定 start_line/end_line（1 基，含端点）时返回该区间（限宽 `MAX_RANGE_LINES`）；
/// - 未指定且超过 `DEFAULT_VIEW_LINES` 行时返回首部窗口；
/// - 头部始终标注总行数与当前展示区间，尾部提示如何读剩余部分。
fn slice_view(content: &str, start_line: Option<usize>, end_line: Option<usize>) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let (from, to, clamped) = match (start_line, end_line) {
        (Some(s), e) => {
            let s = s.max(1);
            let e = e.unwrap_or(total).min(total).max(s);
            let e = if e - s + 1 > MAX_RANGE_LINES {
                (s + MAX_RANGE_LINES - 1, true)
            } else {
                (e, false)
            };
            (s, e.0, e.1 || s > total)
        }
        (None, Some(e)) => {
            let e = e.min(total);
            let s = e.saturating_sub(MAX_RANGE_LINES).max(1);
            (s, e, e > MAX_RANGE_LINES)
        }
        (None, None) if total > DEFAULT_VIEW_LINES => (1, DEFAULT_VIEW_LINES, true),
        (None, None) => (1, total, false),
    };
    let from = from.min(total.max(1));
    let to = to.min(total);
    let mut out = format!("[文件共 {total} 行，当前显示 {from}-{to} 行]\n");
    for line in &lines[from.saturating_sub(1)..to] {
        out.push_str(line);
        out.push('\n');
    }
    if to < total {
        out.push_str(&format!(
            "…[还有 {} 行未显示：用 start_line/end_line 参数按需读取，不要写临时脚本截取]",
            total - to
        ));
    } else if clamped {
        out.push_str("…[区间超出文件末尾，已截断]");
    }
    out
}

#[async_trait]
impl DynTool for FsTool {
    fn name(&self) -> &'static str {
        "fs"
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let op = call
            .args
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let path = call
            .args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if path.trim().is_empty() {
            return Ok(ToolResult {
                call_id: call.id.clone(),
                ok: false,
                content: "fs.path 不能为空".into(),
                continuation_debt: 0,
            });
        }
        let p = std::path::Path::new(&path);

        let content = match op.as_str() {
            "read" => {
                let raw = self.fs.read(p).await?;
                let start_line = call
                    .args
                    .get("start_line")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let end_line = call
                    .args
                    .get("end_line")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                slice_view(&raw, start_line, end_line)
            }
            "write" => {
                let body = call
                    .args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.fs.write(p, &body).await?;
                String::new()
            }
            "list" => {
                let items = self.fs.list(p).await?;
                items
                    .iter()
                    .map(|x| x.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            other => {
                return Ok(ToolResult {
                    call_id: call.id.clone(),
                    ok: false,
                    content: format!("unknown fs op: {other}"),
                    continuation_debt: 0,
                });
            }
        };

        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content,
            continuation_debt: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(n: usize) -> String {
        (1..=n).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn small_file_returns_in_full() {
        let view = slice_view(&text(10), None, None);
        assert!(view.contains("文件共 10 行，当前显示 1-10 行"));
        assert!(view.contains("line10"));
        assert!(!view.contains("还有"));
    }

    #[test]
    fn large_file_defaults_to_head_window_with_hint() {
        let view = slice_view(&text(1000), None, None);
        assert!(view.contains("当前显示 1-250 行"));
        assert!(view.contains("还有 750 行未显示"));
        assert!(!view.contains("line251\n"));
    }

    #[test]
    fn explicit_range_is_respected_and_bounded() {
        let view = slice_view(&text(1000), Some(300), Some(400));
        assert!(view.contains("当前显示 300-400 行"));
        assert!(view.contains("line300"));
        assert!(view.contains("line400"));
        // 超宽区间被限宽到 MAX_RANGE_LINES。
        let wide = slice_view(&text(1000), Some(1), Some(1000));
        assert!(wide.contains("当前显示 1-250 行"));
    }
}
