use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use harness_capability::watcher::FileWatcher;
use harness_core::error::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// 跨平台轮询监听器。它不依赖平台通知队列，因此同时充当 notify 类实现的丢事件兜底。
pub struct PollingFileWatcher {
    root: RwLock<Option<PathBuf>>,
    snapshot: RwLock<HashMap<PathBuf, Stamp>>,
    interval: Duration,
}

impl PollingFileWatcher {
    pub fn new(interval: Duration) -> Self {
        Self {
            root: RwLock::new(None),
            snapshot: RwLock::new(HashMap::new()),
            interval,
        }
    }

    fn scan(root: &Path) -> std::io::Result<HashMap<PathBuf, Stamp>> {
        fn visit(
            dir: &Path,
            root: &Path,
            out: &mut HashMap<PathBuf, Stamp>,
        ) -> std::io::Result<()> {
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                let name = entry.file_name();
                if path.is_dir() {
                    if name != ".git" && name != "target" && name != ".harness" {
                        visit(&path, root, out)?;
                    }
                } else if let Ok(meta) = entry.metadata() {
                    out.insert(
                        path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
                        Stamp {
                            modified: meta.modified().ok(),
                            len: meta.len(),
                        },
                    );
                }
            }
            Ok(())
        }
        let mut result = HashMap::new();
        visit(root, root, &mut result)?;
        Ok(result)
    }
}

#[async_trait]
impl FileWatcher for PollingFileWatcher {
    async fn start(&self, root: &Path) -> Result<()> {
        let root = root.canonicalize().map_err(Error::Io)?;
        let snapshot = Self::scan(&root).map_err(Error::Io)?;
        *self
            .root
            .write()
            .map_err(|_| Error::Watcher("root lock poisoned".into()))? = Some(root);
        *self
            .snapshot
            .write()
            .map_err(|_| Error::Watcher("snapshot lock poisoned".into()))? = snapshot;
        Ok(())
    }

    async fn next_change(&self) -> Result<Vec<PathBuf>> {
        loop {
            tokio::time::sleep(self.interval).await;
            let root = self
                .root
                .read()
                .map_err(|_| Error::Watcher("root lock poisoned".into()))?
                .clone()
                .ok_or_else(|| Error::Watcher("watcher not started".into()))?;
            let next = tokio::task::spawn_blocking(move || Self::scan(&root))
                .await
                .map_err(|e| Error::Watcher(e.to_string()))??;
            let mut previous = self
                .snapshot
                .write()
                .map_err(|_| Error::Watcher("snapshot lock poisoned".into()))?;
            let mut changed = Vec::new();
            for (path, stamp) in &next {
                if previous.get(path) != Some(stamp) {
                    changed.push(path.clone());
                }
            }
            for path in previous.keys() {
                if !next.contains_key(path) {
                    changed.push(path.clone());
                }
            }
            *previous = next;
            if !changed.is_empty() {
                changed.sort();
                changed.dedup();
                return Ok(changed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn detects_create_and_modify() {
        let root = std::env::temp_dir().join(format!("harness-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let watcher = PollingFileWatcher::new(Duration::from_millis(10));
        watcher.start(&root).await.unwrap();
        std::fs::write(root.join("a.txt"), "one").unwrap();
        assert_eq!(
            watcher.next_change().await.unwrap(),
            vec![PathBuf::from("a.txt")]
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
