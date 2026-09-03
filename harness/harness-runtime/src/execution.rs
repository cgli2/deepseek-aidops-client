//! 通用 Agent 执行控制：任务契约、状态、行动门禁、动态预算与完成判定。
//!
//! 该模块不依赖代码、研究或运维领域；领域适配器只负责分类和调整预算。

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::intent::{IntentKind, IntentProfile};
use harness_llm::ToolCall;
use harness_session::{DeliveryCriterion, DeliveryOutcome, DeliveryReport};

/// 判断 edit 是否真正改变了非注释产物。它不是完整语法分析器，只负责挡住最危险的
/// 假绿：在任意源码文件里加一行注释来满足“发生过写入”的计数。无法解析或整文件
/// 写入时保守视为实质变更，避免误伤未知工具 schema。
pub(crate) fn edit_has_substantive_delta(signature: &str) -> bool {
    let Some(raw) = signature.strip_prefix("edit:") else {
        return true;
    };
    let Ok(args) = serde_json::from_str::<serde_json::Value>(raw) else {
        return true;
    };
    let (Some(old_text), Some(new_text)) = (
        args.get("old_text").and_then(|value| value.as_str()),
        args.get("new_text").and_then(|value| value.as_str()),
    ) else {
        return true;
    };
    fn without_comment_only_lines(value: &str) -> String {
        value
            .lines()
            .map(str::trim)
            .filter(|line| {
                !line.is_empty()
                    && !line.starts_with("//")
                    && !line.starts_with('#')
                    && !line.starts_with("/*")
                    && !line.starts_with('*')
                    && !line.starts_with("<!--")
                    && !line.starts_with("-->")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    without_comment_only_lines(old_text) != without_comment_only_lines(new_text)
}

fn objective_allows_comment_only_change(objective: &str) -> bool {
    let lower = objective.to_lowercase();
    [
        "注释",
        "文档",
        "说明文字",
        "readme",
        "markdown",
        "doc comment",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

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
    /// 只读诊断：需要最少一条工作区证据，但不允许静默进入代码写入。
    GuidedInvestigation,
    FastDiagnosis,
    ScopedDelivery,
    /// 多交付面或高风险目标：按独立验收面分阶段推进，每个阶段形成可恢复检查点。
    StagedDelivery,
    OpenEnded,
}

/// 任务形状只使用契约中已经存在的结构化事实，不用业务词表猜“这是不是大任务”。
/// 它决定规划展开到哪一层：精确变换直达下一动作；多验收面/高风险才展开阶段图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskClarity {
    Exact,
    Groundable,
    Discovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskScale {
    Atomic,
    Scoped,
    Staged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskShape {
    pub clarity: TaskClarity,
    pub scale: TaskScale,
}

impl TaskShape {
    pub fn for_contract(contract: &TaskContract) -> Self {
        let intent = IntentProfile::compile(&contract.objective);
        let clarity = if intent.has_transformation_contract || intent.has_structural_action {
            TaskClarity::Exact
        } else if intent.has_code_entity || intent.navigation_present {
            TaskClarity::Groundable
        } else {
            TaskClarity::Discovery
        };
        let scale = if clarity == TaskClarity::Exact
            && contract.risk != RiskLevel::High
            && contract.acceptance_criteria.len() == 1
            && contract.objective.chars().count() <= 500
        {
            TaskScale::Atomic
        } else if contract.risk == RiskLevel::High || contract.acceptance_criteria.len() > 1 {
            TaskScale::Staged
        } else {
            TaskScale::Scoped
        };
        Self { clarity, scale }
    }
}

/// 工具调用的运行时阶段。它是实际执行状态的投影，不接受模型的计划文本推动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPhase {
    Locate,
    Inspect,
    Change,
    Verify,
    Conclude,
    Explore,
}

impl ToolPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Locate => "locate",
            Self::Inspect => "inspect",
            Self::Change => "change",
            Self::Verify => "verify",
            Self::Conclude => "conclude",
            Self::Explore => "explore",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SolvePlan {
    pub mode: SolveMode,
    pub initial_steps: usize,
    pub initial_tool_calls: usize,
    /// 任务总预算由运行时预先确定；命中后只允许收尾，绝不自动扩张。
    pub hard_max_steps: usize,
    pub hard_max_tool_calls: usize,
    pub instructions: String,
}

impl SolvePlan {
    pub fn for_contract(contract: &TaskContract, strategy: StrategyKind) -> Self {
        let text = contract.objective.as_str();
        let intent = IntentProfile::compile(text);
        let shape = TaskShape::for_contract(contract);
        // 交付面数量由验收项（契约真实单元）给出，而非机制层词表数 UI 名词。
        let extra_surfaces = contract.acceptance_criteria.len().saturating_sub(1).min(4);
        let is_atomic_regression =
            shape.scale == TaskScale::Atomic && intent.kind == IntentKind::AtomicRegression;
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
                hard_max_steps: 8,
                hard_max_tool_calls: 10,
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
                hard_max_steps: 12,
                hard_max_tool_calls: 16,
                instructions: "[快速诊断模式] 这是一个可比较的数据/界面不一致问题。严格按 Observe → Compare → Locate → Fix/Report 执行：先确认两端资源身份（路径、项目、环境、配置），再比较原始数据与 Provider/UI 数据。每次工具调用必须用于区分具体假设；先执行最多 3 个确定性探针，再只读取最短依赖链中的文件。禁止全仓泛搜、重复读取或为了理解而扩展范围；证据足够时立即修复并验证，若无需修改则直接给出根因与证据。".into(),
            };
        }
        if intent.kind == IntentKind::Investigation || strategy == StrategyKind::Investigative {
            return Self {
                mode: SolveMode::GuidedInvestigation,
                initial_steps: 6,
                initial_tool_calls: 8,
                hard_max_steps: 10,
                hard_max_tool_calls: 12,
                instructions: "[受控诊断模式] 这是只读诊断请求。先执行一个能区分假设的高信号定位；命中后只读取最短调用链并给出有证据的根因。禁止编辑、委派、目录泛扫和无证据结论；连续两个动作无信息时停止并提出精确问题。".into(),
            };
        }
        if shape.scale == TaskScale::Staged
            && matches!(
                strategy,
                StrategyKind::Transformative
                    | StrategyKind::Verification
                    | StrategyKind::Generative
            )
        {
            let surfaces = contract.acceptance_criteria.len().max(1);
            return Self {
                mode: SolveMode::StagedDelivery,
                // 首窗口只需要完成“共同定位 + 第一个纵向切片”；完整额度随后仍由
                // GoalExecution 的逐面预算供给，避免大目标一开始就灌满上下文。
                initial_steps: 10 + extra_surfaces,
                initial_tool_calls: 12 + extra_surfaces * 2,
                hard_max_steps: 20 + extra_surfaces * 2,
                hard_max_tool_calls: 24 + extra_surfaces * 3,
                instructions: format!(
                    "[分阶段交付模式] 该目标包含 {surfaces} 个独立验收面或高风险边界。先建立共享入口与依赖关系，只展开当前可闭环的最小纵向切片；每个切片严格执行 Locate → Inspect → Change → Verify，验证后形成检查点再推进下一面。不得同时研究所有模块、不得用全仓扫描代替依赖判断，也不得因第一个面完成而提前交付。遇到会改变产品行为、兼容性或数据边界的歧义时，保留已确认事实并只提出一个决策问题。"
                ),
            };
        }
        if matches!(
            strategy,
            StrategyKind::Transformative | StrategyKind::Verification
        ) {
            let initial_steps = 10 + extra_surfaces;
            let initial_tool_calls = 12 + extra_surfaces * 2;
            let hard_max_steps = 20 + extra_surfaces * 2;
            let hard_max_tool_calls = 24 + extra_surfaces * 3;
            let surface_instruction = (contract.acceptance_criteria.len() > 1).then(|| {
                format!(
                    "[多交付面] 用户明确列出 {} 个交付面。将它们作为独立修改面：先用一个共同的高信号符号定位共享实现，再逐一确认各面的映射；不得只修第一个命中处就宣告完成。",
                    contract.acceptance_criteria.len()
                )
            });
            return Self {
                mode: SolveMode::ScopedDelivery,
                // 这是首个收敛检查点而非任务硬截止。对一个明确的小改动，10 步/12 次
                // 工具调用足够完成“定位 → 修改 → 验证”；更复杂的任务仍可凭真实产出续期。
                initial_steps,
                initial_tool_calls,
                // 明确改动允许一次定向换路，但不能由软预算续期变成数十次泛搜。
                hard_max_steps,
                hard_max_tool_calls,
                instructions: format!(
                    "[范围受限交付模式] {}\n先用一个高信号的代码符号/路径搜索定位与验收条件直接相关的最小文件集；命中后集中读取必要区间、完成最小修改、执行一次针对性验证。不要从 UI 文案或泛化自然语言开始搜索；同一成功调用不得重试，失败调用最多定向重试一次。没有新证据时换假设，不做全仓扫描。",
                    surface_instruction.unwrap_or_default()
                ),
            };
        }
        Self {
            mode: SolveMode::OpenEnded,
            initial_steps: 24,
            initial_tool_calls: 32,
            hard_max_steps: 36,
            hard_max_tool_calls: 48,
            instructions: "[渐进探索模式] 当前请求尚未形成精确变换或稳定交付边界。先分离已知事实、可由工作区验证的未知项、以及只有用户能决定的语义；按概率和成本排列有限假设，每次只执行一个能改变下一步决策的高信息增益动作。最多完成一轮受限定位后重新评估：证据足以推出可观察终态则收敛为范围受限交付；不同解释会改变产品行为、兼容性或数据边界时，只提出一个带证据的决策问题。禁止用全仓扫描掩盖目标不清，也禁止在验收条件未形成前进行大面积写入。".into(),
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
    /// 由同一问题中列出的多个界面自动拆分的验收项；它们可共享一次整体验证。
    pub inferred_surface_criteria: bool,
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
        let exact_transformation = crate::goal_execution::extract_exact_transformation(input);
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
        // 用户在一句话中明确列出多个页面/操作面时，它们不是同一个模糊验收项。
        // 拆分后，运行时可以显示真实剩余项，并要求修改覆盖每个交付面。
        let inferred_surface_items = if listed_items.is_empty() {
            [
                ("列表", "列表展示"),
                ("详情", "详情展示"),
                ("新增", "新增表单"),
                ("编辑", "编辑表单"),
                ("表单", "表单展示"),
                ("弹窗", "弹窗展示"),
            ]
            .iter()
            .filter(|(surface, _)| input.contains(*surface))
            .map(|(_, description)| (*description).to_string())
            .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let inferred_surface_criteria = listed_items.is_empty() && inferred_surface_items.len() > 1;
        let mut constraints = Vec::new();
        for marker in ["不要", "仅", "只", "禁止", "不得", "不需要"] {
            if input.contains(marker) {
                constraints.push(format!("遵守用户包含“{marker}”的范围约束"));
            }
        }
        if exact_transformation.is_some() {
            constraints.push("只改变目标值，保持周边行为与调用关系不变".into());
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
        let deliverables = if !listed_items.is_empty() {
            listed_items.clone()
        } else if inferred_surface_items.len() > 1 {
            inferred_surface_items.clone()
        } else {
            vec![objective.clone()]
        };
        let acceptance_criteria = if !listed_items.is_empty() || inferred_surface_items.len() > 1 {
            let items = if listed_items.is_empty() {
                inferred_surface_items
            } else {
                listed_items
            };
            items
                .into_iter()
                .enumerate()
                .map(|(index, description)| Criterion {
                    id: format!("item-{}", index + 1),
                    description,
                })
                .collect()
        } else if let Some(transformation) = &exact_transformation {
            let from = transformation
                .from_value
                .as_deref()
                .map(|value| format!("「{value}」"))
                .unwrap_or_else(|| "目标位置当前值".into());
            vec![Criterion {
                // 保持单交付面稳定 id，避免已有工具调用/续跑报告因描述细化而失联。
                id: "user-objective".into(),
                description: format!(
                    "将 {from} 精确变更为「{}」，且不改变周边行为",
                    transformation.to_value
                ),
            }]
        } else {
            vec![Criterion {
                id: "user-objective".into(),
                // 验收项必须保留用户真正要求的可观察结果。旧的通用占位语句让
                // 任意成功 edit + cargo check 都能被映射到同一个空洞 criterion，
                // 最终出现“改了无关文件但 Verified”的假绿。
                description: format!("完成用户目标并验证可观察结果：{objective}"),
            }]
        };
        Self {
            objective: objective.clone(),
            deliverables,
            acceptance_criteria,
            inferred_surface_criteria,
            scope: Vec::new(),
            constraints,
            uncertainties: Vec::new(),
            risk,
        }
    }

    pub fn render_for_model(&self, strategy: StrategyKind, budget: &Budget) -> String {
        let shape = TaskShape::for_contract(self);
        let constraints = if self.constraints.is_empty() {
            "无额外显式约束".to_string()
        } else {
            self.constraints.join("；")
        };
        format!(
            "[本回合执行契约]\n目标：{}\n任务形状：清晰度={:?}，规模={:?}\n策略：{:?}\n验收：{}\n约束：{}\n进展检查点：每 {} 个步骤或 {} 次工具调用评估一次并按需续期（最多续期 {} 次，用尽后必须基于现有证据收尾）。执行准则：最小路径优先——先直接定位与目标直接相关的最小文件集，禁止全仓库泛扫与重复读取已读文件；每次探索必须消除具体不确定性或决定下一步，范围明确的小任务在首次定位后转入修改与验证，范围未知或调查型任务可保留必要取证；多项任务中每次工具调用只推进当前未满足的一项验收，验证证据也只计入该项；同一工具调用未带来新信息时立即换路或收尾；交付目标达成即停止，不做重复确认与打磨。",
            self.objective,
            shape.clarity,
            shape.scale,
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
    /// 写工具尝试次数（无论成功与否）。一旦模型已经尝试写入，Runtime 就已经获得
    /// “这是变更任务”的结构化事实；即使自然语言分类漏判，也不能再走只读直答出口。
    pub write_attempts: usize,
    /// 成功的写入/编辑次数：区分“正在产出交付”与“空转探索”的关键信号，
    /// 供续期耗尽后的交付延展判定使用。
    pub write_operations: usize,
    /// 已经实际写入的验收项。多交付面任务必须逐项完成写入，不能在第一个编辑后
    /// 提前进入验证阶段。
    pub changed_criteria: HashSet<String>,
    pub evidence: HashMap<String, Evidence>,
    pub decisions: Vec<DecisionRecord>,
    pub satisfied_criteria: HashSet<String>,
    /// 验收项 → 成功验证证据。读取、搜索和模型自述都不能作为“已交付”的证据。
    pub verification_evidence: HashMap<String, Vec<String>>,
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
            write_attempts: 0,
            write_operations: 0,
            changed_criteria: HashSet::new(),
            evidence: HashMap::new(),
            decisions: Vec::new(),
            satisfied_criteria: HashSet::new(),
            verification_evidence: HashMap::new(),
            checkpoint_steps: 0,
            checkpoint_tool_calls: 0,
            checkpoint_evidence: 0,
            checkpoint_successes: 0,
            checkpoint_writes: 0,
        }
    }

    pub fn record_tool_result(&mut self, proposal: &ActionProposal, ok: bool, summary: &str) {
        // 搜索服务把“无命中”作为成功返回（工具本身正常执行），但它对目标定位
        // 是明确的负证据。若把它当作成功，状态机会错误进入 Inspect，随后模型便会
        // 用不同关键词反复搜索直至预算耗尽。
        let effective_ok = ok && !proposal.is_search_miss(summary);
        // 先于 ok 判定记录尝试。失败 edit 后运行一次 cargo check 只能证明旧基线可编译，
        // 不能把“零写入”洗成已交付。
        let is_write = proposal.signature.starts_with("edit:")
            || proposal.signature.contains("\"op\":\"write\"");
        let substantive_write = !is_write
            || edit_has_substantive_delta(&proposal.signature)
            || objective_allows_comment_only_change(&self.contract.objective);
        if is_write {
            self.write_attempts += 1;
        }
        if effective_ok {
            self.successful_tool_results += 1;
            // 归一化签名里 edit 工具以 "edit:" 开头；fs 写入的 JSON 参数含 "op":"write"。
            if is_write && substantive_write {
                self.write_operations += 1;
                self.changed_criteria
                    .extend(proposal.supports.iter().cloned());
            }
            if self.is_verification(proposal)
                && (self.write_operations > 0 || self.strategy == StrategyKind::Verification)
            {
                let evidence = format!(
                    "{} => {}",
                    proposal.signature,
                    summary.chars().take(480).collect::<String>()
                );
                for criterion_id in &proposal.supports {
                    self.verification_evidence
                        .entry(criterion_id.clone())
                        .or_default()
                        .push(evidence.clone());
                }
                self.satisfied_criteria
                    .extend(proposal.supports.iter().cloned());
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

    /// 续跑时只继承上一回合已经由 Runtime 验证过的验收项；搜索、计划文本和
    /// 模型自述都不能让新回合跳过验证。预算本身重新开启一个有界窗口，避免
    /// 因历史累计值在首步立即再次触发硬熔断。
    pub fn restore_verified_criteria(&mut self, report: &DeliveryReport) {
        for criterion in &report.criteria {
            if !criterion.satisfied {
                continue;
            }
            let evidence = if criterion.evidence.is_empty() {
                vec!["上一回合已通过运行时验证".into()]
            } else {
                criterion.evidence.clone()
            };
            self.verification_evidence
                .insert(criterion.id.clone(), evidence);
            self.satisfied_criteria.insert(criterion.id.clone());
        }
    }

    /// 将运行时已经从磁盘产物复核出的静态证据同步到执行投影。
    ///
    /// SolveGraph 与 ExecutionState 都参与最终完成裁决；只更新前者会出现“求解图
    /// 已无下一步，但执行投影仍缺验证”的死区，迫使用户反复输入“继续”。
    pub fn record_static_verification(&mut self, criterion_id: &str, evidence: impl Into<String>) {
        if !self
            .contract
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.id == criterion_id)
        {
            return;
        }
        self.verification_evidence
            .entry(criterion_id.to_string())
            .or_default()
            .push(evidence.into());
        self.satisfied_criteria.insert(criterion_id.to_string());
    }

    /// 动态白名单只由已获得的证据/写入/验证状态推进。高精确任务不再把
    /// plan、delegate 或无关工具 schema 暴露给模型，且 dispatch 前还会二次校验。
    pub fn tool_phase(&self) -> ToolPhase {
        if self.can_complete() && !self.satisfied_criteria.is_empty() {
            return ToolPhase::Conclude;
        }
        // 纯核验请求（如“检查目录”）的首个动作本来就是受控 shell/test，
        // 不能强迫它先做一次无意义搜索再获得验证工具。
        if self.strategy == StrategyKind::Verification && self.write_operations == 0 {
            return ToolPhase::Verify;
        }
        match self.solve_mode {
            SolveMode::AtomicDelivery
            | SolveMode::ScopedDelivery
            | SolveMode::StagedDelivery
            | SolveMode::FastDiagnosis => {
                if self.write_operations > 0 {
                    let all_change_surfaces_written = self
                        .contract
                        .acceptance_criteria
                        .iter()
                        .all(|criterion| self.changed_criteria.contains(&criterion.id));
                    if !all_change_surfaces_written {
                        ToolPhase::Change
                    } else if self.verification_evidence.is_empty() {
                        ToolPhase::Verify
                    } else {
                        ToolPhase::Conclude
                    }
                } else if self.evidence.is_empty() {
                    ToolPhase::Locate
                } else if self.evidence.keys().any(|key| key.starts_with("fs:")) {
                    ToolPhase::Change
                } else if self.evidence.keys().any(|key| key.starts_with("search:")) {
                    ToolPhase::Inspect
                } else {
                    ToolPhase::Change
                }
            }
            SolveMode::GuidedInvestigation => {
                if self.evidence.is_empty() {
                    ToolPhase::Locate
                } else {
                    ToolPhase::Inspect
                }
            }
            SolveMode::OpenEnded => ToolPhase::Explore,
        }
    }

    pub fn allowed_tools(&self) -> Vec<String> {
        let names: &[&str] = match self.tool_phase() {
            ToolPhase::Locate => &["search"],
            ToolPhase::Inspect => &["fs", "search"],
            ToolPhase::Change => &["edit", "fs"],
            ToolPhase::Verify => &["shell"],
            ToolPhase::Conclude => &[],
            ToolPhase::Explore => &["fs", "edit", "shell", "search", "plan", "delegate"],
        };
        names.iter().map(|name| (*name).to_string()).collect()
    }

    pub fn allows_tool(&self, tool: &str) -> bool {
        // 开放探索阶段保留插件/测试注入的自定义工具；精确交付阶段才执行
        // 闭合白名单，避免把扩展生态误判为未知工具。
        if self.tool_phase() == ToolPhase::Explore {
            return true;
        }
        self.allowed_tools().iter().any(|allowed| allowed == tool)
    }

    fn is_verification(&self, proposal: &ActionProposal) -> bool {
        Self::is_verification_signature(&proposal.signature)
    }

    fn is_verification_signature(signature: &str) -> bool {
        if !signature.starts_with("shell:") {
            return false;
        }
        let signature = signature.to_ascii_lowercase();
        [
            "cargo test",
            "cargo check",
            "cargo build",
            "npm test",
            "npm run test",
            "npm run build",
            "pnpm test",
            "yarn test",
            "pytest",
            "python -m pytest",
            "py_compile",
            "go test",
            "mvn test",
            "gradle test",
            "tsc ",
            "git diff --check",
        ]
        .iter()
        .any(|marker| signature.contains(marker))
    }

    /// 对需要实际变更或核验的任务，最终回复前必须有成功验证，且每个验收项均被
    /// 该验证覆盖。这样“模型停止调用工具”不再能伪造成成功交付。
    fn requires_verification(&self) -> bool {
        matches!(
            self.strategy,
            StrategyKind::Transformative | StrategyKind::Verification
        ) || self.solve_mode == SolveMode::AtomicDelivery
            // 语言分类只是路由提示，工具事实才是硬边界。任何写入尝试都会关闭
            // read_only_verified 捷径，直到至少一次写入真正成功且随后验证通过。
            || self.write_attempts > 0
    }

    pub fn can_complete(&self) -> bool {
        if matches!(
            self.strategy,
            StrategyKind::Investigative | StrategyKind::Comparative
        ) {
            return !self.evidence.is_empty();
        }
        !self.requires_verification()
            || (!self.verification_evidence.is_empty()
                && self
                    .contract
                    .acceptance_criteria
                    .iter()
                    .all(|criterion| self.satisfied_criteria.contains(&criterion.id)))
    }

    pub fn delivery_report(
        &self,
        outcome: DeliveryOutcome,
        reason: Option<String>,
    ) -> DeliveryReport {
        // `GoalExecution` 与 `ExecutionState` 是两套互相校验的投影。任何一侧没有
        // 满足验收，都不能输出 outcome=Verified、criterion.satisfied=false 的
        // 自相矛盾报告。这里作为最终落盘前的 fail-closed 防线。
        let inconsistent_verified = outcome == DeliveryOutcome::Verified && !self.can_complete();
        let outcome = if inconsistent_verified {
            DeliveryOutcome::PartialDelivery
        } else {
            outcome
        };
        let reason = if inconsistent_verified {
            Some("求解图声称完成，但执行证据未覆盖全部验收项；已拒绝 Verified，任务仍未完成".into())
        } else {
            reason
        };
        let read_only_verified = outcome == DeliveryOutcome::Verified
            && !self.requires_verification()
            && self.can_complete();
        let read_only_evidence = self
            .evidence
            .values()
            .take(4)
            .map(|evidence| evidence.summary.clone())
            .collect::<Vec<_>>();
        let mut verification = self
            .verification_evidence
            .values()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        if verification.is_empty() && read_only_verified {
            verification = read_only_evidence.clone();
        }
        let criteria = self
            .contract
            .acceptance_criteria
            .iter()
            .map(|criterion| DeliveryCriterion {
                id: criterion.id.clone(),
                description: criterion.description.clone(),
                satisfied: self.satisfied_criteria.contains(&criterion.id) || read_only_verified,
                evidence: self
                    .verification_evidence
                    .get(&criterion.id)
                    .cloned()
                    .unwrap_or_else(|| {
                        if read_only_verified {
                            read_only_evidence.clone()
                        } else {
                            Vec::new()
                        }
                    }),
            })
            .collect();
        DeliveryReport {
            outcome,
            criteria,
            verification,
            reason,
        }
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
                let summary: String = item
                    .summary
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
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
    pub fn is_search_miss(&self, summary: &str) -> bool {
        self.signature.starts_with("search:")
            && ["未找到匹配", "no matches", "0 matches", "no results"]
                .iter()
                .any(|marker| {
                    summary
                        .to_ascii_lowercase()
                        .contains(&marker.to_ascii_lowercase())
                })
    }

    pub fn from_tool_call(call: &ToolCall, state: &ExecutionState) -> Self {
        let explicit = call
            .args
            .get("criterion_ids")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|value| value.as_str())
                    .filter(|id| {
                        state
                            .contract
                            .acceptance_criteria
                            .iter()
                            .any(|criterion| criterion.id == *id)
                    })
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let signature = normalized_signature(call);
        // 旧工具 schema 不要求 criterion_ids。普通读取/修改缺省时只推进首个
        // 未验收项；针对性构建/测试验证本次提交整体，可覆盖所有尚未验证的项。
        let supports = if explicit.is_empty() {
            if state.contract.inferred_surface_criteria
                && ExecutionState::is_verification_signature(&signature)
            {
                state
                    .contract
                    .acceptance_criteria
                    .iter()
                    .filter(|criterion| !state.satisfied_criteria.contains(&criterion.id))
                    .map(|criterion| criterion.id.clone())
                    .collect()
            } else {
                state
                    .contract
                    .acceptance_criteria
                    .iter()
                    .find(|criterion| {
                        if state.contract.inferred_surface_criteria {
                            !state.changed_criteria.contains(&criterion.id)
                        } else {
                            !state.satisfied_criteria.contains(&criterion.id)
                        }
                    })
                    .or_else(|| {
                        state
                            .contract
                            .acceptance_criteria
                            .iter()
                            .find(|criterion| !state.satisfied_criteria.contains(&criterion.id))
                    })
                    .map(|criterion| vec![criterion.id.clone()])
                    .unwrap_or_else(|| {
                        state
                            .contract
                            .acceptance_criteria
                            .first()
                            .map(|criterion| vec![criterion.id.clone()])
                            .unwrap_or_default()
                    })
            }
        } else {
            explicit
        };
        Self {
            signature,
            question: format!("执行工具 {} 以推进当前任务", call.name),
            supports,
            estimated_cost: 1,
        }
    }
}

/// 归一化工具调用签名，让重复守卫按「语义相同」而非「字面相同」判定：
/// - shell：剥离开头的 `cd [/d] <路径> &&` 前缀（取证：94% 命令携带全路径 cd，
///   导致每条命令签名字面唯一、守卫完全失效）；
/// - fs/edit：路径分隔符统一为正斜杠，绝对/相对写法的同一文件归为同一签名。
pub(crate) fn normalized_signature(call: &ToolCall) -> String {
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
    /// 不可续期的总熔断。阶段预算可因真实进展延展，但绝不能越过这两项。
    pub hard_max_steps: usize,
    pub hard_max_tool_calls: usize,
    pub convergence_ratio: f32,
    /// 允许的自动续期次数上限：此前无限续期会让单回合步数无上限增长
    /// （实测一个简单任务跑出 1000+ 步）；用尽后必须强制收尾。
    pub max_renewals: u32,
    pub renewals_used: u32,
    /// 硬熔断后的进展驱动自动续跑次数：有可验证进展时自动发放新探索窗口，
    /// 避免把任务切碎成十几次人工“继续”；达到上限才强制交回用户。
    pub hard_autorenews: u32,
    /// 进展延展已用次数：常规续期耗尽后，最近窗口只要产生可验证的写入或新证据，
    /// 就继续按窗口延展。排障、测试、审查本来就未必会修改代码，不能把它们误杀。
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

/// 分面预算供给的绝对天花板。它与"面数 → 总额"的线性关系无关，只保证极端面数
/// （例如一次请求里被切出十几个面）不会把单回合成本推向无界。
const ABSOLUTE_MAX_STEPS: usize = 120;
const ABSOLUTE_MAX_TOOL_CALLS: usize = 160;

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
            hard_max_steps: max_steps.saturating_mul(3).max(12),
            hard_max_tool_calls: max_tool_calls.saturating_mul(3).max(16),
            convergence_ratio: 0.75,
            max_renewals: std::env::var("HARNESS_MAX_RENEWALS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2)
                .clamp(0, 6),
            renewals_used: 0,
            hard_autorenews: 0,
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

    /// 与步数同理，初始工具调用上限是阶段检查点，不是未完成任务的终止线。
    pub fn cap_initial_tool_window(budget: &mut Budget, calls: usize) {
        let calls = calls.max(1);
        budget.max_tool_calls = budget.max_tool_calls.min(calls);
        budget.tool_window = budget.max_tool_calls;
    }

    /// 求解计划定义的总熔断；它比阶段检查点更严格，到达后不再许可新探索。
    pub fn cap_hard_limits(budget: &mut Budget, steps: usize, calls: usize) {
        budget.hard_max_steps = budget.hard_max_steps.min(steps.max(1));
        budget.hard_max_tool_calls = budget.hard_max_tool_calls.min(calls.max(1));
    }

    /// S3 分面预算供给：与 `cap_hard_limits` 反向，把硬熔断**抬升**到分面需求的下界。
    ///
    /// `cap_hard_limits` 用求解计划的常量压顶，那个常量对面数的增长是次线性且被截断的
    /// （`20 + min(面数-1, 4) * 2`）。面数一多，尾部交付面在算术上就拿不到跑完四个相位
    /// 所需的步数——表现为"剩 N 项验收"永远卡住。供给按 `Σ 每面独立预算` 计算，使
    /// 总额与面数恢复线性；`ABSOLUTE_MAX_*` 仍是不可突破的成本保险。
    pub fn provision_hard_limits(budget: &mut Budget, steps: usize, calls: usize) {
        budget.hard_max_steps = budget
            .hard_max_steps
            .max(steps.min(ABSOLUTE_MAX_STEPS))
            .max(1);
        budget.hard_max_tool_calls = budget
            .hard_max_tool_calls
            .max(calls.min(ABSOLUTE_MAX_TOOL_CALLS))
            .max(1);
    }

    pub fn hard_exhausted(state: &ExecutionState, budget: &Budget) -> bool {
        state.steps >= budget.hard_max_steps || state.tool_calls >= budget.hard_max_tool_calls
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
        let meaningful_progress =
            write_delta > 0 || (read_only_delivery && evidence_delta > 0 && success_delta > 0);

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

    /// 按一个窗口延展步数/工具/时长预算（常规续期、交付延展共用）。
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

    /// Fix1：硬熔断后的进展驱动自动续跑。有可验证进展时，把硬窗口与阶段窗口
    /// 各抬升一个步长，使回合在不突破单次窗口成本的前提下继续推进；总续跑次数
    /// 由 `Budget::hard_autorenews` 上限约束，防止失控（不依赖 `ABSOLUTE_MAX_*`，
    /// 因为续跑本就是允许突破单次固定硬预算、但受次数封顶的受控扩张）。
    pub fn arm_hard_continuation(budget: &mut Budget) {
        budget.hard_max_steps = budget.hard_max_steps.saturating_add(budget.step_window);
        budget.hard_max_tool_calls = budget
            .hard_max_tool_calls
            .saturating_add(budget.tool_window);
        // 同步抬升阶段软预算，避免硬窗口刚续上、阶段却先 Exhausted 再次触发续期。
        budget.max_steps = budget.max_steps.max(budget.hard_max_steps);
        budget.max_tool_calls = budget.max_tool_calls.max(budget.hard_max_tool_calls);
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
        budget: &Budget,
    ) -> GateDecision {
        Self::authorize_impl(proposal, state, budget, &state.allowed_tools(), true)
    }

    /// 受控求解由 GoalExecution 提供唯一阶段工具集；开放式兼容路径仍传入
    /// ExecutionState 的工具集。其余验收关联、总预算与原子安全约束保持共用。
    pub fn authorize_with_tools(
        proposal: &ActionProposal,
        state: &ExecutionState,
        budget: &Budget,
        allowed_tools: &[String],
    ) -> GateDecision {
        Self::authorize_impl(proposal, state, budget, allowed_tools, false)
    }

    fn authorize_impl(
        proposal: &ActionProposal,
        state: &ExecutionState,
        budget: &Budget,
        allowed_tools: &[String],
        legacy_atomic_rules: bool,
    ) -> GateDecision {
        let tool = proposal.signature.split(':').next().unwrap_or_default();
        if state.solve_mode != SolveMode::OpenEnded
            && !allowed_tools.iter().any(|allowed| allowed == tool)
        {
            return GateDecision::Deny(format!(
                "当前 {} 阶段的动态工具白名单不包含 {tool}；允许：{}",
                state.tool_phase().as_str(),
                allowed_tools.join(", ")
            ));
        }
        if proposal.supports.is_empty() {
            return GateDecision::Deny("该调用未关联任何验收标准".into());
        }
        if state.tool_calls >= budget.hard_max_tool_calls {
            return GateDecision::Deny(format!(
                "已达到任务绝对工具调用上限 {}；禁止继续探索，应基于现有证据交付或报告阻塞",
                budget.hard_max_tool_calls
            ));
        }
        // 开放式请求也只获得一轮有界发现窗口。SearchTool 自身已经按
        // dir → crate → workspace 扩展作用域；连续换关键词超过三次不再增加合理的
        // 定位覆盖，只会把“不清晰”伪装成“还没搜够”。此时必须基于已有证据收敛
        // 目标或提出一个决策问题。
        if state.solve_mode == SolveMode::OpenEnded
            && proposal.signature.starts_with("search:")
            && state
                .evidence
                .keys()
                .filter(|signature| signature.starts_with("search:"))
                .count()
                >= 3
        {
            return GateDecision::Deny(
                "渐进探索最多允许三条独立搜索证据；请停止换关键词，基于现有结果收敛为具体目标，或提出一个会改变实现方向的决策问题"
                    .into(),
            );
        }
        // V4 的阶段、搜索锚点和 AlreadySatisfied 语义全部由 SolveGraph 判定。
        // 这些旧原子规则仅服务 authorize() 的 legacy 路径，避免第二状态机否决
        // SolveGraph 已经授权的验证或局部假设切换。
        if legacy_atomic_rules && state.solve_mode == SolveMode::AtomicDelivery {
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
                    "原子交付任务的 shell 仅用于修改后的针对性验证；先定位并完成最小修改".into(),
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
            _ if !model_requested_tools && state.can_complete() => Completion::Complete,
            _ if !model_requested_tools => Completion::Converge(
                "模型已停止调用工具，但变更任务还没有成功验证的验收证据；请执行最小的相关验证，或明确报告阻塞原因，不得宣称已交付".into(),
            ),
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
        let intent = IntentProfile::compile(text);
        if intent.kind == IntentKind::Investigation {
            StrategyKind::Investigative
        } else if ["比较", "选型", "对比"]
            .iter()
            .any(|word| text.contains(word))
        {
            StrategyKind::Comparative
        // “检查 / 审查 / 确认”常是普通提问或代码探索的对象，不能仅凭一个
        // 词就收窄成只能运行 shell 的验证阶段。只有明确的验证动作才进入
        // Verification；其余请求保留完整的探索工具面。
        } else if ["验证", "测试", "编译", "构建", "lint", "格式检查"]
            .iter()
            .any(|word| text.contains(word))
        {
            StrategyKind::Verification
        } else if matches!(
            intent.kind,
            IntentKind::AtomicRegression | IntentKind::ScopedChange
        ) || [
            "修改", "修复", "重构", "更新", "改进", "实现", "改造", "调整", "拆分", "迁移", "新增",
            "添加", "增加", "展示", "显示", "隐藏", "移除", "替换", "去掉", "加上",
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
    fn classifies_generic_intents_by_closed_signals() {
        let policy = GeneralDomainPolicy;
        // 纯提问（以疑问词/问号开头结尾）→ 只读诊断流程（Investigation）。
        assert_eq!(
            policy.select_strategy(&TaskContract::from_input("为什么列表排序会乱？")),
            StrategyKind::Investigative
        );
        // 含代码符号的明确改动 → 范围受限交付（ScopedChange → Transformative）。
        assert_eq!(
            policy.select_strategy(&TaskContract::from_input("修复 ModelForm 的校验规则")),
            StrategyKind::Transformative
        );
        // 无封闭信号、又非提问 → 开放式（不臆测，交由 Phase 1 门禁追问定位）。
        assert_eq!(
            policy.select_strategy(&TaskContract::from_input("排查服务变慢的根因")),
            StrategyKind::Direct
        );
    }

    #[test]
    fn concrete_problem_report_uses_transformative_strategy_without_fix_verb() {
        // 用户没说"修复"，但给出了具体代码符号（Composer）→ 范围受限交付，
        // 而非退化为开放式。这是信号驱动分类取代"含词即改"的直接证据。
        let contract = TaskContract::from_input("会话窗口的 Composer 短文本会自动换行");
        let strategy = GeneralDomainPolicy.select_strategy(&contract);
        assert_eq!(strategy, StrategyKind::Transformative);
        assert_eq!(
            SolvePlan::for_contract(&contract, strategy).mode,
            SolveMode::ScopedDelivery
        );
    }

    #[test]
    fn generic_acceptance_criterion_preserves_the_observable_user_goal() {
        let objective = "文件树右键可以把选定文件添加到对话框附件";
        let contract = TaskContract::from_input(objective);
        assert!(contract.acceptance_criteria[0]
            .description
            .contains(objective));
        assert_ne!(
            contract.acceptance_criteria[0].description,
            "用户要求的交付物已完成并经过与风险相称的验证"
        );
    }

    #[test]
    fn diagnosis_uses_a_bounded_read_only_solve_mode() {
        let contract = TaskContract::from_input("为什么会话窗口里的短文本自动换行？请分析根因");
        let strategy = GeneralDomainPolicy.select_strategy(&contract);
        let plan = SolvePlan::for_contract(&contract, strategy);
        assert_eq!(strategy, StrategyKind::Investigative);
        assert_eq!(plan.mode, SolveMode::GuidedInvestigation);
        let state = ExecutionState::new(contract, strategy);
        assert_eq!(state.allowed_tools(), vec!["search"]);
        assert!(!state.can_complete(), "无证据诊断不得被标记为完成");
    }

    #[test]
    fn verified_read_only_delivery_marks_its_criterion_satisfied() {
        let contract = TaskContract::from_input("诊断登录按钮无反应的根因");
        let mut state = ExecutionState::new(contract, StrategyKind::Investigative);
        state.record_tool_result(
            &ActionProposal {
                signature: "search:{\"pattern\":\"login\"}".into(),
                question: "locate login".into(),
                supports: vec!["user-objective".into()],
                estimated_cost: 1,
            },
            true,
            "src/login.tsx:10: onClick",
        );
        assert!(state.can_complete());
        let report = state.delivery_report(DeliveryOutcome::Verified, None);
        assert!(report.criteria[0].satisfied);
        assert!(!report.criteria[0].evidence.is_empty());
        assert!(!report.verification.is_empty());
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
        // 单点回归 = 明确的 `from→to` 变更契约（"改为"），验收标准内含在描述里。
        // 这种封闭信号才是判定 AtomicRegression 的依据，而非"修复"之类的动词。
        let contract = TaskContract::from_input(
            "输入优化加了 loading 之后结果没有变化，把 loading 状态改为初始值",
        );
        let plan = SolvePlan::for_contract(&contract, StrategyKind::Transformative);
        assert_eq!(plan.mode, SolveMode::AtomicDelivery);
        assert_eq!(plan.initial_steps, 6);
        assert_eq!(plan.initial_tool_calls, 8);
        assert!(plan.instructions.contains("不要创建计划"));
    }

    #[test]
    fn quoted_shortening_gets_an_exact_acceptance_contract() {
        let contract = TaskContract::from_input(
            "弹出菜单应包含“📎 添加到对话框附件”，文字精简一下，“添加到对话”",
        );
        let shape = TaskShape::for_contract(&contract);
        let plan = SolvePlan::for_contract(&contract, StrategyKind::Transformative);

        assert_eq!(shape.clarity, TaskClarity::Exact);
        assert_eq!(shape.scale, TaskScale::Atomic);
        assert_eq!(plan.mode, SolveMode::AtomicDelivery);
        assert_eq!(contract.acceptance_criteria[0].id, "user-objective");
        assert!(contract.acceptance_criteria[0]
            .description
            .contains("添加到对话框附件"));
        assert!(contract.acceptance_criteria[0]
            .description
            .contains("添加到对话"));
        assert!(contract
            .constraints
            .iter()
            .any(|constraint| constraint.contains("周边行为")));
    }

    #[test]
    fn stale_state_after_mutation_is_atomic_regression() {
        // 配置保存后界面仍显示旧状态 = 明确的"改为"变更契约 → 单点回归，短窗口。
        let contract =
            TaskContract::from_input("配置保存后对话框仍显示旧状态，把刷新时机改为提交后立即");
        let plan = SolvePlan::for_contract(&contract, StrategyKind::Transformative);
        assert_eq!(plan.mode, SolveMode::AtomicDelivery);

        let mut budget = BudgetManager::for_contract(&contract, StrategyKind::Transformative);
        BudgetManager::cap_hard_limits(&mut budget, 8, 10);
        let mut state = ExecutionState::new(contract, StrategyKind::Transformative);
        state.steps = 8;
        assert!(BudgetManager::hard_exhausted(&state, &budget));
    }

    #[test]
    fn multi_surface_fields_use_staged_plan_and_scaled_budget() {
        let contract = TaskContract::from_input(
            "应用档案，列表、新增、编辑没有把appCode和subAppCode展示出来，页面上看不到。",
        );
        let policy = GeneralDomainPolicy;
        let strategy = policy.select_strategy(&contract);
        let plan = SolvePlan::for_contract(&contract, strategy);

        assert_eq!(strategy, StrategyKind::Transformative);
        assert_eq!(plan.mode, SolveMode::StagedDelivery);

        let mut budget = BudgetManager::for_contract(&contract, strategy);
        BudgetManager::cap_initial_step_window(&mut budget, plan.initial_steps);
        BudgetManager::cap_initial_tool_window(&mut budget, plan.initial_tool_calls);
        BudgetManager::cap_hard_limits(&mut budget, plan.hard_max_steps, plan.hard_max_tool_calls);
        assert_eq!(budget.hard_max_steps, 24);
        assert_eq!(budget.hard_max_tool_calls, 30);
        assert!(plan.instructions.contains("分阶段交付模式"));
    }

    #[test]
    fn multi_surface_goal_uses_staged_vertical_slices() {
        let contract =
            TaskContract::from_input("实现附件能力\n- 本地文件上传\n- 云文件导入\n- 旧接口兼容");
        let plan = SolvePlan::for_contract(&contract, StrategyKind::Transformative);
        assert_eq!(TaskShape::for_contract(&contract).scale, TaskScale::Staged);
        assert_eq!(plan.mode, SolveMode::StagedDelivery);
        assert!(plan.instructions.contains("最小纵向切片"));
        assert!(plan.instructions.contains("检查点"));
    }

    #[test]
    fn unclear_goal_uses_progressive_discovery_instead_of_unbounded_search() {
        let contract = TaskContract::from_input("优化一下菜单体验");
        let plan = SolvePlan::for_contract(&contract, StrategyKind::Direct);
        assert_eq!(
            TaskShape::for_contract(&contract).clarity,
            TaskClarity::Discovery
        );
        assert_eq!(plan.mode, SolveMode::OpenEnded);
        assert!(plan.instructions.contains("高信息增益"));
        assert!(plan.instructions.contains("禁止用全仓扫描掩盖目标不清"));
        assert!(plan.instructions.contains("一个带证据的决策问题"));
    }

    #[test]
    fn progressive_discovery_stops_keyword_rotation_after_three_searches() {
        let contract = TaskContract::from_input("优化一下菜单体验");
        let mut state = ExecutionState::new(contract, StrategyKind::Direct);
        let budget = BudgetManager::for_contract(&state.contract, state.strategy);
        for index in 0..3 {
            let probe = ActionProposal {
                signature: format!("search:{{\"pattern\":\"probe-{index}\"}}"),
                question: format!("验证假设 {index}"),
                supports: vec!["user-objective".into()],
                estimated_cost: 1,
            };
            state.record_tool_result(&probe, true, &format!("src/menu-{index}.rs"));
        }
        let fourth = ActionProposal {
            signature: "search:{\"pattern\":\"probe-4\"}".into(),
            question: "继续换关键词".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        assert!(matches!(
            ActionGate::authorize(&fourth, &state, &budget),
            GateDecision::Deny(reason) if reason.contains("最多允许三条")
        ));
    }

    #[test]
    fn surface_provisioning_raises_hard_budget_and_respects_the_ceiling() {
        // S3：供给与封顶是**反向**操作。cap_* 只会压低，provision_* 只会抬升；
        // 二者组合后，多面任务不再被求解计划的截断式常量饿死。
        let contract = TaskContract::from_input(
            "应用档案，列表、新增、编辑、详情、导出都要展示 appCode 和 subAppCode。",
        );
        let strategy = GeneralDomainPolicy.select_strategy(&contract);
        let plan = SolvePlan::for_contract(&contract, strategy);
        let mut budget = BudgetManager::for_contract(&contract, strategy);
        BudgetManager::cap_hard_limits(&mut budget, plan.hard_max_steps, plan.hard_max_tool_calls);
        let capped_steps = budget.hard_max_steps;

        // 用真实分面需求驱动，而不是造一个人工数字：这同时证明 agent_loop 的接线顺序
        // （先 cap 后 provision）在多面任务上确实抬高了熔断线。
        let demand = crate::GoalExecution::from_contract(&contract).required_budget();
        assert!(
            demand.surfaces >= 4,
            "应切出多个交付面，实际 {}",
            demand.surfaces
        );
        assert!(
            demand.steps > capped_steps,
            "该多面任务的分面需求 {} 步应高于计划常量 {} 步（这正是饥饿区间）",
            demand.steps,
            capped_steps
        );
        BudgetManager::provision_hard_limits(&mut budget, demand.steps, demand.tool_calls);
        assert_eq!(
            budget.hard_max_steps, demand.steps,
            "分面需求高于计划常量时必须抬升硬熔断"
        );

        // 供给量为 0 不应把上限清零（单面任务会走这条路径）。
        BudgetManager::provision_hard_limits(&mut budget, 0, 0);
        assert!(budget.hard_max_steps >= demand.steps);
        assert!(budget.hard_max_tool_calls > 0);

        // 天花板：极端面数不得把单回合成本推向无界。
        BudgetManager::provision_hard_limits(&mut budget, 100_000, 100_000);
        assert_eq!(budget.hard_max_steps, ABSOLUTE_MAX_STEPS);
        assert_eq!(budget.hard_max_tool_calls, ABSOLUTE_MAX_TOOL_CALLS);

        // 供给不会反向压低已有额度。
        let before = budget.hard_max_steps;
        BudgetManager::provision_hard_limits(&mut budget, 1, 1);
        assert_eq!(budget.hard_max_steps, before, "供给操作不得压低硬熔断");
    }

    #[test]
    fn named_surfaces_become_independent_acceptance_items() {
        let contract = TaskContract::from_input("列表、新增、编辑都需要展示同一字段");
        assert_eq!(contract.acceptance_criteria.len(), 3);
        assert_eq!(contract.acceptance_criteria[0].description, "列表展示");
        assert_eq!(contract.acceptance_criteria[1].description, "新增表单");
        assert_eq!(contract.acceptance_criteria[2].description, "编辑表单");
    }

    #[test]
    fn one_targeted_build_verifies_all_remaining_surfaces() {
        let contract = TaskContract::from_input("列表、新增、编辑都需要展示同一字段");
        let state = ExecutionState::new(contract, StrategyKind::Transformative);
        let proposal = ActionProposal::from_tool_call(
            &ToolCall {
                id: "verify".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo test -p web"}),
            },
            &state,
        );
        assert_eq!(proposal.supports, vec!["item-1", "item-2", "item-3"]);
    }

    #[test]
    fn multi_surface_delivery_stays_in_change_until_every_surface_is_written() {
        let contract = TaskContract::from_input("列表、新增、编辑都需要展示同一字段");
        let mut state = ExecutionState::new(contract, StrategyKind::Transformative);
        let first_edit = ActionProposal {
            signature: "edit:{\"path\":\"list.rs\"}".into(),
            question: "补充列表字段".into(),
            supports: vec!["item-1".into()],
            estimated_cost: 1,
        };
        state.record_tool_result(&first_edit, true, "updated list");

        assert_eq!(state.tool_phase(), ToolPhase::Change);
        let next_edit = ActionProposal::from_tool_call(
            &ToolCall {
                id: "edit-form".into(),
                name: "edit".into(),
                args: serde_json::json!({"path": "form.rs"}),
            },
            &state,
        );
        assert_eq!(next_edit.supports, vec!["item-2"]);
    }

    #[test]
    fn bounded_change_uses_transformative_strategy_and_finite_total_budget() {
        let contract = TaskContract::from_input("在用户列表增加状态字段");
        let policy = GeneralDomainPolicy;
        let strategy = policy.select_strategy(&contract);
        let plan = SolvePlan::for_contract(&contract, strategy);

        assert_eq!(strategy, StrategyKind::Transformative);
        assert_eq!(plan.mode, SolveMode::ScopedDelivery);
        assert_eq!((plan.hard_max_steps, plan.hard_max_tool_calls), (20, 24));
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
    fn review_word_does_not_force_shell_only_verification_mode() {
        let policy = GeneralDomainPolicy;
        // 诊断式提问（"为什么…？"）→ 只读调查，不被"审查/诊断"等动词收窄成
        // 只能跑 shell 的验证阶段；读取与定位工具仍可用。
        assert_eq!(
            policy.select_strategy(&TaskContract::from_input(
                "为什么 composer 里的输入会自动换行？"
            )),
            StrategyKind::Investigative
        );
        // 明确的验证动作 → 验证阶段。
        assert_eq!(
            policy.select_strategy(&TaskContract::from_input("运行测试验证这次修改")),
            StrategyKind::Verification
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
        assert!(msg.unwrap().contains("进展延展"));
        assert!(budget.max_steps > before);
        assert_eq!(budget.delivery_extensions, 1);

        // 持续写入 → 持续延展（不设上限）：未完成但正在产出的任务不被截断。
        state.record_tool_result(&proposal, true, "edit ok");
        state.steps = 14;
        let msg2 = BudgetManager::diagnose_and_renew(&mut state, &mut budget);
        assert!(msg2.unwrap().contains("进展延展"));
        assert_eq!(budget.delivery_extensions, 2);

        // 写入停止（空转）→ 不再延展，交给收尾。
        state.steps = 16;
        assert!(BudgetManager::diagnose_and_renew(&mut state, &mut budget).is_none());
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
    fn unverified_change_cannot_complete_when_model_stops() {
        let contract = TaskContract::from_input("修复一个确定的界面回归");
        let budget = BudgetManager::for_contract(&contract, StrategyKind::Transformative);
        let state = ExecutionState::new(contract, StrategyKind::Transformative);

        assert!(matches!(
            CompletionJudge::evaluate(&state, &budget, false),
            Completion::Converge(_)
        ));
        assert!(!state.can_complete());
    }

    #[test]
    fn successful_verification_unlocks_change_delivery() {
        let contract = TaskContract::from_input("修复一个确定的界面回归");
        let budget = BudgetManager::for_contract(&contract, StrategyKind::Transformative);
        let mut state = ExecutionState::new(contract, StrategyKind::Transformative);
        let edit = ActionProposal {
            signature: "edit:{\"path\":\"ui.rs\"}".into(),
            question: "最小修复".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        let verify = ActionProposal {
            signature: "shell:{\"command\":\"cargo test -p harness-ui\"}".into(),
            question: "运行相关测试".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };

        state.record_tool_result(&edit, true, "updated ui.rs");
        state.record_tool_result(&verify, true, "test result: ok");

        assert!(state.can_complete());
        assert_eq!(
            state.verification_evidence.get("user-objective"),
            Some(&vec![
                "shell:{\"command\":\"cargo test -p harness-ui\"} => test result: ok".into()
            ])
        );
        assert_eq!(
            CompletionJudge::evaluate(&state, &budget, false),
            Completion::Complete
        );
    }

    #[test]
    fn static_disk_verification_updates_the_completion_projection() {
        let contract = TaskContract::from_input("修复一个确定的界面回归");
        let mut state = ExecutionState::new(contract, StrategyKind::Transformative);
        let edit = ActionProposal {
            signature: "edit:{\"path\":\"ui.rs\"}".into(),
            question: "最小修复".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };

        state.record_tool_result(&edit, true, "updated ui.rs");
        assert!(!state.can_complete());
        state.record_static_verification("user-objective", "ui.rs:12 已包含目标值");

        assert!(state.can_complete());
        assert_eq!(
            state.verification_evidence.get("user-objective"),
            Some(&vec!["ui.rs:12 已包含目标值".into()])
        );
    }

    #[test]
    fn failed_write_then_green_build_cannot_verify_unchanged_baseline() {
        // 模拟自然语言分类漏判为 Direct/OpenEnded 的最坏情况：结构化工具事实仍须兜底。
        let contract = TaskContract::from_input("收紧当前控件");
        let budget = BudgetManager::for_contract(&contract, StrategyKind::Direct);
        let mut state = ExecutionState::new(contract, StrategyKind::Direct);
        let edit = ActionProposal {
            signature: "edit:{\"path\":\"ui.rs\"}".into(),
            question: "调整控件".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        let build = ActionProposal {
            signature: "shell:{\"command\":\"cargo check\"}".into(),
            question: "编译检查".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };

        state.record_tool_result(&edit, false, "old_text matched 0");
        state.record_tool_result(&build, true, "Finished dev profile");

        assert_eq!(state.write_attempts, 1);
        assert_eq!(state.write_operations, 0);
        assert!(state.verification_evidence.is_empty());
        assert!(!state.can_complete());
        assert!(matches!(
            CompletionJudge::evaluate(&state, &budget, false),
            Completion::Converge(_)
        ));
        let report = state.delivery_report(DeliveryOutcome::Verified, None);
        assert_eq!(report.outcome, DeliveryOutcome::PartialDelivery);
        assert!(!report.criteria[0].satisfied);
        assert!(report.reason.unwrap().contains("拒绝 Verified"));
    }

    #[test]
    fn comment_only_edit_cannot_satisfy_a_functional_goal() {
        let contract = TaskContract::from_input("文件树右键可以把选定文件添加到对话框附件");
        let mut state = ExecutionState::new(contract, StrategyKind::Transformative);
        let edit = ActionProposal {
            signature: concat!(
                "edit:{\"path\":\"gui/theme.rs\",",
                "\"old_text\":\"//! Theme tokens.\",",
                "\"new_text\":\"//! Theme tokens.\\n// Modern refined palette.\"}"
            )
            .into(),
            question: "apply change".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        let build = ActionProposal {
            signature: "shell:{\"command\":\"cargo check\"}".into(),
            question: "compile".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };

        state.record_tool_result(&edit, true, "updated gui/theme.rs");
        state.record_tool_result(&build, true, "Finished dev profile");

        assert_eq!(state.write_attempts, 1);
        assert_eq!(state.write_operations, 0);
        assert!(state.verification_evidence.is_empty());
        assert!(!state.can_complete());
    }

    #[test]
    fn each_criterion_requires_its_own_recognized_validation() {
        let contract = TaskContract::from_input("实现两项改动\n- 第一项\n- 第二项");
        let mut state = ExecutionState::new(contract, StrategyKind::Transformative);
        let edit = ActionProposal {
            signature: "edit:{\"path\":\"ui.rs\"}".into(),
            question: "第一项改动".into(),
            supports: vec!["item-1".into()],
            estimated_cost: 1,
        };
        state.record_tool_result(&edit, true, "updated");

        // “echo test” 不能冒充验证命令，也不能覆盖第二项验收。
        let fake_check = ActionProposal::from_tool_call(
            &ToolCall {
                id: "fake".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "echo test"}),
            },
            &state,
        );
        assert_eq!(fake_check.supports, vec!["item-1"]);
        state.record_tool_result(&fake_check, true, "test");
        assert!(state.satisfied_criteria.is_empty());

        let first_check = ActionProposal::from_tool_call(
            &ToolCall {
                id: "check-1".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo test -p ui"}),
            },
            &state,
        );
        state.record_tool_result(&first_check, true, "ok");
        assert!(state.satisfied_criteria.contains("item-1"));
        assert!(!state.satisfied_criteria.contains("item-2"));

        let second_action = ActionProposal::from_tool_call(
            &ToolCall {
                id: "edit-2".into(),
                name: "edit".into(),
                args: serde_json::json!({"path": "other.rs"}),
            },
            &state,
        );
        assert_eq!(second_action.supports, vec!["item-2"]);
        assert!(!state.can_complete());
    }

    #[test]
    fn atomic_gate_blocks_broad_second_search_and_pre_change_verification() {
        let contract = TaskContract::from_input(
            "输入优化加了 loading 之后结果没有变化，把 loading 状态改为初始值",
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
    fn dynamic_tool_whitelist_follows_verified_execution_evidence() {
        let contract = TaskContract::from_input(
            "输入优化加了 loading 之后结果没有变化，把 loading 状态改为初始值",
        );
        let mut state = ExecutionState::new(contract, StrategyKind::Transformative);
        assert_eq!(state.tool_phase(), ToolPhase::Locate);
        assert_eq!(state.allowed_tools(), vec!["search"]);

        let locate = ActionProposal {
            signature: "search:{\"pattern\":\"loading\"}".into(),
            question: "定位".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        state.record_tool_result(&locate, true, "composer.rs:91");
        assert_eq!(state.tool_phase(), ToolPhase::Inspect);
        assert_eq!(state.allowed_tools(), vec!["fs", "search"]);

        let inspect = ActionProposal {
            signature: "fs:{\"op\":\"read\",\"path\":\"composer.rs\"}".into(),
            question: "读取".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        state.record_tool_result(&inspect, true, "state update missing");
        assert_eq!(state.tool_phase(), ToolPhase::Change);
        assert_eq!(state.allowed_tools(), vec!["edit", "fs"]);

        let edit = ActionProposal {
            signature: "edit:{\"path\":\"composer.rs\"}".into(),
            question: "修复".into(),
            supports: vec!["user-objective".into()],
            estimated_cost: 1,
        };
        state.record_tool_result(&edit, true, "updated");
        assert_eq!(state.tool_phase(), ToolPhase::Verify);
        assert_eq!(state.allowed_tools(), vec!["shell"]);
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

    #[test]
    fn solve_plan_hard_budget_is_not_expandable() {
        let contract = TaskContract::from_input("完成一项多步骤任务");
        let strategy = GeneralDomainPolicy.select_strategy(&contract);
        let plan = SolvePlan::for_contract(&contract, strategy);
        let mut budget = BudgetManager::for_contract(&contract, strategy);
        BudgetManager::cap_hard_limits(&mut budget, plan.hard_max_steps, plan.hard_max_tool_calls);

        assert_eq!(budget.hard_max_steps, plan.hard_max_steps);
        assert_eq!(budget.hard_max_tool_calls, plan.hard_max_tool_calls);
    }

    /// 签名归一化：cd 全路径前缀与路径分隔符差异不应绕过重复守卫
    /// （取证：94% 命令携带 cd 前缀，每条签名字面唯一，守卫全失效）。
    #[test]
    fn signature_normalization_neutralizes_cd_prefix_and_separators() {
        let contract = TaskContract::from_input("完成任务");
        let state = ExecutionState::new(contract.clone(), StrategyKind::Direct);
        let a = ActionProposal::from_tool_call(
            &ToolCall {
                id: "1".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "cd /d F:\\ws\\proj && cargo check"}),
            },
            &state,
        );
        let b = ActionProposal::from_tool_call(
            &ToolCall {
                id: "2".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "cargo check"}),
            },
            &state,
        );
        assert_eq!(a.signature, b.signature);

        // 纯 cd 命令不被误删为空。
        let only_cd = ActionProposal::from_tool_call(
            &ToolCall {
                id: "3".into(),
                name: "shell".into(),
                args: serde_json::json!({"command": "cd F:\\ws"}),
            },
            &state,
        );
        assert!(only_cd.signature.contains("cd"));

        // 同一文件的反斜杠/正斜杠写法归为同一签名。
        let p1 = ActionProposal::from_tool_call(
            &ToolCall {
                id: "4".into(),
                name: "fs".into(),
                args: serde_json::json!({"op": "read", "path": "src\\main.rs"}),
            },
            &state,
        );
        let p2 = ActionProposal::from_tool_call(
            &ToolCall {
                id: "5".into(),
                name: "fs".into(),
                args: serde_json::json!({"op": "read", "path": "src/main.rs"}),
            },
            &state,
        );
        assert_eq!(p1.signature, p2.signature);
    }
}
