//! WAL writer：独立 tokio 任务，把事件追加到本地 jsonl spool（§16）。
//!
//! - 布局：`.harness/self-monitor/spool/events-YYYYMMDD-N.jsonl`；
//! - 赋 seq：writer 是唯一分配单调序号的地方（WAL 内唯一）；
//! - 失败开放：磁盘写失败只计数 + 发 Health 事件，绝不 panic / 阻塞主流程；
//! - 轮转：单文件超过阈值即切新文件（Phase 1 简化为按大小，日期在文件名中）。

use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::event::AgentEventEnvelope;

/// 单 spool 文件大小阈值（超过即轮转）。
pub const WAL_ROTATE_BYTES: u64 = 64 * 1024 * 1024;

/// WAL writer 统计。
#[derive(Debug, Default)]
pub struct WalStats {
    pub written: AtomicU64,
    pub write_errors: AtomicU64,
    pub rotations: AtomicU64,
}

impl WalStats {
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Relaxed)
    }

    pub fn write_errors(&self) -> u64 {
        self.write_errors.load(Ordering::Relaxed)
    }
}

/// 同步顺序写文件的 WAL。由专属任务持有，不跨线程共享。
pub struct WalWriter {
    dir: PathBuf,
    file_index: u64,
    current_bytes: u64,
    writer: Option<BufWriter<fs::File>>,
    seq: u64,
    stats: Arc<WalStats>,
}

impl WalWriter {
    /// 打开（必要时创建）spool 目录与首个文件。
    pub fn open(dir: impl AsRef<Path>, stats: Arc<WalStats>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let mut this = Self {
            dir,
            file_index: 0,
            current_bytes: 0,
            writer: None,
            seq: 0,
            stats,
        };
        this.rotate()?;
        Ok(this)
    }

    fn file_name(index: u64) -> String {
        // 日期取自文件创建时刻由调用方环境决定；这里用序号保证唯一与有序。
        format!("events-{index:06}.jsonl")
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file_index += 1;
        let path = self.dir.join(Self::file_name(self.file_index));
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        self.writer = Some(BufWriter::new(file));
        self.current_bytes = 0;
        self.stats.rotations.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// 追加一条事件（先赋 seq，再序列化为单行 JSON）。
    pub fn append(&mut self, mut event: AgentEventEnvelope) {
        self.seq += 1;
        event.seq = self.seq;

        let line = match serde_json::to_string(&event) {
            Ok(l) => l,
            Err(_) => {
                self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
                return;
            }
        };

        if self.current_bytes >= WAL_ROTATE_BYTES {
            if let Err(e) = self.rotate() {
                let _ = e; // 轮转失败仍尝试写当前文件。
                self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
            }
        }

        if let Some(w) = self.writer.as_mut() {
            match w.write_all(line.as_bytes()).and_then(|_| w.write_all(b"\n")) {
                Ok(()) => {
                    self.current_bytes += line.len() as u64 + 1;
                    self.stats.written.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    pub fn flush(&mut self) {
        if let Some(w) = self.writer.as_mut() {
            let _ = w.flush();
        }
    }
}

/// 启动 WAL writer 任务：持续消费 tap 队列，直到发送端全部断开。
/// 返回 join handle 与统计；主进程退出前可等待其排空。
pub fn spawn_wal_writer(
    dir: impl AsRef<Path>,
    mut rx: mpsc::Receiver<AgentEventEnvelope>,
) -> io::Result<(JoinHandle<()>, Arc<WalStats>)> {
    let stats = Arc::new(WalStats::default());
    let mut writer = WalWriter::open(dir, Arc::clone(&stats))?;
    let handle = tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            writer.append(event);
        }
        writer.flush();
    });
    Ok((handle, stats))
}
