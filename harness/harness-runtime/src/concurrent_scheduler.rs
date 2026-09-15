//! Phase 2（方案 B）并发执行器：每准入面一条常驻 tokio 任务 + `Arc<Mutex<>>` 共享状态。
//!
//! 依据 `docs/agent-loop-async-concurrent-executor-adr.md`：
//! - §3 方案 B：每个准入面 spawn 一条常驻任务，跑自己的模型流，协调器 `join!` 各任务；
//! - §7 决策点 1 的**第 2 个方案**「Phase 1+2（方案 B，feature flag）」与决策点 3
//!   「先 flag 守护」（§3 方案 B 亦建议"默认关闭、feature flag 守护，验证后再开"）；
//! - §8 as-built：Phase 1 融合流已是默认路径，Phase 2 仍属后续独立任务。
//!
//! 因此本模块**默认关闭**（[`SurfaceConcurrencyMode::Phase1`]）：未显式开启时
//! [`ConcurrentSurfaceScheduler::admits_resident_tasks`] 恒为 `false`，协调器继续走
//! Phase 1 融合流，单面路径字节一致（ADR §5 的 R5 零回归）。
//! 开启方式：`HARNESS_SURFACE_CONCURRENCY=phase2`。
//!
//! 锁纪律（ADR §4 / R1）：面只改自身 items，全局 ledger/budget 走加锁段；
//! 共享状态取的是 `std::sync::Mutex`，**任何加锁段内不得跨 `await`**。

use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// 开关环境变量（与 `HARNESS_GOAL_EXECUTOR` / `HARNESS_GOVERNOR` 同族约定）。
pub const ENV_SURFACE_CONCURRENCY: &str = "HARNESS_SURFACE_CONCURRENCY";

/// ADR §7 决策点 1 的第 2 个方案：Phase 1 为默认，Phase 2 由 feature flag 守护。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SurfaceConcurrencyMode {
    /// as-built 默认路径：融合流（方案 A）。
    #[default]
    Phase1,
    /// 方案 B：每准入面一条常驻 tokio 任务。
    Phase2,
}

impl SurfaceConcurrencyMode {
    pub fn is_phase2(self) -> bool {
        matches!(self, Self::Phase2)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Phase1 => "phase1",
            Self::Phase2 => "phase2",
        }
    }
}

/// 解析开关值。**任何无法识别的值都回退 Phase 1**，即"flag 未明确打开就不改变行为"。
pub fn parse_surface_concurrency_mode(value: Option<&str>) -> SurfaceConcurrencyMode {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("2" | "phase2" | "phase-2" | "phase_2") => SurfaceConcurrencyMode::Phase2,
        _ => SurfaceConcurrencyMode::Phase1,
    }
}

/// 进程级开关（环境变量）。UI 覆盖层（同 `goal_executor_enabled` 的 tuning 通道）
/// 不在本期范围内，留待协调器接线时一并接入。
pub fn surface_concurrency_mode() -> SurfaceConcurrencyMode {
    parse_surface_concurrency_mode(std::env::var(ENV_SURFACE_CONCURRENCY).ok().as_deref())
}

/// 一面在共享状态上留下的轮次归属记录（面只写自己的条目）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceRoundRecord {
    pub surface: String,
    pub round: u32,
}

/// 方案 B 的共享状态聚合：由协调器以 `Arc<Mutex<_>>` 交给各常驻任务。
#[derive(Default)]
pub struct SurfaceSharedState {
    rounds: Vec<SurfaceRoundRecord>,
}

impl SurfaceSharedState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一轮归属。面只改自身 items（ADR §4），互不覆盖。
    pub fn record_round(&mut self, surface: &str, round: u32) {
        self.rounds.push(SurfaceRoundRecord {
            surface: surface.to_string(),
            round,
        });
    }

    pub fn rounds_for(&self, surface: &str) -> usize {
        self.rounds
            .iter()
            .filter(|record| record.surface == surface)
            .count()
    }

    pub fn recorded_rounds(&self) -> &[SurfaceRoundRecord] {
        &self.rounds
    }
}

/// 单面常驻任务的终态。`Cancelled` 覆盖"取消 / 任务被 abort"两种情形，
/// 由协调器在合并历史时补占位 tool result（ADR §6 取消传播）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResidentSurfaceOutcome<T> {
    Completed(T),
    Cancelled,
}

