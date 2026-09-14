//! 事故库与质量体系（Phase 2，§6.3/§8）。
//!
//! 离线消费 WAL（jsonl）事件流：
//! - `read_wal`：顺序读入，对截断尾行容错（崩溃语义：最后一行可能写一半）；
//! - `IncidentBook::fold`：把止血类事件归并为 `Incident`（以 GuardTerminated 为一次事故）；
//! - `quality_report`：输出会话级质量指标（事故数、按严重度分布、最大重复次数、
//!   采集溢出丢弃数），供评估/回放消费。
//!
//! 只读，不写回；不参与在线路径，因此对主线程零开销。

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::event::{AgentEventEnvelope, EventKind, Severity};

/// 事故生命周期状态（§6.3 状态机）。
///
/// `Open`（已观测异常、尚未止血）→ `Mitigated`（GuardTerminated 已止血）
/// → `Closed`（确认恢复后结案）。迁移单向且不可跳步：未止血不得直接结案，
/// 防止"跳过止血直接注销事故"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IncidentState {
    Open,
    Mitigated,
    Closed,
}

/// 一次事故：以 GuardTerminated（止血动作）为锚点，聚合其前的异常信号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incident {
    /// 稳定事故 ID：`{session_id}#{terminated_seq}`，跨重启可复现、可去重。
    pub incident_id: String,
    pub session_id: String,
    /// 触发止血的事件序号。
    pub terminated_seq: u64,
    pub ts_ms: u64,
    pub reason: String,
    /// 该事故窗口内观测到的最大重复结论次数。
    pub max_repeat: u32,
    /// 窗口内异常事件数（RepeatedConclusion 等）。
    pub anomaly_count: u32,
    pub state: IncidentState,
}

/// 事故库：按会话聚合的事故集合。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct IncidentBook {
    pub incidents: Vec<Incident>,
}

impl IncidentBook {
    /// 把一段事件流折叠为事故集合。GuardTerminated 关闭当前事故窗口。
    pub fn fold(events: &[AgentEventEnvelope]) -> Self {
        let mut book = Self::default();
        // 当前会话的开放窗口（尚未被 GuardTerminated 关闭）。
        let mut windows = std::collections::HashMap::new();
        let mut seen = std::collections::HashSet::new();

        for ev in events {
            if !ev.event_id.is_empty() && !seen.insert(ev.event_id.clone()) { continue; }
            let key = (ev.session_id.clone(), ev.turn_id.clone());
            match &ev.kind {
                EventKind::RepeatedConclusion { repeat_count, .. } => {
                    let window = windows.entry(key).or_insert((0u32, 0u32));
                    window.0 = window.0.max(*repeat_count);
                    window.1 += 1;
                }
                EventKind::GuardTerminated { reason } => {
                    let (window_max_repeat, window_anomalies) = windows.remove(&key).unwrap_or_default();
                    book.incidents.push(Incident {
                        incident_id: format!("{}#{}", ev.session_id, ev.seq),
                        session_id: ev.session_id.clone(),
                        terminated_seq: ev.seq,
                        ts_ms: ev.ts_ms,
                        reason: reason.clone(),
                        max_repeat: window_max_repeat,
                        anomaly_count: window_anomalies,
                        state: IncidentState::Mitigated,
                    });
                }
                _ => {}
            }
        }
        book
    }
}

/// 会话级质量报告。
#[derive(Debug, Default, Serialize)]
pub struct QualityReport {
    pub session_id: String,
    pub total_events: u64,
    pub incidents: u32,
    /// 按严重度分布（S0..S3）。
    pub severity_hist: [u32; 4],
    /// 全流观测到的最大重复结论次数（退化程度代理指标）。
    pub max_repeat: u32,
    /// 采集溢出导致的丢弃总数（监控自身健康）。
    pub tap_dropped: u64,
}

/// 顺序读取 WAL（jsonl）。对末尾截断行容错：解析失败仅统计不中断。
/// 返回 `(事件, 坏行数)`。
pub fn read_wal(path: &Path) -> std::io::Result<(Vec<AgentEventEnvelope>, u64)> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut events = Vec::new();
    let mut bad = 0u64;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 { break; }
        if !line.ends_with('\n') { bad += 1; break; }
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<AgentEventEnvelope>(&line) {
            Ok(ev) if ev.schema_version == super::event::SCHEMA_VERSION => {
                if !ev.content_hash.is_empty() && ev.digest() != ev.content_hash {
                    return Err(std::io::Error::other("event hash mismatch"));
                }
                events.push(ev);
            }
            Ok(_) => return Err(std::io::Error::other("unsupported monitoring schema")),
            Err(e) => return Err(std::io::Error::other(e)),
        }
    }
    Ok((events, bad))
}

/// 由事件流生成质量报告（单会话视角；多会话取首个 session_id）。
pub fn quality_report(events: &[AgentEventEnvelope]) -> QualityReport {
    let session = events.first().map(|e| e.session_id.as_str()).unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let filtered = events.iter().filter(|e| e.session_id == session)
        .filter(|e| e.event_id.is_empty() || seen.insert(e.event_id.clone())).cloned().collect::<Vec<_>>();
    let events = filtered.as_slice();
    let book = IncidentBook::fold(events);
    let mut report = QualityReport {
        session_id: events
            .first()
            .map(|e| e.session_id.clone())
            .unwrap_or_default(),
        total_events: events.len() as u64,
        incidents: book.incidents.len() as u32,
        ..QualityReport::default()
    };
    for ev in events {
        if let Some(sev) = ev.severity {
            let idx = match sev {
                Severity::S0 => 0,
                Severity::S1 => 1,
                Severity::S2 => 2,
                Severity::S3 => 3,
            };
            report.severity_hist[idx] += 1;
        }
        match &ev.kind {
            EventKind::RepeatedConclusion { repeat_count, .. } => {
                report.max_repeat = report.max_repeat.max(*repeat_count);
            }
            EventKind::TapOverflow { dropped_total } => {
                report.tap_dropped = report.tap_dropped.max(*dropped_total);
            }
            _ => {}
        }
    }
    report
}
