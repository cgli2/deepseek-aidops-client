//! Durable adapter that connects interactive turns to the long-horizon control plane.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use harness_core::error::{Error, Result};
use harness_core::types::UserInput;
use harness_core::{AppContext, Workspace};
use harness_llm::{ChunkStream, LlmProvider, Message, RequestOptions};
use harness_session::{DeliveryOutcome, DeliveryReport, SessionEvent, SessionLog};
use serde::Serialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent_loop::AgentLoop;
use crate::council::CouncilOrchestrator;
use crate::lha::{
    Admission, LeaseWatchdog, LongHorizonRuntime, OrchestratorError, ProviderLimit, RateLimitError,
    TaskSpec, now_ms,
};

struct ManagedRuntime {
    runtime: Arc<LongHorizonRuntime>,
    _watchdog: LeaseWatchdog,
}

/// Process-wide registry of one durable control plane per workspace.
pub struct LongHorizonManager {
    total_token_budget: u64,
    output_token_reserve: u64,
    lease_ttl: Duration,
    task_timeout: Duration,
    max_retries: u32,
    provider_limit: ProviderLimit,
    runtimes: Mutex<BTreeMap<PathBuf, Arc<ManagedRuntime>>>,
}

impl Default for LongHorizonManager {
    fn default() -> Self {
        Self::from_env()
    }
}

impl LongHorizonManager {
    pub fn from_env() -> Self {
        Self {
            total_token_budget: env_u64("HARNESS_LHA_TOTAL_TOKENS", 10_000_000),
            output_token_reserve: env_u64("HARNESS_LHA_TURN_TOKENS", 4_096),
            lease_ttl: Duration::from_secs(env_u64("HARNESS_LHA_LEASE_SECS", 180).max(3)),
            task_timeout: Duration::from_secs(env_u64("HARNESS_TURN_TIMEOUT_SECS", 1_800).max(1)),
            max_retries: env_u64("HARNESS_LHA_MAX_RETRIES", 2).min(u64::from(u32::MAX)) as u32,
            provider_limit: ProviderLimit {
                requests_per_minute: env_u64("HARNESS_LHA_RPM", 60).max(1),
                tokens_per_minute: env_u64("HARNESS_LHA_TPM", 1_000_000).max(1),
                request_burst: env_u64("HARNESS_LHA_REQUEST_BURST", 8).max(1),
                token_burst: env_u64("HARNESS_LHA_TOKEN_BURST", 128_000).max(1),
            },
            runtimes: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn runtime_for(
        &self,
        workspace: impl AsRef<Path>,
    ) -> std::result::Result<Arc<LongHorizonRuntime>, OrchestratorError> {
        let workspace = std::fs::canonicalize(workspace)?;
        let mut runtimes = self
            .runtimes
            .lock()
            .map_err(|_| OrchestratorError::Poisoned("workspace runtimes"))?;
        if let Some(managed) = runtimes.get(&workspace) {
            return Ok(managed.runtime.clone());
        }

        let runtime = Arc::new(LongHorizonRuntime::open(
            workspace.join(".harness").join("long-horizon"),
            self.total_token_budget,
        )?);
        match runtime.register_provider("default-llm", self.provider_limit, now_ms()) {
            Ok(()) | Err(OrchestratorError::RateLimit(RateLimitError::DuplicateProvider(_))) => {}
            Err(error) => return Err(error),
        }
        let interval = self.lease_ttl.div_f64(3.0).max(Duration::from_secs(1));
        let (watchdog, mut events) = runtime.start_watchdog(interval);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                tracing::warn!(?event, "long-horizon watchdog event");
            }
        });
        runtimes.insert(
            workspace,
            Arc::new(ManagedRuntime {
                runtime: runtime.clone(),
                _watchdog: watchdog,
            }),
        );
        Ok(runtime)
    }
}

#[derive(Serialize)]
struct TurnArtifact<'a> {
    task_id: &'a str,
    session_id: String,
    input: &'a str,
    report: &'a DeliveryReport,
}

enum TurnKind {
    Agent(UserInput),
    Council(String),
}

struct BudgetedLlmProvider {
    upstream: Arc<dyn LlmProvider>,
    runtime: Arc<LongHorizonRuntime>,
    task_id: String,
    output_token_reserve: u64,
}

