//! 会话控制器：每条会话独立 FIFO、独立执行上下文；不同会话可并行运行。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::FutureExt;
use harness_core::types::UserInput;
use harness_core::ui_input::{QueuedInput, UiInputSink};
use harness_core::{AppContext, Registration};
use harness_session::{SessionId, SessionLog};
use harness_tool::{PlanTool, ToolRegistry};
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::AgentLoop;
use crate::council::{CouncilOrchestrator, COUNCIL_PREFIX};

#[derive(Clone)]
pub struct SessionController {
    inner: Arc<Inner>,
}

struct Inner {
    ctx: AppContext,
    rt: Handle,
    queues: std::sync::Mutex<HashMap<SessionId, TurnQueue>>,
    next_queue_id: AtomicU64,
}

/// 每条会话最多一个消费者；不同会话各自有消费者，因此可并行。
struct TurnQueue {
    pending: VecDeque<QueuedInput>,
    running: bool,
    cancellation: Option<CancellationToken>,
}

/// 会话 fork 的服务覆盖必须和执行生命周期一致；保存 Registration 避免其提前 Drop
/// 把固定日志/会话级 PlanTool 从子上下文移除。
struct SessionScope {
    ctx: AppContext,
    _registrations: Vec<Registration>,
}

impl SessionController {
    pub fn new(ctx: AppContext, rt: Handle) -> Self {
        Self {
            inner: Arc::new(Inner {
                ctx,
                rt,
                queues: std::sync::Mutex::new(HashMap::new()),
                next_queue_id: AtomicU64::new(1),
            }),
        }
    }

    /// UI 日志是可切换视图；执行前 pin 成固定句柄，并在 fork 的上下文内覆盖。
    fn current_session_context(&self) -> (SessionId, SessionScope) {
        let log = self.inner.ctx.get::<SessionLog>().pin();
        let id = log.id();
        let child = self.inner.ctx.fork();
        let log_registration = child.provide(log.clone());
        let tools = self
            .inner
            .ctx
            .get::<ToolRegistry>()
            .snapshot_excluding(&["plan"]);
        tools.register(PlanTool::new(log));
        let tools_registration = child.provide(tools);
        (
            id,
            SessionScope {
                ctx: child,
                _registrations: vec![log_registration, tools_registration],
            },
        )
    }

    fn current_id(&self) -> SessionId {
        self.inner.ctx.get::<SessionLog>().id()
    }
    fn any_busy(&self) -> bool {
        self.inner
            .queues
            .lock()
            .map(|q| q.values().any(|v| v.running))
            .unwrap_or(false)
    }
}

impl UiInputSink for SessionController {
    fn submit(&self, text: String) {
        self.submit_with_attachments(text, vec![]);
    }

    fn submit_with_attachments(&self, text: String, attachments: Vec<harness_core::Attachment>) {
        // 附件本身就是有效输入：输入框允许只粘贴文件/截图后直接发送，不能因没有
        // 额外文字而在控制器入口被静默丢弃。
        if text.trim().is_empty() && attachments.is_empty() {
            return;
        }
        let (id, session_ctx) = self.current_session_context();
        let should_start = match self.inner.queues.lock() {
            Ok(mut queues) => {
                let queue = queues.entry(id).or_insert_with(|| TurnQueue {
                    pending: VecDeque::new(),
                    running: false,
                    cancellation: None,
                });
                queue.pending.push_back(QueuedInput {
                    id: self.inner.next_queue_id.fetch_add(1, Ordering::Relaxed),
                    text,
                    attachments,
                });
                if queue.running {
                    false
                } else {
                    queue.running = true;
                    true
                }
            }
            Err(_) => return,
        };
        if should_start {
            let inner = self.inner.clone();
            let rt = inner.rt.clone();
            rt.spawn(async move { run_turn_queue(inner, id, session_ctx).await });
        }
    }

