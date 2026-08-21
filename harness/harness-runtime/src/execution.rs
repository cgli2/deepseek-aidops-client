//! 通用 Agent 执行控制：任务契约、状态、行动门禁、动态预算与完成判定。
//!
//! 该模块不依赖代码、研究或运维领域；领域适配器只负责分类和调整预算。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use harness_llm::ToolCall;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyKind {
    Direct,
    Investigative,
    Comparative,
    Transformative,
    Generative,
    Verification,
    Monitoring,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Criterion {
    pub id: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct TaskContract {
    pub objective: String,
    pub deliverables: Vec<String>,
    pub acceptance_criteria: Vec<Criterion>,
    pub scope: Vec<String>,
    pub constraints: Vec<String>,
    pub uncertainties: Vec<String>,
    pub risk: RiskLevel,
}

impl TaskContract {
    /// 从任意自然语言请求生成保守的基础契约。领域策略可在此基础上细化，
    /// 但 Runtime 始终至少拥有一个可判定的交付标准。
    pub fn from_input(input: &str) -> Self {
        let objective = input.trim().to_string();
        let listed_items: Vec<String> = input
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let is_bullet = line.starts_with("- ") || line.starts_with("* ");
                let is_numbered = line
                    .split_once(['.', '、'])
                    .is_some_and(|(prefix, _)| prefix.chars().all(|c| c.is_ascii_digit()));
                (is_bullet || is_numbered)
                    .then(|| {
                        line.trim_start_matches(['-', '*', ' '])
                            .split_once(['.', '、'])
                            .map(|(_, body)| body.trim())
                            .unwrap_or(line.trim_start_matches(['-', '*', ' ']))
                            .to_string()
                    })
                    .filter(|item| !item.is_empty())
            })
            .take(12)
            .collect();
        let mut constraints = Vec::new();
        for marker in ["不要", "仅", "只", "禁止", "不得", "不需要"] {
            if input.contains(marker) {
                constraints.push(format!("遵守用户包含“{marker}”的范围约束"));
            }
        }
        let risk = if ["删除", "生产", "部署", "权限", "凭据", "数据库"]
            .iter()
            .any(|word| input.contains(word))
        {
            RiskLevel::High
        } else if ["修改", "修复", "重构", "实现", "安装"]
            .iter()
            .any(|word| input.contains(word))
        {
            RiskLevel::Medium
        } else {
            RiskLevel::Low
        };
        let deliverables = if listed_items.is_empty() {
            vec![objective.clone()]
        } else {
            listed_items.clone()
        };
        let acceptance_criteria = if listed_items.is_empty() {
            vec![Criterion {
                id: "user-objective".into(),
                description: "用户要求的交付物已完成并经过与风险相称的验证".into(),
            }]
        } else {
            listed_items
                .into_iter()
                .enumerate()
                .map(|(index, description)| Criterion {
                    id: format!("item-{}", index + 1),
                    description,
                })
                .collect()
        };
        Self {
            objective: objective.clone(),
            deliverables,
            acceptance_criteria,
            scope: Vec::new(),
            constraints,
            uncertainties: Vec::new(),
            risk,
        }
    }

    pub fn render_for_model(&self, strategy: StrategyKind, budget: &Budget) -> String {
        let constraints = if self.constraints.is_empty() {
            "无额外显式约束".to_string()
        } else {
            self.constraints.join("；")
        };
        format!(
            "[本回合执行契约]\n目标：{}\n策略：{:?}\n验收：{}\n约束：{}\n进展检查点：每 {} 个步骤或 {} 次工具调用评估一次并按需续期，不是任务终止线。每次调用都必须直接推进目标或降低阻塞验收的不确定性；已有充分证据时停止搜索，验收未完成时不得仅因达到检查点而收尾。",
            self.objective,
            strategy,
            self.acceptance_criteria
                .iter()
                .map(|item| format!("{}={}", item.id, item.description))
                .collect::<Vec<_>>()
                .join("；"),
            constraints,
            budget.max_steps,
            budget.max_tool_calls,
        )
    }
}

