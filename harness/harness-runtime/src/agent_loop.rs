use std::sync::Arc;

use futures::StreamExt;
use harness_capability::assets::{ChatTurn, ConversationMemory};
use harness_capability::hook::{Hook, HookDecision, HookEvent, HookPayload};
use harness_core::event::Waterfall;
use harness_core::{error::Result, types::UserInput, AppContext};
use harness_llm::{Chunk, LlmProvider, Message, Role, ToolResult, Usage};
use harness_session::{SessionEvent, SessionLog};
use harness_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use crate::events::{PreStep, TurnStopping};

/// Agent 循环 / Turn-Step 生命周期（原 §5.6）。
///
/// `Turn` = 0..n `Step`；`debt` 计数控制续跑；`agent/turn-stopping` 为唯一串行终止点。
pub struct AgentLoop;

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

        // 从追加日志重建多轮上下文；不能每个 turn 都只发送当前一句，否则 GUI 看似能聊天，
        // 实际模型完全不记得上一轮以及之前的工具结果。
        let mut messages = messages_from_events(&log.replay());

        log.append(SessionEvent::TurnStart {
            id: log.gen_id(),
            input: input.text.clone(),
        });

        // 记忆自动沉淀（L0 工作记忆）：每个用户回合写入对话记忆（无后端则落本地文件）。
        // 失败容忍——记忆写入不应影响正常对话流程。
        if let Some(conv) = ctx.try_get::<dyn ConversationMemory>() {
            let _ = conv
                .record_turn(ChatTurn {
                    session_id: log.id().to_string(),
                    role: "user".into(),
                    content: input.text.clone(),
                    ts: String::new(),
                })
                .await;
        }

        let mut debt: usize = 1;
        // 跨步累积本轮助手最终文本，供回合结束时沉淀为 L0 记忆。
        let mut last_assistant = String::new();
        messages.push(Message::user(&input.text));
        let max_steps = max_steps_limit();
        let mut steps = 0usize;
        // 收尾宽限：撞上限后不立刻报错，再给模型一步「只总结不调工具」的机会。
        let mut wrapping_up = false;
        // 重复调用检测：完全相同的工具调用「连续」出现 3 次才判定为死循环；
        // 累计计数会误杀正常任务中隔多步重跑同一命令的场景。
        let mut last_sig: Option<String> = None;
        let mut consec = 0u32;
        // 硬终止标记（取消/流错误/循环守卫）：阻止步末的 debt 记账复活回合，
        // 否则带着「已宣告未执行」的 tool_call 续跑会直接 400。
        let mut hard_stop = false;
        while debt > 0 {
            steps += 1;
            if steps > max_steps {
                if !wrapping_up {
                    wrapping_up = true;
                    // 仅进入本地上下文，不写日志：下回合重建不依赖这条指令。
                    messages.push(Message::user(&format!(
                        "[系统提示] 本回合已达到最大步数（{max_steps}）。请立即停止调用任何工具，直接给出已完成工作的简洁总结。"
                    )));
                } else {
                    log.append(SessionEvent::Assistant {
                        id: log.gen_id(),
                        chunk: Chunk {
                            text: Some(format!(
                                "[error] 回合超过 {max_steps} 步且收尾后仍在调用工具，已强制停止（可用 HARNESS_MAX_STEPS 调整上限）"
                            )),
                            ..Default::default()
                        },
                    });
                    break;
                }
            }
            debt -= 1;
            log.append(SessionEvent::StepStart {
                id: log.gen_id(),
                step: steps,
            });

            // 瀑布前处理：可重写/拒绝消息；空链返回输入本身（终态恒等）。
            let chain: Vec<Arc<dyn Waterfall<PreStep>>> = vec![];
            let pre = bus.waterfall(
                PreStep {
                    input: messages.clone(),
                },
                &chain,
            );
            // PreStep 包含不断增长的完整上下文，持久化会造成 O(n²) 日志膨胀。
            // 真正的会话重建只依赖 TurnStart/Assistant/ToolResult，因此不写入 SessionLog。

            let mut s = llm.stream(pre.input);
            let mut assistant_text = String::new();
            let mut assistant_tools = Vec::new();
            let mut step_had_tools = false;
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
                for tc in &chunk.tool_calls {
                    // 死循环防护：同名同参调用连续 3 次说明模型在原地打转，
                    // 继续执行只会烧光步数预算，提前终止并给出可读原因。
                    let sig = format!("{}:{}", tc.name, tc.args);
                    consec = if last_sig.as_deref() == Some(sig.as_str()) {
                        consec + 1
                    } else {
                        1
                    };
                    last_sig = Some(sig);
                    if consec >= 3 {
                        log.append(SessionEvent::Assistant {
                            id: log.gen_id(),
                            chunk: Chunk {
                                text: Some(format!(
                                    "[error] 检测到连续 3 次相同的工具调用（{}），判定为循环，已停止。请换一种方式描述任务或检查结果是否符合预期。",
                                    tc.name
                                )),
                                ..Default::default()
                            },
                        });
                        debt = 0;
                        hard_stop = true;
                        break;
                    }
                    log.append(SessionEvent::ToolCall {
                        id: log.gen_id(),
                        call: tc.clone(),
                    });

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
                        messages.push(Message::tool(tc.id.clone(), blocked.content.clone()));
                        step_had_tools = true;
                        continue;
                    }

                    let res = match tools.dispatch(tc).await {
                        Ok(result) => result,
                        Err(error) => ToolResult {
                            call_id: tc.id.clone(),
                            ok: false,
                            content: format!("tool execution failed: {error}"),
                            continuation_debt: 0,
                        },
                    };
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
                    messages.push(Message::tool(tc.id.clone(), res.content.clone()));
                    step_had_tools = true;
                }
            }
            last_assistant = assistant_text.clone();
            // 本步用量落盘：Usage 事件不进模型上下文、不影响多轮重建，
            // 仅用于会话级成本计量（usage_total）。
            if step_usage.total_tokens > 0 {
                log.append(SessionEvent::Usage {
                    id: log.gen_id(),
                    usage: step_usage,
                });
            }
            messages.insert(
                messages.len().saturating_sub(assistant_tools.len()),
                Message::assistant_with_tools(assistant_text, assistant_tools),
            );
            // 续跑记账：本步无论并行多少个工具调用只续跑一次。旧的按调用 +1
            // 会让 N 个并行调用触发 N 次额外模型往返，步数预算被成倍消耗。
            // 硬终止（取消/错误/循环守卫）时禁止复活：此时可能有「已宣告未执行」的
            // tool_call 缺对应 tool 消息，续跑必 400。
            if step_had_tools && !hard_stop {
                debt += 1;
            }
            log.append(SessionEvent::StepEnd {
                id: log.gen_id(),
                step: steps,
            });

            // 收尾步无论模型是否仍调工具都强制结束（宽限只给一次）。
            if wrapping_up {
                break;
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
            SessionEvent::ToolResult { result, .. } => {
                messages.push(Message::tool(&result.call_id, &result.content))
            }
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
    messages
}

