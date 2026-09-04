//! Durable event-sourced DAG state, leases, checkpoints, and graceful budget exhaustion.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    Ready,
    Scheduled {
        worker_id: String,
    },
    Running {
        worker_id: String,
        lease_expires_at_ms: u64,
    },
    Validating,
    Succeeded {
        artifact_uri: String,
    },
    Failed {
        reason: String,
    },
    Cancelled {
        reason: String,
    },
    BudgetExhausted {
        report_uri: String,
    },
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Succeeded { .. } | Self::Cancelled { .. } | Self::BudgetExhausted { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpec {
    pub task_id: String,
    pub parent_id: Option<String>,
    pub dependencies: Vec<String>,
    pub inputs: serde_json::Value,
    pub invariants: Vec<String>,
    pub expected_output_schema: serde_json::Value,
    pub timeout_seconds: u64,
    pub max_retries: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub spec: TaskSpec,
    pub status: TaskStatus,
    pub retry_count: u32,
    pub progress_pct: f32,
    pub last_note: Option<String>,
    pub checkpoint_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DagEvent {
    TaskCreated {
        spec: TaskSpec,
    },
    TaskReady {
        task_id: String,
    },
    TaskScheduled {
        task_id: String,
        worker_id: String,
    },
    TaskStarted {
        task_id: String,
        worker_id: String,
        lease_expires_at_ms: u64,
    },
    TaskHeartbeat {
        task_id: String,
        worker_id: String,
        progress_pct: f32,
        note: Option<String>,
        lease_expires_at_ms: u64,
    },
    TaskCheckpointSaved {
        task_id: String,
        checkpoint_id: String,
    },
    TaskValidationStarted {
        task_id: String,
    },
    TaskCompleted {
        task_id: String,
        artifact_uri: String,
    },
    TaskFailed {
        task_id: String,
        reason: String,
    },
    TaskRescheduled {
        task_id: String,
    },
    TaskCancelled {
        task_id: String,
        reason: String,
    },
    TaskBudgetExhausted {
        task_id: String,
        report_uri: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WalRecord {
    pub seq: u64,
    pub event: DagEvent,
}

#[derive(Debug)]
pub enum DagError {
    Io(std::io::Error),
    Json(serde_json::Error),
    CorruptWal(String),
    DuplicateTask(String),
    UnknownTask(String),
    UnknownDependency { task_id: String, dependency: String },
    CycleDetected(String),
    InvalidTransition { task_id: String, detail: String },
    RetryLimitReached(String),
}

impl std::fmt::Display for DagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "DAG I/O error: {error}"),
            Self::Json(error) => write!(f, "DAG JSON error: {error}"),
            Self::CorruptWal(detail) => write!(f, "corrupt DAG WAL: {detail}"),
            Self::DuplicateTask(task) => write!(f, "duplicate task: {task}"),
            Self::UnknownTask(task) => write!(f, "unknown task: {task}"),
            Self::UnknownDependency {
                task_id,
                dependency,
            } => {
                write!(f, "task {task_id} has unknown dependency {dependency}")
            }
            Self::CycleDetected(task) => write!(f, "dependency cycle detected at {task}"),
            Self::InvalidTransition { task_id, detail } => {
                write!(f, "invalid transition for {task_id}: {detail}")
            }
            Self::RetryLimitReached(task) => write!(f, "retry limit reached for {task}"),
        }
    }
}

impl std::error::Error for DagError {}

impl From<std::io::Error> for DagError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for DagError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

pub struct DurableDag {
    wal_path: PathBuf,
    next_seq: u64,
    tasks: BTreeMap<String, TaskRecord>,
}

impl DurableDag {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DagError> {
        let wal_path = path.as_ref().to_path_buf();
        if let Some(parent) = wal_path.parent() {
            fs::create_dir_all(parent)?;
        }
        super::storage::repair_jsonl_tail(&wal_path)?;
        let records = read_wal(&wal_path)?;
        let mut tasks = BTreeMap::new();
        let mut expected_seq = 1;
        for record in &records {
            if record.seq != expected_seq {
                return Err(DagError::CorruptWal(format!(
                    "expected sequence {expected_seq}, got {}",
                    record.seq
                )));
            }
            apply_event(&mut tasks, &record.event)?;
            expected_seq += 1;
        }
        Ok(Self {
            wal_path,
            next_seq: expected_seq,
            tasks,
        })
    }

    pub fn tasks(&self) -> &BTreeMap<String, TaskRecord> {
        &self.tasks
    }

