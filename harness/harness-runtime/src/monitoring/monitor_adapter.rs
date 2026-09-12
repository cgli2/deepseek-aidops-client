//! Real SessionLog -> metadata-only observation WAL and inline protection.
use std::{path::PathBuf, sync::{Arc, atomic::{AtomicU32, AtomicU64, Ordering}}};
use harness_core::{AppContext, Config, Workspace};
use harness_session::{Observation, SessionEvent, SessionLog};
use super::{event::{AgentEventEnvelope, EventClass, EventKind}, hot_guard::{HotGuard, GuardVerdict}, tap::EventTap};

pub struct MonitorTurn {
    tap: Option<EventTap>,
    registration: Option<Observation>,
    writer: Option<tokio::task::JoinHandle<()>>,
    spool: Option<PathBuf>,
    turn: String,
    session: String,
    guard: HotGuard,
    protect: bool,
    evidence: usize,
    sidecar: bool,
}
impl MonitorTurn {
    pub fn start(ctx: &AppContext, log: &SessionLog) -> Self {
        let config = ctx.try_get::<Config>().map(|c| c.self_monitor.clone()).unwrap_or_default();
        let mut state = Self { tap: None, registration: None, writer: None, spool: None,
            turn: uuid::Uuid::new_v4().to_string(), session: log.id().to_string(),
            guard: HotGuard::new(config.repeat_stop.clamp(2, 64) - 1),
            protect: config.enabled && config.mode == "protect", evidence: 0, sidecar: config.sidecar };
        if !config.enabled || config.mode == "off" { return state; }
        let Some(workspace) = ctx.try_get::<Workspace>() else { return state; };
        let spool = workspace.root().join(".harness/self-monitor/spool").join(&state.turn);
        let (tap, rx) = EventTap::new(super::tap::DEFAULT_TAP_CAPACITY);
        match super::wal::spawn_wal_writer(&spool, rx) {
            Ok((writer, _stats)) => {
                state.writer = Some(writer);
                state.spool = Some(spool);
            }
            Err(error) => { tracing::warn!(%error, "monitor degraded; agent continues"); return state; }
        }
        let producer = tap.clone();
        let session = state.session.clone();
        let turn = state.turn.clone();
        let step = AtomicU32::new(0);
        let sequence = AtomicU64::new(0);
        state.registration = Some(log.observe(Arc::new(move |event| {
            let kind = match event {
                SessionEvent::TurnStart { .. } => EventKind::TurnStarted,
                SessionEvent::StepStart { step: n, .. } => { step.store(*n as u32, Ordering::Relaxed); EventKind::StepStarted }
                SessionEvent::ToolResult { result, .. } => EventKind::ToolCompleted { ok: result.ok },
                SessionEvent::Delivery { report, .. } => EventKind::DeliveryJudged {
                    outcome: format!("{:?}", report.outcome), criteria: report.criteria.len(),
                    evidenced: report.criteria.iter().filter(|c| c.satisfied && c.evidence.iter().any(|e| !e.trim().is_empty())).count(),
                },
                SessionEvent::TurnEnd { .. } => EventKind::TurnFinished,
                _ => return,
            };
            let mut e = AgentEventEnvelope::new(&session, EventClass::ControlFlow, kind, crate::lha::now_ms()).with_turn(&turn);
            e.step_id = Some(step.load(Ordering::Relaxed));
            e.event_id = format!("{turn}-{}", sequence.fetch_add(1, Ordering::Relaxed));
            producer.emit(e);
        })));
        state.tap = Some(tap);
        state
    }
    pub fn observe(&mut self, conclusion: &str, evidence_count: usize) -> bool {
        let delta = u8::from(evidence_count > self.evidence);
        self.evidence = evidence_count;
        let verdict = self.guard.observe(conclusion, delta);
        if let Some(tap) = &self.tap {
            for mut event in self.guard.act(&verdict, &self.session, crate::lha::now_ms()).1 {
                event.turn_id = Some(self.turn.clone());
                tap.emit(event);
            }
        }
        self.protect && matches!(verdict, GuardVerdict::Stagnation { .. })
    }
}
impl Drop for MonitorTurn {
    fn drop(&mut self) {
        self.registration.take();
        self.tap.take();
        let Some(writer) = self.writer.take() else { return; };
        let spool = self.spool.take();
        let sidecar = self.sidecar;
        tokio::spawn(async move {
            if tokio::time::timeout(std::time::Duration::from_secs(2), writer).await.is_err() {
                tracing::warn!("monitor writer shutdown exceeded deadline");
                return;
            }
            if sidecar {
                if let Some(spool) = spool { super::observer::supervise(spool).await; }
            }
        });
    }
}
