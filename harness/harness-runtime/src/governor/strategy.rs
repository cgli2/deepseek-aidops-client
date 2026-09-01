//! 策略栈：策略 = (工具集, 搜索范围, 预算窗口, 退出条件) 的可弹出序列（spec §4.2）。

use std::fmt;

/// 单个策略帧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// 带 grounding 锚点直接 change + verify
    GroundedChange,
    /// 全工作区搜索 / 字符串索引一跳
    BroadLocate,
    /// 诊断模式（运行时观察，如 cargo check）
    RuntimeObserve,
    /// 紧凑检查点换路（清历史、最小快照）
    CompactReroute,
    /// 交付可验证子目标
    DegradeGoal,
    /// 栈底常驻：ExhaustedWithArtifact 的构造点
    PartialDeliver,
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Strategy::GroundedChange => "grounded_change",
            Strategy::BroadLocate => "broad_locate",
            Strategy::RuntimeObserve => "runtime_observe",
            Strategy::CompactReroute => "compact_reroute",
            Strategy::DegradeGoal => "degrade_goal",
            Strategy::PartialDeliver => "partial_deliver",
        })
    }
}

/// 策略窗口大小 W：单个策略的尝试步数（spec §4.4 三参数之一）。
pub const WINDOW_STEPS: usize = 4;

/// 自顶向下的策略栈。`frames.last()` 是当前策略；栈底恒为 `PartialDeliver`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyStack {
    frames: Vec<Strategy>,
}

/// 栈底在头部、栈顶（当前策略）在尾部——与 `Vec::last()/pop()` 的取顶方式一致。
/// spec §4.2 的尝试顺序：grounded_change → broad_locate → runtime_observe →
/// compact_reroute → degrade_goal → partial_deliver（栈底常驻）。
const FULL: [Strategy; 6] = [
    Strategy::PartialDeliver,
    Strategy::DegradeGoal,
    Strategy::CompactReroute,
    Strategy::RuntimeObserve,
    Strategy::BroadLocate,
    Strategy::GroundedChange,
];

impl StrategyStack {
    /// 读写任务默认栈：grounding 命中从 grounded_change 起，未命中从 broad_locate 起
    /// （spec §4.5「未命中 → 诊断模式」）。
    pub fn for_task(grounded: bool) -> Self {
        Self {
            frames: if grounded {
                FULL.to_vec()
            } else {
                FULL[..5].to_vec()
            },
        }
    }

    /// Investigation 意图的只读栈变体（spec §4.2）：不含写入型策略。
    pub fn read_only() -> Self {
        Self {
            frames: vec![
                Strategy::PartialDeliver,
                Strategy::RuntimeObserve,
                Strategy::BroadLocate,
            ],
        }
    }

    pub fn current(&self) -> Option<Strategy> {
        self.frames.last().copied()
    }

    /// 剩余帧数（含当前帧）。
    pub fn remaining(&self) -> usize {
        self.frames.len()
    }

    /// 当前是否已在栈底。栈底不可弹出。
    pub fn at_bottom(&self) -> bool {
        matches!(self.current(), Some(Strategy::PartialDeliver))
    }

    /// ask_user 前置之一：剩余策略 ⊆ {degrade_goal, partial_deliver}（即栈深 ≤ 2）。
    pub fn allow_ask_user(&self) -> bool {
        self.frames
            .iter()
            .all(|s| matches!(s, Strategy::DegradeGoal | Strategy::PartialDeliver))
    }

    /// 弹出当前策略；已在栈底返回 `None`（由控制器判定终止）。
    pub fn pop(&mut self) -> Option<Strategy> {
        if self.at_bottom() {
            return None;
        }
        self.frames.pop()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grounded_stack_starts_at_grounded_change() {
        let stack = StrategyStack::for_task(true);
        assert_eq!(stack.current(), Some(Strategy::GroundedChange));
        assert_eq!(stack.remaining(), 6);
        assert!(!stack.allow_ask_user(), "栈顶时不许提问：还有 5 层可试");
    }

    #[test]
    fn ungrounded_stack_skips_grounded_change() {
        let stack = StrategyStack::for_task(false);
        assert_eq!(stack.current(), Some(Strategy::BroadLocate));
        assert_eq!(stack.remaining(), 5);
    }

    #[test]
    fn read_only_stack_has_no_write_strategies() {
        let stack = StrategyStack::read_only();
        assert_eq!(stack.current(), Some(Strategy::BroadLocate));
        assert_eq!(stack.remaining(), 3);
    }

    #[test]
    fn popping_down_to_bottom_enables_ask_user_gate() {
        let mut stack = StrategyStack::for_task(false);
        for expected in [
            Strategy::BroadLocate,
            Strategy::RuntimeObserve,
            Strategy::CompactReroute,
        ] {
            assert_eq!(stack.pop(), Some(expected));
        }
        assert_eq!(stack.current(), Some(Strategy::DegradeGoal));
        assert!(stack.allow_ask_user(), "仅剩 degrade_goal + partial_deliver");
    }

    #[test]
    fn bottom_strategy_is_never_popped() {
        let mut stack = StrategyStack::read_only();
        assert_eq!(stack.pop(), Some(Strategy::BroadLocate));
        assert_eq!(stack.pop(), Some(Strategy::RuntimeObserve));
        assert!(stack.at_bottom());
        assert_eq!(stack.pop(), None, "栈底常驻，返回 None 由控制器判定终止");
        assert_eq!(stack.current(), Some(Strategy::PartialDeliver));
    }

    #[test]
    fn strategy_display_labels_match_spec_naming() {
        assert_eq!(Strategy::PartialDeliver.to_string(), "partial_deliver");
        assert_eq!(Strategy::DegradeGoal.to_string(), "degrade_goal");
    }
}
