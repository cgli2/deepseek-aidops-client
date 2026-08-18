use std::any::Any;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use harness_core::error::Result;

/// 钩子触发时机（借鉴 Codex / Claude Code 的 PreToolUse / PostToolUse 等生命周期钩子）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum HookEvent {
    #[default]
    SessionStart,
    PreTurn,
    PreToolUse,
    PostToolUse,
    PostTurn,
    SessionEnd,
}

impl HookEvent {
    /// 稳定名称，用作配置键与钩子命令的标识。
    pub fn name(&self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session_start",
            HookEvent::PreTurn => "pre_turn",
            HookEvent::PreToolUse => "pre_tool_use",
            HookEvent::PostToolUse => "post_tool_use",
            HookEvent::PostTurn => "post_turn",
            HookEvent::SessionEnd => "session_end",
        }
    }
}

/// 传给钩子命令的负载（序列化为 JSON 后经 stdin 传入）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookPayload {
    pub event: HookEvent,
    pub tool: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub extra: HashMap<String, String>,
}

/// 钩子裁决：放行 / 阻断（带原因）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookDecision {
    Allow,
    Block(String),
}

/// 钩子能力（Definition）。用户在生命周期点注入外部命令：
/// 可用于合规校验（阻断危险命令）、审计、自动格式化等。与"一切皆插件"一致——
/// 钩子本身也是可替换的 Provider（ShellHook / NullHook …）。
pub trait Hook: Any + Send + Sync + 'static {
    /// 返回 `Block(reason)` 时循环应中止该步骤；`Allow` 继续。
    fn run(&self, payload: &HookPayload) -> Result<HookDecision>;
}