    pub fn task(&self, task_id: &str) -> Option<&TaskRecord> {
        self.tasks.get(task_id)
    }

    pub fn create_task(&mut self, spec: TaskSpec) -> Result<(), DagError> {
        validate_spec(&self.tasks, &spec)?;
        self.commit(DagEvent::TaskCreated { spec })
    }

    /// Promote every pending task whose hard dependencies have succeeded.
    pub fn refresh_ready(&mut self) -> Result<Vec<String>, DagError> {
        let ready: Vec<String> = self
            .tasks
            .values()
            .filter(|record| record.status == TaskStatus::Pending)
            .filter(|record| {
                record.spec.dependencies.iter().all(|dependency| {
                    self.tasks
                        .get(dependency)
                        .is_some_and(|value| matches!(value.status, TaskStatus::Succeeded { .. }))
                })
            })
            .map(|record| record.spec.task_id.clone())
            .collect();
        for task_id in &ready {
            self.commit(DagEvent::TaskReady {
                task_id: task_id.clone(),
            })?;
        }
        Ok(ready)
    }

    pub fn schedule(&mut self, task_id: &str, worker_id: &str) -> Result<(), DagError> {
        self.commit(DagEvent::TaskScheduled {
            task_id: task_id.into(),
            worker_id: worker_id.into(),
        })
    }

    pub fn start(
        &mut self,
        task_id: &str,
        worker_id: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<(), DagError> {
        if ttl_ms == 0 {
            return Err(invalid(task_id, "lease TTL must be greater than zero"));
        }
        self.commit(DagEvent::TaskStarted {
            task_id: task_id.into(),
            worker_id: worker_id.into(),
            lease_expires_at_ms: now_ms.saturating_add(ttl_ms),
        })
    }

    pub fn heartbeat(
        &mut self,
        task_id: &str,
        worker_id: &str,
        progress_pct: f32,
        note: Option<String>,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<(), DagError> {
        if ttl_ms == 0 {
            return Err(invalid(task_id, "heartbeat TTL must be greater than zero"));
        }
        self.commit(DagEvent::TaskHeartbeat {
            task_id: task_id.into(),
            worker_id: worker_id.into(),
            progress_pct,
            note,
            lease_expires_at_ms: now_ms.saturating_add(ttl_ms),
        })
    }

    pub fn checkpoint(&mut self, task_id: &str, checkpoint_id: &str) -> Result<(), DagError> {
        self.commit(DagEvent::TaskCheckpointSaved {
            task_id: task_id.into(),
            checkpoint_id: checkpoint_id.into(),
        })
    }

    pub fn begin_validation(&mut self, task_id: &str) -> Result<(), DagError> {
        self.commit(DagEvent::TaskValidationStarted {
            task_id: task_id.into(),
        })
    }

    pub fn complete(&mut self, task_id: &str, artifact_uri: &str) -> Result<(), DagError> {
        self.commit(DagEvent::TaskCompleted {
            task_id: task_id.into(),
            artifact_uri: artifact_uri.into(),
        })
    }

    pub fn fail(&mut self, task_id: &str, reason: &str) -> Result<(), DagError> {
        self.commit(DagEvent::TaskFailed {
            task_id: task_id.into(),
            reason: reason.into(),
        })
    }

    pub fn reschedule(&mut self, task_id: &str) -> Result<(), DagError> {
        self.commit(DagEvent::TaskRescheduled {
            task_id: task_id.into(),
        })
    }

    pub fn cancel(&mut self, task_id: &str, reason: &str) -> Result<(), DagError> {
        self.commit(DagEvent::TaskCancelled {
            task_id: task_id.into(),
            reason: reason.into(),
        })
    }

    /// R7 terminal: persist the partial-delivery report reference instead of suspending.
    pub fn exhaust_budget(&mut self, task_id: &str, report_uri: &str) -> Result<(), DagError> {
        self.commit(DagEvent::TaskBudgetExhausted {
            task_id: task_id.into(),
            report_uri: report_uri.into(),
        })
    }

    /// Reclaim expired leases. Each task is durably failed and, when retries remain,
    /// returned to Pending so the normal readiness pass can enqueue it again.
    pub fn reap_expired(&mut self, now_ms: u64) -> Result<Vec<String>, DagError> {
        let expired: Vec<String> = self
            .tasks
            .values()
            .filter_map(|record| match record.status {
                TaskStatus::Running {
                    lease_expires_at_ms,
                    ..
                } if lease_expires_at_ms <= now_ms => Some(record.spec.task_id.clone()),
                _ => None,
            })
            .collect();
        for task_id in &expired {
            self.fail(task_id, "worker lease expired")?;
            let can_retry = self
                .task(task_id)
                .is_some_and(|record| record.retry_count < record.spec.max_retries);
            if can_retry {
                self.reschedule(task_id)?;
            }
        }
        Ok(expired)
    }

    fn commit(&mut self, event: DagEvent) -> Result<(), DagError> {
        let mut candidate = self.tasks.clone();
        apply_event(&mut candidate, &event)?;
        let record = WalRecord {
            seq: self.next_seq,
            event,
        };
        let mut bytes = serde_json::to_vec(&record)?;
        bytes.push(b'\n');
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.wal_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        self.tasks = candidate;
        self.next_seq += 1;
        Ok(())
    }
}

fn read_wal(path: &Path) -> Result<Vec<WalRecord>, DagError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path)?;
    let ends_with_newline = bytes.ends_with(b"\n");
    let mut records = Vec::new();
    let lines: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    for (index, line) in lines.iter().enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match serde_json::from_slice(line) {
            Ok(record) => records.push(record),
            Err(_) if index == lines.len() - 1 && !ends_with_newline => {
                // A crash may leave one incomplete tail record. Earlier corruption is fatal.
            }
            Err(error) => {
                return Err(DagError::CorruptWal(format!("line {}: {error}", index + 1)));
            }
        }
    }
    Ok(records)
}

