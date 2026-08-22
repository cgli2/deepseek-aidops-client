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
        // 风险分级（取证修正）：“删除”常作为功能描述出现（如“导入、删除、启用、
        // 禁用”），单独出现不应升级为 High；High 仅限真正危险的变更面。
        let risk = if ["生产", "部署", "权限", "凭据", "数据库"]
            .iter()
            .any(|word| input.contains(word))
        {
            RiskLevel::High
        } else if ["删除", "修改", "修复", "重构", "实现", "安装", "改造", "调整"]
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
            "[本回合执行契约]\n目标：{}\n策略：{:?}\n验收：{}\n约束：{}\n进展检查点：每 {} 个步骤或 {} 次工具调用评估一次并按需续期（最多续期 {} 次，用尽后必须基于现有证据收尾）。执行准则：最小路径优先——先直接定位与目标直接相关的最小文件集，禁止全仓库泛扫与重复读取已读文件；探索类调用不超过总调用三成，其余应为直接产出交付的写操作与验证；同一工具调用未带来新信息时立即换路或收尾；交付目标达成即停止，不做重复确认与打磨。",
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
            budget.max_renewals,
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
    /// 成功的写入/编辑次数：区分“正在产出交付”与“空转探索”的关键信号，
    /// 供续期耗尽后的交付延展判定使用。
    pub write_operations: usize,
    pub evidence: HashMap<String, Evidence>,
    pub decisions: Vec<DecisionRecord>,
    pub satisfied_criteria: HashSet<String>,
    checkpoint_steps: usize,
    checkpoint_tool_calls: usize,
    checkpoint_evidence: usize,
    checkpoint_successes: usize,
    checkpoint_writes: usize,
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
            write_operations: 0,
            evidence: HashMap::new(),
            decisions: Vec::new(),
            satisfied_criteria: HashSet::new(),
            checkpoint_steps: 0,
            checkpoint_tool_calls: 0,
            checkpoint_evidence: 0,
            checkpoint_successes: 0,
            checkpoint_writes: 0,
        }
    }

    pub fn record_tool_result(&mut self, proposal: &ActionProposal, ok: bool, summary: &str) {
        if ok {
            self.successful_tool_results += 1;
            // 归一化签名里 edit 工具以 "edit:" 开头；fs 写入的 JSON 参数含 "op":"write"。
            if proposal.signature.starts_with("edit:")
                || proposal.signature.contains("\"op\":\"write\"")
            {
                self.write_operations += 1;
            }
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
            signature: normalized_signature(call),
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

/// 归一化工具调用签名，让重复守卫按「语义相同」而非「字面相同」判定：
/// - shell：剥离开头的 `cd [/d] <路径> &&` 前缀（取证：94% 命令携带全路径 cd，
///   导致每条命令签名字面唯一、守卫完全失效）；
/// - fs/edit：路径分隔符统一为正斜杠，绝对/相对写法的同一文件归为同一签名。
fn normalized_signature(call: &ToolCall) -> String {
    let mut args = call.args.clone();
    if call.name == "shell" {
        if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
            let stripped = strip_cd_prefix(cmd);
            if stripped != cmd {
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("command".into(), serde_json::Value::String(stripped));
                }
            }
        }
    } else if call.name == "fs" || call.name == "edit" {
        if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
            let normalized = path.replace('\\', "/");
            if normalized != path {
                if let Some(obj) = args.as_object_mut() {
                    obj.insert("path".into(), serde_json::Value::String(normalized));
                }
            }
        }
    }
    format!("{}:{}", call.name, args)
}