#[derive(Debug, Clone)]
pub struct Evidence {
    pub question: String,
    pub tool_signature: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct DecisionRecord {
    pub decision: String,
    pub rationale: String,
    pub locked: bool,
}

#[derive(Debug)]
pub struct ExecutionState {
    pub contract: TaskContract,
    pub strategy: StrategyKind,
    pub started_at: Instant,
    pub steps: usize,
    pub tool_calls: usize,
    pub successful_tool_results: usize,
    pub failed_tool_results: usize,
    pub evidence: HashMap<String, Evidence>,
    pub decisions: Vec<DecisionRecord>,
    pub satisfied_criteria: HashSet<String>,
    checkpoint_steps: usize,
    checkpoint_tool_calls: usize,
    checkpoint_evidence: usize,
    checkpoint_successes: usize,
}

impl ExecutionState {
    pub fn new(contract: TaskContract, strategy: StrategyKind) -> Self {
        Self {
            contract,
            strategy,
            started_at: Instant::now(),
            steps: 0,
            tool_calls: 0,
            successful_tool_results: 0,
            failed_tool_results: 0,
            evidence: HashMap::new(),
            decisions: Vec::new(),
            satisfied_criteria: HashSet::new(),
            checkpoint_steps: 0,
            checkpoint_tool_calls: 0,
            checkpoint_evidence: 0,
            checkpoint_successes: 0,
        }
    }