    fn busy(&self) -> bool {
        self.inner
            .queues
            .lock()
            .ok()
            .and_then(|q| q.get(&self.current_id()).map(|v| v.running))
            .unwrap_or(false)
    }
    fn any_busy(&self) -> bool {
        self.any_busy()
    }
    fn queued_count(&self) -> usize {
        self.inner
            .queues
            .lock()
            .ok()
            .and_then(|q| q.get(&self.current_id()).map(|v| v.pending.len()))
            .unwrap_or(0)
    }
    fn queued_inputs(&self) -> Vec<QueuedInput> {
        self.inner
            .queues
            .lock()
            .ok()
            .and_then(|q| {
                q.get(&self.current_id())
                    .map(|v| v.pending.iter().cloned().collect())
            })
            .unwrap_or_default()
    }
    fn remove_queued(&self, item_id: u64) -> bool {
        let Ok(mut queues) = self.inner.queues.lock() else {
            return false;
        };
        let Some(queue) = queues.get_mut(&self.current_id()) else {
            return false;
        };
        let Some(position) = queue.pending.iter().position(|item| item.id == item_id) else {
            return false;
        };
        queue.pending.remove(position);
        true
    }
    fn cancel(&self) {
        if let Ok(queues) = self.inner.queues.lock() {
            if let Some(Some(token)) = queues
                .get(&self.current_id())
                .map(|q| q.cancellation.as_ref())
            {
                token.cancel();
            }
        }
    }
    fn new_session(&self) {
        let log = self.inner.ctx.get::<SessionLog>();
        if let Some(ws) = self.inner.ctx.try_get::<harness_core::Workspace>() {
            log.fresh(ws.root().join(".harness").join("sessions"));
        } else {
            log.clear();
        }
    }
    fn set_permission(&self, mode: String) {
        if let Some(policy) = self.inner.ctx.try_get::<harness_core::AccessPolicy>() {
            policy.set(mode);
        }
    }
    fn switch_workspace(&self, path: &std::path::Path) {
        // 工作区是工具的共享资源；运行中仍不可换项目，避免文件工具跨根。
        if self.any_busy() {
            return;
        }
        if let Some(ws) = self.inner.ctx.try_get::<harness_core::Workspace>() {
            ws.set_root(path.to_path_buf());
        }
        self.inner
            .ctx
            .get::<SessionLog>()
            .switch_dir(path.join(".harness").join("sessions"));
    }
}

async fn run_turn_queue(inner: Arc<Inner>, id: SessionId, scope: SessionScope) {
    let ctx = &scope.ctx;
    loop {
        let input = match inner.queues.lock() {
            Ok(mut queues) => match queues.get_mut(&id).and_then(|q| q.pending.pop_front()) {
                Some(item) => item,
                None => {
                    queues.remove(&id);
                    break;
                }
            },
            Err(_) => break,
        };
        let cancellation = CancellationToken::new();
        if let Ok(mut queues) = inner.queues.lock() {
            if let Some(queue) = queues.get_mut(&id) {
                queue.cancellation = Some(cancellation.clone());
            }
        }
        let timeout_secs = std::env::var("HARNESS_TURN_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1800);
        let is_council = input.text.starts_with(COUNCIL_PREFIX);
        let clean_text = input
            .text
            .strip_prefix(COUNCIL_PREFIX)
            .unwrap_or(&input.text)
            .to_string();
        let attachments = input.attachments;
        let outcome = std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
                if is_council {
                    CouncilOrchestrator::default()
                        .run(
                            &ctx,
                            format!("{clean_text}{}", attachment_note(&attachments)),
                            cancellation,
                        )
                        .await
                } else {
                    AgentLoop::new()
                        .run_turn_cancellable(
                            &ctx,
                            UserInput {
                                text: clean_text,
                                attachments,
                            },
                            cancellation,
                        )
                        .await
                }
            })
            .await
            .map_err(|_| {
                harness_core::error::Error::Runtime(format!(
                    "回合超过 {timeout_secs} 秒未完成，已强制中止以避免无限等待"
                ))
            })?
        })
        .catch_unwind()
        .await;
        if let Some(error) = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e.to_string()),
            Err(_) => Some("后台回合发生异常，已自动恢复界面".into()),
        } {
            let log = ctx.get::<SessionLog>();
            log.append(harness_session::SessionEvent::Assistant {
                id: log.gen_id(),
                chunk: harness_llm::Chunk {
                    text: Some(format!("[error] {error}")),
                    ..Default::default()
                },
            });
            log.append(harness_session::SessionEvent::TurnEnd { id: log.gen_id() });
        }
        if let Ok(mut queues) = inner.queues.lock() {
            if let Some(queue) = queues.get_mut(&id) {
                queue.cancellation = None;
            }
        }
    }
}

fn attachment_note(attachments: &[harness_core::Attachment]) -> String {
    if attachments.is_empty() {
        return String::new();
    }
    let files = attachments
        .iter()
        .map(|a| {
            format!(
                "{}（{}，{}）",
                a.path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("附件"),
                a.mime,
                a.path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("；");
    format!("\n\n[用户附件，必须作为任务输入条件处理：{files}]")
}
