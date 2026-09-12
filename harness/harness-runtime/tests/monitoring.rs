//! monitoring 模块端到端验证：契约冻结、Tap 非阻塞、WAL 序号、Hot Guard 收敛。

use std::sync::Arc;

use harness_runtime::monitoring::event::{
    AgentEventEnvelope, EventClass, EventKind, SCHEMA_VERSION, Severity,
};
use harness_runtime::monitoring::hot_guard::{
    DEFAULT_MAX_REPEAT, GuardAction, GuardVerdict, HotGuard, conclusion_fingerprint,
};
use harness_runtime::monitoring::tap::{EventTap, TapStats};
use harness_runtime::monitoring::wal::{WalStats, spawn_wal_writer};

#[test]
fn event_envelope_schema_frozen_and_roundtrip() {
    let event = AgentEventEnvelope::new(
        "s-1",
        EventClass::ControlFlow,
        EventKind::TurnStarted,
        1_700_000_000_000,
    )
    .with_turn("t-1")
    .with_severity(Severity::S3)
    .with_evidence_delta(1);

    assert_eq!(event.schema_version, SCHEMA_VERSION);

    let json = serde_json::to_string(&event).expect("serialize");
    let back: AgentEventEnvelope = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.session_id, "s-1");
    assert_eq!(back.turn_id.as_deref(), Some("t-1"));
    assert_eq!(back.evidence_delta, 1);
    assert!(matches!(back.kind, EventKind::TurnStarted));
}

#[test]
fn event_envelope_ignores_unknown_fields_forward_compat() {
    // 向前兼容：未知字段被忽略（§6：未知字段向前忽略、向后可读）。
    let json = r#"{
        "schema_version": 1,
        "ts_ms": 42,
        "session_id": "s-2",
        "class": "Health",
        "kind": "TurnFinished",
        "future_field": {"anything": true}
    }"#;
    let event: AgentEventEnvelope = serde_json::from_str(json).expect("unknown fields ignored");
    assert_eq!(event.session_id, "s-2");
    assert_eq!(event.ts_ms, 42);
}

#[test]
fn fingerprint_normalizes_whitespace_punctuation_and_case() {
    let a = conclusion_fingerprint("无法修复该错误。");
    let b = conclusion_fingerprint(" 无法修复该错误 ");
    let c = conclusion_fingerprint("Fixed, DONE!");
    let d = conclusion_fingerprint("fixed done");
    assert_eq!(a, b, "空白差异不应改变指纹");
    assert_eq!(c, d, "大小写与标点差异不应改变指纹");
    assert_ne!(a, c, "语义不同必须指纹不同");
}

#[test]
fn hot_guard_passes_while_evidence_grows() {
    let mut guard = HotGuard::default();
    for _ in 0..(DEFAULT_MAX_REPEAT + 3) {
        // 同一结论但每次都有新证据 → 永不判 stagnation（避免误杀推进中的多步任务）。
        assert_eq!(
            guard.observe("需要继续排查", 1),
            GuardVerdict::Pass,
            "有证据增长时必须放行"
        );
    }
}

#[test]
fn hot_guard_terminates_on_repeated_conclusion_without_evidence() {
    let mut guard = HotGuard::default();
    assert_eq!(guard.observe("我卡住了", 0), GuardVerdict::Pass);
    assert_eq!(guard.observe("我卡住了", 0), GuardVerdict::Pass);
    let verdict = guard.observe("我卡住了", 0);
    let GuardVerdict::Stagnation {
        fingerprint: fp_ref,
        repeat_count,
    } = &verdict
    else {
        panic!("第三次无证据重复必须判 stagnation，实际 {verdict:?}");
    };
    assert_eq!(*repeat_count, DEFAULT_MAX_REPEAT + 1);
    assert_eq!(fp_ref, &conclusion_fingerprint("我卡住了"));

    let (action, events) = guard.act(&verdict, "s-3", 999);
    let GuardAction::TerminateWithStructuredFailure { reason } = action else {
        panic!("stagnation 必须产生终止动作");
    };
    assert!(reason.contains(fp_ref));
    assert_eq!(events.len(), 2, "必须产出异常 + 止血两条事件");
    assert!(matches!(events[0].class, EventClass::Anomaly));
    assert!(matches!(events[1].class, EventClass::Mitigation));
    assert!(matches!(
        events[1].kind,
        EventKind::GuardTerminated { .. }
    ));
}

