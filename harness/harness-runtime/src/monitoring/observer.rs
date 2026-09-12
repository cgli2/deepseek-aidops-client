//! Independent observer entrypoint and bounded, fail-open process supervision.
use std::{fs, io, path::{Path, PathBuf}, time::Duration};
use super::{incident::{read_wal, IncidentBook, quality_report}, storage::atomic_json};

pub fn collect(spool: &Path) -> io::Result<()> {
    let _lock = super::storage::lock(&spool.join("observer"))?;
    let mut paths: Vec<_> = fs::read_dir(spool)?.filter_map(Result::ok).map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl")).collect();
    paths.sort();
    let mut events = Vec::new();
    let mut last_seq = 0;
    let mut hash = String::new();
    let mut total_bytes = 0u64;
    for p in paths {
        total_bytes = total_bytes.saturating_add(fs::metadata(&p)?.len());
        if total_bytes > 128 * 1024 * 1024 { return Err(io::Error::other("observer input budget exceeded")); }
        let (batch, partial) = read_wal(&p)?;
        if partial != 0 { return Err(io::Error::other("incomplete WAL; retry after writer drain")); }
        for event in batch {
            if event.seq <= last_seq || (!event.content_hash.is_empty() && event.previous_hash != hash) {
                return Err(io::Error::other("observer WAL continuity failed"));
            }
            last_seq = event.seq;
            hash = event.content_hash.clone();
            events.push(event);
        }
    }
    events.sort_by_key(|e| if e.source_seq > 0 { e.source_seq } else { e.seq });
    let mut sessions = std::collections::BTreeSet::new();
    sessions.extend(events.iter().map(|e| e.session_id.clone()));
    let reports = sessions.iter().map(|id| {
        let filtered: Vec<_> = events.iter().filter(|e| &e.session_id == id).cloned().collect();
        let incidents = IncidentBook::fold(&filtered);
        let proposals: Vec<_> = incidents.incidents.iter().map(super::proposal::ImprovementProposal::generate_from_incident).collect();
        serde_json::json!({"session": id, "quality": quality_report(&filtered),
            "incidents": incidents, "candidate_proposals": proposals,
            "assessment": "deterministic metadata only; not business verification",
            "active_policy": "builtin-v1", "promotion_available": false})
    }).collect::<Vec<_>>();
    // Reports are derived/idempotent; cursor advances only after durable report write.
    atomic_json(&spool.join("observer/report.json"), &reports)?;
    atomic_json(&spool.join("observer/cursor.json"), &serde_json::json!({"seq": last_seq, "hash": hash}))?;
    atomic_json(&spool.join("observer/health.json"), &serde_json::json!({"state": "healthy", "updated_ms": crate::lha::now_ms()}))
}

pub async fn supervise(spool: PathBuf) {
    let result = async {
        let exe = std::env::current_exe()?;
        let mut dir = exe.parent().ok_or_else(|| io::Error::other("no executable directory"))?;
        if dir.file_name().is_some_and(|n| n == "deps") { dir = dir.parent().unwrap(); }
        let observer = dir.join(if cfg!(windows) { "harness-observer.exe" } else { "harness-observer" });
        if !observer.is_file() { return Err(io::Error::other("harness-observer is not installed beside the runtime")); }
        for attempt in 0..3 {
            let mut command = tokio::process::Command::new(&observer);
            command.arg("--spool").arg(&spool).kill_on_drop(true)
                .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            #[cfg(windows)]
            command.creation_flags(0x08000000);
            match tokio::time::timeout(Duration::from_secs(10), command.status()).await {
                Ok(Ok(status)) if status.success() => return Ok(()),
                _ => tokio::time::sleep(Duration::from_millis(100 << attempt)).await,
            }
        }
        Err(io::Error::other("observer failed after 3 bounded attempts"))
    }.await;
    if let Err(error) = result {
        tracing::warn!(%error, "observer degraded; main task unaffected");
        let _ = atomic_json(&spool.join("observer/health.json"),
            &serde_json::json!({"state": "ObserverDegraded", "reason": error.to_string(), "updated_ms": crate::lha::now_ms()}));
    }
}
