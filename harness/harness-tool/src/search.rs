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

/// 作用域阶梯：给定 dir 起，逐级升到父目录（对应 crate 边界），最后到全工作区（None）。
/// 升级在工具内完成，模型不再自行猜 scope（spec §4.6）。
fn scope_ladder(dir: Option<&std::path::Path>) -> Vec<Option<std::path::PathBuf>> {
    let mut ladder = vec![dir.map(std::path::PathBuf::from)];
    let mut cur = dir;
    while let Some(d) = cur {
        cur = d.parent().filter(|p| !p.as_os_str().is_empty());
        ladder.push(cur.map(std::path::PathBuf::from));
    }
    ladder.dedup();
    ladder
}

fn scope_label(scope: &Option<std::path::PathBuf>) -> String {
    match scope {
        Some(d) => format!("dir=\"{}\"", d.display()),
        None => "全工作区".into(),
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

        let scopes = scope_ladder(dir.as_deref());
        let mut tried: Vec<String> = vec![];
        let mut hits = vec![];
        for scope in scopes {
            tried.push(scope_label(&scope));
            let req = SearchRequest {
                pattern: pattern.clone(),
                dir: scope.clone(),
                max_results,
            };
            match self.search.grep(req).await {
                Ok(h) if h.is_empty() => continue,
                Ok(h) => {
                    hits = h;
                    break;
                }
                Err(e) => {
                    return Ok(ToolResult {
                        call_id: call.id.clone(),
                        ok: false,
                        content: format!("search 失败: {e}"),
                        continuation_debt: 0,
                    });
                }
            }
        }

        if hits.is_empty() {
            return Ok(ToolResult {
                call_id: call.id.clone(),
                ok: true,
                content: format!(
                    "未找到匹配（pattern=\"{pattern}\"）。已试范围：{}。建议：更换/缩短关键词，或改用符号名；不要为此编写临时扫描脚本。",
                    tried.join(" → ")
                ),
                continuation_debt: 0,
            });
        }

        let mut out = String::new();
        if tried.len() > 1 {
            out.push_str(&format!("（scope 自动升级：{}）\n", tried.join(" → ")));
        }
        out.push_str(&format!(
            "共 {} 条命中（格式：相对路径:行号: 内容）：\n",
            hits.len()
        ));
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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use harness_capability::search::{Search, SearchHit, SearchRequest};
    use harness_core::error::Result;
    use harness_llm::{ToolCall, ToolResult};

    use super::SearchTool;
    use crate::registry::DynTool;

    /// 前 `empty_calls` 次 grep 返回空，之后返回单条命中；记录每次请求的 dir。
    struct ScriptedSearch {
        empty_calls: usize,
        calls: Mutex<usize>,
        dirs: Mutex<Vec<Option<std::path::PathBuf>>>,
    }

    #[async_trait]
    impl Search for ScriptedSearch {
        async fn grep(&self, req: SearchRequest) -> Result<Vec<SearchHit>> {
            let n = {
                let mut c = self.calls.lock().unwrap();
                *c += 1;
                *c
            };
            self.dirs.lock().unwrap().push(req.dir.clone());
            if n <= self.empty_calls {
                Ok(vec![])
            } else {
                Ok(vec![SearchHit {
                    path: std::path::PathBuf::from("crate-b/src/lib.rs"),
                    line: 7,
                    text: "found".into(),
                }])
            }
        }
    }

    async fn run_search(tool: &Arc<dyn DynTool>, dir: Option<&str>) -> ToolResult {
        let mut args = serde_json::json!({"pattern": "GitCli"});
        if let Some(d) = dir {
            args["dir"] = serde_json::Value::String(d.into());
        }
        tool.call(&ToolCall {
            id: "c1".into(),
            name: "search".into(),
            args,
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn first_scope_hit_does_not_escalate() {
        let s = Arc::new(ScriptedSearch {
            empty_calls: 0,
            calls: Mutex::new(0),
            dirs: Mutex::new(vec![]),
        });
        let tool = SearchTool::new(s.clone());
        let res = run_search(&tool, Some("crate-a/src")).await;
        assert!(res.ok);
        assert_eq!(*s.dirs.lock().unwrap(), vec![Some("crate-a/src".into())]);
        assert!(!res.content.contains("scope 自动升级"));
    }

    #[tokio::test]
    async fn empty_dir_escalates_to_parent_then_workspace() {
        let s = Arc::new(ScriptedSearch {
            empty_calls: 2, // dir 与父级都空，第三级（工作区）命中
            calls: Mutex::new(0),
            dirs: Mutex::new(vec![]),
        });
        let tool = SearchTool::new(s.clone());
        let res = run_search(&tool, Some("workspace/crate-a/src")).await;
        assert!(res.ok);
        // 阶梯 = dir → 父级 → 再父级（相对路径逐级 parent）→ 全工作区；
        // 前两级空，第三级（dir="workspace"）命中，共发出 3 次请求。
        assert_eq!(
            *s.dirs.lock().unwrap(),
            vec![
                Some("workspace/crate-a/src".into()),
                Some("workspace/crate-a".into()),
                Some("workspace".into()),
            ]
        );
        assert!(res.content.contains("scope 自动升级"), "{}", res.content);
        assert!(
            res.content.contains("crate-b/src/lib.rs:7"),
            "{}",
            res.content
        );
    }

    #[tokio::test]
    async fn all_scopes_empty_reports_tried_ladder() {
        let s = Arc::new(ScriptedSearch {
            empty_calls: usize::MAX,
            calls: Mutex::new(0),
            dirs: Mutex::new(vec![]),
        });
        let tool = SearchTool::new(s.clone());
        let res = run_search(&tool, Some("a/b/c")).await;
        assert!(res.ok);
        assert!(res.content.contains("已试范围"), "{}", res.content);
        assert!(res.content.contains("dir=\"a/b/c\""), "{}", res.content);
        assert!(res.content.contains("全工作区"), "{}", res.content);
        // 不再要求模型猜下一层：建议里不再出现「去掉 dir 限定」这种手动升级指引
        assert!(!res.content.contains("去掉 dir 限定"), "{}", res.content);
    }

    #[tokio::test]
    async fn no_dir_starts_at_workspace_scope_once() {
        let s = Arc::new(ScriptedSearch {
            empty_calls: usize::MAX,
            calls: Mutex::new(0),
            dirs: Mutex::new(vec![]),
        });
        let tool = SearchTool::new(s.clone());
        let res = run_search(&tool, None).await;
        assert!(res.ok);
        assert_eq!(*s.dirs.lock().unwrap(), vec![None]);
    }
}
