//! AgentEventEnvelope：自监控事件的权威契约（schema v1，冻结）。
//!
//! 规则（§0/§6）：
//! - 事件记录"发生过什么"，不记录"应该发生什么"；
//! - 新增字段升 minor、禁止原地改语义；未知字段向前忽略、向后可读；
//! - 不复制大正文进事件，只放摘要与证据引用（脱敏在进入持久化前完成，Phase 1 先不落敏感源）。

use serde::{Deserialize, Serialize};

/// 当前冻结的 schema 版本。回放/归并/评估都以此对齐。
pub const SCHEMA_VERSION: u32 = 1;

/// 事件严重度（§6.2：S0 致命 … S3 提示）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Severity {
    S0,
    S1,
    S2,
    S3,
}

/// 事件大类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventClass {
    /// 控制流：阶段迁移、回合开始/结束、预算消耗。
    ControlFlow,
    /// 证据流：工具结果、diff、测试输出等可复核事实的引用。
    Evidence,
    /// 异常流：确定性 detector 命中的异常信号。
    Anomaly,
    /// 止血流：Hot Guard 的干预动作（收敛/降级/熔断）。
    Mitigation,
    /// 健康流：监控系统自身健康（避免"监控坏了却显示正常"）。
    Health,
}

/// 事件类型。Phase 1 只冻结最小必要集，后续升 minor 追加。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    TurnStarted,
    TurnFinished,
    /// 模型输出了一条结论性文本（供重复结论检测）。
    ConclusionEmitted { fingerprint: String },
    /// 确定性 detector：同一结论重复且证据增量为 0。
    RepeatedConclusion { fingerprint: String, repeat_count: u32 },
    /// Hot Guard 收敛：由 Runtime 直接生成结构化终态，不再送模型。
    GuardTerminated { reason: String },
    /// 事件采集溢出（队列满被丢弃）。
    TapOverflow { dropped_total: u64 },
    StepStarted,
    ToolCompleted { ok: bool },
    DeliveryJudged { outcome: String, criteria: usize, evidenced: usize },
}

/// 事件信封：所有事件的统一外壳。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEventEnvelope {
    #[serde(default)]
    pub source_seq: u64,
    #[serde(default)]
    pub event_id: String,
    #[serde(default)]
    pub previous_hash: String,
    #[serde(default)]
    pub content_hash: String,
    pub schema_version: u32,
    /// 单调递增事件序号（writer 侧赋值，WAL 内唯一）。
    #[serde(default)]
    pub seq: u64,
    /// 毫秒时间戳。
    pub ts_ms: u64,
    /// 稳定关联键：session/turn/step。回放与归并的主键。
    pub session_id: String,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub step_id: Option<u32>,
    /// 版本维度：runtime 与 policy 版本，便于跨版本比较事故率。
    #[serde(default)]
    pub runtime_version: String,
    #[serde(default)]
    pub policy_version: String,
    pub class: EventClass,
    pub kind: EventKind,
    #[serde(default)]
    pub severity: Option<Severity>,
    /// 证据增量计数（0/1）。重复检测以"证据是否增长"而非"轮次数"判定。
    #[serde(default)]
    pub evidence_delta: u8,
}

impl AgentEventEnvelope {
    /// 构造最小事件：版本/时间戳/会话关联 + 分类与类型。
    pub fn new(
        session_id: impl Into<String>,
        class: EventClass,
        kind: EventKind,
        ts_ms: u64,
    ) -> Self {
        Self {
            source_seq: 0,
            event_id: uuid::Uuid::new_v4().to_string(),
            previous_hash: String::new(),
            content_hash: String::new(),
            schema_version: SCHEMA_VERSION,
            seq: 0,
            ts_ms,
            session_id: session_id.into(),
            turn_id: None,
            step_id: None,
            runtime_version: env!("CARGO_PKG_VERSION").to_string(),
            policy_version: "builtin-v1".into(),
            class,
            kind,
            severity: None,
            evidence_delta: 0,
        }
    }

    pub fn with_turn(mut self, turn_id: impl Into<String>) -> Self {
        self.turn_id = Some(turn_id.into());
        self
    }

    pub fn critical(&self) -> bool {
        matches!(self.class, EventClass::Anomaly | EventClass::Mitigation | EventClass::Health)
            || matches!(self.kind, EventKind::TurnFinished | EventKind::DeliveryJudged { .. } | EventKind::ToolCompleted { .. })
    }

    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut copy = self.clone();
        copy.content_hash.clear();
        format!("{:x}", Sha256::digest(serde_json::to_vec(&copy).expect("event serialization")))
    }

    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = Some(severity);
        self
    }

    pub fn with_evidence_delta(mut self, delta: u8) -> Self {
        self.evidence_delta = delta;
        self
    }
}