#[async_trait::async_trait]
impl LlmProvider for BudgetedLlmProvider {
    fn name(&self) -> &'static str {
        self.upstream.name()
    }

    fn tools(&self) -> Vec<harness_llm::ToolSchema> {
        self.upstream.tools()
    }

    fn supports_vision(&self) -> bool {
        self.upstream.supports_vision()
    }

    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.stream_with_options(messages, RequestOptions::default())
    }

    fn stream_with_options(&self, messages: Vec<Message>, options: RequestOptions) -> ChunkStream {
        let runtime = self.runtime.clone();
        let task_id = self.task_id.clone();
        let upstream = self.upstream.clone();
        let estimated_tokens = estimate_request_tokens(
            &messages,
            options
                .max_output_tokens
                .unwrap_or(self.output_token_reserve),
        );
        Box::pin(
            futures::stream::once(async move {
                loop {
                    match runtime.admit_llm(&task_id, "default-llm", estimated_tokens, now_ms()) {
                        Ok(Admission::Granted) => break,
                        Ok(Admission::Backpressure { retry_after_ms }) => {
                            tokio::time::sleep(Duration::from_millis(retry_after_ms.max(1))).await;
                        }
                        Ok(Admission::RequestTooLarge { requested, burst }) => {
                            return llm_error_stream(format!(
                                "request estimate {requested} exceeds provider burst {burst}"
                            ));
                        }
                        Ok(Admission::GracefulExhaustion) => {
                            return llm_error_stream(
                                "global token budget exhausted; partial report persisted".into(),
                            );
                        }
                        Err(error) => return llm_error_stream(error.to_string()),
                    }
                }
                let limiter = runtime.clone();
                let provider = upstream.stream_with_options(messages, options);
                Box::pin(provider.map(move |item| {
                    if item
                        .as_ref()
                        .err()
                        .is_some_and(|error| error.to_string().contains("429"))
                    {
                        let _ = limiter.record_429("default-llm", now_ms());
                    }
                    item
                })) as ChunkStream
            })
            .flatten(),
        )
    }
}

impl TurnKind {
    fn text(&self) -> &str {
        match self {
            Self::Agent(input) => &input.text,
            Self::Council(goal) => goal,
        }
    }

    fn attachments(&self) -> serde_json::Value {
        match self {
            Self::Agent(input) => json!(
                input
                    .attachments
                    .iter()
                    .map(|attachment| json!({
                        "path": attachment.path,
                        "mime": attachment.mime,
                    }))
                    .collect::<Vec<_>>()
            ),
            Self::Council(_) => json!([]),
        }
    }
}

pub async fn run_durable_agent_turn(
    ctx: &AppContext,
    input: UserInput,
    cancellation: CancellationToken,
) -> Result<()> {
    if ctx.try_get::<LongHorizonManager>().is_none() {
        return AgentLoop::new()
            .run_turn_cancellable(ctx, input, cancellation, None)
            .await;
    }
    run_durable_turn(ctx, TurnKind::Agent(input), cancellation).await
}

pub async fn run_durable_council_turn(
    ctx: &AppContext,
    goal: String,
    cancellation: CancellationToken,
) -> Result<()> {
    if ctx.try_get::<LongHorizonManager>().is_none() {
        return CouncilOrchestrator::default()
            .run(ctx, goal, cancellation)
            .await;
    }
    run_durable_turn(ctx, TurnKind::Council(goal), cancellation).await
}

