//! 单一闭环控制器（spec §4.1）：observe → measure → decide。
//!
//! 决策只有四分支：continue / switch_strategy / degrade / terminate。旧守卫降级为
//! 传感器——只产信号，不终止回合；终止权收归 `TurnGovernor`。出口仅两类：
//! `Delivered` 与 `ExhaustedWithArtifact`。ask_user 是工具调用而非出口，受三重前置约束。

pub mod sensors;
pub mod strategy;

pub use sensors::{artifact_text, delta_between, WindowDelta};
pub use strategy::{Strategy, StrategyStack, WINDOW_STEPS};

use std::collections::BTreeSet;

use crate::case_file::{normalize_question, CaseFile};

/// 会话 prompt tokens 硬顶（spec §3 R3）。回放套件引用同一常量，禁止两处各自写死。
pub const PROMPT_CAP: u64 = 300_000;

/// 候选列表标记：ask_user 的问题必须带工作区派生的候选（R2「禁开放模板」）。
pub const CANDIDATE_MARKERS: [&str; 2] = ["候选：", "可选："];

/// 续跑式回复前缀。单点定义：agent_loop 的 resume 判定、控制器 ask_user 前置、
/// 回放套件 R1 度量器共用，避免「同一概念三处各写一份」的漂移。
pub fn is_continuation_request(text: &str) -> bool {
    let trimmed = text.trim().to_lowercase();
    ["继续", "接着", "续跑", "恢复", "continue", "resume"]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// 问题是否带候选列表（R2 硬前置）。
pub fn has_candidates(question: &str) -> bool {
    CANDIDATE_MARKERS.iter().any(|m| question.contains(m))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    /// 交付成立（Runtime 验收通过）。
    Delivered,
    /// 策略栈耗尽：带 R4 四要素资产收尾，绝不裸停交还用户。
    ExhaustedWithArtifact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Continue,
    SwitchStrategy,
    Degrade,
    Terminate(Termination),
}

/// 回合控制器。一个回合一个实例，持有策略栈与窗口基线。
#[derive(Debug, Clone)]
pub struct TurnGovernor {
    pub stack: StrategyStack,
    window_base: CaseFile,
    window_base_step: usize,
    window_base_writes: usize,
    eliminated: BTreeSet<String>,
    /// 决策审计轨迹：遥测与 A/B 对照用，不参与决策本身。
    pub decisions: Vec<Decision>,
}

impl TurnGovernor {
    /// `grounded` 来自 grounding 命中；`read_only` 对应 Investigation 意图。
    pub fn new(case: &CaseFile, grounded: bool, read_only: bool) -> Self {
        let stack = if read_only {
            StrategyStack::read_only()
        } else {
            StrategyStack::for_task(grounded)
        };
        Self {
            stack,
            window_base: case.clone(),
            window_base_step: 0,
            window_base_writes: 0,
            eliminated: BTreeSet::new(),
            decisions: vec![],
        }
    }

    pub fn current_strategy(&self) -> Option<Strategy> {
        self.stack.current()
    }

    pub fn eliminated(&self) -> &BTreeSet<String> {
        &self.eliminated
    }

    /// 测试/A-B 对照用：窗口基线投影（eliminated 已在此前 pop 时并入）。
    pub fn window_base(&self) -> &CaseFile {
        &self.window_base
    }

    /// 换路/降级提示引用的当前策略标签。
    pub fn strategy_hint(&self) -> String {
        self.stack
            .current()
            .map(|s| s.to_string())
            .unwrap_or_else(|| Strategy::PartialDeliver.to_string())
    }

    /// R3 前置判顶：`prompt_so_far` 为会话累计 prompt tokens，`last_prompt_tokens`
    /// 为上一轮请求的实际 prompt。上下文单调不减，故后者是下一轮增量的下界，
    /// 据此拦停可保证累计值不越过硬顶（spec §3「超顶只允许 partial_deliver」）。
    pub fn should_stop_before_request(&self, prompt_so_far: u64, last_prompt_tokens: u64) -> bool {
        prompt_so_far.saturating_add(last_prompt_tokens) >= PROMPT_CAP
    }

    /// 步末观测：最新投影 + 累计步数 + 累计写入数 → 唯一决策。
    pub fn observe(&mut self, now: &CaseFile, step: usize, total_writes: usize) -> Decision {
        let steps_in_window = step.saturating_sub(self.window_base_step);
        let mut delta = delta_between(&self.window_base, now);
        delta.write_increment = total_writes.saturating_sub(self.window_base_writes);

        let decision = if steps_in_window < WINDOW_STEPS {
            Decision::Continue
        } else if delta.gain() > 0 {
            self.restart_window(now, step, total_writes);
            Decision::Continue
        } else if let Some(popped) = self.stack.pop() {
            self.eliminated.insert(popped.to_string());
            self.restart_window(now, step, total_writes);
            if self.stack.at_bottom() {
                Decision::Degrade
            } else {
                Decision::SwitchStrategy
            }
        } else {
            // pop 返回 None（已在栈底）且窗口零增益 → 唯一的终止分支。
            Decision::Terminate(Termination::ExhaustedWithArtifact)
        };
        self.decisions.push(decision);
        decision
    }

    /// ask_user 三重前置（spec §4.2）：栈深 ≤ 2、非续跑回复、不在 asked 且带候选。
    pub fn ask_user_allowed(&self, case: &CaseFile, input_text: &str, question: &str) -> bool {
        self.stack.allow_ask_user()
            && !is_continuation_request(input_text)
            && has_candidates(question)
            && !case.asked.contains(&normalize_question(question))
    }

    /// 窗口重新起算：新基线并入已记录的排除策略，使控制器自身产生的排除信号不被重复计数。
    fn restart_window(&mut self, now: &CaseFile, step: usize, total_writes: usize) {
        let mut base = now.clone();
        base.eliminated.extend(self.eliminated.iter().cloned());
        self.window_base = base;
        self.window_base_step = step;
        self.window_base_writes = total_writes;
    }
}

#[cfg(test)]
mod tests {
    use super::strategy::WINDOW_STEPS;
    use super::*;
    use crate::case_file::CaseFile;
    use harness_llm::Chunk;
    use harness_session::SessionEvent;

    fn case_with_anchors(anchors: &[&str]) -> CaseFile {
        CaseFile::from_replay(&[
            SessionEvent::TurnStart {
                id: 0,
                input: "定位问题".into(),
            },
            SessionEvent::Assistant {
                id: 1,
                chunk: Chunk {
                    text: Some(anchors.join(" ")),
                    ..Default::default()
                },
            },
        ])
    }

    #[test]
    fn continues_until_window_is_full() {
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, true, false);
        for step in 1..WINDOW_STEPS {
            assert_eq!(gov.observe(&case, step, 0), Decision::Continue, "step {step}");
        }
    }

    #[test]
    fn zero_gain_at_window_end_switches_strategy() {
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, true, false);
        assert_eq!(gov.observe(&case, WINDOW_STEPS, 0), Decision::SwitchStrategy);
        assert_eq!(gov.current_strategy(), Some(Strategy::BroadLocate), "grounded_change 已弹出");
    }

    #[test]
    fn gain_resets_baseline_so_old_anchors_do_not_extend_window() {
        let mut gov = TurnGovernor::new(&CaseFile::default(), true, false);
        let now = case_with_anchors(&["a/b.rs"]);
        // 第一个满窗：新锚点有增益 → Continue 并重开窗口，grounded_change 保留。
        assert_eq!(gov.observe(&now, WINDOW_STEPS, 0), Decision::Continue);
        assert_eq!(gov.current_strategy(), Some(Strategy::GroundedChange));
        // 第二个满窗：锚点没变（已被新基线吸收）→ 零增益 → 换路。
        // 若基线未吸收上一窗的增益，这里会误判 gain=1 而给 grounded_change 续命。
        assert_eq!(gov.observe(&now, WINDOW_STEPS * 2, 0), Decision::SwitchStrategy);
        assert_eq!(gov.current_strategy(), Some(Strategy::BroadLocate));
    }

    #[test]
    fn write_increment_counts_as_gain() {
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, true, false);
        // 锚点零增长但有 1 次成功写入 → 仍算有增益，不换路（写入型策略的进展来源）。
        assert_eq!(gov.observe(&case, WINDOW_STEPS, 1), Decision::Continue);
    }

    #[test]
    fn degrade_at_bottom_then_terminate() {
        // 只读栈：broad_locate → runtime_observe →（栈底）partial_deliver
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, false, true);
        assert_eq!(gov.observe(&case, WINDOW_STEPS, 0), Decision::SwitchStrategy);
        assert_eq!(gov.current_strategy(), Some(Strategy::RuntimeObserve));
        assert_eq!(gov.observe(&case, WINDOW_STEPS * 2, 0), Decision::Degrade);
        assert!(gov.stack.at_bottom());
        assert_eq!(
            gov.observe(&case, WINDOW_STEPS * 3, 0),
            Decision::Terminate(Termination::ExhaustedWithArtifact),
            "栈底零增益是全系统唯一的回合终止来源"
        );
    }

    #[test]
    fn eliminated_strategies_are_recorded_once() {
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, false, true);
        gov.observe(&case, WINDOW_STEPS, 0);
        assert_eq!(gov.eliminated().len(), 1);
        let now = {
            let mut c = case.clone();
            c.eliminated = gov.eliminated().clone();
            c
        };
        assert_eq!(delta_between(&case, &now).new_eliminations, 1);
        // 新基线吸收 eliminated，故同一条排除不会被再计一次。
        assert_eq!(gov.observe(&now, WINDOW_STEPS * 2, 0), Decision::Degrade);
        assert_eq!(delta_between(gov.window_base(), &now).new_eliminations, 0);
    }

    #[test]
    fn prompt_ceiling_blocks_request_before_it_is_issued() {
        let gov = TurnGovernor::new(&CaseFile::default(), true, false);
        assert!(!gov.should_stop_before_request(PROMPT_CAP - 10, 0));
        assert!(gov.should_stop_before_request(PROMPT_CAP - 10, 10), "累计+增量下界到顶");
        assert!(gov.should_stop_before_request(PROMPT_CAP, 0), "已到顶即停，不得再探索");
        assert!(gov.should_stop_before_request(PROMPT_CAP + 1, 0), "超顶同样拦死");
    }

    #[test]
    fn ask_user_requires_all_three_preconditions() {
        let case = CaseFile::default();
        let mut gov = TurnGovernor::new(&case, false, false);
        let q = "目标模块是哪个（候选：harness-tool、harness-runtime）";
        assert!(!gov.ask_user_allowed(&case, "这个问题解决了吗？", q), "前置①栈深不满足");

        while !gov.stack.allow_ask_user() {
            gov.stack.pop();
        }
        assert!(gov.ask_user_allowed(&case, "目标在哪", q));
        assert!(!gov.ask_user_allowed(&case, "继续", q), "R1：续跑回复永不问用户");
        assert!(
            !gov.ask_user_allowed(&case, "目标在哪", "目标模块是哪个？"),
            "R2：开放模板问题一律禁止"
        );
        let mut asked = case.clone();
        asked.asked.insert(normalize_question(q));
        assert!(!gov.ask_user_allowed(&asked, "目标在哪", q), "R2：同一问题不得问第二次");
    }

    #[test]
    fn continuation_prefix_list_is_single_sourced() {
        for text in ["继续", "接着做", "续跑一下", "恢复任务", "Continue please", "RESUME"] {
            assert!(is_continuation_request(text), "{text}");
        }
        assert!(!is_continuation_request("这个问题解决了吗？"));
    }
}
