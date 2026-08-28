use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use base64::Engine as _;
use futures::StreamExt;
use harness_capability::assets::{
    ChatTurn, ConversationMemory, FactKind, LifecycleLayer, MemoryFact, Skill, SkillLibrary,
};
use harness_capability::compaction::Compaction;
use harness_capability::hook::{Hook, HookDecision, HookEvent, HookPayload};
use harness_core::{error::Result, types::UserInput, AppContext};
use harness_llm::{Chunk, LlmProvider, Message, RequestOptions, Role, ToolCall, ToolResult, Usage};
use harness_session::{
    DeliveryOutcome, DeliveryReport, ExecutionTelemetry, SessionEvent, SessionLog,
};
use harness_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use crate::events::{PreStep, TurnStopping};
use crate::execution::{
    ActionGate, ActionProposal, BudgetManager, Completion, CompletionJudge, DomainPolicy,
    ExecutionState, GateDecision, GeneralDomainPolicy, SolvePlan, TaskContract,
};
use crate::TaskLedger;

/// Agent 循环 / Turn-Step 生命周期（原 §5.6）。
///
/// `Turn` = 0..n `Step`；`debt` 计数控制续跑；`agent/turn-stopping` 为唯一串行终止点。
pub struct AgentLoop;

/// 相同调用的结果守卫。成功调用在工作区未发生写入前不应原样重试；失败调用
/// 只允许一次定向重试。结果只保留哈希，避免控制状态复制工具原文。
#[derive(Default)]
struct ToolRepeatGuard {
    previous: Option<(String, u64, bool)>,
    recovery_attempts: HashMap<String, u8>,
    /// 同一调用（名称+参数）的累计执行次数：即使每次输出略有差异（扫描类命令
    /// 常见），超过阈值也判定为低价值重复。取证：同回合扫描脚本被跑 13 次。
    cumulative: HashMap<String, u8>,
}

/// 同回合内同一失败调用（名称+参数完全相同）最多执行两次（首次 + 一次定向重试）。
const MAX_SAME_CALLS_PER_TURN: u8 = 2;

/// 用户的“继续”不是一个新的、只有一句话的 Direct 任务。它必须接回最近一个
/// 未验证结束的根任务，否则复杂迁移会被重新分类为 Direct，并误用 36/48 这类
/// 小任务硬预算。
#[derive(Clone)]
struct ResumeState {
    objective: String,
    report: DeliveryReport,
}

fn is_continuation_request(text: &str) -> bool {
    let trimmed = text.trim().to_lowercase();
    ["继续", "接着", "续跑", "恢复", "continue", "resume"]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// 从追加日志逆向找到最近的未完成根任务。若最近一回合本身也是“继续”，继续
/// 向前穿透，直到命中真实目标；这使连续多次续跑始终继承同一契约与策略。
fn latest_resumable_task(events: &[SessionEvent]) -> Option<ResumeState> {
    let mut end = events.len();
    // 目标文本要穿透“继续”找到根任务，但验收状态必须采用最近一次 Delivery；
    // 新版续跑回合的 TurnStart 仍记录用户原话，而其 Delivery 已是根任务的报告。
    let mut latest_report: Option<DeliveryReport> = None;
    loop {
        let delivery_index = (0..end).rev().find(|&index| {
            matches!(
                events[index],
                SessionEvent::Delivery {
                    report: DeliveryReport {
                        outcome: DeliveryOutcome::Blocked | DeliveryOutcome::Interrupted,
                        ..
                    },
                    ..
                }
            )
        })?;
        let report = match &events[delivery_index] {
            SessionEvent::Delivery { report, .. } => report.clone(),
            _ => unreachable!("delivery index was matched above"),
        };
        let turn_index = (0..delivery_index)
            .rev()
            .find(|&index| matches!(events[index], SessionEvent::TurnStart { .. }))?;
        let objective = match &events[turn_index] {
            SessionEvent::TurnStart { input, .. } => input.clone(),
            _ => unreachable!("turn index was matched above"),
        };
        if !is_continuation_request(&objective) {
            return Some(ResumeState {
                objective,
                report: latest_report.unwrap_or(report),
            });
        }
        latest_report.get_or_insert(report);
        end = turn_index;
    }
}

fn resume_instruction(resume: &ResumeState) -> String {
    let completed = resume
        .report
        .criteria
        .iter()
        .filter(|criterion| criterion.satisfied)
        .map(|criterion| criterion.id.as_str())
        .collect::<Vec<_>>();
    let remaining = resume
        .report
        .criteria
        .iter()
        .filter(|criterion| !criterion.satisfied)
        .map(|criterion| format!("{}={}", criterion.id, criterion.description))
        .collect::<Vec<_>>();
    format!(
        "[续跑任务]\n这是同一任务的后续执行，不要重新创建计划或按本句重新分类。\n原始目标：{}\n已验证：{}\n未完成：{}\n上次停止原因：{}\n从第一个未完成验收项继续；保留已验证项，不重复已完成的定位、修改或验证。",
        resume.objective,
        if completed.is_empty() { "无".into() } else { completed.join("、") },
        if remaining.is_empty() { "无".into() } else { remaining.join("；") },
        resume.report.reason.as_deref().unwrap_or("未提供")
    )
}

/// 单次模型响应内的原子任务门禁。它补足跨步骤状态机的时间差：首个 search 的
/// 结果尚未写入执行状态时，也不能让模型并行发起更多定位搜索。
#[derive(Default)]
struct AtomicStepGate {
    search_queued: bool,
}

impl AtomicStepGate {
    fn allows(&mut self, atomic: bool, signature: &str) -> bool {
        if !atomic || !signature.starts_with("search:") {
            return true;
        }
        if self.search_queued {
            return false;
        }
        self.search_queued = true;
        true
    }
}

impl ToolRepeatGuard {
    fn should_block(&self, signature: &str) -> bool {
        let repeated_success = self
            .previous
            .as_ref()
            .is_some_and(|(previous, _, ok)| previous == signature && *ok);
        let failed_retries_exhausted = self
            .cumulative
            .get(signature)
            .is_some_and(|count| *count >= MAX_SAME_CALLS_PER_TURN);
        repeated_success || failed_retries_exhausted
    }

    fn record_result(&mut self, signature: &str, result: &ToolResult) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        result.ok.hash(&mut hasher);
        result.content.hash(&mut hasher);
        let fingerprint = hasher.finish();
        // 成功写入改变了观察对象：之后允许重新运行同一验证命令，但仍不会允许
        // 紧随其后的原样重复。其它成功读取/搜索不改变对象，必须换参数或换路径。
        let changed_workspace = result.ok
            && (signature.starts_with("edit:")
                || (signature.starts_with("fs:") && signature.contains("\"op\":\"write\"")));
        if changed_workspace {
            self.cumulative.clear();
            self.recovery_attempts.clear();
            self.previous = None;
        }
        let changed_path =
            self.previous
                .as_ref()
                .is_none_or(|(previous_signature, previous_fingerprint, _)| {
                    previous_signature != signature || *previous_fingerprint != fingerprint
                });
        if changed_path {
            // 一次真正不同的观察意味着模型已改变调查路径；旧的恢复次数不再相关。
            self.recovery_attempts.clear();
        }
        self.previous = Some((signature.to_string(), fingerprint, result.ok));
        // 累计计数：与「连续相同结果」互补，拦截输出有微小差异的空转重复。
        *self.cumulative.entry(signature.to_string()).or_default() = self
            .cumulative
            .get(signature)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
    }

    fn note_recovery(&mut self, signature: &str) -> u8 {
        let attempts = self
            .recovery_attempts
            .entry(signature.to_string())
            .or_default();
        *attempts = attempts.saturating_add(1);
        *attempts
    }
}

/// 默认确定性压缩器：复用会话重建、工具输出压缩与上下文预算规则。
/// 作为能力注册后，后续可替换为远程摘要器；默认实现绝不额外请求模型。
#[derive(Default)]
pub struct DeterministicCompaction;

#[async_trait::async_trait]
impl Compaction for DeterministicCompaction {
    async fn compact(&self, events: Vec<SessionEvent>) -> Result<Vec<Message>> {
        Ok(messages_from_events(&events))
    }
}

impl AgentLoop {
    pub fn new() -> Self {
        Self
    }

    /// 跑一个 turn，直到唯一终止检查点返回 `will_stop`。
    pub async fn run_turn(&self, ctx: &AppContext, input: UserInput) -> Result<()> {
        self.run_turn_cancellable(ctx, input, CancellationToken::new())
            .await
    }

