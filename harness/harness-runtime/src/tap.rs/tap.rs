//! Event Tap：旁路非阻塞事件采集（§3/§6）。
//!
//! 硬约束（§18）：
//! - 主线程 P99 < 1ms：只做 try_send，绝不 await / 锁 / I/O；
//! - 失败开放：队列满即丢弃并计数（发 TapOverflow 健康事件），绝不反压 Agent；
//! - Normal 事件允许受控丢弃，Critical 由 writer 侧保证持久化。

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;

use super::event::AgentEventEnvelope;

/// 有界队列默认容量。积压超限时先丢 Normal、发健康事件，不阻塞主流程。
pub const DEFAULT_TAP_CAPACITY: usize = 4096;

/// Tap 侧统计（原子计数，可无锁读取，供健康面板与 Telemetry 上报）。
#[derive(Debug, Default)]
pub struct TapStats {
    /// 成功入队的事件数。
    pub sent: AtomicU64,
    /// 因队列满被丢弃的事件数。
    pub dropped: AtomicU64,
}

impl TapStats {
    pub fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// 非阻塞事件探针。克隆廉价（Arc + mpsc Sender），可分发到各采集点。
#[derive(Clone)]
pub struct EventTap {
    tx: mpsc::Sender<AgentEventEnvelope>,
    stats: Arc<TapStats>,
}

impl EventTap {
    /// 创建 tap 与接收端。接收端交给 WAL writer（独立 tokio 任务）。
    pub fn new(capacity: usize) -> (Self, mpsc::Receiver<AgentEventEnvelope>) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        (
            Self {
                tx,
                stats: Arc::new(TapStats::default()),
            },
            rx,
        )
    }

    /// 空操作 tap：事件直接计数为 dropped。用于监控关闭时的零成本占位。
    pub fn disabled() -> Self {
        let (tx, _rx) = mpsc::channel(1);
        // 丢弃接收端 → 所有 try_send 立即失败并计数，开销接近零。
        Self {
            tx,
            stats: Arc::new(TapStats::default()),
        }
    }

    pub fn stats(&self) -> Arc<TapStats> {
        Arc::clone(&self.stats)
    }

    /// 旁路上报一个事件。永不阻塞：失败仅计数。
    #[inline]
    pub fn emit(&self, event: AgentEventEnvelope) {
        match self.tx.try_send(event) {
            Ok(()) => {
                self.stats.sent.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                // 队列满或 writer 已退出：失败开放，静默丢弃。
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
