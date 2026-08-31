//! 基于外部命令的钩子 Provider（借鉴 Codex：钩子是用户提供的 shell 命令）。
//!
//! 配置：`Config.hooks["<event>"] = "<command>"`（event 见 `HookEvent::name`）。
//! 运行时把 `HookPayload` 序列化为 JSON 经 stdin 传入命令，命令返回 JSON
//! `{"decision":"allow"|"block","reason":"..."}` 决定放行 / 阻断。命令执行失败默认 *阻断*（fail-closed）。
//!
//! 未配置任何钩子时退化为 `NullHook`（全放行），保证循环不中断。

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::Arc;

use harness_capability::hook::{Hook, HookDecision, HookEvent, HookPayload};
use harness_core::error::{Error, Result};

/// 空钩子：永远放行（未配置钩子时的默认实现）。
pub struct NullHook;

impl Hook for NullHook {
    fn run(&self, _payload: &HookPayload) -> Result<HookDecision> {
        Ok(HookDecision::Allow)
    }
}

/// 外部命令钩子 Provider。
pub struct ShellHook {
    /// event 名称 -> 命令。
    handlers: HashMap<&'static str, String>,
}

impl ShellHook {
    /// 无配置：等价于 `NullHook`（全部放行）。
    pub fn new() -> Arc<dyn Hook> {
        Arc::new(NullHook)
    }

    /// 从配置构造；仅注册了命令的事件会触发外部命令，其余事件放行。
    pub fn from_config(hooks: &HashMap<String, String>) -> Arc<dyn Hook> {
        let mut handlers: HashMap<&'static str, String> = HashMap::new();
        for (k, v) in hooks {
            if let Some(ev) = event_by_name(k) {
                handlers.insert(ev.name(), v.clone());
            }
        }
        if handlers.is_empty() {
            return Arc::new(NullHook);
        }
        Arc::new(ShellHook { handlers })
    }
}

fn event_by_name(name: &str) -> Option<HookEvent> {
    match name {
        "session_start" => Some(HookEvent::SessionStart),
        "pre_turn" => Some(HookEvent::PreTurn),
        "pre_tool_use" => Some(HookEvent::PreToolUse),
        "post_tool_use" => Some(HookEvent::PostToolUse),
        "post_turn" => Some(HookEvent::PostTurn),
        "session_end" => Some(HookEvent::SessionEnd),
        _ => None,
    }
}

impl Hook for ShellHook {
    fn run(&self, payload: &HookPayload) -> Result<HookDecision> {
        let Some(cmd) = self.handlers.get(payload.event.name()) else {
            return Ok(HookDecision::Allow);
        };
        let input = serde_json::to_string(payload).map_err(Error::Serde)?;
        let mut child = Command::new(cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(Error::Io)?;
        // 把 HookPayload（JSON）写入钩子命令的 stdin。
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            stdin.write_all(input.as_bytes()).map_err(Error::Io)?;
        }
        let out = child.wait_with_output().map_err(Error::Io)?;
        if !out.status.success() {
            // 钩子自身失败 -> 默认阻断（安全优先）。
            return Ok(HookDecision::Block(format!(
                "hook '{}' exited with {}: {}",
                cmd,
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let parsed: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(Error::Serde)?;
        match parsed.get("decision").and_then(|d| d.as_str()) {
            Some("block") => Ok(HookDecision::Block(
                parsed
                    .get("reason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("blocked by hook")
                    .to_string(),
            )),
            _ => Ok(HookDecision::Allow),
        }
    }
}
