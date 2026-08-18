use std::any::Any;

use harness_core::error::Result;

/// 一条记忆条目。跨会话持久化的键值记忆，按 `scope` 隔离（项目 / 用户 / 会话）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryEntry {
    pub scope: MemoryScope,
    pub key: String,
    pub value: String,
    /// RFC3339 秒级时间戳字符串；骨架用 `String` 以避免引入 chrono 依赖。
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryScope {
    /// 项目级约定（如"本仓库用 redb 不用 SQLite"）。
    Project,
    /// 用户级偏好（如"回复用中文"）。
    User,
    /// 单次会话内的临时记忆（进程内有效）。
    Session,
}

impl MemoryScope {
    pub fn dir_name(&self) -> &'static str {
        match self {
            MemoryScope::Project => "project",
            MemoryScope::User => "user",
            MemoryScope::Session => "session",
        }
    }
}

/// 记忆机制能力（Definition）。借鉴 Codex 的跨会话记忆：
/// 代理把"学到的偏好 / 项目约定"写入记忆，后续会话检索复用。
///
/// 与 `SessionLog`（运行时真相源，单会话、只追加）正交——记忆是 *跨会话*、*可检索* 的持久层。
pub trait Memory: Any + Send + Sync + 'static {
    /// 写入 / 更新一条记忆（按 scope+key upsert）。
    fn write(&self, entry: MemoryEntry) -> Result<()>;
    /// 读取某 scope 下的全部记忆。
    fn read(&self, scope: MemoryScope) -> Result<Vec<MemoryEntry>>;
    /// 子串检索（骨架为朴素子串；可换 fts / 向量索引）。
    fn search(&self, query: &str) -> Result<Vec<MemoryEntry>>;
}
