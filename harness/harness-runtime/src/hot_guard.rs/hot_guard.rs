//! Hot Guard：主进程内的确定性在线保护（§11）。
//!
//! 职责：在最便宜的时机识别"不会变好"的循环并止血。
//! 首发 detector（§11.1）：同一结论重复 N 次且证据增量为 0 → 判 stagnation，
//! 由 Runtime 直接生成结构化终态，不再送模型（禁止空转消耗回合）。
//!
//! 判定以"证据是否增长"而非"轮次数"为准，避免误杀"正在推进的多步任务"。

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

use super::event::{AgentEventEnvelope, EventClass, EventKind, Severity};

/// 触发收敛前允许的最大重复次数（同一指纹、证据无增长）。
pub const DEFAULT_MAX_REPEAT: u32 = 2;

/// 结论归一化窗口：只保留最近 K 条指纹做比较，防内存膨胀。
const FINGERPRINT_WINDOW: usize = 64;

/// Hot Guard 判定结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    /// 正常，放行。
    Pass,
    /// 命中 stagnation：需要收敛。携带结构化原因。
    Stagnation { fingerprint: String, repeat_count: u32 },
}

/// Hot Guard 采取的动作（§11：先一次性收敛，不反复软提醒）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardAction {
    /// 无需动作。
    None,
    /// 终止当前回合并由 Runtime 生成结构化 SystemFailure。
    TerminateWithStructuredFailure { reason: String },
}

/// 结论文本 → 语义指纹（§11.1）。
/// 归一化：去空白/标点、小写化，降低"换说法但同义"的逃逸。
pub fn conclusion_fingerprint(text: &str) -> String {
    let mut normalized: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_ascii_punctuation())
        .flat_map(|c| c.to_lowercase())
        .collect();
    // 截断到固定上限，避免超长文本哈希成本与超长 key。
    const MAX_LEN: usize = 256;
    if normalized.len() > MAX_LEN {
        normalized.truncate(MAX_LEN);
    }
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// 确定性 Hot Guard。对每个 turn 维护结论指纹的滑动窗口。
#[derive(Debug)]
pub struct HotGuard {
    max_repeat: u32,
    /// 指纹 → 连续无证据增长的重复次数。
    repeat_counts: HashMap<String, u32>,
    /// 最近看到的指纹（保持窗口有界）。
    window: VecDeque<String>,
}

impl Default for HotGuard {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_REPEAT)
    }
}

impl HotGuard {
    pub fn new(max_repeat: u32) -> Self {
        Self {
            max_repeat: max_repeat.max(1),
            repeat_counts: HashMap::new(),
            window: VecDeque::new(),
        }
    }

    /// 观察一条模型结论。
    ///
    /// `evidence_delta`: 本步是否带来了新证据（工具结果/diff/测试输出）。
    /// 语义：同一结论**且**无证据增长才计数；有新证据立即清零。
    pub fn observe(&mut self, conclusion_text: &str, evidence_delta: u8) -> GuardVerdict {
        let fp = conclusion_fingerprint(conclusion_text);

        // 维护滑动窗口有界。
        self.window.push_back(fp.clone());
        if self.window.len() > FINGERPRINT_WINDOW {
            if let Some(old) = self.window.pop_front() {
                self.repeat_counts.remove(&old);
            }
        }

        if evidence_delta > 0 {
            // 有证据增长：该结论的"无进展重复"清零，正常推进。
            self.repeat_counts.insert(fp, 0);
            return GuardVerdict::Pass;
        }

        // 无证据增长：累加该结论的重复计数。
        let count = self.repeat_counts.entry(fp.clone()).or_insert(0);
        *count += 1;

        if *count > self.max_repeat {
            GuardVerdict::Stagnation {
                fingerprint: fp,
                repeat_count: *count,
            }
        } else {
            GuardVerdict::Pass
        }
    }

    /// 基于判定结果生成动作 + 异常/止血事件（写 WAL，供复盘）。
    pub fn act(&self, verdict: &GuardVerdict, session_id: &str, ts_ms: u64) -> (GuardAction, Vec<AgentEventEnvelope>) {
        match verdict {
            GuardVerdict::Pass => (GuardAction::None, Vec::new()),
            GuardVerdict::Stagnation {
                fingerprint,
                repeat_count,
            } => {
                let reason = format!(
                    "同一结论重复 {repeat_count} 次且证据增量为 0（fingerprint={fingerprint}），判定 stagnation，停止空转。"
                );
                let anomaly = AgentEventEnvelope::new(
                    session_id,
                    EventClass::Anomaly,
                    EventKind::RepeatedConclusion {
                        fingerprint: fingerprint.clone(),
                        repeat_count: *repeat_count,
                    },
                    ts_ms,
                )
                .with_severity(Severity::S2);
                let mitigation = AgentEventEnvelope::new(
                    session_id,
                    EventClass::Mitigation,
                    EventKind::GuardTerminated {
                        reason: reason.clone(),
                    },
                    ts_ms,
                )
                .with_severity(Severity::S2);
                (
                    GuardAction::TerminateWithStructuredFailure { reason },
                    vec![anomaly, mitigation],
                )
            }
        }
    }
}
