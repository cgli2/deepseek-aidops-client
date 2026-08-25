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

/// 求解模式决定默认的探索强度，而非业务领域。明确、可比较的异常优先走短闭环，
/// 防止被通用 Agent 当成开放式研究题而全仓泛搜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveMode {
    /// 单点回归：用户给出了一个具体交互、前后行为和可观察的失败结果。
    /// 该模式由 Runtime 走受控短路径，不把任务交给开放式 Agent 自行探索。
    AtomicDelivery,
    FastDiagnosis,
    ScopedDelivery,
    OpenEnded,
}

#[derive(Debug, Clone)]
pub struct SolvePlan {
    pub mode: SolveMode,
    pub initial_steps: usize,
    pub initial_tool_calls: usize,
    pub instructions: String,
}

impl SolvePlan {
    pub fn for_contract(contract: &TaskContract, strategy: StrategyKind) -> Self {
        let text = contract.objective.as_str();
        let is_atomic_regression = contract.risk != RiskLevel::High
            && contract.acceptance_criteria.len() == 1
            && text.chars().count() <= 500
            && [
                "之前", "原来", "加了", "之后", "不再", "失效", "不生效", "没有变化", "回归",
            ]
            .iter()
            .any(|word| text.contains(word));
        if is_atomic_regression
            && !matches!(
                strategy,
                StrategyKind::Investigative | StrategyKind::Comparative | StrategyKind::Monitoring
            )
        {
            return Self {
                mode: SolveMode::AtomicDelivery,
                // 这是“单路径回归”的首个状态机窗口，而不是硬性中断线。窗口内若发生
                // 写入，后续验证仍可正常继续；没有写入的泛搜则不能靠续期维持空转。
                initial_steps: 6,
                initial_tool_calls: 8,
                instructions: "[原子交付模式] 这是一个单点回归，不要创建计划、委派子代理或解释长篇思路。严格按：1) 用一个与用户描述直接对应的高信号符号/路径定位；2) 仅读取命中处及紧邻调用链；3) 做最小修复；4) 运行一次相关验证并交付。首次 search 命中后，不得再做无目录限定的搜索；成功调用不得重试，写入后才可重跑相同验证。每一步只执行当前阶段唯一必要的动作。".into(),
            };
        }
        let mentions_surface = ["界面", "面板", "UI", "显示", "结果"]
            .iter()
            .any(|word| text.contains(word));
        let mentions_source = ["命令", "终端", "接口", "API", "数据库", "文件"]
            .iter()
            .any(|word| text.contains(word));
        let mentions_mismatch = ["不一致", "不对", "为空", "没有", "但", "而"]
            .iter()
            .any(|word| text.contains(word));
        if mentions_surface && mentions_source && mentions_mismatch {
            return Self {
                mode: SolveMode::FastDiagnosis,
                initial_steps: 8,
                initial_tool_calls: 10,
                instructions: "[快速诊断模式] 这是一个可比较的数据/界面不一致问题。严格按 Observe → Compare → Locate → Fix/Report 执行：先确认两端资源身份（路径、项目、环境、配置），再比较原始数据与 Provider/UI 数据。每次工具调用必须用于区分具体假设；先执行最多 3 个确定性探针，再只读取最短依赖链中的文件。禁止全仓泛搜、重复读取或为了理解而扩展范围；证据足够时立即修复并验证，若无需修改则直接给出根因与证据。".into(),
            };
        }
        if matches!(strategy, StrategyKind::Transformative | StrategyKind::Verification) {
            return Self {
                mode: SolveMode::ScopedDelivery,
                // 这是首个收敛检查点而非任务硬截止。对一个明确的小改动，10 步/12 次
                // 工具调用足够完成“定位 → 修改 → 验证”；更复杂的任务仍可凭真实产出续期。
                initial_steps: 10,
                initial_tool_calls: 12,
                instructions: "[范围受限交付模式] 先用一个高信号的代码符号/路径搜索定位与验收条件直接相关的最小文件集；命中后集中读取必要区间、完成最小修改、执行一次针对性验证。不要从 UI 文案或泛化自然语言开始搜索；同一成功调用不得重试，失败调用最多定向重试一次。没有新证据时换假设，不做全仓扫描。".into(),
            };
        }
        Self {
            mode: SolveMode::OpenEnded,
            initial_steps: 24,
            initial_tool_calls: 32,
            instructions: "[探索模式] 先列出可验证假设并按信息增益选择下一步；每个阶段结束时压缩已确认事实，避免重复探索。".into(),
        }
    }
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
        } else if [
            "删除", "修改", "修复", "重构", "实现", "安装", "改造", "调整",
        ]
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
            "[本回合执行契约]\n目标：{}\n策略：{:?}\n验收：{}\n约束：{}\n进展检查点：每 {} 个步骤或 {} 次工具调用评估一次并按需续期（最多续期 {} 次，用尽后必须基于现有证据收尾）。执行准则：最小路径优先——先直接定位与目标直接相关的最小文件集，禁止全仓库泛扫与重复读取已读文件；每次探索必须消除具体不确定性或决定下一步，范围明确的小任务在首次定位后转入修改与验证，范围未知或调查型任务可保留必要取证；同一工具调用未带来新信息时立即换路或收尾；交付目标达成即停止，不做重复确认与打磨。",
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
    pub solve_mode: SolveMode,
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
        let solve_mode = SolvePlan::for_contract(&contract, strategy).mode;
        Self {
            contract,
            strategy,
            solve_mode,
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

    /// 上游因输出/上下文长度而未返回可用内容时，Runtime 仅携带此紧凑检查点重试，
    /// 不把数十轮工具原文再次塞回请求导致“越重试越长”。
    pub fn compact_checkpoint(&self) -> String {
        let evidence = self
            .evidence
            .values()
            .filter(|item| !item.summary.trim().is_empty())
            .take(4)
            .map(|item| {
                let summary: String = item.summary.split_whitespace().collect::<Vec<_>>().join(" ");
                format!("- {}", summary.chars().take(160).collect::<String>())
            })
            .collect::<Vec<_>>();
        format!(
            "[长度恢复检查点]\n目标：{}\n已执行：{} 步、{} 次工具调用（成功 {}、失败 {}）\n已满足验收：{}\n关键证据：{}\n下一步：只执行一个最能完成未满足验收条件的动作，或直接给出结构化结论；不要复述全过程。",
            self.contract.objective,
            self.steps,
            self.tool_calls,
            self.successful_tool_results,
            self.failed_tool_results,
            if self.satisfied_criteria.is_empty() { "暂无".into() } else { self.satisfied_criteria.iter().cloned().collect::<Vec<_>>().join("、") },
            if evidence.is_empty() { "暂无可复用证据".into() } else { evidence.join("\n") },
        )
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
    /// 进展延展已用次数：常规续期耗尽后，最近窗口只要产生可验证的写入或新证据，
    /// 就继续按窗口延展。排障、测试、审查本来就未必会修改代码，不能把它们误杀。
    pub delivery_extensions: u32,
    /// 连续“预算耗尽且没有可验证进展”的窗口数：用于增强换路提示，
    /// 不作为中断未完成任务的依据。
    pub stagnant_windows: u32,
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
            stagnant_windows: 0,
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

    /// 与步数同理，初始工具调用上限是阶段检查点，不是未完成任务的终止线。
    pub fn cap_initial_tool_window(budget: &mut Budget, calls: usize) {
        let calls = calls.max(1);
        budget.max_tool_calls = budget.max_tool_calls.min(calls);
        budget.tool_window = budget.max_tool_calls;
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
        let stagnant = evidence_delta == 0 || success_delta == 0;
        // “进展”不等同于“写了代码”：成功测试、定位到新根因、得到新的只读证据
        // 都能实质推进任务。仅在没有任何可验证进展时才记为停滞。
        // 交付型任务中，“又成功读到一个文件/搜索到一条命中”不是续期理由；否则模型
        // 只要不断换关键词泛搜，就能把预算无限延长。调查/比较/验证类任务则允许由独立
        // 证据续期，因为它们的交付物本来就是结论而非写入。
        let read_only_delivery = matches!(
            state.strategy,
            StrategyKind::Investigative | StrategyKind::Comparative | StrategyKind::Verification
        );
        let meaningful_progress = write_delta > 0
            || (read_only_delivery && evidence_delta > 0 && success_delta > 0);

        state.checkpoint_steps = state.steps;
        state.checkpoint_tool_calls = state.tool_calls;
        state.checkpoint_evidence = state.evidence.len();
        state.checkpoint_successes = state.successful_tool_results;
        state.checkpoint_writes = state.write_operations;

        if budget.renewals_used >= budget.max_renewals {
            // 进展延展：常规续期已用尽，但窗口内仍有可验证进展就继续。
            // 不能只认代码写入，否则排障/测试/审查等任务会在完成前被错误中断。
            if meaningful_progress {
                budget.delivery_extensions += 1;
                budget.stagnant_windows = 0;
                Self::extend_window(budget);
                let progress = if write_delta > 0 {
                    format!("{write_delta} 次成功的代码修改")
                } else {
                    format!("{evidence_delta} 条新证据和 {success_delta} 次成功结果")
                };
                return Some(format!(
                    "[进展延展·第{}次] 最近窗口检测到 {progress}，任务仍在有效推进：预算已自动延展。围绕未满足验收条件继续，完成后进行必要验证并输出总结。{}",
                    budget.delivery_extensions,
                    evidence_digest(state)
                ));
            }
            budget.stagnant_windows += 1;
            return None;
        }
        budget.renewals_used += 1;
        Self::extend_window(budget);
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

    /// 按一个窗口延展步数/工具/时长预算（续期、交付延展、自动接续共用）。
    pub fn extend_window(budget: &mut Budget) {
        budget.max_steps = budget.max_steps.saturating_add(budget.step_window);
        budget.max_tool_calls = budget.max_tool_calls.saturating_add(budget.tool_window);
        budget.max_duration = budget.max_duration.saturating_add(budget.duration_window);
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
        state: &ExecutionState,
        _budget: &Budget,
    ) -> GateDecision {
        if proposal.supports.is_empty() {
            return GateDecision::Deny("该调用未关联任何验收标准".into());
        }
        if state.solve_mode == SolveMode::AtomicDelivery {
            let sig = proposal.signature.as_str();
            if sig.starts_with("plan:") || sig.starts_with("delegate:") {
                return GateDecision::Deny(
                    "原子交付任务不允许计划或委派；直接执行当前路径的定位、修改或验证".into(),
                );
            }
            if sig.starts_with("fs:") && sig.contains("\"op\":\"list\"") {
                return GateDecision::Deny(
                    "原子交付任务禁止列目录泛扫；先使用一个高信号 search 定位".into(),
                );
            }
            if sig.starts_with("search:") {
                let already_located = state
                    .evidence
                    .keys()
                    .any(|signature| signature.starts_with("search:"));
                if already_located && !sig.contains("\"dir\":") {
                    return GateDecision::Deny(
                        "首次定位已有结果；后续 search 必须限定到命中目录或验证不同的局部假设"
                            .into(),
                    );
                }
            }
            if sig.starts_with("shell:") && state.write_operations == 0 {
                return GateDecision::Deny(
                    "原子交付任务的 shell 仅用于修改后的针对性验证；先定位并完成最小修改"
                        .into(),
                );
            }
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
        } else if [
            "修改", "修复", "重构", "更新", "改进", "实现", "改造", "调整", "拆分", "迁移",
        ]
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
    fn routes_surface_source_mismatches_to_fast_diagnosis() {
        let contract = TaskContract::from_input(
            "终端 git 命令能看到变更，但界面 Git 面板显示为空，为什么不一致？",
        );
        let plan = SolvePlan::for_contract(&contract, StrategyKind::Investigative);
        assert_eq!(plan.mode, SolveMode::FastDiagnosis);
        assert!(plan.initial_tool_calls <= 10);
        assert!(plan.instructions.contains("Observe"));
    }

    #[test]
    fn scoped_delivery_has_a_small_initial_convergence_window() {
        let contract = TaskContract::from_input("调整一个会话气泡的对齐方式");
        let plan = SolvePlan::for_contract(&contract, StrategyKind::Transformative);
        assert_eq!(plan.mode, SolveMode::ScopedDelivery);
        assert!(plan.initial_steps <= 10);
        assert!(plan.initial_tool_calls <= 12);
        assert!(plan.instructions.contains("高信号"));
    }

    #[test]
    fn atomic_regression_uses_a_short_state_machine_window() {
        let contract = TaskContract::from_input(
            "输入优化加了 loading 之后，结果没有变化，修复这个单点回归",
        );
        let plan = SolvePlan::for_contract(&contract, StrategyKind::Transformative);
        assert_eq!(plan.mode, SolveMode::AtomicDelivery);
        assert_eq!(plan.initial_steps, 6);
        assert_eq!(plan.initial_tool_calls, 8);
        assert!(plan.instructions.contains("不要创建计划"));
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

        // 无写入的空转：不延展，交给收尾；空转窗口开始计数。
        state.steps = 10;
        assert!(BudgetManager::diagnose_and_renew(&mut state, &mut budget).is_none());
        assert_eq!(budget.stagnant_windows, 1);

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
        assert!(msg.unwrap().contains("进展延展"));
        assert!(budget.max_steps > before);
        assert_eq!(budget.delivery_extensions, 1);
        // 有真实产出后空转计数清零：不会被误判为卡死。
        assert_eq!(budget.stagnant_windows, 0);

        // 持续写入 → 持续延展（不设上限）：未完成但正在产出的任务不被截断。
        state.record_tool_result(&proposal, true, "edit ok");
        state.steps = 14;
        let msg2 = BudgetManager::diagnose_and_renew(&mut state, &mut budget);
        assert!(msg2.unwrap().contains("进展延展"));
        assert_eq!(budget.delivery_extensions, 2);

        // 写入停止（空转）→ 不再延展，交给收尾；空转计数重新累计。
        state.steps = 16;
        assert!(BudgetManager::diagnose_and_renew(&mut state, &mut budget).is_none());
        assert_eq!(budget.stagnant_windows, 1);
    }

    #[test]
    fn evidence_progress_extends_without_code_changes() {
        let contract = TaskContract::from_input("排查服务延迟的根因");
        let mut budget = BudgetManager::for_contract(&contract, StrategyKind::Investigative);
        budget.renewals_used = budget.max_renewals;
        let mut state = ExecutionState::new(contract, StrategyKind::Investigative);
        let proposal = ActionProposal {
            signature: "shell:{\"command\":\"collect latency metrics\"}".into(),
            question: "收集延迟指标".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        state.record_tool_result(&proposal, true, "发现数据库连接池等待是主要耗时");
        state.steps = 10;

        let message = BudgetManager::diagnose_and_renew(&mut state, &mut budget).unwrap();
        assert!(message.contains("进展延展"));
        assert_eq!(state.write_operations, 0);
        assert_eq!(budget.stagnant_windows, 0);
    }

    #[test]
    fn delivery_task_does_not_extend_for_read_only_exploration() {
        let contract = TaskContract::from_input("调整一个会话气泡的对齐方式");
        let mut budget = BudgetManager::for_contract(&contract, StrategyKind::Transformative);
        budget.renewals_used = budget.max_renewals;
        let mut state = ExecutionState::new(contract, StrategyKind::Transformative);
        let proposal = ActionProposal {
            signature: "search:{\"pattern\":\"bubble\"}".into(),
            question: "搜索候选文件".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        state.record_tool_result(&proposal, true, "找到若干候选文件");
        state.steps = 10;

        assert!(BudgetManager::diagnose_and_renew(&mut state, &mut budget).is_none());
        assert_eq!(budget.delivery_extensions, 0);
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
    fn atomic_gate_blocks_broad_second_search_and_pre_change_verification() {
        let contract = TaskContract::from_input(
            "输入优化加了 loading 之后，结果没有变化，修复这个单点回归",
        );
        let budget = BudgetManager::for_contract(&contract, StrategyKind::Transformative);
        let mut state = ExecutionState::new(contract, StrategyKind::Transformative);
        assert_eq!(state.solve_mode, SolveMode::AtomicDelivery);
        let locate = ActionProposal {
            signature: "search:{\"pattern\":\"optimizing\"}".into(),
            question: "定位状态字段".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        assert_eq!(
            ActionGate::authorize(&locate, &state, &budget),
            GateDecision::Allow
        );
        state.record_tool_result(&locate, true, "composer.rs:91");
        assert!(matches!(
            ActionGate::authorize(&locate, &state, &budget),
            GateDecision::Deny(_)
        ));
        let scoped_search = ActionProposal {
            signature: "search:{\"dir\":\"harness-ui/src/gui\",\"pattern\":\"poll_optimize\"}"
                .into(),
            question: "验证紧邻调用链".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        assert_eq!(
            ActionGate::authorize(&scoped_search, &state, &budget),
            GateDecision::Allow
        );
        let verify = ActionProposal {
            signature: "shell:{\"command\":\"cargo test -p harness-ui\"}".into(),
            question: "验证修复".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        assert!(matches!(
            ActionGate::authorize(&verify, &state, &budget),
            GateDecision::Deny(_)
        ));
        let edit = ActionProposal {
            signature: "edit:{\"path\":\"harness-ui/src/gui/composer.rs\"}".into(),
            question: "修复回填".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        state.record_tool_result(&edit, true, "updated");
        assert_eq!(
            ActionGate::authorize(&verify, &state, &budget),
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
