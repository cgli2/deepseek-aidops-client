use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use futures::StreamExt;
use harness_capability::assets::{ChatTurn, ConversationMemory, Skill, SkillLibrary};
use harness_capability::compaction::Compaction;
use harness_capability::hook::{Hook, HookDecision, HookEvent, HookPayload};
use harness_core::{AppContext, error::Result, types::UserInput};
use harness_llm::{Chunk, LlmProvider, Message, Role, ToolCall, ToolResult, Usage};
use harness_session::{SessionEvent, SessionLog};
use harness_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use crate::events::{PreStep, TurnStopping};
use crate::execution::{
    ActionGate, ActionProposal, BudgetManager, Completion, CompletionJudge, DomainPolicy,
    ExecutionState, GateDecision, GeneralDomainPolicy, TaskContract,
};

/// Agent 循环 / Turn-Step 生命周期（原 §5.6）。
///
/// `Turn` = 0..n `Step`；`debt` 计数控制续跑；`agent/turn-stopping` 为唯一串行终止点。
pub struct AgentLoop;

/// 将循环定义为“相同调用连续产生相同结果”，而非仅仅相同命令。
/// 结果只保留哈希，避免控制状态复制工具原文。
#[derive(Default)]
struct ToolRepeatGuard {
    previous: Option<(String, u64)>,
    identical_outcomes: u8,
    recovery_attempts: HashMap<String, u8>,
    /// 同一调用（名称+参数）的累计执行次数：即使每次输出略有差异（扫描类命令
    /// 常见），超过阈值也判定为低价值重复。取证：同回合扫描脚本被跑 13 次。
    cumulative: HashMap<String, u8>,
}

/// 同回合内同一调用（名称+参数完全相同）的最大执行次数。
const MAX_SAME_CALLS_PER_TURN: u8 = 4;

impl ToolRepeatGuard {
    fn should_block(&self, signature: &str) -> bool {
        let consecutive_identical = self
            .previous
            .as_ref()
            .is_some_and(|(previous, _)| previous == signature)
            && self.identical_outcomes >= 2;
        let cumulative_exhausted = self
            .cumulative
            .get(signature)
            .is_some_and(|count| *count >= MAX_SAME_CALLS_PER_TURN);
        consecutive_identical || cumulative_exhausted
    }

