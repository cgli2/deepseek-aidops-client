use std::sync::Arc;

use async_trait::async_trait;
use harness_core::error::Result;
use harness_llm::{ToolCall, ToolResult};
use harness_session::{PlanItem, SessionEvent, SessionLog};

use crate::registry::DynTool;

/// 模型可见的 `plan` 工具：发布/更新结构化任务计划（长周期任务规划能力）。
///
/// 计划以 `PlanUpdate` 事件写入会话日志（真相源），GUI 渲染为计划气泡；
/// 返回值带编号回显，便于模型在后续步骤对齐进度。
pub struct PlanTool {
    log: Arc<SessionLog>,
}

impl PlanTool {
    pub fn new(log: Arc<SessionLog>) -> Arc<Self> {
        Arc::new(Self { log })
    }
}

#[async_trait]
impl DynTool for PlanTool {
    fn name(&self) -> &'static str {
        "plan"
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let items = parse_items(call);
        if items.is_empty() {
            return Ok(ToolResult {
                call_id: call.id.clone(),
                ok: false,
                content: "plan 需要非空的 items 数组（每项含 text）".into(),
                continuation_debt: 0,
            });
        }
        self.log.append(SessionEvent::PlanUpdate {
            id: self.log.gen_id(),
            items: items.clone(),
        });
        let mut content = String::from("计划已发布/更新：\n");
        for (i, item) in items.iter().enumerate() {
            content.push_str(&format!("{}. [{}] {}\n", i + 1, item.status, item.text));
        }
        content.push_str("完成每步后请再次调用 plan 更新状态。");
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content,
            continuation_debt: 0,
        })
    }
}

/// 从工具参数解析计划条目。模型的 `done` 必须降级为 `claimed_done`；真实
/// `verified` 仅能由 Runtime 在 Delivery 事件中给出，不能经 tool 参数伪造。
fn parse_items(call: &ToolCall) -> Vec<PlanItem> {
    let Some(arr) = call.args.get("items").and_then(|v| v.as_array()) else {
        return vec![];
    };
    arr.iter()
        .enumerate()
        .filter_map(|(index, v)| {
            let text = v.get("text").and_then(|t| t.as_str())?.trim().to_string();
            if text.is_empty() {
                return None;
            }
            let status = match v.get("status").and_then(|s| s.as_str()) {
                Some("doing") => "doing",
                Some("done") | Some("verified") => "claimed_done",
                Some("blocked") => "blocked",
                _ => "pending",
            }
            .to_string();
            let id = v
                .get("id")
                .and_then(|id| id.as_str())
                .filter(|id| !id.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("plan-{}", index + 1));
            let criterion_ids = v
                .get("criterion_ids")
                .or_else(|| v.get("criteria"))
                .and_then(|items| items.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .filter(|item| !item.trim().is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let evidence = v
                .get("evidence")
                .and_then(|items| items.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .filter(|item| !item.trim().is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            Some(PlanItem {
                id,
                text,
                status,
                criterion_ids,
                evidence,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn plan_writes_event_and_echoes() {
        let log = SessionLog::new();
        let tool = PlanTool::new(log.clone());
        let call = ToolCall {
            id: "c1".into(),
            name: "plan".into(),
            args: json!({"items":[{"text":"读代码","status":"doing"},{"text":"写总结"}]}),
        };
        let res = tool.call(&call).await.unwrap();
        assert!(res.ok);
        assert!(res.content.contains("[doing] 读代码"));
        assert!(res.content.contains("[pending] 写总结"));
        let has_plan = log
            .replay()
            .iter()
            .any(|ev| matches!(ev, SessionEvent::PlanUpdate { .. }));
        assert!(has_plan);
    }

    #[tokio::test]
    async fn empty_items_rejected() {
        let tool = PlanTool::new(SessionLog::new());
        let call = ToolCall {
            id: "c2".into(),
            name: "plan".into(),
            args: json!({"items":[]}),
        };
        assert!(!tool.call(&call).await.unwrap().ok);
    }

    #[tokio::test]
    async fn model_done_is_recorded_as_claim_not_verified() {
        let log = SessionLog::new();
        let tool = PlanTool::new(log.clone());
        let call = ToolCall {
            id: "c3".into(),
            name: "plan".into(),
            args: json!({"items":[{
                "id":"fix",
                "text":"修复问题",
                "status":"done",
                "criterion_ids":["item-1"],
                "evidence":["模型声明已修改"]
            }]}),
        };
        assert!(tool.call(&call).await.unwrap().ok);
        assert!(matches!(
            log.replay().last(),
            Some(SessionEvent::PlanUpdate { items, .. })
                if items[0].id == "fix"
                    && items[0].status == "claimed_done"
                    && items[0].criterion_ids == ["item-1"]
        ));
    }
}