    pub fn record_tool_result(&mut self, proposal: &ActionProposal, ok: bool, summary: &str) {
        if ok {
            self.successful_tool_results += 1;
        } else {
            self.failed_tool_results += 1;
        }
        self.evidence.insert(
            proposal.signature.clone(),
            Evidence {
                question: proposal.question.clone(),
                tool_signature: proposal.signature.clone(),
                summary: summary.chars().take(600).collect(),
            },
        );
    }
}

#[derive(Debug, Clone)]
pub struct ActionProposal {
    pub signature: String,
    pub question: String,
    pub supports: Vec<String>,
    pub estimated_cost: usize,
}

impl ActionProposal {
    pub fn from_tool_call(call: &ToolCall, contract: &TaskContract) -> Self {
        Self {
            signature: format!("{}:{}", call.name, call.args),
            question: format!("执行工具 {} 以推进当前任务", call.name),
            supports: contract
                .acceptance_criteria
                .iter()
                .map(|criterion| criterion.id.clone())
                .collect(),
            estimated_cost: 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Budget {
    pub max_steps: usize,
    pub max_tool_calls: usize,
    pub max_duration: Duration,
    pub convergence_ratio: f32,
    step_window: usize,
    tool_window: usize,
    duration_window: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetPhase {
    Normal,
    Converge,
    Exhausted,
}

pub struct BudgetManager;

impl BudgetManager {
    pub fn for_contract(contract: &TaskContract, strategy: StrategyKind) -> Budget {
        let uncertainty = contract.uncertainties.len().min(5);
        let (base_steps, base_calls) = match strategy {
            StrategyKind::Direct => (12, 16),
            StrategyKind::Transformative | StrategyKind::Verification => (24, 32),
            StrategyKind::Generative | StrategyKind::Comparative => (28, 36),
            StrategyKind::Investigative => (40, 48),
            StrategyKind::Monitoring => (16, 20),
        };
        let risk_bonus = match contract.risk {
            RiskLevel::Low => 0,
            RiskLevel::Medium => 6,
            RiskLevel::High => 12,
        };
        let max_steps = base_steps + risk_bonus + uncertainty * 2;
        let max_tool_calls = base_calls + risk_bonus + uncertainty * 3;
        let max_duration = Duration::from_secs(match contract.risk {
            RiskLevel::Low => 8 * 60,
            RiskLevel::Medium => 15 * 60,
            RiskLevel::High => 25 * 60,
        });
        Budget {
            max_steps,
            max_tool_calls,
            max_duration,
            convergence_ratio: 0.75,
            step_window: max_steps,
            tool_window: max_tool_calls,
            duration_window: max_duration,
        }
    }

    /// 管理员/UI 配置定义的是“多少步检查一次进展”，不是未完成任务的终止线。
    pub fn cap_initial_step_window(budget: &mut Budget, steps: usize) {
        let steps = steps.max(1);
        budget.max_steps = budget.max_steps.min(steps);
        budget.step_window = budget.max_steps;
    }

    pub fn phase(state: &ExecutionState, budget: &Budget) -> BudgetPhase {
        if state.steps >= budget.max_steps
            || state.tool_calls >= budget.max_tool_calls
            || state.started_at.elapsed() >= budget.max_duration
        {
            return BudgetPhase::Exhausted;
        }
        let step_ratio = state.steps as f32 / budget.max_steps as f32;
        let call_ratio = state.tool_calls as f32 / budget.max_tool_calls as f32;
        if step_ratio.max(call_ratio) >= budget.convergence_ratio {
            BudgetPhase::Converge
        } else {
            BudgetPhase::Normal
        }
    }

    /// 到达软预算后评估这一阶段是否产生了新证据，并续发下一阶段预算。
    /// 停滞不会被误判为完成，而会得到更强的换路诊断提示。
    pub fn diagnose_and_renew(state: &mut ExecutionState, budget: &mut Budget) -> String {
        let step_delta = state.steps.saturating_sub(state.checkpoint_steps);
        let call_delta = state.tool_calls.saturating_sub(state.checkpoint_tool_calls);
        let evidence_delta = state
            .evidence
            .len()
            .saturating_sub(state.checkpoint_evidence);
        let success_delta = state
            .successful_tool_results
            .saturating_sub(state.checkpoint_successes);
        let repeated_or_low_value = call_delta.saturating_sub(evidence_delta);
        let stagnant = call_delta > 0 && (evidence_delta == 0 || success_delta == 0);

        state.checkpoint_steps = state.steps;
        state.checkpoint_tool_calls = state.tool_calls;
        state.checkpoint_evidence = state.evidence.len();
        state.checkpoint_successes = state.successful_tool_results;
        budget.max_steps = budget.max_steps.saturating_add(budget.step_window);
        budget.max_tool_calls = budget.max_tool_calls.saturating_add(budget.tool_window);
        budget.max_duration = budget.max_duration.saturating_add(budget.duration_window);

        if stagnant {
            format!(
                "[执行检查点] 最近 {step_delta} 步、{call_delta} 次工具调用仅产生 {evidence_delta} 条新证据、{success_delta} 次成功结果，约 {repeated_or_low_value} 次调用没有增加独立证据。任务尚未完成，禁止直接收尾。先明确当前阻塞原因，放弃重复路径，更新计划，并选择最能推进未满足验收条件的下一步；预算已自动续期。"
            )
        } else {
            format!(
                "[执行检查点] 最近 {step_delta} 步、{call_delta} 次工具调用产生 {evidence_delta} 条新证据、{success_delta} 次成功结果。任务尚未完成时继续推进，但只围绕未满足的验收条件；预算已自动续期。"
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    Allow,
    Deny(String),
}

pub struct ActionGate;

impl ActionGate {
    pub fn authorize(
        proposal: &ActionProposal,
        _state: &ExecutionState,
        _budget: &Budget,
    ) -> GateDecision {
        if proposal.supports.is_empty() {
            return GateDecision::Deny("该调用未关联任何验收标准".into());
        }
        GateDecision::Allow
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    Complete,
    Continue,
    Converge(String),
}

pub struct CompletionJudge;

impl CompletionJudge {
    pub fn evaluate(
        state: &ExecutionState,
        budget: &Budget,
        model_requested_tools: bool,
    ) -> Completion {
        match BudgetManager::phase(state, budget) {
            BudgetPhase::Exhausted => Completion::Continue,
            BudgetPhase::Converge if model_requested_tools => Completion::Converge(
                "已进入收敛阶段；仅执行完成当前交付所必需的动作，禁止扩大范围".into(),
            ),
            _ if !model_requested_tools => Completion::Complete,
            _ => Completion::Continue,
        }
    }
}

/// 领域策略扩展点。默认实现只基于语言意图分类；代码、研究、运维插件可覆盖它，
/// 无需修改通用 Agent Loop。
pub trait DomainPolicy: Send + Sync {
    fn select_strategy(&self, contract: &TaskContract) -> StrategyKind;
    fn adjust_budget(&self, _contract: &TaskContract, _budget: &mut Budget) {}
}

#[derive(Default)]
pub struct GeneralDomainPolicy;

impl DomainPolicy for GeneralDomainPolicy {
    fn select_strategy(&self, contract: &TaskContract) -> StrategyKind {
        let text = contract.objective.as_str();
        if ["排查", "调查", "为什么", "根因", "诊断"]
            .iter()
            .any(|word| text.contains(word))
        {
            StrategyKind::Investigative
        } else if ["比较", "选型", "对比"]
            .iter()
            .any(|word| text.contains(word))
        {
            StrategyKind::Comparative
        } else if ["验证", "确认", "检查", "审查"]
            .iter()
            .any(|word| text.contains(word))
        {
            StrategyKind::Verification
        } else if ["修改", "修复", "重构", "更新", "改进", "实现"]
            .iter()
            .any(|word| text.contains(word))
        {
            StrategyKind::Transformative
        } else if ["创建", "生成", "编写", "设计"]
            .iter()
            .any(|word| text.contains(word))
        {
            StrategyKind::Generative
        } else if ["等待", "监控", "观察"]
            .iter()
            .any(|word| text.contains(word))
        {
            StrategyKind::Monitoring
        } else {
            StrategyKind::Direct
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_generic_intents_without_domain_specific_rules() {
        let policy = GeneralDomainPolicy;
        assert_eq!(
            policy.select_strategy(&TaskContract::from_input("请修复三个问题")),
            StrategyKind::Transformative
        );
        assert_eq!(
            policy.select_strategy(&TaskContract::from_input("排查服务变慢的根因")),
            StrategyKind::Investigative
        );
    }

    #[test]
    fn turns_numbered_or_bulleted_requests_into_acceptance_items() {
        let contract = TaskContract::from_input("请完成：\n1. 建立契约\n2. 增加预算\n- 补充测试");
        assert_eq!(contract.deliverables.len(), 3);
        assert_eq!(contract.acceptance_criteria[0].id, "item-1");
        assert_eq!(contract.acceptance_criteria[1].description, "增加预算");
        assert_eq!(contract.acceptance_criteria[2].description, "补充测试");
    }

    #[test]
    fn budget_scales_with_risk_and_enters_convergence() {
        let low = TaskContract::from_input("解释这段内容");
        let high = TaskContract::from_input("删除生产数据库配置");
        let low_budget = BudgetManager::for_contract(&low, StrategyKind::Direct);
        let high_budget = BudgetManager::for_contract(&high, StrategyKind::Direct);
        assert!(high_budget.max_tool_calls > low_budget.max_tool_calls);

        let mut state = ExecutionState::new(low, StrategyKind::Direct);
        state.tool_calls = (low_budget.max_tool_calls as f32 * 0.8) as usize;
        assert_eq!(
            BudgetManager::phase(&state, &low_budget),
            BudgetPhase::Converge
        );
    }

    #[test]
    fn action_gate_requires_acceptance_link_but_soft_budget_does_not_block() {
        let contract = TaskContract::from_input("完成任务");
        let budget = BudgetManager::for_contract(&contract, StrategyKind::Direct);
        let mut state = ExecutionState::new(contract.clone(), StrategyKind::Direct);
        let proposal = ActionProposal {
            signature: "fs:{}".into(),
            question: "读取输入".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        assert_eq!(
            ActionGate::authorize(&proposal, &state, &budget),
            GateDecision::Allow
        );
        state.tool_calls = budget.max_tool_calls;
        assert_eq!(
            ActionGate::authorize(&proposal, &state, &budget),
            GateDecision::Allow
        );
    }

    #[test]
    fn exhausted_budget_diagnoses_progress_and_renews_instead_of_stopping() {
        let contract = TaskContract::from_input("完成一项多步骤任务");
        let mut budget = BudgetManager::for_contract(&contract, StrategyKind::Direct);
        let original_limit = budget.max_steps;
        let mut state = ExecutionState::new(contract, StrategyKind::Direct);
        state.steps = original_limit;
        state.tool_calls = 5;

        assert_eq!(
            BudgetManager::phase(&state, &budget),
            BudgetPhase::Exhausted
        );
        let diagnosis = BudgetManager::diagnose_and_renew(&mut state, &mut budget);
        assert!(diagnosis.contains("任务尚未完成"));
        assert!(diagnosis.contains("预算已自动续期"));
        assert!(budget.max_steps > original_limit);
        assert_ne!(
            BudgetManager::phase(&state, &budget),
            BudgetPhase::Exhausted
        );
    }
}
