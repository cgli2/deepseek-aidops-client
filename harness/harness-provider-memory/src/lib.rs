//! 文件型记忆 Provider（借鉴 Codex 的 AGENTS / memory 文件持久化）。
//!
//! 存储布局：`<root>/.harness-memory/<scope>/<key>` —— 每个条目一个文件（内容为 value）。
//! 写即落盘（追加友好、崩溃安全），并维护内存索引加速检索。可换成 SQLite / 向量库而不影响 Consumer。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use harness_capability::memory::{Memory, MemoryEntry, MemoryScope};
use harness_core::error::{Error, Result};

/// 文件型记忆 Provider。
pub struct FileMemory {
    root: PathBuf,
    index: Mutex<HashMap<(MemoryScope, String), MemoryEntry>>,
}

impl FileMemory {
    pub fn new(root: impl AsRef<Path>) -> Arc<Self> {
        let root = root.as_ref().join(".harness-memory");
        let _ = std::fs::create_dir_all(&root);
        Arc::new(Self {
            root,
            index: Mutex::new(HashMap::new()),
        })
    }

    fn scope_dir(&self, scope: MemoryScope) -> PathBuf {
        self.root.join(scope.dir_name())
    }

    fn entry_path(&self, scope: MemoryScope, key: &str) -> PathBuf {
        self.scope_dir(scope).join(sanitize(key))
    }
}

/// 把任意 key 规整为合法文件名（非 [A-Za-z0-9_.-] 一律下划线）。
fn sanitize(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl Memory for FileMemory {
    fn write(&self, entry: MemoryEntry) -> Result<()> {
        let dir = self.scope_dir(entry.scope);
        std::fs::create_dir_all(&dir).map_err(Error::Io)?;
        std::fs::write(self.entry_path(entry.scope, &entry.key), &entry.value)
            .map_err(Error::Io)?;
        self.index
            .lock()
            .unwrap()
            .insert((entry.scope, entry.key.clone()), entry);
        Ok(())
    }

    fn read(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>> {
        let dir = self.scope_dir(scope);
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if let Ok(value) = std::fs::read_to_string(e.path()) {
                    let key = e.file_name().to_string_lossy().into_owned();
                    out.push(MemoryEntry {
                        scope,
                        key,
                        value,
                        updated_at: String::new(),
                    });
                }
            }
        }
        Ok(out)
    }

    fn search(&self, query: &str) -> Result<Vec<MemoryEntry>> {
        let q = query.to_lowercase();
        let mut out = Vec::new();
        for scope in [
            MemoryScope::Project,
            MemoryScope::User,
            MemoryScope::Session,
        ] {
            for e in self.read(scope)? {
                if e.key.to_lowercase().contains(&q) || e.value.to_lowercase().contains(&q) {
                    out.push(e);
                }
            }
        }
        Ok(out)
    }
}