/// 反复剥离开头的 `cd [/d] <任意路径>` 段（`&&` / `&` / `;` 分隔）。
/// 若整条命令只有 cd，保留最后一段以免误删全部语义。
fn strip_cd_prefix(cmd: &str) -> String {
    let mut rest = cmd.trim().to_string();
    loop {
        let lower = rest.to_ascii_lowercase();
        if !lower.starts_with("cd ") && !lower.starts_with("cd/") {
            break;
        }
        // 找第一个命令分隔符；没有分隔符说明整条就是 cd，不能删。
        let split_at = ["&&", "&", ";"]
            .iter()
            .filter_map(|sep| rest.find(sep))
            .min();
        let Some(at) = split_at else { break };
        let sep_len = if rest[at..].starts_with("&&") { 2 } else { 1 };
        rest = rest[at + sep_len..].trim().to_string();
        if rest.is_empty() {
            return cmd.trim().to_string();
        }
    }
    rest
}

#[derive(Debug, Clone)]
pub struct Budget {
    pub max_steps: usize,
    pub max_tool_calls: usize,
    pub max_duration: Duration,
    pub convergence_ratio: f32,
    /// 允许的自动续期次数上限：此前无限续期会让单回合步数无上限增长
    /// （实测一个简单任务跑出 1000+ 步）；用尽后必须强制收尾。
    pub max_renewals: u32,
    pub renewals_used: u32,
    /// 交付延展已用次数：常规续期耗尽后，若最近窗口有实际写入产出，
    /// 额外按窗口延展，避免“刚进入编辑阶段就被截断、要用户手动接续”。
    pub delivery_extensions: u32,
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
            max_renewals: std::env::var("HARNESS_MAX_RENEWALS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2)
                .clamp(0, 6),
            renewals_used: 0,
            delivery_extensions: 0,
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
    /// 续期次数超过 `max_renewals` 后返回 `None`：调用方必须强制收尾，
    /// 否则预算会被无限续发（旧实现单回合可跑到数百步）。
    pub fn diagnose_and_renew(state: &mut ExecutionState, budget: &mut Budget) -> Option<String> {
        let step_delta = state.steps.saturating_sub(state.checkpoint_steps);
        let call_delta = state.tool_calls.saturating_sub(state.checkpoint_tool_calls);
        let evidence_delta = state
            .evidence
            .len()
            .saturating_sub(state.checkpoint_evidence);
        let success_delta = state
            .successful_tool_results
            .saturating_sub(state.checkpoint_successes);
        let write_delta = state
            .write_operations
            .saturating_sub(state.checkpoint_writes);
        let repeated_or_low_value = call_delta.saturating_sub(evidence_delta);
        let stagnant = call_delta > 0 && (evidence_delta == 0 || success_delta == 0);

        state.checkpoint_steps = state.steps;
        state.checkpoint_tool_calls = state.tool_calls;
        state.checkpoint_evidence = state.evidence.len();
        state.checkpoint_successes = state.successful_tool_results;
        state.checkpoint_writes = state.write_operations;

        if budget.renewals_used >= budget.max_renewals {
            // 交付延展：常规续期已用尽，但最近窗口有真实写入/编辑产出，说明任务
            // 正在交付阶段（如前期探索耗尽预算、刚进入编辑），自动延展一个窗口，
            // 不把半成品丢给用户手动接续。无写入的空转仍返回 None 走收尾。
            if write_delta > 0 && budget.delivery_extensions < 2 {
                budget.delivery_extensions += 1;
                budget.max_steps = budget.max_steps.saturating_add(budget.step_window);
                budget.max_tool_calls = budget.max_tool_calls.saturating_add(budget.tool_window);
                budget.max_duration = budget.max_duration.saturating_add(budget.duration_window);
                let left = 2 - budget.delivery_extensions;
                return Some(format!(
                    "[交付延展] 最近窗口检测到 {write_delta} 次成功的代码修改，任务正处于活跃交付阶段：预算已延展（剩余 {left} 次交付延展）。不要开启新的探索与扫描，尽快完成剩余修改与一次性验证，然后输出总结交付。{}",
                    evidence_digest(state)
                ));
            }
            return None;
        }
        budget.renewals_used += 1;
        budget.max_steps = budget.max_steps.saturating_add(budget.step_window);
        budget.max_tool_calls = budget.max_tool_calls.saturating_add(budget.tool_window);
        budget.max_duration = budget.max_duration.saturating_add(budget.duration_window);
        let remaining = budget.max_renewals - budget.renewals_used;

        Some(if stagnant {
            format!(
                "[执行检查点] 最近 {step_delta} 步、{call_delta} 次工具调用仅产生 {evidence_delta} 条新证据、{success_delta} 次成功结果，约 {repeated_or_low_value} 次调用没有增加独立证据。任务尚未完成，先明确阻塞原因、放弃重复路径，选择最能推进未满足验收条件的下一步；预算已续期（剩余 {remaining} 次，用尽后必须基于现有证据收尾交付）。{}",
                evidence_digest(state)
            )
        } else {
            format!(
                "[执行检查点] 最近 {step_delta} 步、{call_delta} 次工具调用产生 {evidence_delta} 条新证据、{success_delta} 次成功结果。任务尚未完成时继续推进，但只围绕未满足的验收条件；预算已续期（剩余 {remaining} 次，用尽后必须基于现有证据收尾交付）。{}",
                evidence_digest(state)
            )
        })
    }

    /// 续期耗尽后的最终收尾窗口：给足步骤完成汇总交付，不再扩张。
    pub fn arm_final_window(state: &ExecutionState, budget: &mut Budget) {
        budget.max_steps = state.steps + 6;
        budget.max_tool_calls = state.tool_calls + 4;
        budget.max_duration = budget.max_duration + Duration::from_secs(300);
    }
}

/// 把已获得的证据要点注入检查点提示：上下文被压缩后模型容易“忘记”自己
/// 查过什么、重复读同一文件（取证：单文件最高被读 18 次）；把已有结论
/// 直接放到提示里，减少重复探索、帮助聚焦未满足的验收条件。
fn evidence_digest(state: &ExecutionState) -> String {
    let mut items: Vec<String> = state
        .evidence
        .values()
        .filter(|e| !e.summary.trim().is_empty())
        .take(5)
        .map(|e| {
            let compact: String = e.summary.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("- {}", compact.chars().take(120).collect::<String>())
        })
        .collect();
    if items.is_empty() {
        return String::new();
    }
    items.sort();
    format!("\n[已有证据要点（不要重复获取）]\n{}", items.join("\n"))
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
        } else if ["修改", "修复", "重构", "更新", "改进", "实现", "改造", "调整", "拆分", "迁移"]
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
    fn classifies_ui_rework_as_transformative_not_direct() {
        // 取证：纯 UI 改造任务（“界面排版重新改造一下，分成两个tab页”）旧实现
        // 未命中任何关键词被归为 Direct（12 步），预算与任务规模不匹配。
        let policy = GeneralDomainPolicy;
        assert_eq!(
            policy.select_strategy(&TaskContract::from_input(
                "插件管理，界面排版重新改造一下，分成两个tab页"
            )),
            StrategyKind::Transformative
        );
    }