/// 每准入面一条常驻任务的调度器。
#[derive(Clone, Debug)]
pub struct ConcurrentSurfaceScheduler {
    mode: SurfaceConcurrencyMode,
    max_parallel: usize,
}

impl ConcurrentSurfaceScheduler {
    pub fn new(mode: SurfaceConcurrencyMode, max_parallel: usize) -> Self {
        Self {
            mode,
            max_parallel: max_parallel.max(1),
        }
    }

    /// 从环境开关构造（默认 Phase 1）。
    pub fn from_env(max_parallel: usize) -> Self {
        Self::new(surface_concurrency_mode(), max_parallel)
    }

    pub fn mode(&self) -> SurfaceConcurrencyMode {
        self.mode
    }

    pub fn max_parallel(&self) -> usize {
        self.max_parallel
    }

    /// 是否真的开常驻任务。**必须 Phase 2 且准入面 > 1**：
    /// 开关未开或只有单面时返回 `false`，调用方走原单流/融合流路径（R5 零回归）。
    pub fn admits_resident_tasks(&self, admitted_surfaces: usize) -> bool {
        self.mode.is_phase2() && admitted_surfaces > 1
    }

    /// 对准入面并发 spawn 常驻任务并 `join` 各任务，按面返回终态（保序 = 准入顺序）。
    ///
    /// `make_task(surface_id, shared, cancel)` 由调用方提供该面的模型流循环闭包，
    /// 返回 `None` 表示该面在取消/终态后主动停止。
    pub async fn run_resident<F, Fut, T>(
        &self,
        admitted: Vec<String>,
        shared: Arc<Mutex<SurfaceSharedState>>,
        cancel: CancellationToken,
        make_task: F,
    ) -> Vec<(String, ResidentSurfaceOutcome<T>)>
    where
        F: Fn(String, Arc<Mutex<SurfaceSharedState>>, CancellationToken) -> Fut,
        Fut: Future<Output = Option<T>> + Send + 'static,
        T: Send + 'static,
    {
        let mut handles: Vec<(String, JoinHandle<Option<T>>)> = Vec::new();
        for surface in admitted.into_iter().take(self.max_parallel) {
            let task_shared = Arc::clone(&shared);
            let task_cancel = cancel.clone();
            let future = make_task(surface.clone(), task_shared, task_cancel);
            handles.push((surface, tokio::spawn(future)));
        }

        let mut outcomes = Vec::with_capacity(handles.len());
        for (surface, handle) in handles {
            let outcome = match handle.await {
                Ok(Some(value)) => ResidentSurfaceOutcome::Completed(value),
                Ok(None) => ResidentSurfaceOutcome::Cancelled,
                Err(_) => ResidentSurfaceOutcome::Cancelled,
            };
            outcomes.push((surface, outcome));
        }
        outcomes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 当前线程运行时 + 全功能开关，避免依赖 tokio 宏特性。
    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime")
            .block_on(future)
    }

    #[test]
    fn parse_defaults_to_phase1_and_accepts_phase2() {
        assert_eq!(
            parse_surface_concurrency_mode(None),
            SurfaceConcurrencyMode::Phase1
        );
        for raw in ["", "  ", "0", "off", "false", "phase1", "legacy", "Phase 1"] {
            assert_eq!(
                parse_surface_concurrency_mode(Some(raw)),
                SurfaceConcurrencyMode::Phase1,
                "未识别值必须回退 Phase 1：{raw:?}"
            );
        }
        for raw in ["2", "phase2", "PHASE-2", " phase_2 "] {
            assert_eq!(
                parse_surface_concurrency_mode(Some(raw)),
                SurfaceConcurrencyMode::Phase2,
                "应识别为 Phase 2：{raw:?}"
            );
        }
    }

    #[test]
    fn phase1_is_default_and_never_admits_resident_tasks() {
        let scheduler = ConcurrentSurfaceScheduler::new(SurfaceConcurrencyMode::default(), 4);
        assert_eq!(scheduler.mode(), SurfaceConcurrencyMode::Phase1);
        // R5 零回归：默认关闭时任何面数都不开常驻任务。
        assert!(!scheduler.admits_resident_tasks(0));
        assert!(!scheduler.admits_resident_tasks(1));
        assert!(!scheduler.admits_resident_tasks(4));
    }

    #[test]
    fn phase2_requires_more_than_one_admitted_surface() {
        let scheduler = ConcurrentSurfaceScheduler::new(SurfaceConcurrencyMode::Phase2, 4);
        assert!(!scheduler.admits_resident_tasks(1));
        assert!(scheduler.admits_resident_tasks(2));
    }