    pub async fn run_turn_cancellable(
        &self,
        ctx: &AppContext,
        input: UserInput,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let log = ctx.get::<SessionLog>();
        let llm = ctx.get::<dyn LlmProvider>();
        let tools = ctx.get::<ToolRegistry>();
        let hook = ctx.get::<dyn Hook>();
        let bus = ctx.events();

        // 仅附文件/图片也必须形成完整任务，不能把空 user message 交给上游模型；
        // 后者常被网关当作无效输入或返回空输出，表现为会话窗口毫无响应。
        let input_text = attachment_prompt(&input);
        // 会话正文可以重放，但任务的策略/验收状态不能从一句“继续”重新推断。
        // 只在明确续跑表达式下恢复，普通新请求仍完全按其自身目标执行。
        let history = log.replay();
        let resume = is_continuation_request(&input_text)
            .then(|| latest_resumable_task(&history))
            .flatten();
        let task_text = resume
            .as_ref()
            .map(|state| state.objective.clone())
            .unwrap_or_else(|| input_text.clone());

        // 先把自然语言请求编译成通用执行契约，再由可替换的领域策略选择执行方式。
        // 没有注册领域策略时使用通用分类器，不把代码修复等场景写死在 Agent Loop。
        let contract = TaskContract::from_input(&task_text);
        let default_policy = GeneralDomainPolicy;
        let strategy = ctx
            .try_get::<dyn DomainPolicy>()
            .map(|policy| policy.select_strategy(&contract))
            .unwrap_or_else(|| default_policy.select_strategy(&contract));
        let mut budget = BudgetManager::for_contract(&contract, strategy);
        if let Some(policy) = ctx.try_get::<dyn DomainPolicy>() {
            policy.adjust_budget(&contract, &mut budget);
        }
        // 现有 UI/env 步数设置继续作为管理员硬上限；动态预算只会进一步收紧。
        BudgetManager::cap_initial_step_window(&mut budget, max_steps_limit());
        let solve_plan = SolvePlan::for_contract(&contract, strategy);
        BudgetManager::cap_initial_step_window(&mut budget, solve_plan.initial_steps);
        BudgetManager::cap_initial_tool_window(&mut budget, solve_plan.initial_tool_calls);
        if solve_plan.mode == crate::execution::SolveMode::AtomicDelivery {
            // 原子回归允许一次定位、局部读取、修改和验证；阶段续期不能把它扩张为
            // 数十轮泛搜。超限后仍会输出可恢复的 Blocked，而非宣称完成。
            BudgetManager::cap_hard_limits(&mut budget, 8, 10);
        }
        let mut execution = ExecutionState::new(contract, strategy);
        if let Some(state) = &resume {
            execution.restore_verified_criteria(&state.report);
        }
        let mut ledger = resume
            .as_ref()
            .map(|state| TaskLedger::from_delivery(&execution.contract, &state.report))
            .unwrap_or_else(|| TaskLedger::from_contract(&execution.contract));

        // 从追加日志重建多轮上下文；不能每个 turn 都只发送当前一句，否则 GUI 看似能聊天，
        // 实际模型完全不记得上一轮以及之前的工具结果。
        // 记忆自动沉淀（L0 工作记忆）：每个用户回合写入对话记忆（无后端则落本地文件）。
        // 失败容忍——记忆写入不影响对话流程；放后台任务，不阻塞首帧。
        if let Some(conv) = ctx.try_get::<dyn ConversationMemory>() {
            let turn = ChatTurn {
                session_id: log.id().to_string(),
                role: "user".into(),
                content: input_text.clone(),
                ts: String::new(),
            };
            tokio::spawn(async move {
                let _ = conv.record_turn(turn).await;
            });
        }
        // 预处理并行化：历史压缩与技能匹配互相独立，join 执行；此前串行排在
        // 第一次 llm.stream() 之前，每一项都直接叠加进首 token 延迟。
        let compaction = ctx.try_get::<dyn Compaction>();
        let skill_library = ctx.try_get::<dyn SkillLibrary>();
        let conversation_memory = ctx.try_get::<dyn ConversationMemory>();
        let experience_workspace = ctx
            .try_get::<harness_core::Workspace>()
            .map(|ws| ws.root().display().to_string());
        let (compacted, matched_skills, recalled_experience) = tokio::join!(
            async {
                match compaction {
                    Some(compaction) => compaction.compact(history.clone()).await.ok(),
                    None => None,
                }
            },
            async {
                match skill_library {
                    Some(skills) => skills.match_skills(&input_text).await.ok(),
                    None => None,
                }
            },
            async {
                match conversation_memory {
                    Some(memory) => memory
                        .recall(&input_text, LifecycleLayer::L2)
                        .await
                        .ok()
                        .map(|facts| {
                            facts
                                .into_iter()
                                .filter(|fact| {
                                    experience_workspace.as_ref().is_none_or(|workspace| {
                                        !fact.source.contains("workspace=")
                                            || fact
                                                .source
                                                .contains(&format!("workspace={workspace}"))
                                    })
                                })
                                .collect::<Vec<_>>()
                        }),
                    None => None,
                }
            }
        );
        // 压缩器不可用不能阻断对话；保留旧路径作为可靠回退。
        let mut messages = compacted.unwrap_or_else(|| messages_from_events(&history));

        log.append(SessionEvent::TurnStart {
            id: log.gen_id(),
            input: input_text.clone(),
        });
        // 在网络握手、排队或模型首个分片到来前立即驱动 UI 的思考气泡，避免主界面
        // 留白而被误认为卡死。Thinking 事件不会被写入下一轮模型上下文，也不消耗 token。
        log.append(SessionEvent::Thinking {
            id: log.gen_id(),
            text: "正在理解你的问题…".into(),
        });
        append_telemetry(&log, &execution, &ledger, "任务已编译，等待首次定位");

        let mut debt: usize = 1;
        // 跨步累积本轮助手最终文本，供回合结束时沉淀为 L0 记忆。
        let mut last_assistant = String::new();
        let (image_data_urls, image_notes) =
            inline_image_data_urls(&input.attachments, llm.supports_vision());
        let attachment_context = render_attachment_context(&input.attachments, &image_notes);
        messages.push(Message::user_with_images(&input_text, image_data_urls));
        if !attachment_context.is_empty() {
            messages.insert(1, Message::system(&attachment_context));
        }
        messages.insert(
            1,
            Message::system(
                execution
                    .contract
                    .render_for_model(execution.strategy, &budget),
            ),
        );
        messages.insert(1, Message::system(solve_plan.instructions.clone()));
        if let Some(state) = &resume {
            messages.insert(1, Message::system(resume_instruction(state)));
        }
        // 技能注入点：只匹配启用的 SKILL.md 资产，并在本回合的系统上下文中
        // 提供可执行步骤与验收条件。禁用或删除后，SkillLibrary 不会返回它们，
        // 因而从下一回合起立即不再影响模型行为。匹配已在预处理阶段与压缩并行完成。
        if let Some(matched) = &matched_skills {
            if let Some(instructions) = render_skill_instructions(matched) {
                messages.insert(1, Message::system(&instructions));
            }
        }
        if let Some(facts) = recalled_experience.as_deref() {
            if let Some(instructions) = render_experience(facts) {
                messages.insert(1, Message::system(instructions));
            }
        }
        // 项目事实注入：manifest 位置/工具链/打包入口等稳定环境信息一次告知，
        // 避免模型每回合花十几次调用重新探索构建环境（取证：单回合 15 次 shell
        // 花在发现 cargo 根与 GNU→MSVC 工具链切换上）。
        if let Some(workspace) = ctx.try_get::<harness_core::Workspace>() {
            let facts = crate::facts::project_facts(&workspace.root());
            if !facts.is_empty() {
                messages.insert(1, Message::system(&facts));
            }
        }
        // 工具超时一次读取：此前每个工具调用都重新 std::env::var。
        let tool_timeout_secs: u64 = std::env::var("HARNESS_TOOL_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300)
            .clamp(5, 3_600);
        let mut steps = 0usize;
        // 只有“同一调用连续得到相同结果”才被视为停滞；先要求模型换路，不立即终止。
        const MAX_LOOP_RECOVERY_PROMPTS: u8 = 2;
        let mut repeat_guard = ToolRepeatGuard::default();
        // 硬终止标记（取消/流错误/反复无视循环恢复）：阻止步末的 debt 记账复活回合，
        // 否则带着「已宣告未执行」的 tool_call 续跑会直接 400。
        let mut hard_stop = false;
        let mut cancelled = false;
        let mut delivery_verified = false;
        let mut budget_exhausted = false;
        let mut absolute_budget_hit = false;
        // 对确有有效结果的较大任务，首次总熔断自动开一次有界续跑窗口；避免用户
        // 被迫反复发送“继续”，同时仍保留第二次熔断作为死循环保险。
        const MAX_HARD_BUDGET_EXTENSIONS: u8 = 1;
        let mut hard_budget_extensions = 0u8;
        let mut convergence_notified = false;
        // 预算续期耗尽后只给一次最终收尾窗口（2 步）；窗口也用尽则强制停止。
        let mut final_window_armed = false;
        // 上游可能正常结束却没有正文/工具调用（例如网关截断、reasoning-only 帧）。
        // 这不是完成；允许有限恢复重试，避免把占位文本污染会话上下文。
        const MAX_EMPTY_RESPONSE_RETRIES: usize = 2;

        /// 连续多少个“预算耗尽且无写入”窗口后才判定卡死/死循环并中断；
        /// 未达上限一律自动续期换路继续，不让用户手动发“继续”。
        const MAX_STAGNANT_WINDOWS: u32 = 3;
        let mut empty_response_retries = 0usize;
        let mut length_recovery_pending = false;
        while debt > 0 {
            steps += 1;
            execution.steps = steps;
            debt -= 1;
            if BudgetManager::hard_exhausted(&execution, &budget) {
                let can_auto_resume = hard_budget_extensions < MAX_HARD_BUDGET_EXTENSIONS
                    && execution.solve_mode != crate::execution::SolveMode::AtomicDelivery
                    && execution.successful_tool_results > 0;
                if can_auto_resume {
                    hard_budget_extensions += 1;
                    BudgetManager::extend_hard_window(&mut budget);
                    // 软窗口也要前移，否则下一步会立即落回预算诊断分支，尚未给模型
                    // 执行“换路/写入/验证”的机会。
                    BudgetManager::extend_window(&mut budget);
                    messages.push(Message::user(format!(
                        "[自动续跑·{hard_budget_extensions}/{MAX_HARD_BUDGET_EXTENSIONS}] 已达到本阶段总预算，但已有 {} 次成功工具结果。无需等待用户输入：保留当前证据，停止重复探索，直接围绕未满足验收项完成写入或验证。再次达到总预算时必须基于证据收尾或报告明确阻塞。",
                        execution.successful_tool_results
                    )));
                    append_telemetry(
                        &log,
                        &execution,
                        &ledger,
                        "总预算已自动延展，继续当前未满足验收项",
                    );
                    debt = 1;
                    continue;
                }
                absolute_budget_hit = true;
                hard_stop = true;
                log.append(SessionEvent::Assistant {
                    id: log.gen_id(),
                    chunk: Chunk {
                        text: Some(format!(
                            "[blocked] 已达到本任务的绝对熔断（{} 步 / {} 次工具调用）；停止新的探索以避免空转。当前证据会被保留，可在补充范围或约束后继续。",
                            budget.hard_max_steps, budget.hard_max_tool_calls
                        )),
                        ..Default::default()
                    },
                });
                break;
            }
            log.append(SessionEvent::StepStart {
                id: log.gen_id(),
                step: steps,
            });
            append_telemetry(&log, &execution, &ledger, "请求模型执行当前阶段");

            // 瀑布前处理：可重写/拒绝消息；空链返回输入本身（终态恒等）。
            // 链从事件总线注册表收集：插件可经 `on_waterfall::<PreStep>` 注入
            // around-middleware（如 Trellis 的 spec 注入 / 任务状态机）；
            // 无注册时为空 vec = 与旧行为完全一致。
            let chain = bus.collect_waterfall::<PreStep>();
            // 空链直接短路：无插件注册 PreStep 中间件（默认场景）时省去 waterfall 分发。
            let pre_input = if chain.is_empty() {
                messages.clone()
            } else {
                bus.waterfall(
                    PreStep {
                        input: messages.clone(),
                    },
                    &chain,
                )
                .input
            };
            // PreStep 包含不断增长的完整上下文，持久化会造成 O(n²) 日志膨胀。
            // 真正的会话重建只依赖 TurnStart/Assistant/ToolResult，因此不写入 SessionLog。

            // 每一步都执行上下文预算，而非仅在回合开始时裁剪；工具循环越长，
            // 节省的重复 prompt token 越明显。
            // 原子任务以低推理、短输出请求模型：工具门禁只能减少后续回合，只有这里
            // 能抑制首个 tool call 前的隐藏长思考与 `omitted` token 消耗。非原子任务
            // 完全保留用户的模型设置和默认输出预算。
            let request_options = if execution.solve_mode
                == crate::execution::SolveMode::AtomicDelivery
                || length_recovery_pending
            {
                RequestOptions {
                    max_output_tokens: Some(if length_recovery_pending {
                        1_024
                    } else {
                        1_536
                    }),
                    // DeepSeek/OpenAI 兼容端使用 `none` 表示关闭；`off` 是旧目录
                    // 的内部别名，直接透传会被网关以 HTTP 400 拒绝并中断整个回合。
                    reasoning_effort: Some("none".into()),
                    allowed_tools: Some(execution.allowed_tools()),
                }
            } else {
                RequestOptions {
                    allowed_tools: Some(execution.allowed_tools()),
                    ..Default::default()
                }
            };
            let mut s = llm.stream_with_options(apply_context_budget(pre_input), request_options);
            let mut assistant_text = String::new();
            let mut assistant_tools = Vec::new();
            let mut assistant_reasoning = String::new();
            let mut step_had_tools = false;
            let mut loop_recovery_prompts = Vec::new();
            let mut loop_recovery_exhausted = false;
            let mut empty_response_reason: Option<String> = None;
            // 本步（单次请求）的 token 用量累计（AIOps 成本计量）。
            let mut step_usage = Usage::default();
            loop {
                let item = tokio::select! {
                    _ = cancellation.cancelled() => {
                        log.append(SessionEvent::Assistant { id: log.gen_id(), chunk: Chunk { text: Some("[已停止]".into()), ..Default::default() } });
                        cancelled = true;
                        debt = 0;
                        hard_stop = true;
                        break;
                    }
                    item = s.next() => item,
                };
                let Some(item) = item else {
                    break;
                };
                // 错误不再上抛吞掉：写入日志可见，并终止回合（置 debt=0），
                // 否则 TurnEnd 永不写入会让 UI 轮询死循环。
                let chunk = match item {
                    Ok(c) => c,
                    Err(e) => {
                        log.append(SessionEvent::Assistant {
                            id: log.gen_id(),
                            chunk: Chunk {
                                text: Some(format!("[error] {e}")),
                                ..Default::default()
                            },
                        });
                        debt = 0;
                        hard_stop = true;
                        break;
                    }
                };
                // 思考链同时写 UI Thinking 事件和 Assistant 流事件。后者在模型产生
                // tool_call 时是 DeepSeek 的协议上下文，不能只留在 UI 日志里。
                if let Some(r) = &chunk.reasoning {
                    log.append(SessionEvent::Thinking {
                        id: log.gen_id(),
                        text: r.clone(),
                    });
                }
                if let Some(u) = &chunk.usage {
                    step_usage = step_usage.saturating_add(*u);
                }
                if chunk.empty_response {
                    empty_response_reason = chunk
                        .finish_reason
                        .clone()
                        .or_else(|| Some("unknown".into()));
                    continue;
                }
                if chunk.text.is_some() || !chunk.tool_calls.is_empty() || chunk.reasoning.is_some() {
                    log.append(SessionEvent::Assistant {
                        id: log.gen_id(),
                        chunk: chunk.clone(),
                    });
                }
                if let Some(text) = &chunk.text {
                    assistant_text.push_str(text);
                }
                if let Some(reasoning) = &chunk.reasoning {
                    assistant_reasoning.push_str(reasoning);
                }
                assistant_tools.extend(chunk.tool_calls.clone());
                // 工具调用并行化：门禁（循环守卫/行动门禁/访问策略/PreToolUse 钩子）
                // 串行执行以保全状态与顺序语义；通过门禁的调用收集后 join 并行分发。
                // 此前同一步的 N 个工具调用逐个 await，与系统提示词鼓励的并行调用相悖。
                let mut pending: Vec<(&ToolCall, String, ActionProposal)> = Vec::new();
                let mut pending_signatures = HashSet::new();
                // 原子任务的第一阶段只能有一个定位动作。否则模型即使知道“后续要
                // 缩小范围”，也可能在同一响应里并发发出 N 个不同关键词的泛搜。
                let mut atomic_step_gate = AtomicStepGate::default();
                for tc in &chunk.tool_calls {
                    // 守卫与行动门禁共用归一化签名，避免仅因路径分隔符或 `cd` 前缀
                    // 不同就绕过重复判定。
                    let proposal = ActionProposal::from_tool_call(tc, &execution);
                    let sig = proposal.signature.clone();
                    log.append(SessionEvent::ToolCall {
                        id: log.gen_id(),
                        call: tc.clone(),
                    });

                    if repeat_guard.should_block(&sig) {
                        let recovery = repeat_guard.note_recovery(&sig);
                        let blocked = ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            content: format!(
                                "[tool-loop guard] 工具 {} 的相同参数调用不会带来新信息：成功调用不得原样重试，失败调用最多首次加一次定向重试。本次未执行；请分析已有结果、换用不同参数/工具或直接基于现有证据收尾。",
                                tc.name
                            ),
                            continuation_debt: 0,
                        };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: blocked.clone(),
                        });
                        messages.push(Message::tool(tc.id.clone(), blocked.content));
                        step_had_tools = true;
                        if recovery <= MAX_LOOP_RECOVERY_PROMPTS {
                            loop_recovery_prompts.push(format!(
                                "[循环恢复] 工具 {} 的相同调用已被拦截（恢复提示 {recovery}/{MAX_LOOP_RECOVERY_PROMPTS}）。任务尚未完成：先解释现有结果，再选择不同参数、不同工具或下一项验证；禁止原样重试。",
                                tc.name
                            ));
                        } else {
                            loop_recovery_exhausted = true;
                        }
                        continue;
                    }

                    if !atomic_step_gate.allows(
                        execution.solve_mode == crate::execution::SolveMode::AtomicDelivery,
                        &sig,
                    ) {
                        let blocked = ToolResult {
                                call_id: tc.id.clone(),
                                ok: false,
                                content: "[atomic-delivery guard] 单点回归的当前阶段只允许一个定位 search；请先使用该结果缩小到具体文件/行号，再决定下一步。".into(),
                                continuation_debt: 0,
                            };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: blocked.clone(),
                        });
                        messages.push(Message::tool(tc.id.clone(), blocked.content));
                        step_had_tools = true;
                        continue;
                    }