fn validate_spec(tasks: &BTreeMap<String, TaskRecord>, spec: &TaskSpec) -> Result<(), DagError> {
    if !valid_task_id(&spec.task_id) {
        return Err(DagError::InvalidTransition {
            task_id: spec.task_id.clone(),
            detail: "task_id must use only ASCII letters, digits, '.', '_' or '-'".into(),
        });
    }
    if spec
        .parent_id
        .as_deref()
        .is_some_and(|parent_id| !valid_task_id(parent_id))
    {
        return Err(DagError::InvalidTransition {
            task_id: spec.task_id.clone(),
            detail: "parent_id contains unsafe characters".into(),
        });
    }
    if tasks.contains_key(&spec.task_id) {
        return Err(DagError::DuplicateTask(spec.task_id.clone()));
    }
    for dependency in &spec.dependencies {
        if dependency == &spec.task_id {
            return Err(DagError::CycleDetected(spec.task_id.clone()));
        }
        if !tasks.contains_key(dependency) {
            return Err(DagError::UnknownDependency {
                task_id: spec.task_id.clone(),
                dependency: dependency.clone(),
            });
        }
        if reaches(tasks, dependency, &spec.task_id) {
            return Err(DagError::CycleDetected(spec.task_id.clone()));
        }
    }
    Ok(())
}

fn valid_task_id(task_id: &str) -> bool {
    !task_id.is_empty()
        && task_id != "."
        && task_id != ".."
        && task_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn reaches(tasks: &BTreeMap<String, TaskRecord>, from: &str, target: &str) -> bool {
    if from == target {
        return true;
    }
    tasks.get(from).is_some_and(|record| {
        record
            .spec
            .dependencies
            .iter()
            .any(|next| reaches(tasks, next, target))
    })
}

fn task_mut<'a>(
    tasks: &'a mut BTreeMap<String, TaskRecord>,
    task_id: &str,
) -> Result<&'a mut TaskRecord, DagError> {
    tasks
        .get_mut(task_id)
        .ok_or_else(|| DagError::UnknownTask(task_id.into()))
}

fn invalid(task_id: &str, detail: impl Into<String>) -> DagError {
    DagError::InvalidTransition {
        task_id: task_id.into(),
        detail: detail.into(),
    }
}