    #[test]
    fn phase2_runs_one_resident_task_per_admitted_surface_in_parallel() {
        block_on(async {
            let scheduler = ConcurrentSurfaceScheduler::new(SurfaceConcurrencyMode::Phase2, 4);
            let admitted = vec!["surface-a".to_string(), "surface-b".to_string(), "surface-c".to_string()];
            assert!(scheduler.admits_resident_tasks(admitted.len()));

            let shared = Arc::new(Mutex::new(SurfaceSharedState::new()));
            let cancel = CancellationToken::new();
            // 3 面共享一个屏障：只有三条任务真正同时在飞，屏障才放行。
            let barrier = Arc::new(tokio::sync::Barrier::new(admitted.len()));
            let in_flight = Arc::new(AtomicUsize::new(0));

            let barrier_for_task = Arc::clone(&barrier);
            let in_flight_for_task = Arc::clone(&in_flight);
            let outcomes = scheduler
                .run_resident(admitted.clone(), Arc::clone(&shared), cancel, move |surface, shared, _cancel| {
                    let barrier = Arc::clone(&barrier_for_task);
                    let in_flight = Arc::clone(&in_flight_for_task);
                    async move {
                        in_flight.fetch_add(1, Ordering::SeqCst);
                        barrier.wait().await;
                        let round = {
                            let mut state = shared.lock().unwrap_or_else(|err| err.into_inner());
                            state.record_round(&surface, 1);
                            state.rounds_for(&surface) as u32
                        };
                        Some(round)
                    }
                })
                .await;

            assert_eq!(outcomes.len(), 3);
            assert_eq!(
                outcomes,
                vec![
                    ("surface-a".to_string(), ResidentSurfaceOutcome::Completed(1)),
                    ("surface-b".to_string(), ResidentSurfaceOutcome::Completed(1)),
                    ("surface-c".to_string(), ResidentSurfaceOutcome::Completed(1)),
                ]
            );
            assert_eq!(in_flight.load(Ordering::SeqCst), 3, "三面必须同时在飞");
            let state = shared.lock().unwrap_or_else(|err| err.into_inner());
            assert_eq!(state.recorded_rounds().len(), 3);
            assert_eq!(state.rounds_for("surface-b"), 1);
            assert_eq!(state.rounds_for("surface-missing"), 0);
        });
    }

    #[test]
    fn cancelled_resident_tasks_report_cancelled_instead_of_completed() {
        block_on(async {
            let scheduler = ConcurrentSurfaceScheduler::new(SurfaceConcurrencyMode::Phase2, 4);
            let shared = Arc::new(Mutex::new(SurfaceSharedState::new()));
            let cancel = CancellationToken::new();
            cancel.cancel();

            let outcomes = scheduler
                .run_resident(
                    vec!["surface-a".to_string(), "surface-b".to_string()],
                    shared,
                    cancel.clone(),
                    move |_surface, _shared, cancel| async move {
                        if cancel.is_cancelled() {
                            return None;
                        }
                        Some(1)
                    },
                )
                .await;

            assert_eq!(
                outcomes,
                vec![
                    ("surface-a".to_string(), ResidentSurfaceOutcome::Cancelled),
                    ("surface-b".to_string(), ResidentSurfaceOutcome::Cancelled),
                ]
            );
        });
    }

    #[test]
    fn max_parallel_bounds_resident_task_count() {
        block_on(async {
            let scheduler = ConcurrentSurfaceScheduler::new(SurfaceConcurrencyMode::Phase2, 2);
            let admitted = vec![
                "s1".to_string(),
                "s2".to_string(),
                "s3".to_string(),
                "s4".to_string(),
            ];
            let shared = Arc::new(Mutex::new(SurfaceSharedState::new()));
            let outcomes = scheduler
                .run_resident(admitted, shared, CancellationToken::new(), move |surface, shared, _cancel| {
                    async move {
                        let mut state = shared.lock().unwrap_or_else(|err| err.into_inner());
                        state.record_round(&surface, 1);
                        Some(surface)
                    }
                })
                .await;

            assert_eq!(outcomes.len(), 2, "上限 2 只开 2 条常驻任务");
            assert_eq!(outcomes[0].0, "s1");
            assert_eq!(outcomes[1].0, "s2");
        });
    }
}
