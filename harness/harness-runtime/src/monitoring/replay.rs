//! Phase 3：回放规约与仿真执行器（§6.4 / §9）。
//!
//! 提供：
//! - `ReplayStep`: 单步输入与期望输出/工具调用桩
//! - `ReplaySpec`: 从事件流或事故集生成的确定性回放规格说明
//! - `DeterministicRunner`: 基于桩数据的确定性执行器
//! - `FaultInjection`: 故障注入（超时、乱序、截断、错误）

use serde::{Deserialize, Serialize};
use super::event::AgentEventEnvelope;
use super::incident::Incident;

/// 故障注入模式
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FaultKind {
    ToolTimeout { tool_name: String },
    CorruptedOutput { step_index: usize },
    ForcedError { step_index: usize, message: String },
}

/// 单次回放步骤的桩定义
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayStep {
    pub step_id: usize,
    pub input_prompt: String,
    pub mocked_model_response: String,
    pub tool_calls: Vec<MockedToolCall>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockedToolCall {
    pub tool_name: String,
    pub arguments_json: String,
    pub simulated_output: String,
}

/// 回放规格说明
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaySpec {
    pub spec_id: String,
    pub origin_session_id: String,
    pub description: String,
    pub steps: Vec<ReplayStep>,
    pub injected_faults: Vec<FaultKind>,
}

impl ReplaySpec {
    /// 从 Phase 2 的 Incident 及相关联的事件序列构建 ReplaySpec
    pub fn from_incident(incident: &Incident, events: &[AgentEventEnvelope]) -> Self {
        let mut steps = Vec::new();
        let mut step_idx = 0;

        for ev in events {
            if ev.session_id != incident.session_id {
                continue;
            }
            // 将与该事故相关的事件抽象转换为回放桩步骤
            steps.push(ReplayStep {
                step_id: step_idx,
                input_prompt: format!("Event seq={}", ev.seq),
                mocked_model_response: format!("{:?}", ev.kind),
                tool_calls: Vec::new(),
            });
            step_idx += 1;
        }

        Self {
            spec_id: format!("replay-inc-{}", incident.terminated_seq),
            origin_session_id: incident.session_id.clone(),
            description: format!("Replay generated from incident: {}", incident.reason),
            steps,
            injected_faults: Vec::new(),
        }
    }

    /// 添加故障注入
    pub fn with_fault(mut self, fault: FaultKind) -> Self {
        self.injected_faults.push(fault);
        self
    }
}

/// 回放结果
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayOutcome {
    pub spec_id: String,
    pub completed_steps: usize,
    pub success: bool,
    pub failure_reason: Option<String>,
}

/// 确定性仿真回放运行器
pub struct DeterministicRunner;

impl DeterministicRunner {
    /// 运行 ReplaySpec 评测，保证 100% 确定性结果
    pub fn run(spec: &ReplaySpec) -> ReplayOutcome {
        let mut guard = super::hot_guard::HotGuard::default();
        let mut evidence = std::collections::HashSet::new();
        if spec.steps.is_empty() {
            return ReplayOutcome { spec_id: spec.spec_id.clone(), completed_steps: 0,
                success: false, failure_reason: Some("empty replay has no verification evidence".into()) };
        }
        for (i, step) in spec.steps.iter().enumerate() {
            // 检查是否有针对当前步骤的故障注入
            for fault in &spec.injected_faults {
                match fault {
                    FaultKind::ToolTimeout { tool_name } if step.tool_calls.iter().any(|t| &t.tool_name == tool_name) => {
                        return ReplayOutcome { spec_id: spec.spec_id.clone(), completed_steps: i,
                            success: false, failure_reason: Some(format!("tool timeout: {tool_name}")) };
                    }
                    FaultKind::CorruptedOutput { step_index } if *step_index == i => {
                        return ReplayOutcome {
                            spec_id: spec.spec_id.clone(),
                            completed_steps: i,
                            success: false,
                            failure_reason: Some("Fault injected: CorruptedOutput".to_string()),
                        };
                    }
                    FaultKind::ForcedError { step_index, message } if *step_index == i => {
                        return ReplayOutcome {
                            spec_id: spec.spec_id.clone(),
                            completed_steps: i,
                            success: false,
                            failure_reason: Some(format!("Fault injected: {}", message)),
                        };
                    }
                    _ => {}
                }
            }
            let delta = step.tool_calls.iter().filter(|call| !call.simulated_output.trim().is_empty())
                .filter(|call| evidence.insert((call.tool_name.clone(), call.arguments_json.clone(), call.simulated_output.clone())))
                .count();
            if matches!(guard.observe(&step.mocked_model_response, u8::from(delta > 0)),
                super::hot_guard::GuardVerdict::Stagnation { .. }) {
                return ReplayOutcome { spec_id: spec.spec_id.clone(), completed_steps: i,
                    success: false, failure_reason: Some("repeated conclusion without evidence".into()) };
            }
            // 正常执行模拟步骤
            if step.mocked_model_response.is_empty() {
                return ReplayOutcome {
                    spec_id: spec.spec_id.clone(),
                    completed_steps: i,
                    success: false,
                    failure_reason: Some("Empty mocked model response".to_string()),
                };
            }
        }

        ReplayOutcome {
            spec_id: spec.spec_id.clone(),
            completed_steps: spec.steps.len(),
            success: true,
            failure_reason: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::monitoring::event::{EventClass, EventKind};
    use crate::monitoring::incident::IncidentState;

    #[test]
    fn test_replay_spec_from_incident_and_deterministic_run() {
        let incident = Incident {
            incident_id: "sess-test-p3#10".to_string(),
            session_id: "sess-test-p3".to_string(),
            terminated_seq: 10,
            anomaly_count: 2,
            max_repeat: 3,
            reason: "stagnation detected".to_string(),
            ts_ms: 1000,
            state: IncidentState::Mitigated,
        };

        let events = vec![
            AgentEventEnvelope::new("sess-test-p3", EventClass::ControlFlow, EventKind::TurnStarted, 100),
            AgentEventEnvelope::new("sess-test-p3", EventClass::Anomaly, EventKind::RepeatedConclusion {
                fingerprint: "fp1".to_string(),
                repeat_count: 2,
            }, 101),
            AgentEventEnvelope::new("other-sess", EventClass::Anomaly, EventKind::RepeatedConclusion {
                fingerprint: "fp2".to_string(),
                repeat_count: 5,
            }, 102),
        ];

        let spec = ReplaySpec::from_incident(&incident, &events);
        assert_eq!(spec.origin_session_id, "sess-test-p3");
        assert_eq!(spec.steps.len(), 2, "other-sess should be filtered out");

        let outcome = DeterministicRunner::run(&spec);
        assert!(outcome.success);
        assert_eq!(outcome.completed_steps, 2);
    }

    #[test]
    fn test_fault_injection_in_replay() {
        let spec = ReplaySpec {
            spec_id: "test-fault".to_string(),
            origin_session_id: "sess-1".to_string(),
            description: "Fault injection test".to_string(),
            steps: vec![
                ReplayStep {
                    step_id: 0,
                    input_prompt: "p0".to_string(),
                    mocked_model_response: "r0".to_string(),
                    tool_calls: vec![],
                },
                ReplayStep {
                    step_id: 1,
                    input_prompt: "p1".to_string(),
                    mocked_model_response: "r1".to_string(),
                    tool_calls: vec![],
                },
            ],
            injected_faults: vec![],
        }
        .with_fault(FaultKind::ForcedError {
            step_index: 1,
            message: "injected network error".to_string(),
        });

        let outcome = DeterministicRunner::run(&spec);
        assert!(!outcome.success);
        assert_eq!(outcome.completed_steps, 1);
        assert!(outcome.failure_reason.unwrap().contains("injected network error"));
    }
}