#[test]
fn hot_guard_resets_repeat_count_on_new_evidence() {
    let mut guard = HotGuard::default();
    assert_eq!(guard.observe("结论X", 0), GuardVerdict::Pass);
    assert_eq!(guard.observe("结论X", 0), GuardVerdict::Pass);
    // 新证据清零重复计数。
    assert_eq!(guard.observe("结论X", 1), GuardVerdict::Pass);
    // 重新计数，需再满 N+1 次才触发。
    assert_eq!(guard.observe("结论X", 0), GuardVerdict::Pass);
    assert_eq!(guard.observe("结论X", 0), GuardVerdict::Pass);
    assert!(matches!(
        guard.observe("结论X", 0),
        GuardVerdict::Stagnation { .. }
    ));
}

#[tokio::test]
async fn tap_is_non_blocking_and_counts_drops() {
    let capacity = 4;
    let (tap, _rx) = EventTap::new(capacity);
    let stats: Arc<TapStats> = tap.stats();

    // 填满队列（不消费）。
    for i in 0..capacity {
        tap.emit(AgentEventEnvelope::new(
            "s-4",
            EventClass::ControlFlow,
            EventKind::TurnStarted,
            i as u64,
        ));
    }
    assert_eq!(stats.sent() as usize, capacity);
    assert_eq!(stats.dropped(), 0);

    // 队列满后继续 emit：立即返回、只计数（失败开放，绝不阻塞）。
    for i in 0..10 {
        tap.emit(AgentEventEnvelope::new(
            "s-4",
            EventClass::ControlFlow,
            EventKind::TurnStarted,
            i as u64,
        ));
    }
    assert_eq!(stats.dropped(), 10);
}

#[tokio::test]
async fn disabled_tap_counts_everything_as_dropped() {
    let tap = EventTap::disabled();
    tap.emit(AgentEventEnvelope::new(
        "s-5",
        EventClass::ControlFlow,
        EventKind::TurnStarted,
        0,
    ));
    assert_eq!(tap.stats().sent(), 0);
    assert_eq!(tap.stats().dropped(), 1);
}

#[tokio::test]
async fn wal_writer_assigns_monotonic_seq_and_persists_jsonl() {
    let dir = std::env::temp_dir().join(format!(
        "harness-monitor-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    let (tap, rx) = EventTap::new(16);
    let stats = Arc::new(WalStats::default());
    let (handle, _wal_stats) = spawn_wal_writer(&dir, rx).expect("spawn wal writer");

    for i in 0..5u64 {
        tap.emit(AgentEventEnvelope::new(
            "s-6",
            EventClass::Evidence,
            EventKind::ConclusionEmitted {
                fingerprint: format!("fp-{i}"),
            },
            i,
        ));
    }
    drop(tap); // 关闭发送端 → writer 排空后退出。
    handle.await.expect("wal writer join");

    // 读回 jsonl：行数一致、seq 单调递增从 1 开始、契约字段完整。
    let spool_dir = dir;
    let mut files: Vec<_> = std::fs::read_dir(&spool_dir)
        .expect("spool dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();
    assert_eq!(files.len(), 1, "单文件 spool（未触发轮转）");

    let content = std::fs::read_to_string(&files[0]).expect("read spool");
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 5);
    for (idx, line) in lines.iter().enumerate() {
        let event: AgentEventEnvelope = serde_json::from_str(line).expect("jsonl line");
        assert_eq!(event.seq, idx as u64 + 1, "seq 必须单调递增");
        assert_eq!(event.schema_version, SCHEMA_VERSION);
        assert_eq!(event.session_id, "s-6");
    }

    let _ = std::fs::remove_dir_all(&spool_dir);
    let _ = stats;
}