    #[test]
    fn feature_list_delete_does_not_escalate_risk_to_high() {
        // 取证：任务里“导入、删除、启用、禁用”是功能枚举，不应被判为高风险；
        // 真正危险面（生产/数据库）仍保持 High。
        let ui = TaskContract::from_input("自定义插件可以导入、删除、启用、禁用");
        assert_eq!(ui.risk, RiskLevel::Medium);
        let danger = TaskContract::from_input("删除生产数据库配置");
        assert_eq!(danger.risk, RiskLevel::High);
    }

    #[test]
    fn delivery_extension_granted_only_when_writes_progress() {
        let contract = TaskContract::from_input("修改界面布局");
        let mut budget = BudgetManager::for_contract(&contract, StrategyKind::Transformative);
        let mut state = ExecutionState::new(contract, StrategyKind::Transformative);
        budget.renewals_used = budget.max_renewals; // 常规续期耗尽

        // 无写入的空转：不延展，交给收尾。
        state.steps = 10;
        assert!(BudgetManager::diagnose_and_renew(&mut state, &mut budget).is_none());

        // 有写入的活跃交付：自动延展一个窗口。
        let proposal = ActionProposal {
            signature: "edit:harness-ui/src/gui/model.rs".into(),
            question: "拆分枚举".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        state.record_tool_result(&proposal, true, "edit ok");
        state.steps = 12;
        let before = budget.max_steps;
        let msg = BudgetManager::diagnose_and_renew(&mut state, &mut budget);
        assert!(msg.unwrap().contains("交付延展"));
        assert!(budget.max_steps > before);
        assert_eq!(budget.delivery_extensions, 1);
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
        let diagnosis = BudgetManager::diagnose_and_renew(&mut state, &mut budget).unwrap();
        assert!(diagnosis.contains("任务尚未完成"));
        assert!(diagnosis.contains("预算已续期"));
        assert!(budget.max_steps > original_limit);
        assert_ne!(
            BudgetManager::phase(&state, &budget),
            BudgetPhase::Exhausted
        );
    }

    /// 续期必须有上限（取证：无限续期让简单任务跑出 1000+ 步）；
    /// 用尽后返回 None，调用方据此强制收尾。
    #[test]
    fn renewals_are_capped_and_final_window_is_bounded() {
        let contract = TaskContract::from_input("完成一项多步骤任务");
        let mut budget = BudgetManager::for_contract(&contract, StrategyKind::Direct);
        let mut state = ExecutionState::new(contract, StrategyKind::Direct);
        assert!(budget.max_renewals >= 1);
        for _ in 0..budget.max_renewals {
            state.steps = budget.max_steps;
            assert!(BudgetManager::diagnose_and_renew(&mut state, &mut budget).is_some());
        }
        state.steps = budget.max_steps;
        assert!(BudgetManager::diagnose_and_renew(&mut state, &mut budget).is_none());

        // 最终收尾窗口给足 6 步完成汇总交付，不再扩张。
        BudgetManager::arm_final_window(&state, &mut budget);
        assert_eq!(budget.max_steps, state.steps + 6);
    }

    /// 签名归一化：cd 全路径前缀与路径分隔符差异不应绕过重复守卫
    /// （取证：94% 命令携带 cd 前缀，每条签名字面唯一，守卫全失效）。
    #[test]
    fn signature_normalization_neutralizes_cd_prefix_and_separators() {
        let contract = TaskContract::from_input("完成任务");
        let a = ActionProposal::from_tool_call(
            &ToolCall {
                id: "1".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "cd /d F:\\ws\\proj && cargo check"}),
            },
            &contract,
        );
        let b = ActionProposal::from_tool_call(
            &ToolCall {
                id: "2".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo check"}),
            },
            &contract,
        );
        assert_eq!(a.signature, b.signature);

        // 纯 cd 命令不被误删为空。
        let only_cd = ActionProposal::from_tool_call(
            &ToolCall {
                id: "3".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "cd F:\\ws"}),
            },
            &contract,
        );
        assert!(only_cd.signature.contains("cd"));

        // 同一文件的反斜杠/正斜杠写法归为同一签名。
        let p1 = ActionProposal::from_tool_call(
            &ToolCall {
                id: "4".into(),
                name: "fs".into(),
                args: serde_json::json!({"op": "read", "path": "src\\main.rs"}),
            },
            &contract,
        );
        let p2 = ActionProposal::from_tool_call(
            &ToolCall {
                id: "5".into(),
                name: "fs".into(),
                args: serde_json::json!({"op": "read", "path": "src/main.rs"}),
            },
            &contract,
        );
        assert_eq!(p1.signature, p2.signature);
    }
}
