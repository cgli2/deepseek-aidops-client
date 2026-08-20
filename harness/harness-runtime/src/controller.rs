//! 会话控制器：把 UI 输入变成按 FIFO 串行执行的后台 agent turn。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::FutureExt;
use harness_core::types::UserInput;
use harness_core::ui_input::{QueuedInput, UiInputSink};
use harness_core::AppContext;
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::AgentLoop;

#[derive(Clone)]
pub struct SessionController {
    inner: Arc<Inner>,
}

struct Inner {
    ctx: AppContext,
    rt: Handle,
    queue: std::sync::Mutex<TurnQueue>,
    next_queue_id: AtomicU64,
    cancellation: std::sync::Mutex<Option<CancellationToken>>,
}

/// 单消费者 FIFO 队列。`running` 与 `pending` 共用同一把锁，避免消费者退出与
/// 新输入入队交错时遗留无人执行的任务。
struct TurnQueue {
    pending: VecDeque<QueuedInput>,
    running: bool,
}

impl SessionController {
    pub fn new(ctx: AppContext, rt: Handle) -> Self {
        Self {
            inner: Arc::new(Inner {
                ctx,
                rt,
                queue: std::sync::Mutex::new(TurnQueue {
                    pending: VecDeque::new(),
                    running: false,
                }),
                next_queue_id: AtomicU64::new(1),
                cancellation: std::sync::Mutex::new(None),
            }),
        }
    }
}

impl UiInputSink for SessionController {
    fn submit(&self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let should_start = match self.inner.queue.lock() {
            Ok(mut queue) => {
                queue.pending.push_back(QueuedInput {
                    id: self.inner.next_queue_id.fetch_add(1, Ordering::Relaxed),
                    text,
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
            rt.spawn(async move { run_turn_queue(inner).await });
        }
    }

    fn busy(&self) -> bool {
        self.inner.queue.lock().map(|queue| queue.running).unwrap_or(false)
    }

    fn queued_count(&self) -> usize {
        self.inner
            .queue
            .lock()
            .map(|queue| queue.pending.len())
            .unwrap_or(0)
    }

    fn queued_inputs(&self) -> Vec<QueuedInput> {
        self.inner
            .queue
            .lock()
            .map(|queue| queue.pending.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn remove_queued(&self, id: u64) -> bool {
        let Ok(mut queue) = self.inner.queue.lock() else {
            return false;
        };
        let Some(position) = queue.pending.iter().position(|item| item.id == id) else {
            return false;
        };
        queue.pending.remove(position);
        true
    }

    fn cancel(&self) {
        if let Ok(active) = self.inner.cancellation.lock() {
            if let Some(token) = active.as_ref() {
                token.cancel();
            }
        }
    }

    fn new_session(&self) {
        if self.busy() {
            return;
        }
        let log = self.inner.ctx.get::<harness_session::SessionLog>();
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
        if self.busy() {
            return;
        }
        if let Some(ws) = self.inner.ctx.try_get::<harness_core::Workspace>() {
            ws.set_root(path.to_path_buf());
        }
        let log = self.inner.ctx.get::<harness_session::SessionLog>();
        log.switch_dir(path.join(".harness").join("sessions"));
    }
}

async fn run_turn_queue(inner: Arc<Inner>) {
    loop {
        let text = match inner.queue.lock() {
            Ok(mut queue) => match queue.pending.pop_front() {
                Some(item) => item.text,
                None => {
                    queue.running = false;
                    break;
                }
            },
            Err(_) => break,
        };

        let loop_ = AgentLoop::new();
        let cancellation = CancellationToken::new();
        if let Ok(mut active) = inner.cancellation.lock() {
            *active = Some(cancellation.clone());
        }
        let turn_timeout_secs: u64 = std::env::var("HARNESS_TURN_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1800);
        let outcome = std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(
                std::time::Duration::from_secs(turn_timeout_secs),
                loop_.run_turn_cancellable(
                    &inner.ctx,
                    UserInput {
                        text,
                        attachments: vec![],
                    },
                    cancellation,
                ),
            )
            .await
            .map_err(|_| harness_core::error::Error::Runtime(format!(
                "回合超过 {turn_timeout_secs} 秒未完成，已强制中止以避免无限等待"
            )))?
        })
        .catch_unwind()
        .await;
        if let Some(error) = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(_) => Some("后台回合发生异常，已自动恢复界面".to_string()),
        } {
            let log = inner.ctx.get::<harness_session::SessionLog>();
            log.append(harness_session::SessionEvent::Assistant {
                id: log.gen_id(),
                chunk: harness_llm::Chunk {
                    text: Some(format!("[error] {error}")),
                    ..Default::default()
                },
            });
            log.append(harness_session::SessionEvent::TurnEnd { id: log.gen_id() });
        }
        if let Ok(mut active) = inner.cancellation.lock() {
            *active = None;
        }
    }
}