                    // 同一回复里出现完全相同的并行调用时，执行其中一个不会比执行全部
                    // 少任何信息，只会放大空跑成本。这里在分发前去重，仍给每个调用补齐
                    // 协议要求的 tool result。
                    if !pending_signatures.insert(sig.clone()) {
                        let blocked = ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            content: format!(
                                "[tool-loop guard] 工具 {} 与本步骤中已排队调用的参数完全相同，已跳过重复执行；请使用第一个结果继续。",
                                tc.name
                            ),
                            continuation_debt: 0,
                        };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: blocked.clone(),
                        });
                        messages.push(Message::tool(tc.id.clone(), blocked.content));
                        step_had_tools = true;
                        continue;
                    }

                    // 通用行动门禁：每个工具动作必须关联验收目标。调用/时间预算是软检查点，
                    // 只触发进展诊断与续期，不会因为任务耗时较长而拒绝必要动作。
                    if let GateDecision::Deny(reason) =
                        ActionGate::authorize(&proposal, &execution, &budget)
                    {
                        let denied = ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            content: format!("[execution gate] {reason}"),
                            continuation_debt: 0,
                        };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: denied.clone(),
                        });
                        repeat_guard.record_result(&sig, &denied);
                        messages.push(Message::tool(tc.id.clone(), denied.content));
                        step_had_tools = true;
                        continue;
                    }
                    execution.tool_calls =
                        execution.tool_calls.saturating_add(proposal.estimated_cost);

                    if let Some(policy) = ctx.try_get::<harness_core::AccessPolicy>() {
                        if !policy.allows(&tc.name, &tc.args) {
                            let denied = ToolResult {
                                call_id: tc.id.clone(),
                                ok: false,
                                content: format!("访问权限“{}”拒绝了该工具调用", policy.mode()),
                                continuation_debt: 0,
                            };
                            log.append(SessionEvent::ToolResult {
                                id: log.gen_id(),
                                result: denied.clone(),
                            });
                            repeat_guard.record_result(&sig, &denied);
                            messages.push(Message::tool(tc.id.clone(), denied.content));
                            step_had_tools = true;
                            continue;
                        }
                    }

                    // 钩子（PreToolUse）：可阻断危险工具调用（fail-closed 在 Provider 侧）。
                    let pre = hook.run(&HookPayload {
                        event: HookEvent::PreToolUse,
                        tool: Some(tc.name.clone()),
                        input: Some(format!("{:?}", tc)),
                        ..Default::default()
                    })?;
                    if let HookDecision::Block(reason) = pre {
                        let blocked = ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            content: format!("[blocked by hook] {reason}"),
                            continuation_debt: 0,
                        };
                        log.append(SessionEvent::ToolResult {
                            id: log.gen_id(),
                            result: blocked.clone(),
                        });
                        repeat_guard.record_result(&sig, &blocked);
                        messages.push(Message::tool(tc.id.clone(), blocked.content.clone()));
                        step_had_tools = true;
                        continue;
                    }

                    // 通过全部门禁；延迟到本批收集完毕后并行执行。
                    pending.push((tc, sig, proposal));
                }
                // 并行执行阶段：只有纯 I/O 的 dispatch 并行；结果顺序与 tool_calls
                // 声明顺序一致，保证 tool 消息与 assistant 宣告一一配对。
                if !pending.is_empty() {
                    let futs = pending.iter().map(|(tc, _sig, _proposal)| {
                        let tools = Arc::clone(&tools);
                        async move {
                            // 工具调用必须有超时和取消通道。此前只有模型流设置了 idle
                            // 超时，某个 shell / 插件工具卡住时 UI 会一直 busy。
                            let outcome = tokio::time::timeout(
                                std::time::Duration::from_secs(tool_timeout_secs),
                                tools.dispatch(tc),
                            )
                            .await;
                            match outcome {
                                Ok(Ok(result)) => result,
                                Ok(Err(error)) => ToolResult {
                                    call_id: tc.id.clone(),
                                    ok: false,
                                    content: format!("tool execution failed: {error}"),
                                    continuation_debt: 0,
                                },
                                Err(_) => ToolResult {
                                    call_id: tc.id.clone(),
                                    ok: false,
                                    content: format!(
                                        "工具 {} 超过 {tool_timeout_secs} 秒未返回，已停止本次调用",
                                        tc.name
                                    ),
                                    continuation_debt: 0,
                                },
                            }
                        }
                    });
                    let joined = tokio::select! {
                        _ = cancellation.cancelled() => None,
                        results = futures::future::join_all(futs) => Some(results),
                    };
                    match joined {
                        Some(results) => {
                            for ((tc, sig, proposal), res) in pending.iter().zip(results) {
                                // 钩子（PostToolUse）：审计 / 后处理挂钩点。
                                let _ = hook.run(&HookPayload {
                                    event: HookEvent::PostToolUse,
                                    tool: Some(tc.name.clone()),
                                    output: Some(format!("{:?}", res)),
                                    ..Default::default()
                                });
                                log.append(SessionEvent::ToolResult {
                                    id: log.gen_id(),
                                    result: res.clone(),
                                });
                                repeat_guard.record_result(sig, &res);
                                execution.record_tool_result(proposal, res.ok, &res.content);
                                for criterion in &proposal.supports {
                                    ledger.activate(criterion);
                                    if res.ok && execution.satisfied_criteria.contains(criterion) {
                                        ledger.add_evidence(
                                            criterion,
                                            res.content.chars().take(480).collect(),
                                        );
                                        ledger.verify(criterion);
                                    }
                                }
                                append_telemetry(
                                    &log,
                                    &execution,
                                    &ledger,
                                    if res.ok {
                                        "已记录工具结果"
                                    } else {
                                        "工具结果失败，等待调整"
                                    },
                                );
                                messages.push(Message::tool(tc.id.clone(), res.content.clone()));
                            }
                            step_had_tools = true;
                        }
                        None => {
                            // 取消：仍须为每个已宣告的 tool_call 补占位结果，否则
                            // 续跑/恢复会因「已宣告未执行」的孤儿调用直接 400。
                            for (tc, _sig, _proposal) in &pending {
                                messages.push(Message::tool(
                                    tc.id.clone(),
                                    format!("[已停止] 工具 {} 已取消", tc.name),
                                ));
                            }
                            step_had_tools = true;
                            cancelled = true;
                            debt = 0;
                            hard_stop = true;
                            break;
                        }
                    }
                }
            }
            let should_recover_empty = empty_response_reason.is_some()
                && assistant_text.trim().is_empty()
                && assistant_tools.is_empty();
            if !assistant_text.trim().is_empty() {
                last_assistant = assistant_text.clone();
            }
            // 本步用量落盘：Usage 事件不进模型上下文、不影响多轮重建，
            // 仅用于会话级成本计量（usage_total）。
            if step_usage.total_tokens > 0 {
                log.append(SessionEvent::Usage {
                    id: log.gen_id(),
                    usage: step_usage,
                });
            }
            if !should_recover_empty {
                length_recovery_pending = false;
                messages.insert(
                    messages.len().saturating_sub(assistant_tools.len()),
                    Message::assistant_with_tools_and_reasoning(
                        assistant_text,
                        assistant_tools,
                        (!assistant_reasoning.is_empty()).then_some(assistant_reasoning),
                    ),
                );
            }
            for prompt in loop_recovery_prompts {
                messages.push(Message::user(prompt));
            }
            // 续跑记账：本步无论并行多少个工具调用只续跑一次。旧的按调用 +1
            // 会让 N 个并行调用触发 N 次额外模型往返，步数预算被成倍消耗。
            // 硬终止（取消/错误/循环守卫）时禁止复活：此时可能有「已宣告未执行」的
            // tool_call 缺对应 tool 消息，续跑必 400。
            if should_recover_empty && !hard_stop {
                let reason = empty_response_reason.as_deref().unwrap_or("unknown");
                // `length` 不是普通空响应：完整历史重试只会进一步扩大请求。改为
                // 紧凑检查点 + 一次短恢复，让模型直接收敛到下一步或最终结论。
                let retry_limit = if reason == "length" {
                    1
                } else {
                    MAX_EMPTY_RESPONSE_RETRIES
                };
                if empty_response_retries < retry_limit {
                    empty_response_retries += 1;
                    debt += 1;
                    if reason == "length" {
                        // 原实现保留所有系统消息再追加 checkpoint；技能、事实和契约叠加后
                        // 仍可能超上下文。恢复请求只保留可执行的当前任务快照。
                        messages.clear();
                        messages.push(Message::system(format!(
                            "[长度恢复·最小快照]\n{}\n只允许：输出一个下一步工具调用，或给出含阻塞原因的最终结论；禁止重新规划、泛搜和复述历史。",
                            execution.compact_checkpoint()
                        )));
                        messages.push(Message::user(&input_text));
                        length_recovery_pending = true;
                    }
                    messages.push(Message::user(format!(
                        "[恢复请求] 上一次模型响应为空（finish_reason={reason}），没有生成正文或工具调用；这不代表任务完成。请基于现有上下文继续：若需要信息或执行操作，调用恰当工具；否则给出可验证的完整答复。不要只输出思考过程。自动重试第 {empty_response_retries}/{retry_limit} 次。"
                    )));
                } else {
                    log.append(SessionEvent::Assistant {
                        id: log.gen_id(),
                        chunk: Chunk {
                            text: Some(format!(
                                "[error] 模型连续 {} 次返回空响应（最后 finish_reason={reason}）。请求未被视为完成；请检查模型/网关日志、输出 token 限制或切换模型后重试。",
                                retry_limit + 1
                            )),
                            ..Default::default()
                        },
                    });
                    debt = 0;
                    hard_stop = true;
                }
            } else if loop_recovery_exhausted {
                log.append(SessionEvent::Assistant {
                    id: log.gen_id(),
                    chunk: Chunk {
                        text: Some(
                            "[error] 模型在收到两次循环恢复提示后仍重复同一工具调用；任务未完成，但继续执行不会产生新信息。请检查该工具结果、补充任务约束或切换模型后继续。".into(),
                        ),
                        ..Default::default()
                    },
                });
                debt = 0;
                hard_stop = true;
            } else if step_had_tools && !hard_stop {
                debt += 1;
            }
            log.append(SessionEvent::StepEnd {
                id: log.gen_id(),
                step: steps,
            });

            if should_recover_empty {
                // 恢复重试已经重新记账，不能再被“本步没有工具”误判为完成。
            } else if BudgetManager::phase(&execution, &budget)
                == crate::execution::BudgetPhase::Exhausted
            {
                budget_exhausted = true;
                match BudgetManager::diagnose_and_renew(&mut execution, &mut budget) {
                    Some(diagnosis) => {
                        convergence_notified = false;
                        messages.push(Message::user(&diagnosis));
                    }
                    None if !final_window_armed => {
                        // 续期次数用尽：进入最终收尾窗口（非截断）——给足步骤
                        // 汇总交付，但不再允许扩张探索（旧实现无限续期，
                        // 单回合可跑出 591 步）。
                        final_window_armed = true;
                        BudgetManager::arm_final_window(&execution, &mut budget);
                        messages.push(Message::user(
                            "[收尾阶段] 预算续期已用完，请在接下来 6 步内完成交付：停止新的探索与扫描，把已完成的改动固化（必要的写入/构建验证），然后输出结构化总结：1) 已完成的交付物与验证结果；2) 未完成部分及原因；3) 建议的后续步骤。请先自评：若剩余工作能在窗口内完成，就集中精力做完；若判断无法完成，立即停止新的工具调用，直接输出上述总结，不要浪费收尾窗口。",
                        ));
                    }
                    None if budget.stagnant_windows >= MAX_STAGNANT_WINDOWS => {
                        // 预算窗口只是进展检查点，不能以“没有代码修改”中断尚未完成的
                        // 排障、测试或只读任务。真正的重复工具调用仍由专门守卫处理；
                        // 此处只加强收尾约束并自动接续，模型可据现有证据交付结果。
                        convergence_notified = false;
                        BudgetManager::extend_window(&mut budget);
                        messages.push(Message::user(
                            "[强制收敛，不中断任务] 连续多个检查窗口未得到可验证的新进展。不要重复已执行的工具调用，也不要继续泛扫；请立刻基于已有证据选择其一：1) 执行一项能直接验证或完成未满足验收条件的不同动作；2) 若已无法继续，停止工具调用并输出完整交付总结（已完成、证据、未完成原因、下一步）。任务仍在继续，除非用户取消或发生明确错误。",
                        ));
                    }
                    None => {
                        // 收尾窗口也已耗尽但未达卡死阈值：任务未完成，自动延展一个窗口
                        // 并强制换路继续，无需用户手动发“继续”（仅死循环才中断）。
                        convergence_notified = false;
                        BudgetManager::extend_window(&mut budget);
                        messages.push(Message::user(
                            "[自动接续] 任务尚未完成，预算已自动延展，无需人工介入。请立即改变策略：停止重复的探索与读取，基于现有证据直接进行写入修改与交付，完成后做一次构建验证并输出总结；若判断已陷入循环无法推进，停止工具调用，直接输出当前进展总结与阻塞原因。",
                        ));
                    }
                }
            } else {
                match CompletionJudge::evaluate(&execution, &budget, step_had_tools) {
                    Completion::Converge(reason) if !convergence_notified => {
                        convergence_notified = true;
                        messages.push(Message::user(&format!("[系统提示] {reason}")));
                        // 收敛提示必须伴随一次新的模型请求；此前无工具调用时 debt
                        // 已归零，提示虽写入上下文却没有机会被模型执行，回合直接以
                        // Blocked 结束。只补一次，后续仍由预算/重复守卫负责收尾。
                        if !hard_stop {
                            debt += 1;
                        }
                    }
                    Completion::Complete => {
                        delivery_verified = true;
                        debt = 0;
                    }
                    _ => {}
                }
            }

            // 唯一终止检查点（serial，无 next()）。
            let stop = bus
                .serial(TurnStopping {
                    will_stop: debt == 0,
                })
                .await;
            if stop.will_stop {
                break;
            }
        }

        // 记忆自动沉淀（L0）：记录本轮助手最终回复（无后端则落本地文件）。失败容忍。
        if !last_assistant.trim().is_empty() {
            if let Some(conv) = ctx.try_get::<dyn ConversationMemory>() {
                let _ = conv
                    .record_turn(ChatTurn {
                        session_id: log.id().to_string(),
                        role: "assistant".into(),
                        content: last_assistant.clone(),
                        ts: String::new(),
                    })
                    .await;
            }
        }

        let (outcome, reason) = if delivery_verified {
            (harness_session::DeliveryOutcome::Verified, None)
        } else if absolute_budget_hit {
            (
                harness_session::DeliveryOutcome::Blocked,
                Some("已达到绝对探索预算；为避免空跑而停止，尚缺完整验收证据".into()),
            )
        } else if cancelled {
            (
                harness_session::DeliveryOutcome::Cancelled,
                Some("用户取消了回合；未获得完整验收证据".into()),
            )
        } else if hard_stop {
            (
                harness_session::DeliveryOutcome::Interrupted,
                Some("回合因取消、模型异常或重复调用保护而中断；未获得完整验收证据".into()),
            )
        } else if budget_exhausted {
            (
                harness_session::DeliveryOutcome::Blocked,
                Some("执行曾进入预算收敛/延展阶段，但回合结束前未获得完整验收证据".into()),
            )
        } else {
            (
                harness_session::DeliveryOutcome::Blocked,
                Some("回合结束前未获得完整验收证据；结果不能标记为已交付".into()),
            )
        };
        log.append(SessionEvent::Delivery {
            id: log.gen_id(),
            report: execution.delivery_report(outcome.clone(), reason),
        });
        // 只在 Runtime 验证通过后沉淀经验卡；模型文本或 TurnEnd 绝不触发写入，
        // 这样下一次检索到的是可复核的解决路径而不是自报完成。
        if outcome == harness_session::DeliveryOutcome::Verified {
            if let Some(conv) = ctx.try_get::<dyn ConversationMemory>() {
                let workspace = ctx
                    .try_get::<harness_core::Workspace>()
                    .map(|ws| ws.root().display().to_string())
                    .unwrap_or_else(|| "unknown-workspace".into());
                let evidence = execution
                    .verification_evidence
                    .values()
                    .flatten()
                    .take(3)
                    .map(|item| item.chars().take(220).collect::<String>())
                    .collect::<Vec<_>>()
                    .join(" | ");
                let fingerprint = stable_fingerprint(&execution.contract.objective);
                let card =
                    MemoryFact {
                        id: format!("solve-card:{fingerprint}"),
                        kind: FactKind::Decision,
                        content: format!(
                        "[SolveCard]\\n问题：{}\\n工作区：{}\\n有效验证：{}\\n结果：已验证交付。",
                        execution.contract.objective.chars().take(600).collect::<String>(),
                        workspace,
                        evidence,
                    ),
                        layer: LifecycleLayer::L2,
                        confidence: 0.9,
                        source: format!(
                            "solve-card;workspace={workspace};fingerprint={fingerprint}"
                        ),
                    };
                let _ = conv.remember(card).await;
            }
        }
        append_telemetry(&log, &execution, &ledger, "回合结束，交付状态已落盘");
        log.append(SessionEvent::TurnEnd { id: log.gen_id() });
        Ok(())
    }
}