async fn run_durable_turn(
    ctx: &AppContext,
    turn: TurnKind,
    cancellation: CancellationToken,
) -> Result<()> {
    let manager = ctx
        .try_get::<LongHorizonManager>()
        .ok_or(Error::ServiceMissing("LongHorizonManager"))?;
    let workspace = ctx
        .try_get::<Workspace>()
        .ok_or(Error::ServiceMissing("Workspace"))?;
    let runtime = manager
        .runtime_for(workspace.root())
        .map_err(runtime_error)?;
    let log = ctx.get::<SessionLog>();
    let session_id = log.id().to_string();
    let task_id = format!("turn-{}", Uuid::new_v4());
    let worker_id = format!("process-{}", std::process::id());
    let logical_key = format!("sessions/{session_id}/turns");
    let cursor = log.replay().len();

    runtime
        .submit(TaskSpec {
            task_id: task_id.clone(),
            parent_id: None,
            dependencies: vec![],
            inputs: json!({
                "session_id": session_id,
                "prompt": turn.text(),
                "attachments": turn.attachments(),
            }),
            invariants: vec!["delivery.verified".into()],
            expected_output_schema: json!({"type": "DeliveryReport"}),
            timeout_seconds: manager.task_timeout.as_secs(),
            max_retries: manager.max_retries,
        })
        .map_err(runtime_error)?;
    runtime
        .claim_task(
            &task_id,
            &worker_id,
            now_ms(),
            duration_ms(manager.lease_ttl),
        )
        .map_err(runtime_error)?;

    let heartbeat_cancel = CancellationToken::new();
    let heartbeat_handle = tokio::spawn(heartbeat_loop(
        runtime.clone(),
        task_id.clone(),
        worker_id.clone(),
        manager.lease_ttl,
        heartbeat_cancel.clone(),
    ));
    let execution_ctx = ctx.fork();
    let upstream = ctx.get::<dyn LlmProvider>();
    let budgeted: Arc<dyn LlmProvider> = Arc::new(BudgetedLlmProvider {
        upstream,
        runtime: runtime.clone(),
        task_id: task_id.clone(),
        output_token_reserve: manager.output_token_reserve,
    });
    let _budgeted_registration = execution_ctx.provide(budgeted);
    let outcome = match turn {
        TurnKind::Agent(input) => {
            AgentLoop::new()
                .run_turn_cancellable(&execution_ctx, input, cancellation.clone(), None)
                .await
        }
        TurnKind::Council(goal) => {
            CouncilOrchestrator::default()
                .run(&execution_ctx, goal, cancellation.clone())
                .await
        }
    };
    heartbeat_cancel.cancel();
    let _ = heartbeat_handle.await;

    if let Err(error) = &outcome {
        let _ = runtime.fail_task(&task_id, &error.to_string(), &worker_id, now_ms());
        return outcome;
    }

    let (_, events) = log.replay_from(cursor);
    let report = events.iter().rev().find_map(|event| match event {
        SessionEvent::Delivery { report, .. } => Some(report.clone()),
        _ => None,
    });
    let Some(report) = report else {
        runtime
            .fail_task(
                &task_id,
                "turn ended without a delivery report",
                &worker_id,
                now_ms(),
            )
            .map_err(runtime_error)?;
        return Err(Error::Runtime(
            "long-horizon turn ended without a delivery report".into(),
        ));
    };

    match report.outcome {
        DeliveryOutcome::Verified => {
            let artifact = serde_json::to_vec_pretty(&TurnArtifact {
                task_id: &task_id,
                session_id,
                input: turn_text_from_report_input(&events).unwrap_or_default(),
                report: &report,
            })?;
            if let Err(error) = runtime.finalize_delivery(
                &task_id,
                &logical_key,
                &artifact,
                &report,
                &worker_id,
                now_ms(),
            ) {
                let _ = runtime.fail_task(&task_id, &error.to_string(), &worker_id, now_ms());
                return Err(runtime_error(error));
            }
        }
        DeliveryOutcome::Cancelled => {
            runtime
                .cancel_task(
                    &task_id,
                    report.reason.as_deref().unwrap_or("turn cancelled"),
                    &worker_id,
                    now_ms(),
                )
                .map_err(runtime_error)?;
        }
        _ => {
            runtime
                .fail_task(
                    &task_id,
                    report.reason.as_deref().unwrap_or("turn was not verified"),
                    &worker_id,
                    now_ms(),
                )
                .map_err(runtime_error)?;
        }
    }
    outcome
}

async fn heartbeat_loop(
    runtime: Arc<LongHorizonRuntime>,
    task_id: String,
    worker_id: String,
    lease_ttl: Duration,
    cancellation: CancellationToken,
) {
    let mut interval = tokio::time::interval(lease_ttl.div_f64(3.0).max(Duration::from_secs(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            _ = interval.tick() => {
                if runtime
                    .heartbeat(
                        &task_id,
                        &worker_id,
                        0.0,
                        Some("interactive turn is active".into()),
                        now_ms(),
                        duration_ms(lease_ttl),
                    )
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

fn estimate_request_tokens(messages: &[Message], output_tokens: u64) -> u64 {
    let input_bytes = messages.iter().fold(0_u64, |total, message| {
        let content = u64::try_from(message.content.len()).unwrap_or(u64::MAX);
        let images = message.image_data_urls.iter().fold(0_u64, |sum, image| {
            sum.saturating_add(u64::try_from(image.len()).unwrap_or(u64::MAX))
        });
        let tools = message.tool_calls.iter().fold(0_u64, |sum, call| {
            sum.saturating_add(u64::try_from(call.name.len()).unwrap_or(u64::MAX))
                .saturating_add(u64::try_from(call.args.to_string().len()).unwrap_or(u64::MAX))
        });
        total
            .saturating_add(content)
            .saturating_add(images)
            .saturating_add(tools)
    });
    input_bytes.div_ceil(4).saturating_add(output_tokens).max(1)
}

fn llm_error_stream(message: String) -> ChunkStream {
    Box::pin(futures::stream::once(
        async move { Err(Error::Llm(message)) },
    ))
}

fn turn_text_from_report_input(events: &[SessionEvent]) -> Option<&str> {
    events.iter().find_map(|event| match event {
        SessionEvent::TurnStart { input, .. } => Some(input.as_str()),
        _ => None,
    })
}

fn runtime_error(error: OrchestratorError) -> Error {
    Error::Runtime(format!("long-horizon control plane: {error}"))
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manager_reuses_one_runtime_per_workspace() {
        let root = std::env::temp_dir().join(format!("lha_manager_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let manager = LongHorizonManager::from_env();
        let first = manager.runtime_for(&root).unwrap();
        let second = manager.runtime_for(&root).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        std::fs::remove_dir_all(root).ok();
    }
}
