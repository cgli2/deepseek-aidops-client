use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use harness_core::AppContext;

use crate::agent_loop::AgentLoop;
use crate::task::{SessionId, Task};

/// 会话句柄（用于结构化取消 + 等待完成）。
struct SessionHandle {
    token: CancellationToken,
    handle: JoinHandle<()>,
}

/// 多任务调度器（原 §7）：单 tokio 多线程运行时 + `JoinSet` + `CancellationToken` 层级取消。
pub struct Scheduler {
    rt: Handle,
    sessions: Arc<RwLock<HashMap<SessionId, SessionHandle>>>,
    cancel: CancellationToken,
}

impl Scheduler {
    pub fn new(rt: Handle) -> Arc<Self> {
        Arc::new(Self {
            rt,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            cancel: CancellationToken::new(),
        })
    }

    /// 扇出一个会话（`tokio::select!` 在任务与取消信号之间）。
    pub async fn spawn_session(&self, ctx: AppContext, task: Task) -> SessionId {
        let token = self.cancel.child_token();
        let id = task.session;
        let loop_ = AgentLoop::new();
        // 克隆一份给句柄（原 token 被 move 进 spawned 任务用于取消监听）。
        let token_for_handle = token.clone();
        let handle = self.rt.spawn(async move {
            tokio::select! {
                r = loop_.run_turn(&ctx, task.input) => { let _ = r; }
                _ = token.cancelled() => { /* 结构化取消：父取消 → 子取消 */ }
            }
        });
        self.sessions.write().unwrap().insert(
            id,
            SessionHandle {
                token: token_for_handle,
                handle,
            },
        );
        id
    }

    /// 等待单个会话结束（主循环据此判定回合完成、避免永久阻塞在 ctrl_c）。
    pub async fn wait_session(&self, id: &SessionId) {
        if let Some(h) = self.sessions.write().unwrap().remove(id) {
            let _ = h.handle.await;
        }
    }

    /// 取消单个会话。
    pub fn cancel_session(&self, id: &SessionId) {
        if let Some(h) = self.sessions.write().unwrap().get(id) {
            h.token.cancel();
        }
    }

    /// 取消全部（层级取消：父 → 所有子代理）。
    pub fn cancel_all(&self) {
        self.cancel.cancel();
    }
}