fn append_telemetry(
    log: &SessionLog,
    execution: &ExecutionState,
    ledger: &TaskLedger,
    detail: &str,
) {
    let current = ledger
        .current_item()
        .map(|item| format!("{}：{}", item.id, item.description))
        .unwrap_or_else(|| "全部验收项已处理".into());
    log.append(SessionEvent::Telemetry {
        id: log.gen_id(),
        telemetry: ExecutionTelemetry {
            intent: format!(
                "{:?}",
                crate::IntentProfile::compile(&execution.contract.objective).kind
            ),
            phase: execution.tool_phase().as_str().into(),
            allowed_tools: execution.allowed_tools(),
            step: execution.steps,
            tool_calls: execution.tool_calls,
            evidence_count: execution.evidence.len(),
            verified_count: ledger.verified_count(),
            blocked_count: ledger.blocked_count(),
            detail: format!("{detail}；当前验收：{current}"),
        },
    });
}

fn render_experience(facts: &[MemoryFact]) -> Option<String> {
    let cards = facts
        .iter()
        .filter(|fact| fact.content.starts_with("[SolveCard]"))
        .take(3)
        .map(|fact| fact.content.chars().take(700).collect::<String>())
        .collect::<Vec<_>>();
    (!cards.is_empty()).then(|| {
        format!(
            "[已验证历史经验]\\n以下是检索出的经验卡，只能作为候选线索；先用当前工作区证据验证，不能把它当作当前事实：\\n{}",
            cards.join("\\n---\\n")
        )
    })
}

