//! Capability-based deterministic worker routing.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum WorkerRole {
    Planner,
    Coder,
    Verifier,
    Aggregator,
    ConflictResolver,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ModelClass {
    Fast,
    Code,
    Reasoning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerDescriptor {
    pub worker_id: String,
    pub roles: Vec<WorkerRole>,
    pub model_class: ModelClass,
    pub active_tasks: u32,
    pub tokens_per_minute: u64,
    pub affinity_paths: Vec<PathBuf>,
    pub healthy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingRequirements {
    pub role: WorkerRole,
    pub minimum_model_class: ModelClass,
    pub target_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingDecision {
    pub worker_id: String,
    pub score: i64,
}

#[derive(Default)]
pub struct CapabilityRouter {
    workers: BTreeMap<String, WorkerDescriptor>,
}

impl CapabilityRouter {
    pub fn register(&mut self, worker: WorkerDescriptor) -> Result<(), String> {
        if worker.worker_id.trim().is_empty() || worker.roles.is_empty() {
            return Err("worker id and at least one role are required".into());
        }
        self.workers.insert(worker.worker_id.clone(), worker);
        Ok(())
    }

    pub fn update_load(&mut self, worker_id: &str, active_tasks: u32) -> bool {
        self.workers.get_mut(worker_id).is_some_and(|worker| {
            worker.active_tasks = active_tasks;
            true
        })
    }

    pub fn select(&self, requirements: &RoutingRequirements) -> Option<RoutingDecision> {
        self.workers
            .values()
            .filter(|worker| {
                worker.healthy
                    && worker.roles.contains(&requirements.role)
                    && worker.model_class >= requirements.minimum_model_class
            })
            .map(|worker| RoutingDecision {
                worker_id: worker.worker_id.clone(),
                score: score(worker, requirements),
            })
            .max_by(|left, right| {
                left.score
                    .cmp(&right.score)
                    .then_with(|| right.worker_id.cmp(&left.worker_id))
            })
    }
}

fn score(worker: &WorkerDescriptor, requirements: &RoutingRequirements) -> i64 {
    let affinity = requirements
        .target_paths
        .iter()
        .filter(|target| {
            worker
                .affinity_paths
                .iter()
                .any(|cached| target.starts_with(cached) || cached.starts_with(target))
        })
        .count() as i64;
    let model_headroom = (worker.model_class as i64 - requirements.minimum_model_class as i64) * 25;
    10_000 + affinity * 250 + model_headroom
        - i64::from(worker.active_tasks) * 500
        - (worker.tokens_per_minute / 1_000).min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_respects_role_health_load_and_affinity() {
        let mut router = CapabilityRouter::default();
        for (id, active, affinity, healthy) in [
            ("busy", 4, "src", true),
            ("affine", 0, "src/runtime", true),
            ("dead", 0, "src/runtime", false),
        ] {
            router
                .register(WorkerDescriptor {
                    worker_id: id.into(),
                    roles: vec![WorkerRole::Coder],
                    model_class: ModelClass::Code,
                    active_tasks: active,
                    tokens_per_minute: 1_000,
                    affinity_paths: vec![PathBuf::from(affinity)],
                    healthy,
                })
                .unwrap();
        }
        let decision = router
            .select(&RoutingRequirements {
                role: WorkerRole::Coder,
                minimum_model_class: ModelClass::Code,
                target_paths: vec![PathBuf::from("src/runtime/lib.rs")],
            })
            .unwrap();
        assert_eq!(decision.worker_id, "affine");
    }
}