    fn record_result(&mut self, signature: &str, result: &ToolResult) {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        result.ok.hash(&mut hasher);
        result.content.hash(&mut hasher);
        let fingerprint = hasher.finish();
        let changed_path =
            self.previous
                .as_ref()
                .is_none_or(|(previous_signature, previous_fingerprint)| {
                    previous_signature != signature || *previous_fingerprint != fingerprint
                });
        if changed_path {
            // 一次真正不同的观察意味着模型已改变调查路径；旧的恢复次数不再相关。
            self.recovery_attempts.clear();
        }
        self.identical_outcomes = match &self.previous {
            Some((previous_signature, previous_fingerprint))
                if previous_signature == signature && *previous_fingerprint == fingerprint =>
            {
                self.identical_outcomes.saturating_add(1)
            }
            _ => 1,
        };
        self.previous = Some((signature.to_string(), fingerprint));
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

        let input_text = input.text.clone();

        // 先把自然语言请求编译成通用执行契约，再由可替换的领域策略选择执行方式。
        // 没有注册领域策略时使用通用分类器，不把代码修复等场景写死在 Agent Loop。
        let contract = TaskContract::from_input(&input_text);
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
        let mut execution = ExecutionState::new(contract, strategy);

        // 从追加日志重建多轮上下文；不能每个 turn 都只发送当前一句，否则 GUI 看似能聊天，
        // 实际模型完全不记得上一轮以及之前的工具结果。
        let history = log.replay();
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
        let (compacted, matched_skills) = tokio::join!(
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

        let mut debt: usize = 1;
        // 跨步累积本轮助手最终文本，供回合结束时沉淀为 L0 记忆。
        let mut last_assistant = String::new();
        let attachment_context = render_attachment_context(&input.attachments);
        messages.push(Message::user(&input_text));
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
        // 技能注入点：只匹配启用的 SKILL.md 资产，并在本回合的系统上下文中
        // 提供可执行步骤与验收条件。禁用或删除后，SkillLibrary 不会返回它们，
        // 因而从下一回合起立即不再影响模型行为。匹配已在预处理阶段与压缩并行完成。
        if let Some(matched) = &matched_skills {
            if let Some(instructions) = render_skill_instructions(matched) {
                messages.insert(1, Message::system(&instructions));
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
        let mut convergence_notified = false;
        // 预算续期耗尽后只给一次最终收尾窗口（2 步）；窗口也用尽则强制停止。
        let mut final_window_armed = false;
        // 上游可能正常结束却没有正文/工具调用（例如网关截断、reasoning-only 帧）。
        // 这不是完成；允许有限恢复重试，避免把占位文本污染会话上下文。
        const MAX_EMPTY_RESPONSE_RETRIES: usize = 2;
        let mut empty_response_retries = 0usize;
        while debt > 0 {
            steps += 1;
            execution.steps = steps;
            debt -= 1;
            log.append(SessionEvent::StepStart {
                id: log.gen_id(),
                step: steps,
            });

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
            let mut s = llm.stream(apply_context_budget(pre_input));
            let mut assistant_text = String::new();
            let mut assistant_tools = Vec::new();
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
                // 思考链增量只写 Thinking 事件（UI「思考中」反馈），不进回复文本/模型上下文；
                // 仅当有文本或工具调用时写 Assistant 事件，避免空消息噪声。
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
                if chunk.text.is_some() || !chunk.tool_calls.is_empty() {
                    log.append(SessionEvent::Assistant {
                        id: log.gen_id(),
                        chunk: chunk.clone(),
                    });
                }
                if let Some(text) = &chunk.text {
                    assistant_text.push_str(text);
                }
                assistant_tools.extend(chunk.tool_calls.clone());
                // 工具调用并行化：门禁（循环守卫/行动门禁/访问策略/PreToolUse 钩子）
                // 串行执行以保全状态与顺序语义；通过门禁的调用收集后 join 并行分发。
                // 此前同一步的 N 个工具调用逐个 await，与系统提示词鼓励的并行调用相悖。
                let mut pending: Vec<(&ToolCall, String, ActionProposal)> = Vec::new();
                for tc in &chunk.tool_calls {
                    let sig = format!("{}:{}", tc.name, tc.args);
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
                                "[tool-loop guard] 工具 {} 相同参数的调用已重复多次（连续相同结果或累计达到 {MAX_SAME_CALLS_PER_TURN} 次上限）；本次未执行。请分析已有结果、换用不同参数/工具或直接基于现有证据收尾，不要原样重试。",
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
                                "[循环恢复] 工具 {} 的相同调用已连续两次产生相同结果，本次已拦截（恢复提示 {recovery}/{MAX_LOOP_RECOVERY_PROMPTS}）。任务尚未完成：先解释现有结果，再选择不同参数、不同工具或下一项验证；禁止原样重试。",
                                tc.name
                            ));
                        } else {
                            loop_recovery_exhausted = true;
                        }
                        continue;
                    }

                    // 通用行动门禁：每个工具动作必须关联验收目标。调用/时间预算是软检查点，
                    // 只触发进展诊断与续期，不会因为任务耗时较长而拒绝必要动作。
                    let proposal = ActionProposal::from_tool_call(tc, &execution.contract);
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
                messages.insert(
                    messages.len().saturating_sub(assistant_tools.len()),
                    Message::assistant_with_tools(assistant_text, assistant_tools),
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
                if empty_response_retries < MAX_EMPTY_RESPONSE_RETRIES {
                    empty_response_retries += 1;
                    debt += 1;
                    messages.push(Message::user(format!(
                        "[恢复请求] 上一次模型响应为空（finish_reason={reason}），没有生成正文或工具调用；这不代表任务完成。请基于现有上下文继续：若需要信息或执行操作，调用恰当工具；否则给出可验证的完整答复。不要只输出思考过程。自动重试第 {empty_response_retries}/{MAX_EMPTY_RESPONSE_RETRIES} 次。"
                    )));
                } else {
                    log.append(SessionEvent::Assistant {
                        id: log.gen_id(),
                        chunk: Chunk {
                            text: Some(format!(
                                "[error] 模型连续 {} 次返回空响应（最后 finish_reason={reason}）。请求未被视为完成；请检查模型/网关日志、输出 token 限制或切换模型后重试。",
                                MAX_EMPTY_RESPONSE_RETRIES + 1
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
                            "[收尾阶段] 预算续期已用完，请在接下来 6 步内完成交付：停止新的探索与扫描，把已完成的改动固化（必要的写入/构建验证），然后输出结构化总结：1) 已完成的交付物与验证结果；2) 未完成部分及原因；3) 建议的后续步骤。请先自评：若剩余工作能在窗口内完成，就集中精力做完；若判断无法完成，立即停止新的工具调用，直接输出上述总结，不要浪费收尾窗口。若确需继续，用户可基于当前进展另起指令接续。",
                        ));
                    }
                    None => {
                        // 收尾窗口也已耗尽：暂停回合并结构化交接，不粗暴丢弃进展；
                        // 上下文保留在会话里，用户可另起指令从断点接续。
                        log.append(SessionEvent::Assistant {
                            id: log.gen_id(),
                            chunk: Chunk {
                                text: Some(
                                    "[预算耗尽通知] 本回合的步数与续期预算已全部用尽，执行已暂停以避免低效空转。\n以上过程保留了全部进展与上下文：可另起一条指令（如“继续完成上述未完成部分”）从断点接续，无需从头开始；也可在设置中调高步数上限后重试。".into(),
                                ),
                                ..Default::default()
                            },
                        });
                        debt = 0;
                        hard_stop = true;
                    }
                }
            } else {
                match CompletionJudge::evaluate(&execution, &budget, step_had_tools) {
                    Completion::Converge(reason) if !convergence_notified => {
                        convergence_notified = true;
                        messages.push(Message::user(&format!("[系统提示] {reason}")));
                    }
                    Completion::Complete => debt = 0,
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

        log.append(SessionEvent::TurnEnd { id: log.gen_id() });
        Ok(())
    }
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
    for event in events {
        match event {
            SessionEvent::TurnStart { input, .. } => messages.push(Message::user(input)),
            SessionEvent::Assistant { chunk, .. } => {
                // SSE 流式下每个文本增量都是一条 Assistant 事件；重建上下文时必须把相邻的
                // 纯文本 assistant 分片合并为一条消息，否则会向模型发送大量连续 assistant
                // 消息（OpenAI 兼容协议不接受，多轮对话直接报错）。
                let text = chunk.text.clone().unwrap_or_default();
                if chunk.tool_calls.is_empty() {
                    if let Some(last) = messages.last_mut() {
                        if last.role == Role::Assistant && last.tool_calls.is_empty() {
                            last.content.push_str(&text);
                            continue;
                        }
                    }
                }
                messages.push(Message::assistant_with_tools(
                    text,
                    chunk.tool_calls.clone(),
                ));
            }
            SessionEvent::ToolResult { result, .. } => messages.push(Message::tool(
                &result.call_id,
                &compress_tool_context(&result.content),
            )),
            // Thinking / Step* / PlanUpdate 仅 UI 展示，不进入模型上下文。
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
fn render_attachment_context(attachments: &[harness_core::Attachment]) -> String {
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
    out
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
- 多步任务先调用 plan 发布结构化计划；在里程碑节点批量更新状态（doing/done），不必每完成一小步就更新一次。\n\
- 相互独立的多个操作尽量在同一次回复里作为多个工具调用一起发出，减少往返。\n\
- 独立且耗时的子任务用 delegate 委托子代理，主线程只整合结果。\n\
- 回合结束前给出简洁、可读的最终总结。\n\
\n\
## 效率与收敛（必须遵守）\n\
- 最小路径优先：先用 search 定位与目标直接相关的最小文件集，再有针对性地读取；禁止全仓库递归扫描、批量试探性搜索与自造临时扫描脚本。\n\
- 读取大文件用 fs 的 start_line/end_line 按区间读取；严禁编写临时脚本（python/ps1 等）截取或提取文件内容。\n\
- 纯界面/配置类任务：search 定位到少数目标文件后集中批量编辑，全部改完再做一次构建验证，不要每改一处就编译一次。\n\
- 已读过的文件不要重复读取；同一命令/同一参数的调用最多重试有限次数，输出无新信息时立即换路或基于现有证据推进。\n\
- 探索（读取/搜索/列目录）应尽快收敛到写操作与验证；交付目标达成后立即停止，不做重复确认与打磨。\n\
- 步数预算有限且续期次数封顶：收到检查点/收尾提示时必须服从，基于现有证据交付总结，不要继续扩张探索。\n\
- shell 命令已在工作区根目录执行：不要重复 cd 到工作区，直接用相对路径。\n\
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
        assert_eq!(sk.resource_files, vec!["resources/checklist.txt".to_string()]);

        // 5) AgentLoop 每回合注入模型上下文的即此渲染结果：
        // 触发边界与执行步骤、验证规则都必须在场。
        let instructions =
            render_skill_instructions(&matched).expect("命中技能应渲染出上下文指令");
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
    fn rebuilds_multi_turn_and_tool_context() {
        let events = vec![
            SessionEvent::TurnStart {
                id: 1,
                input: "第一问".into(),
            },
            SessionEvent::Assistant {
                id: 2,
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
                id: 3,
                result: ToolResult {
                    call_id: "c1".into(),
                    ok: true,
                    content: "内容".into(),
                    continuation_debt: 0,
                },
            },
            SessionEvent::TurnEnd { id: 4 },
        ];
        let messages = messages_from_events(&events);
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[1].role, Role::User);
        assert_eq!(messages[2].tool_calls[0].id, "c1");
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
        assert!(
            compacted
                .iter()
                .any(|m| m.content.contains("较早会话已按上下文预算压缩"))
        );
        assert!(compacted.iter().any(|m| m.content == "LATEST-QUESTION"));
        assert!(compacted.iter().map(message_chars).sum::<usize>() < 105_000);
    }

    #[test]
    fn repeat_guard_blocks_identical_outcomes_and_cumulative_spin() {
        let mut guard = ToolRepeatGuard::default();
        let sig = "shell:{\"cmd\":\"status\"}";
        let first = ToolResult {
            call_id: "c1".into(),
            ok: true,
            content: "still running".into(),
            continuation_debt: 0,
        };
        let changed = ToolResult {
            content: "finished".into(),
            ..first.clone()
        };
        // 第 1 次执行：不拦截。
        guard.record_result(sig, &first);
        assert!(!guard.should_block(sig));
        // 第 2 次执行（输出变化）：连续规则未触发。
        guard.record_result(sig, &changed);
        assert!(!guard.should_block(sig));
        // 第 3 次执行（与上次结果相同）：连续相同结果规则拦截。
        guard.record_result(sig, &changed);
        assert!(guard.should_block(sig));
        assert_eq!(guard.note_recovery(sig), 1);
        // 第 4 次执行（输出又变了，连续规则重置）：但累计次数达到上限，
        // 累计规则接管拦截——拦截输出微差的空转重复（取证：扫描脚本跑 13 次）。
        let new_observation = ToolResult {
            content: "state changed".into(),
            ..changed
        };
        guard.record_result(sig, &new_observation);
        assert!(guard.should_block(sig));
        assert_eq!(guard.note_recovery(sig), 1);
        // 不同签名的调用不受影响。
        guard.record_result("shell:{\"cmd\":\"other\"}", &first);
        assert!(!guard.should_block("shell:{\"cmd\":\"other\"}"));
    }
}
