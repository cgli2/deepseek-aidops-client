//! Bounded priority queues. No disk I/O or blocking in the producer.
use std::sync::{Arc, atomic::{AtomicU64, Ordering}, mpsc};
use super::event::AgentEventEnvelope;
pub const DEFAULT_TAP_CAPACITY: usize = 4096;
#[derive(Debug, Default)]
pub struct TapStats {
    sequence: AtomicU64,
    pub sent: AtomicU64,
    pub dropped: AtomicU64,
    pub critical_dropped: AtomicU64,
}
impl TapStats {
    pub fn sent(&self) -> u64 { self.sent.load(Ordering::Relaxed) }
    pub fn dropped(&self) -> u64 { self.dropped.load(Ordering::Relaxed) }
}
#[derive(Clone)]
pub struct EventTap {
    normal: mpsc::SyncSender<AgentEventEnvelope>,
    critical: mpsc::SyncSender<AgentEventEnvelope>,
    emergency: mpsc::SyncSender<AgentEventEnvelope>,
    stats: Arc<TapStats>,
}
pub struct TapReceiver {
    normal: mpsc::Receiver<AgentEventEnvelope>,
    critical: mpsc::Receiver<AgentEventEnvelope>,
    emergency: mpsc::Receiver<AgentEventEnvelope>,
    pub stats: Arc<TapStats>,
}
impl TapReceiver {
    pub fn try_recv(&self) -> Result<AgentEventEnvelope, mpsc::TryRecvError> {
        let a = self.emergency.try_recv();
        if a.is_ok() { return a; }
        let b = self.critical.try_recv();
        if b.is_ok() { return b; }
        let c = self.normal.try_recv();
        if c.is_ok() { return c; }
        if [a.err(), b.err(), c.err()].iter().all(|e| *e == Some(mpsc::TryRecvError::Disconnected)) {
            Err(mpsc::TryRecvError::Disconnected)
        } else { Err(mpsc::TryRecvError::Empty) }
    }
}
impl EventTap {
    pub fn new(capacity: usize) -> (Self, TapReceiver) {
        let (normal, nr) = mpsc::sync_channel(capacity.max(1));
        let (critical, cr) = mpsc::sync_channel(capacity.max(1));
        let (emergency, er) = mpsc::sync_channel(64);
        let stats = Arc::new(TapStats::default());
        (Self { normal, critical, emergency, stats: stats.clone() },
            TapReceiver { normal: nr, critical: cr, emergency: er, stats })
    }
    pub fn disabled() -> Self { Self::new(1).0 }
    pub fn stats(&self) -> Arc<TapStats> { self.stats.clone() }
    pub fn emit(&self, mut event: AgentEventEnvelope) {
        event.source_seq = self.stats.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let critical = event.critical();
        let result = if critical { self.critical.try_send(event) } else { self.normal.try_send(event) };
        match result {
            Ok(()) => { self.stats.sent.fetch_add(1, Ordering::Relaxed); }
            Err(error) => {
                if critical {
                    let e = match error { mpsc::TrySendError::Full(e) | mpsc::TrySendError::Disconnected(e) => e };
                    if self.emergency.try_send(e).is_ok() {
                        self.stats.sent.fetch_add(1, Ordering::Relaxed);
                        return;
                    } else {
                        self.stats.critical_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}