fn stable_fingerprint(text: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// 生成紧凑的技能系统指令，避免用户导入的长技能文档无限放大上下文。
fn render_skill_instructions(skills: &[Skill]) -> Option<String> {
    const MAX_SKILLS: usize = 4;
    const MAX_STEP_CHARS: usize = 360;
    let mut out = String::from("[已启用的匹配技能]\n");
    for skill in skills.iter().take(MAX_SKILLS) {
        let steps = skill
            .steps
            .join("；")
            .chars()
            .take(MAX_STEP_CHARS)
            .collect::<String>();
        let checks = skill
            .verification_rules
            .join("；")
            .chars()
            .take(MAX_STEP_CHARS)
            .collect::<String>();
        out.push_str(&format!(
            "- {}：适用范围：{}\n  执行：{}\n  验证：{}\n",
            skill.name,
            skill.trigger_boundary,
            if steps.is_empty() {
                "遵循技能文档的步骤"
            } else {
                &steps
            },
            if checks.is_empty() {
                "完成后进行必要验证"
            } else {
                &checks
            },
        ));
    }
    (out.lines().count() > 1).then_some(out)
}

fn messages_from_events(events: &[SessionEvent]) -> Vec<Message> {
    let mut messages = vec![Message::system(SYSTEM_PROMPT)];
    // DeepSeek can stream reasoning_content in frames before the frame containing
    // its tool call. Older logs stored those frames only as Thinking events, while
    // newer logs additionally store them in the Assistant chunk. Keep fragments
    // until the visible assistant frame is available in either representation.
    let mut pending_reasoning = String::new();
    for event in events {
        match event {
            SessionEvent::TurnStart { input, .. } => {
                // A reasoning-only response without text or a tool call is not a
                // valid reusable assistant turn. Do not attach it to a later user
                // turn (and never invent reasoning content).
                pending_reasoning.clear();
                messages.push(Message::user(input));
            }
            SessionEvent::Assistant { chunk, .. } => {
                // SSE 流式下每个文本增量都是一条 Assistant 事件；重建上下文时必须把相邻的
                // 纯文本 assistant 分片合并为一条消息，否则会向模型发送大量连续 assistant
                // 消息（OpenAI 兼容协议不接受，多轮对话直接报错）。
                let text = chunk.text.clone().unwrap_or_default();
                if let Some(reasoning) = &chunk.reasoning {
                    // 新日志顺序为 Thinking → Assistant(reasoning)，避免将同一 SSE
                    // 分片记录两次；旧日志只有 Thinking，仍可在此后工具帧完成恢复。
                    if !pending_reasoning.ends_with(reasoning) {
                        pending_reasoning.push_str(reasoning);
                    }
                }
                if text.is_empty() && chunk.tool_calls.is_empty() {
                    continue;
                }
                let reasoning_content = (!pending_reasoning.is_empty())
                    .then(|| std::mem::take(&mut pending_reasoning));
                if chunk.tool_calls.is_empty() {
                    if let Some(last) = messages.last_mut() {
                        if last.role == Role::Assistant && last.tool_calls.is_empty() {
                            last.content.push_str(&text);
                            if let Some(reasoning) = reasoning_content {
                                last.reasoning_content
                                    .get_or_insert_with(String::new)
                                    .push_str(&reasoning);
                            }
                            continue;
                        }
                    }
                }
                messages.push(Message::assistant_with_tools_and_reasoning(
                    text,
                    chunk.tool_calls.clone(),
                    reasoning_content,
                ));
            }
            SessionEvent::ToolResult { result, .. } => {
                pending_reasoning.clear();
                messages.push(Message::tool(
                    &result.call_id,
                    &compress_tool_context(&result.content),
                ));
            }
            // 旧版日志的 Thinking 事件是恢复 DeepSeek tool-call 协议的唯一来源。
            // 新版同时有 Assistant(reasoning) 会在上方去重。
            SessionEvent::Thinking { text, .. } => pending_reasoning.push_str(text),
            // Step* / PlanUpdate 仅 UI 展示，不进入模型上下文。
            _ => {}
        }
    }
    // 旧日志防御：合并后的 assistant 文本可能残留 DSML 裸标记，剥离后再发给模型。
    for m in messages.iter_mut() {
        if m.role == Role::Assistant {
            m.content = harness_llm::dsml::strip_dsml(&m.content);
        }
    }
    // 协议净化：DeepSeek/OpenAI 要求 assistant 消息的每个 tool_call 后面必须紧跟
    // 同 call_id 的 tool 消息。取消/流错误/循环守卫等异常终止会在日志里留下
    // 「已宣告未执行」的 tool_call，直接发送即 HTTP 400；剔除无响应的 tool_call
    // 与孤儿 tool 消息，保证任何日志重建出的上下文都协议合法。
    let responded: std::collections::HashSet<String> = messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .filter_map(|m| m.tool_call_id.clone())
        .collect();
    let mut announced: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in messages.iter_mut() {
        if m.role == Role::Assistant {
            m.tool_calls.retain(|tc| responded.contains(&tc.id));
            for tc in &m.tool_calls {
                announced.insert(tc.id.clone());
            }
        }
    }
    messages.retain(|m| match m.role {
        Role::Tool => m
            .tool_call_id
            .as_deref()
            .is_some_and(|id| announced.contains(id)),
        // 剥离 tool_calls 后内容为空的 assistant 消息无信息量，且部分服务端拒收。
        Role::Assistant => !(m.content.is_empty() && m.tool_calls.is_empty()),
        _ => true,
    });
    apply_context_budget(compress_stale_tool_results(messages))
}

/// 陈旧工具结果渐进压缩：仅最近 `RECENT_FULL` 条工具输出保留完整（已被
/// `compress_tool_context` 限长）正文，更早的输出收缩为首部摘录。
/// 取证：单步上下文不算大，但上千步 replay 堆叠的旧探索输出会淹没当前目标，
/// 模型反复重读同一文件（最高 18 次）。在保留近期交付所需细节的同时
/// 压缩 prompt 体积、把注意力拉回当前目标；真正需要旧输出时可重新调用工具。
fn compress_stale_tool_results(mut messages: Vec<Message>) -> Vec<Message> {
    const RECENT_FULL: usize = 12;
    const STALE_EXCERPT: usize = 300;
    let tool_indices: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::Tool)
        .map(|(i, _)| i)
        .collect();
    let stale_count = tool_indices.len().saturating_sub(RECENT_FULL);
    for &idx in tool_indices.iter().take(stale_count) {
        let message = &mut messages[idx];
        if message.content.chars().count() > STALE_EXCERPT {
            let excerpt: String = message.content.chars().take(STALE_EXCERPT).collect();
            message.content =
                format!("{excerpt}\n…[较早的工具输出已节选；如需完整内容请重新调用工具获取]");
        }
    }
    messages
}

