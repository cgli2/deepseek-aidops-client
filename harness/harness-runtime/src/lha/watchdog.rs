//! P1 lease watchdog for zombie-worker reclamation.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::DurableDag;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchdogEvent {
    Reclaimed(Vec<String>),
    Error(String),
    Stopped,
}

pub struct LeaseWatchdog {
    cancellation: CancellationToken,
    handle: Option<JoinHandle<()>>,
}

impl LeaseWatchdog {
    pub fn start(
        dag: Arc<Mutex<DurableDag>>,
        interval: Duration,
    ) -> (Self, mpsc::UnboundedReceiver<WatchdogEvent>) {
        let cancellation = CancellationToken::new();
        let child = cancellation.clone();
        let (events, receiver) = mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval.max(Duration::from_millis(10)));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = child.cancelled() => {
                        let _ = events.send(WatchdogEvent::Stopped);
                        break;
                    }
                    _ = ticker.tick() => {
                        let outcome = dag
                            .lock()
                            .map_err(|_| "durable DAG lock poisoned".to_string())
                            .and_then(|mut dag| {
                                let reclaimed = dag.reap_expired(now_ms()).map_err(|e| e.to_string())?;
                                if !reclaimed.is_empty() {
                                    dag.refresh_ready().map_err(|e| e.to_string())?;
                                }
                                Ok(reclaimed)
                            });
                        match outcome {
                            Ok(reclaimed) if !reclaimed.is_empty() => {
                                let _ = events.send(WatchdogEvent::Reclaimed(reclaimed));
                            }
                            Ok(_) => {}
                            Err(error) => {
                                let _ = events.send(WatchdogEvent::Error(error));
                            }
                        }
                    }
                }
            }
        });
        (
            Self {
                cancellation,
                handle: Some(handle),
            },
            receiver,
        )
    }

    pub async fn stop(mut self) {
        self.cancellation.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for LeaseWatchdog {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;

    use super::*;
    use crate::lha::{TaskSpec, TaskStatus};

    fn spec() -> TaskSpec {
        TaskSpec {
            task_id: "work".into(),
            parent_id: None,
            dependencies: vec![],
            inputs: json!({}),
            invariants: vec![],
            expected_output_schema: json!({}),
            timeout_seconds: 60,
            max_retries: 1,
        }
    }

    #[tokio::test]
    async fn watchdog_reclaims_expired_lease_and_requeues_task() {
        let root = std::env::temp_dir().join(format!("lha_watchdog_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let path: PathBuf = root.join("dag.jsonl");
        let mut dag = DurableDag::open(&path).unwrap();
        dag.create_task(spec()).unwrap();
        dag.refresh_ready().unwrap();
        dag.schedule("work", "dead-worker").unwrap();
        dag.start("work", "dead-worker", 0, 1).unwrap();
        let dag = Arc::new(Mutex::new(dag));
        let (watchdog, mut events) = LeaseWatchdog::start(dag.clone(), Duration::from_millis(10));
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event, WatchdogEvent::Reclaimed(vec!["work".into()]));
        assert_eq!(
            dag.lock().unwrap().task("work").unwrap().status,
            TaskStatus::Ready
        );
        watchdog.stop().await;
        fs::remove_dir_all(root).ok();
    }
}
