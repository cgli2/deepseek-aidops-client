use std::sync::Arc;

use async_trait::async_trait;
use harness_capability::subagent::Subagent;
use harness_core::error::Result;
use harness_llm::{ToolCall, ToolResult};

use crate::registry::DynTool;

/// 模型可见的 `delegate` 工具：把耗时/独立子任务委托给子代理执行。
///
/// 仅依赖能力接缝 Definition `Arc<dyn Subagent>`（三角色模式，不变量 2）：
/// 换 Provider（进程内 / fork / ACP）源码零改动。主回合不被子任务细节阻塞。
pub struct DelegateTool {
    sub: Arc<dyn Subagent>,
}

impl DelegateTool {
    pub fn new(sub: Arc<dyn Subagent>) -> Arc<Self> {
        Arc::new(Self { sub })
    }
}

#[async_trait]
impl DynTool for DelegateTool {
    fn name(&self) -> &'static str {
        "delegate"
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let task = call
            .args
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .trim()
            .to_string();
        if task.is_empty() {
            return Ok(ToolResult {
                call_id: call.id.clone(),
                ok: false,
                content: "delegate 需要非空的 task 描述".into(),
                continuation_debt: 0,
            });
        }
        match self.sub.spawn(&task).await {
            Ok(answer) => Ok(ToolResult {
                call_id: call.id.clone(),
                ok: true,
                content: format!("[子代理结果]\n{answer}"),
                continuation_debt: 0,
            }),
            Err(e) => Ok(ToolResult {
                call_id: call.id.clone(),
                ok: false,
                content: format!("子代理执行失败: {e}"),
                continuation_debt: 0,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct EchoSub;

    #[async_trait]
    impl Subagent for EchoSub {
        async fn spawn(&self, task: &str) -> Result<String> {
            Ok(format!("done: {task}"))
        }
    }

    #[tokio::test]
    async fn delegates_task_to_subagent() {
        let tool = DelegateTool::new(Arc::new(EchoSub));
        let call = ToolCall {
            id: "d1".into(),
            name: "delegate".into(),
            args: json!({"task":"统计文件数"}),
        };
        let res = tool.call(&call).await.unwrap();
        assert!(res.ok);
        assert!(res.content.contains("done: 统计文件数"));
    }

    #[tokio::test]
    async fn empty_task_rejected() {
        let tool = DelegateTool::new(Arc::new(EchoSub));
        let call = ToolCall {
            id: "d2".into(),
            name: "delegate".into(),
            args: json!({"task":"  "}),
        };
        assert!(!tool.call(&call).await.unwrap().ok);
    }
}
