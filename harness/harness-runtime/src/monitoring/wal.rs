//! Dedicated writer thread, recoverable sequence/hash chain and bounded flushing.
use std::{fs::{self, OpenOptions}, io::{self, Write, BufRead}, path::{Path, PathBuf},
    sync::{Arc, atomic::{AtomicU64, Ordering}}, time::Duration};
use super::{event::{AgentEventEnvelope, EventClass, EventKind, SCHEMA_VERSION}, tap::TapReceiver};
pub const WAL_ROTATE_BYTES: u64 = 64 * 1024 * 1024;
#[derive(Debug, Default)]
pub struct WalStats { pub written: AtomicU64, pub write_errors: AtomicU64, pub rotations: AtomicU64 }
impl WalStats {
    pub fn written(&self) -> u64 { self.written.load(Ordering::Relaxed) }
    pub fn write_errors(&self) -> u64 { self.write_errors.load(Ordering::Relaxed) }
}
pub struct WalWriter {
    dir: PathBuf, file_index: u64, current_bytes: u64, writer: fs::File,
    seq: u64, hash: String, stats: Arc<WalStats>, _lock: fs::File,
}
impl WalWriter {
    pub fn open(dir: impl AsRef<Path>, stats: Arc<WalStats>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let lock = super::storage::lock(&dir)?;
        let mut files: Vec<_> = fs::read_dir(&dir)?.filter_map(Result::ok)
            .map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "jsonl")
                && p.file_name().unwrap().to_string_lossy().starts_with("events-")).collect();
        files.sort();
        let mut seq = 0;
        let mut hash = String::new();
        for (index, path) in files.iter().enumerate() {
            let file = OpenOptions::new().read(true).write(true).open(path)?;
            let mut reader = io::BufReader::new(&file);
            let mut offset = 0;
            loop {
                let mut line = Vec::new();
                let size = reader.read_until(b'\n', &mut line)?;
                if size == 0 { break; }
                if !line.ends_with(b"\n") && index + 1 == files.len() {
                    fs::write(path.with_extension("partial"), &line)?;
                    file.set_len(offset)?;
                    file.sync_all()?;
                    break;
                }
                let ev: AgentEventEnvelope = serde_json::from_slice(&line).map_err(io::Error::other)?;
                if ev.schema_version != SCHEMA_VERSION || ev.seq <= seq {
                    return Err(io::Error::other("WAL schema or sequence mismatch"));
                }
                if !ev.content_hash.is_empty() && (ev.previous_hash != hash || ev.digest() != ev.content_hash) {
                    return Err(io::Error::other("WAL hash chain mismatch"));
                }
                seq = ev.seq;
                hash = ev.content_hash;
                offset += size as u64;
            }
        }
        let file_index = files.last().and_then(|p| p.file_stem()).and_then(|s| s.to_str())
            .and_then(|s| s.strip_prefix("events-")).and_then(|s| s.parse().ok()).unwrap_or(1);
        let path = dir.join(format!("events-{file_index:06}.jsonl"));
        let writer = OpenOptions::new().create(true).append(true).open(path)?;
        let current_bytes = writer.metadata()?.len();
        Ok(Self { dir, file_index, current_bytes, writer, seq, hash, stats, _lock: lock })
    }
    fn append_checked(&mut self, mut event: AgentEventEnvelope) -> io::Result<()> {
        if self.current_bytes >= WAL_ROTATE_BYTES {
            self.writer.sync_all()?;
            self.file_index += 1;
            self.writer = OpenOptions::new().create_new(true).write(true)
                .open(self.dir.join(format!("events-{:06}.jsonl", self.file_index)))?;
            self.current_bytes = 0;
            self.stats.rotations.fetch_add(1, Ordering::Relaxed);
        }
        if let EventKind::GuardTerminated { reason } = &mut event.kind {
            *reason = "runtime terminated a no-progress loop".into();
        }
        event.seq = self.seq + 1;
        event.previous_hash = self.hash.clone();
        event.content_hash = event.digest();
        let mut bytes = serde_json::to_vec(&event)?;
        bytes.push(b'\n');
        if let Err(error) = self.writer.write_all(&bytes) {
            self.writer.set_len(self.current_bytes)?;
            return Err(error);
        }
        self.current_bytes += bytes.len() as u64;
        self.seq = event.seq;
        self.hash = event.content_hash.clone();
        if event.critical() { self.writer.sync_data()?; }
        self.stats.written.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    pub fn append(&mut self, event: AgentEventEnvelope) {
        if let Err(error) = self.append_checked(event) {
            self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(%error, "monitor WAL write failed");
        }
    }
    pub fn flush(&mut self) {
        if let Err(error) = self.writer.sync_data() {
            self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(%error, "monitor WAL flush failed");
        }
    }
}
pub fn spawn_wal_writer(dir: impl AsRef<Path>, rx: TapReceiver)
    -> io::Result<(tokio::task::JoinHandle<()>, Arc<WalStats>)> {
    let stats = Arc::new(WalStats::default());
    let mut writer = WalWriter::open(dir, stats.clone())?;
    let (done, completed) = tokio::sync::oneshot::channel();
    std::thread::Builder::new().name("monitor-wal".into()).spawn(move || {
        let mut reported = 0;
        loop {
            let dropped = rx.stats.dropped();
            if dropped > reported {
                writer.append(AgentEventEnvelope::new("monitor", EventClass::Health,
                    EventKind::TapOverflow { dropped_total: dropped }, crate::lha::now_ms()));
                reported = dropped;
            }
            match rx.try_recv() {
                Ok(event) => writer.append(event),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    writer.flush();
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
        writer.flush();
        drop(writer);
        let _ = done.send(());
    })?;
    Ok((tokio::spawn(async move { let _ = completed.await; }), stats))
}