fn apply_event(tasks: &mut BTreeMap<String, TaskRecord>, event: &DagEvent) -> Result<(), DagError> {
    match event {
        DagEvent::TaskCreated { spec } => {
            validate_spec(tasks, spec)?;
            tasks.insert(
                spec.task_id.clone(),
                TaskRecord {
                    spec: spec.clone(),
                    status: TaskStatus::Pending,
                    retry_count: 0,
                    progress_pct: 0.0,
                    last_note: None,
                    checkpoint_id: None,
                },
            );
        }
        DagEvent::TaskReady { task_id } => {
            let record = task_mut(tasks, task_id)?;
            if record.status != TaskStatus::Pending {
                return Err(invalid(task_id, "only Pending may become Ready"));
            }
            record.status = TaskStatus::Ready;
        }
        DagEvent::TaskScheduled { task_id, worker_id } => {
            let record = task_mut(tasks, task_id)?;
            if record.status != TaskStatus::Ready || worker_id.trim().is_empty() {
                return Err(invalid(task_id, "Ready task and non-empty worker required"));
            }
            record.status = TaskStatus::Scheduled {
                worker_id: worker_id.clone(),
            };
        }
        DagEvent::TaskStarted {
            task_id,
            worker_id,
            lease_expires_at_ms,
        } => {
            let record = task_mut(tasks, task_id)?;
            match &record.status {
                TaskStatus::Scheduled { worker_id: owner } if owner == worker_id => {
                    record.status = TaskStatus::Running {
                        worker_id: worker_id.clone(),
                        lease_expires_at_ms: *lease_expires_at_ms,
                    };
                }
                _ => {
                    return Err(invalid(
                        task_id,
                        "task must be scheduled to the same worker",
                    ));
                }
            }
        }
        DagEvent::TaskHeartbeat {
            task_id,
            worker_id,
            progress_pct,
            note,
            lease_expires_at_ms,
        } => {
            let record = task_mut(tasks, task_id)?;
            if !progress_pct.is_finite()
                || !(0.0..=100.0).contains(progress_pct)
                || *progress_pct < record.progress_pct
            {
                return Err(invalid(
                    task_id,
                    "progress must be finite, bounded, and monotonic",
                ));
            }
            match &mut record.status {
                TaskStatus::Running {
                    worker_id: owner,
                    lease_expires_at_ms: lease,
                } if owner == worker_id && *lease_expires_at_ms > *lease => {
                    *lease = *lease_expires_at_ms;
                    record.progress_pct = *progress_pct;
                    record.last_note = note.clone();
                }
                _ => {
                    return Err(invalid(
                        task_id,
                        "heartbeat must come from the lease owner and extend the lease",
                    ));
                }
            }
        }
        DagEvent::TaskCheckpointSaved {
            task_id,
            checkpoint_id,
        } => {
            let record = task_mut(tasks, task_id)?;
            if !matches!(record.status, TaskStatus::Running { .. })
                || checkpoint_id.trim().is_empty()
            {
                return Err(invalid(task_id, "running task and checkpoint id required"));
            }
            record.checkpoint_id = Some(checkpoint_id.clone());
        }
        DagEvent::TaskValidationStarted { task_id } => {
            let record = task_mut(tasks, task_id)?;
            if !matches!(record.status, TaskStatus::Running { .. }) {
                return Err(invalid(task_id, "only Running may enter validation"));
            }
            record.status = TaskStatus::Validating;
        }
        DagEvent::TaskCompleted {
            task_id,
            artifact_uri,
        } => {
            let record = task_mut(tasks, task_id)?;
            if record.status != TaskStatus::Validating || artifact_uri.trim().is_empty() {
                return Err(invalid(task_id, "validation and artifact URI required"));
            }
            record.status = TaskStatus::Succeeded {
                artifact_uri: artifact_uri.clone(),
            };
            record.progress_pct = 100.0;
        }
        DagEvent::TaskFailed { task_id, reason } => {
            let record = task_mut(tasks, task_id)?;
            if !matches!(
                record.status,
                TaskStatus::Scheduled { .. } | TaskStatus::Running { .. } | TaskStatus::Validating
            ) {
                return Err(invalid(task_id, "only active tasks may fail"));
            }
            record.status = TaskStatus::Failed {
                reason: reason.clone(),
            };
        }
        DagEvent::TaskRescheduled { task_id } => {
            let record = task_mut(tasks, task_id)?;
            if !matches!(record.status, TaskStatus::Failed { .. }) {
                return Err(invalid(task_id, "only Failed may be rescheduled"));
            }
            if record.retry_count >= record.spec.max_retries {
                return Err(DagError::RetryLimitReached(task_id.clone()));
            }
            record.retry_count += 1;
            record.status = TaskStatus::Pending;
            record.progress_pct = 0.0;
            record.last_note = None;
        }
        DagEvent::TaskCancelled { task_id, reason } => {
            let record = task_mut(tasks, task_id)?;
            if record.status.is_terminal() {
                return Err(invalid(task_id, "terminal task cannot be cancelled"));
            }
            record.status = TaskStatus::Cancelled {
                reason: reason.clone(),
            };
        }
        DagEvent::TaskBudgetExhausted {
            task_id,
            report_uri,
        } => {
            let record = task_mut(tasks, task_id)?;
            if record.status.is_terminal() || report_uri.trim().is_empty() {
                return Err(invalid(
                    task_id,
                    "non-terminal task and report URI required",
                ));
            }
            record.status = TaskStatus::BudgetExhausted {
                report_uri: report_uri.clone(),
            };
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wal(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lha_dag_{tag}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("events.jsonl")
    }

    fn spec(task_id: &str, dependencies: &[&str]) -> TaskSpec {
        TaskSpec {
            task_id: task_id.into(),
            parent_id: None,
            dependencies: dependencies.iter().map(|value| (*value).into()).collect(),
            inputs: json!({}),
            invariants: vec!["tests.failed=0".into()],
            expected_output_schema: json!({"type":"artifact"}),
            timeout_seconds: 900,
            max_retries: 2,
        }
    }

    fn finish(dag: &mut DurableDag, task_id: &str) {
        dag.refresh_ready().unwrap();
        dag.schedule(task_id, "worker-1").unwrap();
        dag.start(task_id, "worker-1", 10, 100).unwrap();
        dag.begin_validation(task_id).unwrap();
        dag.complete(task_id, &format!("vault://{task_id}"))
            .unwrap();
    }

    #[test]
    fn dependencies_activate_only_after_verified_completion_and_replay() {
        let path = wal("dependencies");
        let mut dag = DurableDag::open(&path).unwrap();
        dag.create_task(spec("design", &[])).unwrap();
        dag.create_task(spec("code", &["design"])).unwrap();
        assert_eq!(dag.refresh_ready().unwrap(), vec!["design"]);
        finish(&mut dag, "design");
        assert_eq!(dag.refresh_ready().unwrap(), vec!["code"]);
        drop(dag);
        let restored = DurableDag::open(&path).unwrap();
        assert!(matches!(
            restored.task("design").unwrap().status,
            TaskStatus::Succeeded { .. }
        ));
        assert_eq!(restored.task("code").unwrap().status, TaskStatus::Ready);
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn expired_lease_is_reclaimed_and_rescheduled() {
        let path = wal("lease");
        let mut dag = DurableDag::open(&path).unwrap();
        dag.create_task(spec("code", &[])).unwrap();
        dag.refresh_ready().unwrap();
        dag.schedule("code", "worker-1").unwrap();
        dag.start("code", "worker-1", 10, 100).unwrap();
        assert!(dag.reap_expired(109).unwrap().is_empty());
        assert_eq!(dag.reap_expired(110).unwrap(), vec!["code"]);
        assert_eq!(dag.task("code").unwrap().status, TaskStatus::Pending);
        assert_eq!(dag.task("code").unwrap().retry_count, 1);
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn budget_exhaustion_is_a_replayable_terminal_state() {
        let path = wal("budget");
        let mut dag = DurableDag::open(&path).unwrap();
        dag.create_task(spec("code", &[])).unwrap();
        dag.exhaust_budget("code", "vault://reports/partial.json")
            .unwrap();
        drop(dag);
        let restored = DurableDag::open(&path).unwrap();
        assert!(matches!(
            restored.task("code").unwrap().status,
            TaskStatus::BudgetExhausted { .. }
        ));
        assert!(restored.task("code").unwrap().status.is_terminal());
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn incomplete_tail_record_is_ignored_during_recovery() {
        let path = wal("tail");
        let mut dag = DurableDag::open(&path).unwrap();
        dag.create_task(spec("code", &[])).unwrap();
        drop(dag);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"seq":2,"event"#).unwrap();
        drop(file);
        let mut restored = DurableDag::open(&path).unwrap();
        assert!(restored.task("code").is_some());
        assert_eq!(restored.refresh_ready().unwrap(), vec!["code"]);
        drop(restored);
        assert_eq!(
            DurableDag::open(&path)
                .unwrap()
                .task("code")
                .unwrap()
                .status,
            TaskStatus::Ready
        );
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn task_ids_cannot_escape_control_plane_paths() {
        let path = wal("unsafe_id");
        let mut dag = DurableDag::open(&path).unwrap();
        for task_id in ["", ".", "..", "../escape", "nested/task"] {
            assert!(matches!(
                dag.create_task(spec(task_id, &[])),
                Err(DagError::InvalidTransition { .. })
            ));
        }
        dag.create_task(spec("safe.task_1-2", &[])).unwrap();
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