/// 工具原文对用户日志仍完整保留；送回模型时按层压缩，避免一次构建/搜索输出挤掉
/// 近期对话。短内容不变，长内容保留开头、错误附近和结尾。
/// 显式上传的附件是本回合输入条件。文本附件直接提供受限摘录；二进制附件保留
/// 文件名、MIME 与路径，要求 Agent 在用户授权范围内按需调用合适工具处理。
fn render_attachment_context(attachments: &[harness_core::Attachment], image_notes: &[String]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let mut out = String::from("[用户已附加以下文件；必须作为任务输入条件处理]\n");
    for attachment in attachments {
        let name = attachment
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("未命名文件");
        out.push_str(&format!(
            "- {name}（{}，路径：{}）",
            attachment.mime,
            attachment.path.display()
        ));
        let ext = attachment
            .path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "txt" | "md" | "rs" | "toml" | "json" | "yaml" | "yml" | "csv" | "log" | "xml"
        ) {
            if let Ok(text) = std::fs::read_to_string(&attachment.path) {
                let excerpt: String = text.chars().take(8_000).collect();
                out.push_str(&format!("\n  文本摘录：\n{excerpt}"));
                if text.chars().count() > 8_000 {
                    out.push_str("\n  [摘录已截断，请按需读取文件]\n");
                }
            }
        } else {
            out.push_str("\n  [二进制或富媒体附件：请基于文件类型和任务要求处理，不得忽略]\n");
        }
        out.push('\n');
    }
    if !image_notes.is_empty() {
        out.push_str("[图片处理状态]\n");
        for note in image_notes {
            out.push_str("- ");
            out.push_str(note);
            out.push('\n');
        }
    }
    out
}

/// 单张图片上限，防止一次粘贴把原图 base64 膨胀后挤爆模型上下文或请求体。
const MAX_INLINE_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

fn attachment_prompt(input: &UserInput) -> String {
    if input.text.trim().is_empty() && !input.attachments.is_empty() {
        "请分析并说明我附上的附件内容；如包含图片，请识别其中可见的信息。若当前模型或文件类型不支持直接识别，请明确说明原因及下一步可行操作。".to_string()
    } else {
        input.text.clone()
    }
}

/// 把可识别的本地图片转换为 Provider 可发送的 data URL。
///
/// 视觉能力明确关闭时绝不上传原图，避免把图片传给文本模型并得到难以理解的 400；
/// 同时返回可见状态说明，保证用户不会误以为图片已经被识别。
fn inline_image_data_urls(
    attachments: &[harness_core::Attachment],
    vision_enabled: bool,
) -> (Vec<String>, Vec<String>) {
    let mut urls = Vec::new();
    let mut notes = Vec::new();
    for attachment in attachments {
        let Some(mime) = vision_image_mime(attachment) else {
            continue;
        };
        let name = attachment
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("未命名图片");
        if !vision_enabled {
            notes.push(format!("{name}：当前所选模型不支持图片识别，未发送原图。"));
            continue;
        }
        let bytes = match std::fs::read(&attachment.path) {
            Ok(bytes) if bytes.len() as u64 <= MAX_INLINE_IMAGE_BYTES => bytes,
            Ok(bytes) => {
                notes.push(format!(
                    "{name}：图片大小 {} MiB，超过 {} MiB 上限，未发送原图。",
                    bytes.len() / 1024 / 1024,
                    MAX_INLINE_IMAGE_BYTES / 1024 / 1024
                ));
                continue;
            }
            Err(error) => {
                notes.push(format!("{name}：无法读取图片（{error}），未发送原图。"));
                continue;
            }
        };
        urls.push(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ));
        notes.push(format!("{name}：已作为图片内容发送给视觉模型。"));
    }
    (urls, notes)
}

