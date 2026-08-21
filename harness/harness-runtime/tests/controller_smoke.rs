//! 冒烟测试：直接驱动 `SessionController::submit`（不依赖 GUI），验证 turn 是否真的写入 `SessionLog`。
//! 若通过，则"发送之后啥也没有"是 GUI 显示层的问题（滚动/可见性）；若失败，则是核心逻辑问题。

use std::sync::Arc;
use std::time::Duration;

use harness_capability::hook::{Hook, HookDecision, HookPayload};
use harness_core::AppContext;
use harness_core::error::Result;
use harness_core::ui_input::UiInputSink;
use harness_llm::{Chunk, LlmProvider, ReplayLlm};
use harness_runtime::SessionController;
use harness_session::{SessionEvent, SessionLog};
use harness_tool::ToolRegistry;

struct NoopHook;
impl Hook for NoopHook {
    fn run(&self, _: &HookPayload) -> Result<HookDecision> {
        Ok(HookDecision::Allow)
    }
}

#[tokio::test]
async fn submit_writes_turn_events() {
    let ctx = AppContext::new();
    let log = SessionLog::new();
    // 必须持有 Registration：其 Drop 会从 TypeMap 移除服务（可逆注册）。
    let mut _regs = Vec::new();
    _regs.push(ctx.provide(log.clone()));

    let llm: Arc<dyn LlmProvider> = ReplayLlm::new(vec![Chunk {
        text: Some("hi there from replay".into()),
        tool_calls: vec![],
        reasoning: None,
        usage: None,
        ..Default::default()
    }]);
    _regs.push(ctx.provide(llm));

    let tools = ToolRegistry::new();
    _regs.push(ctx.provide(tools));

    let hook: Arc<dyn Hook> = Arc::new(NoopHook);
    _regs.push(ctx.provide(hook));

    let ctrl = SessionController::new(ctx.clone(), tokio::runtime::Handle::current());
    ctrl.submit("hello".into());
    ctrl.submit("queued follow-up".into());
    assert!(ctrl.busy());

    // 轮询等待 turn 结束（最多 2s）。
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let evs = log.replay();
        if evs
            .iter()
            .filter(|e| matches!(e, SessionEvent::TurnEnd { .. }))
            .count()
            == 2
        {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let evs = log.replay();
    assert!(
        evs.iter()
            .any(|e| matches!(e, SessionEvent::TurnStart { .. })),
        "expected TurnStart in log, got: {:?}",
        evs
    );
    assert!(
        evs.iter()
            .any(|e| matches!(e, SessionEvent::TurnEnd { .. })),
        "expected TurnEnd in log, got: {:?}",
        evs
    );
    let inputs: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            SessionEvent::TurnStart { input, .. } => Some(input.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(inputs, vec!["hello", "queued follow-up"]);
}
