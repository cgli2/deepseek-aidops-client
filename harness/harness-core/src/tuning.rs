//! 运行时调参（进程级）：上下文预算 / 进展检查间隔 / 最大输出 tokens。
//!
//! 供 GUI「参数配置」页写入并即时生效，无需重启。三个值均为 `Option`：
//! - `Some(v)`：UI 显式配置，优先于环境变量；
//! - `None`：未配置，回退到 `HARNESS_CONTEXT_MAX_CHARS` / `HARNESS_MAX_STEPS` /
//!   `HARNESS_MAX_TOKENS` 环境变量，再回退到各自默认值。
//!
//! 线程安全：全部走 `RwLock`，agent 循环（tokio）与 UI 线程可并发读写。

use std::sync::RwLock;

static CONTEXT_BUDGET_CHARS: RwLock<Option<usize>> = RwLock::new(None);
static MAX_STEPS: RwLock<Option<usize>> = RwLock::new(None);
static MAX_OUTPUT_TOKENS: RwLock<Option<u64>> = RwLock::new(None);
static SELF_MONITOR_ENABLED: RwLock<Option<bool>> = RwLock::new(None);

/// 设置上下文预算（字符数）。`None` 表示未配置（回退环境变量 / 默认）。
pub fn set_context_budget_chars(value: Option<usize>) {
    if let Ok(mut guard) = CONTEXT_BUDGET_CHARS.write() {
        *guard = value;
    }
}

/// 读取 UI 配置的上下文预算（字符数）。
pub fn context_budget_chars() -> Option<usize> {
    CONTEXT_BUDGET_CHARS.read().map(|g| *g).unwrap_or(None)
}

/// 设置单回合进展检查的步数间隔。保留旧名称以兼容既有配置键。
/// `None` 表示未配置。
pub fn set_max_steps(value: Option<usize>) {
    if let Ok(mut guard) = MAX_STEPS.write() {
        *guard = value;
    }
}

/// 读取 UI 配置的进展检查步数间隔。
pub fn max_steps() -> Option<usize> {
    MAX_STEPS.read().map(|g| *g).unwrap_or(None)
}

/// 设置单次请求最大输出 tokens。`None` 表示未配置。
pub fn set_max_output_tokens(value: Option<u64>) {
    if let Ok(mut guard) = MAX_OUTPUT_TOKENS.write() {
        *guard = value;
    }
}

/// 读取 UI 配置的最大输出 tokens。
pub fn max_output_tokens() -> Option<u64> {
    MAX_OUTPUT_TOKENS.read().map(|g| *g).unwrap_or(None)
}

/// 设置自我监控门禁开关。`None` 表示未配置（回退配置文件 `[self_monitor].enabled`）。
pub fn set_self_monitor_enabled(value: Option<bool>) {
    if let Ok(mut guard) = SELF_MONITOR_ENABLED.write() {
        *guard = value;
    }
}

/// 读取 UI 配置的自我监控门禁开关。
pub fn self_monitor_enabled() -> Option<bool> {
    SELF_MONITOR_ENABLED.read().map(|g| *g).unwrap_or(None)
}

static SELF_MONITOR_MODE: RwLock<Option<String>> = RwLock::new(None);

/// 设置自我监控模式（`off` / `observe` / `protect` / `evolve`）。
/// `None` 表示未配置，回退配置文件 `[self_monitor].mode`。
pub fn set_self_monitor_mode(value: Option<String>) {
    if let Ok(mut guard) = SELF_MONITOR_MODE.write() {
        *guard = value;
    }
}

/// 读取 UI 配置的自我监控模式。
pub fn self_monitor_mode() -> Option<String> {
    SELF_MONITOR_MODE.read().map(|g| g.clone()).unwrap_or(None)
}

static SELF_MONITOR_EVOLUTION: RwLock<Option<bool>> = RwLock::new(None);

/// 设置演进门禁（灰度/晋级）开关。`None` 表示未配置，回退默认关闭。
/// 打开后运行时才允许把 `mode` 提为 `evolve` 并裁决低风险阈值类变更。
pub fn set_self_monitor_evolution(value: Option<bool>) {
    if let Ok(mut guard) = SELF_MONITOR_EVOLUTION.write() {
        *guard = value;
    }
}

/// 读取 UI 配置的演进门禁开关。
pub fn self_monitor_evolution() -> Option<bool> {
    SELF_MONITOR_EVOLUTION.read().map(|g| *g).unwrap_or(None)
}

static GOAL_EXECUTOR_ENABLED: RwLock<Option<bool>> = RwLock::new(None);

/// 设置目标执行框架（V4 受控任务）开关。`None` 表示未配置
/// （回退环境变量 `HARNESS_GOAL_EXECUTOR`，再回退默认启用）。
pub fn set_goal_executor_enabled(value: Option<bool>) {
    if let Ok(mut guard) = GOAL_EXECUTOR_ENABLED.write() {
        *guard = value;
    }
}

/// 读取 UI 配置的目标执行框架开关。
pub fn goal_executor_enabled() -> Option<bool> {
    GOAL_EXECUTOR_ENABLED.read().map(|g| *g).unwrap_or(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_none_and_settable() {
        set_context_budget_chars(None);
        set_max_steps(None);
        set_max_output_tokens(None);
        assert_eq!(context_budget_chars(), None);
        assert_eq!(max_steps(), None);
        assert_eq!(max_output_tokens(), None);

        set_context_budget_chars(Some(60_000));
        set_max_steps(Some(200));
        set_max_output_tokens(Some(8_192));
        assert_eq!(context_budget_chars(), Some(60_000));
        assert_eq!(max_steps(), Some(200));
        assert_eq!(max_output_tokens(), Some(8_192));

        // 自我监控门禁开关与模式：UI 写入后必须能被运行时读回。
        assert_eq!(self_monitor_enabled(), None);
        assert_eq!(self_monitor_mode(), None);
        assert_eq!(self_monitor_evolution(), None);
        set_self_monitor_enabled(Some(false));
        set_self_monitor_mode(Some("observe".into()));
        set_self_monitor_evolution(Some(true));
        assert_eq!(self_monitor_enabled(), Some(false));
        assert_eq!(self_monitor_mode(), Some("observe".into()));
        assert_eq!(self_monitor_evolution(), Some(true));

        // 复位，避免污染其他测试。
        set_context_budget_chars(None);
        set_max_steps(None);
        set_max_output_tokens(None);
        set_self_monitor_enabled(None);
        set_self_monitor_mode(None);
        set_self_monitor_evolution(None);
        assert_eq!(self_monitor_enabled(), None);
        assert_eq!(self_monitor_mode(), None);
        assert_eq!(self_monitor_evolution(), None);
    }

    #[test]
    fn goal_executor_switch_is_settable_and_clears() {
        // UI 写入后运行时必须能读回，且可复位为「未配置」以回退环境变量。
        assert_eq!(goal_executor_enabled(), None);
        set_goal_executor_enabled(Some(false));
        assert_eq!(goal_executor_enabled(), Some(false));
        set_goal_executor_enabled(Some(true));
        assert_eq!(goal_executor_enabled(), Some(true));
        set_goal_executor_enabled(None);
        assert_eq!(goal_executor_enabled(), None);
    }
}