/// 结构化系统提示词：语言跟随、工具契约、长周期工作流、安全边界。
const SYSTEM_PROMPT: &str = "You are a reliable desktop assistant and coding agent.\n\
\n\
## 语言与格式\n\
- 始终用与用户最新消息相同的语言回复（中文提问用中文答）。\n\
- 结论先行；需要时用简洁的 markdown 列表展开步骤，不写冗长铺垫。\n\
\n\
## 工具契约\n\
- 只允许使用提供给你的工具：fs / edit / shell / plan / delegate。\n\
- 严禁在正文里输出任何形式的工具调用标记（DSML、XML invoke、tool_calls 文本等）；调用工具必须走 function calling 通道。\n\
- 问候、提问、普通对话直接回答，不使用工具。\n\
\n\
## 复杂任务工作流\n\
- 多步任务先调用 plan 发布结构化计划；在里程碑节点批量更新状态（doing/done），不必每完成一小步就更新一次。\n\
- 相互独立的多个操作尽量在同一次回复里作为多个工具调用一起发出，减少往返。\n\
- 独立且耗时的子任务用 delegate 委托子代理，主线程只整合结果。\n\
- 回合结束前给出简洁、可读的最终总结。\n\
\n\
## 安全\n\
- 仅当用户明确要求检查/修改/构建/测试/操作工作区时才使用文件系统或 shell 工具。\n\
- 永不搜索或泄露 API key、凭据、token 等秘密。";

/// 步上限（env `HARNESS_MAX_STEPS` 覆盖，默认 128）：真实编码任务单回合常需数十次
/// 工具往返；失控风险由 turn watchdog 与 SSE idle 超时兜底，不靠小上限硬防。
fn max_steps_limit() -> usize {
    std::env::var("HARNESS_MAX_STEPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(128)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_llm::ToolCall;

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
}