fn vision_image_mime(attachment: &harness_core::Attachment) -> Option<&'static str> {
    match attachment
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn compress_tool_context(content: &str) -> String {
    // 6k→8k→16k：旧阈值对编译/测试类长输出裁得过狠，模型拿不到关键尾部
    // （错误栈通常在末尾）而重复调用工具；取证进一步发现 8k 会把 fs 区间读
    // 的代码内容截掉大半，模型看不到目标区域转而自造截取脚本；16k 与
    // fs 单次区间读上限（250 行）匹配，保证按需读取的内容完整可达。
    const LIMIT: usize = 16_000;
    if content.chars().count() <= LIMIT {
        return content.into();
    }
    let lines: Vec<&str> = content.lines().collect();
    let mut selected: Vec<&str> = lines.iter().take(36).copied().collect();
    selected.extend(
        lines
            .iter()
            .filter(|line| {
                let lower = line.to_ascii_lowercase();
                lower.contains("error") || lower.contains("failed") || lower.contains("warning")
            })
            .take(24)
            .copied(),
    );
    selected.extend(lines.iter().rev().take(28).rev().copied());
    selected.dedup();
    let mut out = format!("[工具输出已压缩：原 {} 字符]\n", content.chars().count());
    for line in selected {
        if out.chars().count() + line.chars().count() + 1 > LIMIT {
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// 确定性的上下文预算器：保留系统提示和最近完整回合，较早对话压成短摘要。
/// 从 User 边界裁剪，避免拆开 assistant tool_call / tool result 协议对。
fn apply_context_budget(messages: Vec<Message>) -> Vec<Message> {
    // UI「参数配置」显式设置优先；其次 `HARNESS_CONTEXT_MAX_CHARS` 环境变量；最后默认 96k 字符。
    // 旧默认 48k 对长工具会话裁剪过激：早期证据被压成短摘录后模型会重新读取同样
    // 的文件/命令输出，多余往返的耗时与 token 远超保留上下文的成本。
    let budget = harness_core::tuning::context_budget_chars()
        .or_else(|| {
            std::env::var("HARNESS_CONTEXT_MAX_CHARS")
                .ok()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(96_000usize)
        .clamp(12_000, 240_000);
    let total: usize = messages.iter().map(message_chars).sum();
    if total <= budget {
        return messages;
    }

    let user_starts: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == Role::User).then_some(index))
        .collect();
    let Some(&latest_user) = user_starts.last() else {
        return messages;
    };
    let mut start = latest_user;
    let mut retained: usize = messages[start..].iter().map(message_chars).sum();
    for &candidate in user_starts.iter().rev().skip(1) {
        let added: usize = messages[candidate..start].iter().map(message_chars).sum();
        if retained.saturating_add(added) > budget.saturating_sub(4_000) {
            break;
        }
        start = candidate;
        retained += added;
    }
    if start <= 1 {
        return messages;
    }

    let mut result: Vec<Message> = messages[..start]
        .iter()
        .filter(|message| message.role == Role::System)
        .cloned()
        .collect();
    let omitted = start.saturating_sub(result.len());
    let mut summary = format!("[较早会话已按上下文预算压缩，共省略 {omitted} 条消息]\n");
    for message in messages[1..start].iter().rev() {
        if !matches!(message.role, Role::User | Role::Assistant)
            || message.content.trim().is_empty()
        {
            continue;
        }
        let role = if message.role == Role::User {
            "用户"
        } else {
            "助手"
        };
        let compact = message
            .content
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let excerpt: String = compact.chars().take(480).collect();
        let line = format!("{role}: {excerpt}\n");
        if summary.chars().count() + line.chars().count() > 4_000 {
            break;
        }
        summary.push_str(&line);
    }
    result.push(Message::system(summary));
    result.extend(messages.into_iter().skip(start));
    result
}

fn message_chars(message: &Message) -> usize {
    message.content.chars().count()
        + message
            .tool_calls
            .iter()
            .map(|call| call.name.len() + call.args.to_string().chars().count())
            .sum::<usize>()
}

/// 结构化系统提示词：语言跟随、工具契约、长周期工作流、安全边界。
const SYSTEM_PROMPT: &str = "You are a reliable desktop assistant and coding agent.\n\
\n\
## 语言与格式\n\
- 始终用与用户最新消息相同的语言回复（中文提问用中文答）。\n\
- 结论先行；需要时用简洁的 markdown 列表展开步骤，不写冗长铺垫。\n\
\n\
## 工具契约\n\
- 只允许使用提供给你的工具：fs / edit / shell / search / plan / delegate。\n\
- 定位代码/文本位置一律优先用 search（一次调用返回文件:行号:内容）；严禁用 shell findstr/dir/grep 全仓扫描或编写临时扫描脚本来找代码。\n\
- 严禁在正文里输出任何形式的工具调用标记（DSML、XML invoke、tool_calls 文本等）；调用工具必须走 function calling 通道。\n\
- 问候、提问、普通对话直接回答，不使用工具。\n\
\n\
## 复杂任务工作流\n\
- 只有多个独立交付物、范围不明确或高风险任务才调用 plan；单点、范围明确的修复直接执行“定位 → 修改 → 验证”，不要为计划本身增加往返。\n\
- 相互独立的多个操作尽量在同一次回复里作为多个工具调用一起发出，减少往返。\n\
- 独立且耗时的子任务用 delegate 委托子代理，主线程只整合结果。\n\
- 回合结束前给出简洁、可读的最终总结。\n\
\n\
## 效率与收敛（必须遵守）\n\
- 最小路径优先：先用 search 定位与目标直接相关的最小文件集，再有针对性地读取；禁止全仓库递归扫描、批量试探性搜索与自造临时扫描脚本。\n\
- 读取大文件用 fs 的 start_line/end_line 按区间读取；严禁编写临时脚本（python/ps1 等）截取或提取文件内容。\n\
- 纯界面/配置类任务：search 定位到少数目标文件后集中批量编辑，全部改完再做一次构建验证，不要每改一处就编译一次。\n\
- 已读过的文件不要重复读取；成功的同一命令/同一参数不得原样重试，失败调用只允许一次带明确原因的定向重试。search 有命中后，下一次读取必须使用命中的路径和最小行区间；下一次搜索必须缩小目录或验证不同假设。\n\
- 探索（读取/搜索/列目录）应尽快收敛到写操作与验证；交付目标达成后立即停止，不做重复确认与打磨。\n\
- 步数预算有限且续期次数封顶：收到检查点/收尾提示时必须服从，基于现有证据交付总结，不要继续扩张探索。\n\
- shell 命令已在工作区根目录执行：不要重复 cd 到工作区，直接用相对路径。\n\
- 编译验证按 [项目事实] 给出的命令模板一次到位；命令失败后先读全量错误文本再换路一次，禁止用试错方式探索环境（manifest 位置、工具链、目录结构）。\n\
\n\
## 安全\n\
- 仅当用户明确要求检查/修改/构建/测试/操作工作区时才使用文件系统或 shell 工具。\n\
- 永不搜索或泄露 API key、凭据、token 等秘密。";

/// 进展检查间隔（沿用 env `HARNESS_MAX_STEPS` 保持兼容，默认 128）。
/// 这不是完成期限；到点后诊断调用价值并自动续期。失控由循环守卫、取消与 turn timeout 兜底。
fn max_steps_limit() -> usize {
    // UI 显式设置优先；其次兼容旧环境变量；最后默认 128。
    harness_core::tuning::max_steps()
        .or_else(|| {
            std::env::var("HARNESS_MAX_STEPS")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(128)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_capability::assets::Skill;
    use harness_llm::ToolCall;

    #[test]
    fn renders_matched_skills_as_compact_system_instructions() {
        let skills = vec![Skill {
            id: "review".into(),
            name: "Code review".into(),
            version: "1.0".into(),
            trigger_boundary: "review code".into(),
            steps: vec!["inspect diff".into(), "run tests".into()],
            verification_rules: vec!["findings recorded".into()],
            resource_files: vec![],
            confidence: 1.0,
            enabled: true,
            source_path: String::new(),
        }];
        let rendered = render_skill_instructions(&skills).expect("matched skill should render");
        assert!(rendered.contains("Code review"));
        assert!(rendered.contains("inspect diff；run tests"));
        assert!(rendered.contains("findings recorded"));
        assert!(render_skill_instructions(&[]).is_none());
    }

    #[test]
    fn retrieval_only_injects_verified_solve_cards_as_untrusted_hints() {
        let cards = vec![
            MemoryFact {
                id: "solve-card:abc".into(),
                kind: FactKind::Decision,
                content: "[SolveCard]\n问题：刷新失败\n结果：已验证交付。".into(),
                layer: LifecycleLayer::L2,
                confidence: 0.9,
                source: "solve-card".into(),
            },
            MemoryFact {
                id: "ordinary-fact".into(),
                kind: FactKind::Fact,
                content: "这不是可执行经验".into(),
                layer: LifecycleLayer::L2,
                confidence: 0.8,
                source: "test".into(),
            },
        ];
        let rendered = render_experience(&cards).expect("solve card should be injected");
        assert!(rendered.contains("刷新失败"));
        assert!(rendered.contains("候选线索"));
        assert!(!rendered.contains("这不是可执行经验"));
    }

    /// 全链路 Demo 回归（约定目录 + 自动加载）：外部技能包落盘进约定目录 →
    /// `sync_skill_packs` 自动注册但**默认未启用** → 用户勾选启用后中文语境
    /// match_skills 命中 → render_skill_instructions 产出含触发边界/步骤/验证的系统
    /// 指令（AgentLoop 每回合写入模型上下文的内容）→ 禁用后立即不再命中，
    /// 重复同步更新内容且不产生副本（全局开关即时生效）。
    #[tokio::test]
    async fn imported_skill_pack_flows_into_agent_context() {
        use harness_capability::index;
        use harness_provider_memory::NativeSkillLibrary;

        // 1) 构造外部来源技能包（SKILL.md + 资源文件），等价于 extensions/skill-packs 示例。
        let dir = std::env::temp_dir().join(format!(
            "harness-skill-e2e-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let src = dir.join("src");
        let pack = src.join("release-checklist");
        std::fs::create_dir_all(pack.join("resources")).unwrap();
        std::fs::write(
            pack.join("SKILL.md"),
            "# 发布检查清单\n\nversion: 1.1\n\n## 触发边界\n发布新版本、上线前需要检查清单\n\n## 执行步骤\n- 跑全量测试\n- 核对版本号\n\n## 验证规则\n- 测试全部通过\n",
        )
        .unwrap();
        std::fs::write(pack.join("resources").join("checklist.txt"), "ok").unwrap();

        // 2) 落盘进约定目录 + 自动加载对账（与 GUI 导入/启动扫描同一入口）。
        let lib = NativeSkillLibrary::new(&dir);
        let packs_dir = dir.join(".harness-memory").join("skills");
        let installed = index::install_skill_packs_from(&src, &packs_dir).unwrap();
        assert_eq!(installed.len(), 1);
        let report = index::sync_skill_packs(&*lib, &packs_dir).await.unwrap();
        assert_eq!(report.added, 1);

        // 3) 默认未启用：勾选前不得参与匹配（新安全语义）。
        assert!(
            lib.match_skills("发布新版本").await.unwrap().is_empty(),
            "自动加载的技能默认不得参与匹配"
        );

        // 4) 用户在面板勾选启用后，中文自然语言输入必须命中
        //（CJK n-gram 匹配，回归「占坑不拉」缺陷）。
        let id = format!("{}release-checklist", index::SKILL_PACK_ID_PREFIX);
        lib.set_skill_enabled(&id, true).await.unwrap();
        let matched = lib
            .match_skills("明天要发布新版本，帮我过一遍检查")
            .await
            .unwrap();
        assert!(!matched.is_empty(), "发布场景中文输入未命中任何技能");
        let sk = matched
            .iter()
            .find(|s| s.name.contains("发布检查清单"))
            .expect("应命中发布检查清单技能");
        assert_eq!(sk.version, "1.1");
        assert_eq!(
            sk.resource_files,
            vec!["resources/checklist.txt".to_string()]
        );

        // 5) AgentLoop 每回合注入模型上下文的即此渲染结果：
        // 触发边界与执行步骤、验证规则都必须在场。
        let instructions = render_skill_instructions(&matched).expect("命中技能应渲染出上下文指令");
        assert!(instructions.contains("发布检查清单"));
        assert!(instructions.contains("发布新版本"));
        assert!(instructions.contains("跑全量测试"));
        assert!(instructions.contains("测试全部通过"));

        // 6) 禁用 → 立即不再命中：GUI 开关经同一 SkillLibrary 全局同步。
        lib.set_skill_enabled(&id, false).await.unwrap();
        assert!(lib.match_skills("发布新版本").await.unwrap().is_empty());

        // 7) 重复同步（更新内容）：不产生副本，禁用状态不被静默打开。
        let rep2 = index::sync_skill_packs(&*lib, &packs_dir).await.unwrap();
        assert_eq!(rep2.added, 0);
        assert_eq!(rep2.updated, 1);
        let all = lib.list_skills().await.unwrap();
        assert_eq!(all.len(), 1, "重复导入不得创建副本");
        assert!(!all[0].enabled, "重新同步不得覆盖用户的禁用状态");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_context_keeps_errors_and_bounds_long_output() {
        let input = format!(
            "{}\nerror: expected failure\n{}",
            "first line\n".repeat(2_000),
            "last line\n".repeat(2_000)
        );
        let compacted = compress_tool_context(&input);
        assert!(compacted.chars().count() <= 16_000);
        assert!(compacted.contains("error: expected failure"));
        assert!(compacted.contains("工具输出已压缩"));
    }

    #[test]
    fn attachment_only_input_becomes_an_explicit_analysis_request() {
        let prompt = attachment_prompt(&UserInput {
            text: String::new(),
            attachments: vec![harness_core::Attachment {
                path: "screenshot.png".into(),
                mime: "image/png".into(),
            }],
        });
        assert!(prompt.contains("分析"));
        assert!(prompt.contains("图片"));
    }

    #[test]
    fn inline_image_data_urls_only_include_supported_images() {
        let dir = std::env::temp_dir().join(format!(
            "harness-attachment-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("image.png");
        std::fs::write(&image, [1_u8, 2, 3]).unwrap();
        let attachments = vec![harness_core::Attachment {
            path: image,
            mime: "image/png".into(),
        }];

        let (urls, notes) = inline_image_data_urls(&attachments, true);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].starts_with("data:image/png;base64,"));
        assert!(notes[0].contains("已作为图片内容发送"));

        let (urls, notes) = inline_image_data_urls(&attachments, false);
        assert!(urls.is_empty());
        assert!(notes[0].contains("不支持图片识别"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rebuilds_multi_turn_and_tool_context() {
        let events = vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "第一问".into(),
            },
            // 旧版只写 Thinking，因此也必须能恢复。
            SessionEvent::Thinking {
                id: 2,
                text: "先读取文件。".into(),
            },
            // DeepSeek may send reasoning_content in its own SSE frame before
            // the tool-call frame; 新版会再保存在 Assistant chunk，不能重复。
            SessionEvent::Assistant {
                id: 3,
                chunk: Chunk {
                    reasoning: Some("先读取文件。".into()),
                    ..Default::default()
                },
            },
            SessionEvent::Assistant {
                id: 4,
                chunk: Chunk {
                    text: Some("处理中".into()),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "fs".into(),
                        args: serde_json::json!({"op":"read","path":"a.txt"}),
                    }],
                    ..Default::default()
                },
            },
            SessionEvent::ToolResult {
                id: 5,
                result: ToolResult {
                    call_id: "c1".into(),
                    ok: true,
                    content: "内容".into(),
                    continuation_debt: 0,
                },
            },
            SessionEvent::TurnEnd { id: 6 },
        ];
        let messages = messages_from_events(&events);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[2].tool_calls[0].id, "c1");
        assert_eq!(messages[2].reasoning_content.as_deref(), Some("先读取文件。"));
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn sanitizes_unresponded_tool_calls_and_orphan_results() {
        // 模拟循环守卫/中断后的日志：c2 已宣告但从未执行，另有一条孤儿 tool 结果。
        let events = vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "q".into(),
            },
            SessionEvent::Assistant {
                id: 2,
                chunk: Chunk {
                    text: Some("执行中".into()),
                    tool_calls: vec![
                        ToolCall {
                            id: "c1".into(),
                            name: "shell".into(),
                            args: serde_json::json!({"command":"dir"}),
                        },
                        ToolCall {
                            id: "c2".into(),
                            name: "shell".into(),
                            args: serde_json::json!({"command":"dir"}),
                        },
                    ],
                    ..Default::default()
                },
            },
            SessionEvent::ToolResult {
                id: 3,
                result: ToolResult {
                    call_id: "c1".into(),
                    ok: true,
                    content: "ok".into(),
                    continuation_debt: 0,
                },
            },
            SessionEvent::ToolResult {
                id: 4,
                result: ToolResult {
                    call_id: "ghost".into(),
                    ok: true,
                    content: "孤儿结果".into(),
                    continuation_debt: 0,
                },
            },
            SessionEvent::TurnEnd { id: 5 },
        ];
        let messages = messages_from_events(&events);
        // system + user + assistant（仅保留有响应的 c1）+ tool(c1)，孤儿结果被剔除。
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[2].tool_calls.len(), 1);
        assert_eq!(messages[2].tool_calls[0].id, "c1");
        assert_eq!(messages[3].tool_call_id.as_deref(), Some("c1"));
    }

    #[test]
    fn context_budget_keeps_latest_turn_and_compacts_old_history() {
        let mut messages = vec![Message::system("system")];
        for index in 0..14 {
            messages.push(Message::user(format!("old-{index}-{}", "x".repeat(8_000))));
            messages.push(Message::assistant(format!("answer-{index}")));
        }
        messages.push(Message::user("LATEST-QUESTION"));
        let compacted = apply_context_budget(messages);
        assert!(compacted
            .iter()
            .any(|m| m.content.contains("较早会话已按上下文预算压缩")));
        assert!(compacted.iter().any(|m| m.content == "LATEST-QUESTION"));
        assert!(compacted.iter().map(message_chars).sum::<usize>() < 105_000);
    }

    #[test]
    fn repeat_guard_blocks_success_retries_but_allows_retry_after_a_write() {
        let mut guard = ToolRepeatGuard::default();
        let sig = "shell:{\"cmd\":\"status\"}";
        let success = ToolResult {
            call_id: "c1".into(),
            ok: true,
            content: "still running".into(),
            continuation_debt: 0,
        };
        // 成功读取后，原样调用不会获得新信息，下一次调用必须被拦截。
        guard.record_result(sig, &success);
        assert!(guard.should_block(sig));

        // 成功写入改变观察对象，允许重新运行同一验证命令。
        let write = ToolResult {
            call_id: "edit-1".into(),
            ok: true,
            content: "updated".into(),
            continuation_debt: 0,
        };
        guard.record_result("edit:{\"path\":\"src/app.rs\"}", &write);
        assert!(!guard.should_block(sig));

        // 失败调用允许一次定向重试；第二次失败后不再重试。
        let failed = ToolResult {
            call_id: "c2".into(),
            ok: false,
            content: "temporary failure".into(),
            continuation_debt: 0,
        };
        guard.record_result(sig, &failed);
        assert!(!guard.should_block(sig));
        guard.record_result(sig, &failed);
        assert!(guard.should_block(sig));
        assert_eq!(guard.note_recovery(sig), 1);

        // 不同签名的调用不受影响。
        assert!(!guard.should_block("shell:{\"cmd\":\"other\"}"));
    }

    #[test]
    fn atomic_step_gate_allows_only_one_search_per_model_response() {
        let mut gate = AtomicStepGate::default();
        assert!(gate.allows(true, "search:{\"pattern\":\"optimizing\"}"));
        assert!(!gate.allows(true, "search:{\"pattern\":\"loading\"}"));
        // 读取和写入不是定位泛扫；它们由跨步骤 ActionGate 判断是否符合阶段。
        assert!(gate.allows(true, "fs:{\"op\":\"read\"}"));
        assert!(gate.allows(false, "search:{\"pattern\":\"anything\"}"));
    }

    #[test]
    fn continuation_recovers_the_root_task_instead_of_reclassifying_the_short_command() {
        let root_report = DeliveryReport {
            outcome: DeliveryOutcome::Blocked,
            criteria: vec![harness_session::DeliveryCriterion {
                id: "item-1".into(),
                description: "迁移旧配置".into(),
                satisfied: true,
                evidence: vec!["migration test passed".into()],
            }],
            verification: vec!["migration test passed".into()],
            reason: Some("预算窗口结束".into()),
        };
        // 新版续跑回合虽然记录了用户的短句，但 DeliveryReport 仍对应根任务的
        // 验收项；下一次续跑必须继承这个最新报告，而不是退回首轮报告。
        let generic_resume_report = DeliveryReport {
            outcome: DeliveryOutcome::Blocked,
            criteria: vec![harness_session::DeliveryCriterion {
                id: "item-1".into(),
                description: "迁移旧配置".into(),
                satisfied: true,
                evidence: vec!["migration test passed again".into()],
            }],
            verification: vec!["migration test passed again".into()],
            reason: Some("下一阶段预算窗口结束".into()),
        };
        let events = vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "迁移配置中心\n- 迁移旧配置\n- 验证 CLI".into(),
            },
            SessionEvent::Delivery {
                id: 2,
                report: root_report,
            },
            SessionEvent::TurnEnd { id: 3 },
            SessionEvent::TurnStart {
                id: 4,
                input: "继续把第一阶段做完".into(),
            },
            SessionEvent::Delivery {
                id: 5,
                report: generic_resume_report,
            },
        ];

        let resumed = latest_resumable_task(&events).expect("should find root task");
        assert!(resumed.objective.starts_with("迁移配置中心"));
        assert_eq!(resumed.report.criteria[0].id, "item-1");
        assert_eq!(
            resumed.report.criteria[0].evidence,
            ["migration test passed again"]
        );
        assert!(resume_instruction(&resumed).contains("不要重新创建计划"));
    }
}
