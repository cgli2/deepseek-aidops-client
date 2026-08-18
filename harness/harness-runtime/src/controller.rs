//! 会话控制器：把 UI 的用户输入驱动成后台 agent turn。
//!
//! GUI 是事件总线纯消费者（只渲染 `SessionLog`），但需要"反向通道"把用户在输入框敲的字变成一次
//! `AgentLoop::run_turn`。`SessionController` 就是这个反向通道：持有 `AppContext` + `SessionLog`
//! + tokio `Handle`，`submit(text)` 在后台串行跑 turn（带忙标记供 UI 禁用「发送」）。
//!
//! 回合串行化：用 `tokio::sync::Mutex` 保证同一时刻只有一个 turn 在跑，避免用户连点导致
//! 多个回合穿插写入同一条 `SessionLog`。`busy` 标记供 UI 轮询以禁用输入。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::FutureExt;
use harness_core::types::UserInput;
use harness_core::ui_input::UiInputSink;
use harness_core::AppContext;
use tokio::runtime::Handle;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::AgentLoop;

/// 驱动会话回合的控制器（UI → 后台 turn 的反向通道）。
#[derive(Clone)]
pub struct SessionController {
    inner: Arc<Inner>,
}

struct Inner {
    ctx: AppContext,
    rt: Handle,
    turn_lock: Arc<Mutex<()>>,
    busy: Arc<AtomicBool>,
    cancellation: Arc<std::sync::Mutex<Option<CancellationToken>>>,
}

impl SessionController {
    pub fn new(ctx: AppContext, rt: Handle) -> Self {
        Self {
            inner: Arc::new(Inner {
                ctx,
                rt,
                turn_lock: Arc::new(Mutex::new(())),
                busy: Arc::new(AtomicBool::new(false)),
                cancellation: Arc::new(std::sync::Mutex::new(None)),
            }),
        }
    }
}

impl UiInputSink for SessionController {
    fn submit(&self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        if self
            .inner
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .is_err()
        {
            return;
        }
        let inner = self.inner.clone();
        // 先把 Handle 克隆出来（spawn 借用了它），再整体 move `inner` 进异步任务，避免借用冲突。
        let rt = inner.rt.clone();
        rt.spawn(async move {
            // 串行化：同一时刻只跑一个回合。
            let _guard = inner.turn_lock.lock().await;
            let loop_ = AgentLoop::new();
            let cancellation = CancellationToken::new();
            if let Ok(mut active) = inner.cancellation.lock() {
                *active = Some(cancellation.clone());
            }
            let input = UserInput {
                text,
                attachments: vec![],
            };
            // turn 级 watchdog：超时强制中止并写错误事件，保证 busy 复位
            //（「30 分钟无响应」假死的兑底防线；流层另有 idle 超时）。
            let turn_timeout_secs: u64 = std::env::var("HARNESS_TURN_TIMEOUT_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1800);
            let outcome = std::panic::AssertUnwindSafe(async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(turn_timeout_secs),
                    loop_.run_turn_cancellable(&inner.ctx, input, cancellation),
                )
                .await
                .map_err(|_| {
                    harness_core::error::Error::Runtime(format!(
                        "回合超过 {turn_timeout_secs} 秒未完成，已强制中止以避免无限等待"
                    ))
                })?
            })
            .catch_unwind()
            .await;
            let failure = match outcome {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(error.to_string()),
                Err(_) => Some("后台回合发生异常，已自动恢复界面".to_string()),
            };
            if let Some(error) = failure {
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
            inner.busy.store(false, Ordering::Release);
        });
    }

    fn busy(&self) -> bool {
        self.inner.busy.load(Ordering::Relaxed)
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
        // 新建对话：换新会话文件，旧会话原样保留为历史可回看（不再截断复用）。
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
        // 1) 工具根切换：shell/fs/editor 共享 Workspace，下一次操作即落在新工作区。
        if let Some(ws) = self.inner.ctx.try_get::<harness_core::Workspace>() {
            ws.set_root(path.to_path_buf());
        }
        // 2) 会话历史切换：同一 SessionLog 实例重载新项目目录的最近会话。
        let log = self.inner.ctx.get::<harness_session::SessionLog>();
        log.switch_dir(path.join(".harness").join("sessions"));
    }
}
