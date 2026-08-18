use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use harness_llm::{Chunk, Message, ToolCall, ToolResult, Usage};

pub type EventId = u64;
pub type SessionId = Uuid;

/// 持久会话事件（turn/*、step/*、assistant/*、tool/*）。实时扩展点不写入日志（原 §5.6）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionEvent {
    TurnStart { id: EventId, input: String },
    PreStep { id: EventId, msg: Vec<Message> },
    StepStart { id: EventId, step: usize },
    StepEnd { id: EventId, step: usize },
    Assistant { id: EventId, chunk: Chunk },
    /// 模型思考链增量：仅 UI「思考中」反馈，不进入模型上下文。
    Thinking { id: EventId, text: String },
    ToolCall { id: EventId, call: ToolCall },
    ToolResult { id: EventId, result: ToolResult },
    /// plan 工具发布的结构化任务计划（长周期任务规划真相源）。
    PlanUpdate { id: EventId, items: Vec<PlanItem> },
    TurnStopping { id: EventId, will_stop: bool },
    TurnEnd { id: EventId },
    /// 一次请求的 token 用量（AIOps 成本计量）。由 agent 循环从 `Chunk.usage` 派生，
    /// 不进入模型上下文，不影响多轮重建（落在 `_ => {}` 分支）。
    Usage { id: EventId, usage: Usage },
}

/// 计划条目（status ∈ pending / doing / done）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    pub text: String,
    #[serde(default = "default_status")]
    pub status: String,
}

/// 会话历史列表条目（侧栏「历史记录」面板展示用）。
#[derive(Debug, Clone)]
pub struct SessionMeta {
    /// 会话文件名（如 `{uuid}.jsonl`）。
    pub file: String,
    /// 首条用户输入截断作标题；无回合时回退文件名。
    pub title: String,
    /// 所属项目名（sessions 目录的上两级目录名）。
    pub project: String,
    pub mtime: std::time::SystemTime,
}

fn default_status() -> String {
    "pending".into()
}

/// 取事件内嵌的单调 id（open_latest 重建 next 计数器用）。
fn event_id(ev: &SessionEvent) -> EventId {
    match ev {
        SessionEvent::TurnStart { id, .. }
        | SessionEvent::PreStep { id, .. }
        | SessionEvent::StepStart { id, .. }
        | SessionEvent::StepEnd { id, .. }
        | SessionEvent::Assistant { id, .. }
        | SessionEvent::Thinking { id, .. }
        | SessionEvent::ToolCall { id, .. }
        | SessionEvent::ToolResult { id, .. }
        | SessionEvent::PlanUpdate { id, .. }
        | SessionEvent::TurnStopping { id, .. }
        | SessionEvent::TurnEnd { id }
        | SessionEvent::Usage { id, .. } => *id,
    }
}

struct LogInner {
    events: Vec<SessionEvent>,
    next: EventId,
    path: Option<std::path::PathBuf>,
}

/// 会话追加日志（真相源）。仅追加写；fork/resume/replay 全从日志派生（原 §5.5）。
pub struct SessionLog {
    id: SessionId,
    inner: Mutex<LogInner>,
}

impl SessionLog {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            id: Uuid::new_v4(),
            inner: Mutex::new(LogInner {
                events: Vec::new(),
                next: 0,
                path: None,
            }),
        })
    }

    /// 创建带 JSONL 落盘的会话日志。内存仍用于低延迟投影，磁盘文件用于诊断和恢复。
    pub fn persistent(dir: impl AsRef<std::path::Path>) -> Arc<Self> {
        let id = Uuid::new_v4();
        let dir = dir.as_ref();
        let _ = std::fs::create_dir_all(dir);
        Arc::new(Self {
            id,
            inner: Mutex::new(LogInner {
                events: Vec::new(),
                next: 0,
                path: Some(dir.join(format!("{id}.jsonl"))),
            }),
        })
    }

    pub fn id(&self) -> SessionId {
        self.id
    }

    /// 单调自增事件 id（全局唯一，跨 turn/step）。
    pub fn gen_id(&self) -> EventId {
        let mut g = self.inner.lock().unwrap();
        let id = g.next;
        g.next += 1;
        id
    }

    /// 仅追加写一条会话事件。
    pub fn append(&self, ev: SessionEvent) {
        let path = {
            let mut g = self.inner.lock().unwrap();
            g.events.push(ev.clone());
            g.path.clone()
        };
        if let Some(path) = path {
            use std::io::Write;
            if let (Ok(mut file), Ok(line)) = (
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path),
                serde_json::to_string(&ev),
            ) {
                let _ = writeln!(file, "{line}");
                // 每条事件强制 flush：异常退出/断电也不丢已完成的 TurnEnd 与工具结果。
                let _ = file.flush();
            }
        }
    }

    /// 重放全部事件（模型可见状态只能从此重建，完成文档 §8 不变量 1）。
    pub fn replay(&self) -> Vec<SessionEvent> {
        self.inner.lock().unwrap().events.clone()
    }

    /// 仅复制指定下标之后的新事件，避免 GUI 高频刷新反复克隆完整会话。
    pub fn replay_from(&self, start: usize) -> (usize, Vec<SessionEvent>) {
        let inner = self.inner.lock().unwrap();
        let start = start.min(inner.events.len());
        (inner.events.len(), inner.events[start..].to_vec())
    }

    /// 累计本会话全部 token 用量（AIOps 成本计量）。仅统计 `Usage` 事件，
    /// 不影响模型上下文重建。
    pub fn usage_total(&self) -> Usage {
        let mut total = Usage::default();
        for ev in self.inner.lock().unwrap().events.iter() {
            if let SessionEvent::Usage { usage, .. } = ev {
                total = total.saturating_add(*usage);
            }
        }
        total
    }

    /// 清空当前内存上下文并截断持久日志。注意：这会清空当前会话文件内容，
    /// 历史浏览请用 [`SessionLog::fresh`]（新建文件、旧会话保留可回看）。
    pub fn clear(&self) {
        let path = {
            if let Ok(mut inner) = self.inner.lock() {
                inner.events.clear();
                inner.next = 0;
                inner.path.clone()
            } else {
                None
            }
        };
        if let Some(path) = path {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path);
        }
    }

    /// 派生新会话：复制当前前缀，后续独立追加（fork/resume，原 §5.5）。
    pub fn fork(&self) -> Arc<Self> {
        let g = self.inner.lock().unwrap();
        Arc::new(Self {
            id: Uuid::new_v4(),
            inner: Mutex::new(LogInner {
                events: g.events.clone(),
                next: g.next,
                path: None,
            }),
        })
    }

    /// 切换持久化目录并重载该目录最近会话到内存（项目切换入口）：
    /// 同一 `Arc<SessionLog>` 实例继续作为真相源，GUI / PlanTool 等所有持有者
    /// 立即看到新项目的历史；后续追加落盘到新目录的最新会话文件。
    pub fn switch_dir(&self, dir: impl AsRef<std::path::Path>) {
        let dir = dir.as_ref();
        let (events, next, path) = load_latest(dir);
        // 无历史会话：在新项目目录创建新会话文件，保证后续追加可落盘。
        let path = Some(path.unwrap_or_else(|| {
            let _ = std::fs::create_dir_all(dir);
            dir.join(format!("{}.jsonl", Uuid::new_v4()))
        }));
        {
            let mut g = self.inner.lock().unwrap();
            g.events = events;
            g.next = next;
            g.path = path;
        }
        let needs_close = matches!(
            self.inner.lock().unwrap().events.last(),
            Some(ev) if !matches!(ev, SessionEvent::TurnEnd { .. })
        );
        if needs_close {
            self.append(SessionEvent::TurnEnd { id: self.gen_id() });
        }
    }

    /// 扫描目录恢复最近会话（重启恢复入口，日志即真相源）：
    /// 取 mtime 最新的 `.jsonl` 逐行重建事件（坏行跳过）并复用该文件追加；
    /// 尾事件非 `TurnEnd` 时补一条以闭合被中断的回合；无文件则新建。
    pub fn open_latest(dir: impl AsRef<std::path::Path>) -> Arc<Self> {
        let dir = dir.as_ref();
        let (events, next, path) = load_latest(dir);
        let id = Uuid::new_v4();
        // 无历史会话：新建会话文件以便后续追加落盘。
        let path = Some(path.unwrap_or_else(|| {
            let _ = std::fs::create_dir_all(dir);
            dir.join(format!("{id}.jsonl"))
        }));
        let log = Arc::new(Self {
            id,
            inner: Mutex::new(LogInner { events, next, path }),
        });
        let needs_close = matches!(
            log.inner.lock().unwrap().events.last(),
            Some(ev) if !matches!(ev, SessionEvent::TurnEnd { .. })
        );
        if needs_close {
            log.append(SessionEvent::TurnEnd { id: log.gen_id() });
        }
        log
    }

    /// 当前持久化文件路径（历史面板高亮当前会话用）。
    pub fn path(&self) -> Option<std::path::PathBuf> {
        self.inner.lock().unwrap().path.clone()
    }

    /// 持久化目录（历史列表扫描用）。
    pub fn dir(&self) -> Option<std::path::PathBuf> {
        self.path().and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    /// 新建会话（「新建对话」）：换新 uuid 文件，旧会话文件原样保留，
    /// 成为历史记录可回看；并顺带按上限清理最旧会话防磁盘无限增长。
    pub fn fresh(&self, dir: impl AsRef<std::path::Path>) {
        let dir = dir.as_ref();
        let _ = std::fs::create_dir_all(dir);
        let file = dir.join(format!("{}.jsonl", Uuid::new_v4()));
        {
            let mut g = self.inner.lock().unwrap();
            g.events.clear();
            g.next = 0;
            g.path = Some(file.clone());
        }
        prune_dir(dir, 50, file.file_name().and_then(|f| f.to_str()));
    }

    /// 切换到指定历史会话文件（点击恢复入口）：重建该文件全部事件并复用其
    /// 继续追加；尾事件非 TurnEnd 时补闭合（与 open_latest 同一中断修复策略）。
    pub fn switch_to_file(&self, dir: impl AsRef<std::path::Path>, file: &str) -> bool {
        let path = dir.as_ref().join(file);
        if !path.is_file() {
            return false;
        }
        let (events, next) = load_file(&path);
        {
            let mut g = self.inner.lock().unwrap();
            g.events = events;
            g.next = next;
            g.path = Some(path);
        }
        let needs_close = matches!(
            self.inner.lock().unwrap().events.last(),
            Some(ev) if !matches!(ev, SessionEvent::TurnEnd { .. })
        );
        if needs_close {
            self.append(SessionEvent::TurnEnd { id: self.gen_id() });
        }
        true
    }
}

/// 列出目录内全部会话元数据，mtime 倒序（历史面板数据源）。
/// 标题取首条 TurnStart 输入（流式读行，避免为取标题全量加载大文件）。
pub fn list_sessions(dir: impl AsRef<std::path::Path>) -> Vec<SessionMeta> {
    let dir = dir.as_ref();
    let project = dir
        .parent()
        .and_then(|d| d.parent())
        .and_then(|d| d.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("会话")
        .to_string();
    let mut metas: Vec<SessionMeta> = std::fs::read_dir(dir)
        .ok()
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .filter_map(|p| {
                    let file = p.file_name()?.to_str()?.to_string();
                    let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
                    Some(SessionMeta {
                        file,
                        title: session_title(&p).unwrap_or_else(|| "(空会话)".into()),
                        project: project.clone(),
                        mtime,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    metas.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    metas
}

/// 删除指定会话文件（连同重命名旁挂 `.title`）。
pub fn delete_session(dir: impl AsRef<std::path::Path>, file: &str) -> bool {
    let path = dir.as_ref().join(file);
    let _ = std::fs::remove_file(path.with_extension("title"));
    std::fs::remove_file(path).is_ok()
}

/// 设置/清除会话自定义标题：写入旁挂文件 `<uuid>.title`（与 `<uuid>.jsonl` 同目录）。
/// 空标题则删除该旁挂文件，恢复默认（首条输入截断）。返回是否成功。
/// 标题是展示元数据，不进日志、不影响多轮重建（对齐 cc-switch 会话可重命名）。
pub fn rename_session(dir: impl AsRef<std::path::Path>, file: &str, title: &str) -> bool {
    let path = dir.as_ref().join(file);
    let sidecar = path.with_extension("title");
    let title = title.trim();
    if title.is_empty() {
        return match std::fs::remove_file(&sidecar) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
            Err(_) => false,
        };
    }
    std::fs::write(&sidecar, title).is_ok()
}

/// 保留最近 `keep` 个会话（按 mtime），其余删除；`active` 文件永不删。
pub fn prune_sessions(dir: impl AsRef<std::path::Path>, keep: usize, active: Option<&str>) {
    prune_dir(dir.as_ref(), keep, active);
}

fn prune_dir(dir: &std::path::Path, keep: usize, active: Option<&str>) {
    let mut metas = list_sessions(dir);
    if metas.len() <= keep {
        return;
    }
    metas.sort_by(|a, b| a.mtime.cmp(&b.mtime)); // 最旧在前
    for meta in metas.iter().take(metas.len() - keep) {
        if active.is_some_and(|a| meta.file == a) {
            continue;
        }
        delete_session(dir, &meta.file);
    }
}

/// 读会话标题：自定义标题（旁挂 `<uuid>.title`）优先，回退首条 TurnStart 输入（截断 30 字）。
fn session_title(path: &std::path::Path) -> Option<String> {
    // 自定义标题优先：重命名时写入的旁挂文件。
    let sidecar = path.with_extension("title");
    if let Ok(t) = std::fs::read_to_string(&sidecar) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    for line in std::io::BufReader::new(file).lines().take(64) {
        let Ok(line) = line else { break };
        if !line.contains("TurnStart") {
            continue;
        }
        if let Ok(SessionEvent::TurnStart { input, .. }) =
            serde_json::from_str::<SessionEvent>(&line)
        {
            let t: String = input.trim().chars().take(30).collect();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

/// 扫描目录取 mtime 最新的 `.jsonl` 重建事件（坏行跳过）；无文件返回空集。
fn load_latest(
    dir: &std::path::Path,
) -> (Vec<SessionEvent>, EventId, Option<std::path::PathBuf>) {
    let latest = std::fs::read_dir(dir)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        });
    let Some(path) = latest else {
        let _ = std::fs::create_dir_all(dir);
        return (Vec::new(), 0, None);
    };

    let (events, next) = load_file(&path);
    (events, next, Some(path))
}

/// 逐行重建单个会话文件的事件（坏行跳过），返回事件流与 next 计数器。
fn load_file(path: &std::path::Path) -> (Vec<SessionEvent>, EventId) {
    let mut events: Vec<SessionEvent> = Vec::new();
    let mut next: EventId = 0;
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<SessionEvent>(line) {
                next = next.max(event_id(&ev) + 1);
                events.push(ev);
            }
        }
    }
    (events, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_latest_resumes_and_closes_interrupted_turn() {
        let dir =
            std::env::temp_dir().join(format!("harness-session-test-{}", Uuid::new_v4()));
        let log = SessionLog::persistent(&dir);
        log.append(SessionEvent::TurnStart {
            id: log.gen_id(),
            input: "hi".into(),
        });
        log.append(SessionEvent::Assistant {
            id: log.gen_id(),
            chunk: Chunk {
                text: Some("part".into()),
                ..Default::default()
            },
        });
        // 模拟中断：未写 TurnEnd 即丢弃句柄。
        drop(log);

        let resumed = SessionLog::open_latest(&dir);
        let events = resumed.replay();
        assert!(matches!(events.last(), Some(SessionEvent::TurnEnd { .. })));
        assert!(events
            .iter()
            .any(|e| matches!(e, SessionEvent::TurnStart { input, .. } if input == "hi")));

        // 复用同一文件继续追加，二次恢复可见新事件。
        resumed.append(SessionEvent::TurnStart {
            id: resumed.gen_id(),
            input: "again".into(),
        });
        let resumed2 = SessionLog::open_latest(&dir);
        assert!(resumed2
            .replay()
            .iter()
            .any(|e| matches!(e, SessionEvent::TurnStart { input, .. } if input == "again")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_keeps_old_session_and_history_supports_switch_delete_prune() {
        let dir =
            std::env::temp_dir().join(format!("harness-history-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // 会话 A：一条完整回合。
        let log = SessionLog::persistent(&dir);
        let file_a = log.path().unwrap();
        log.append(SessionEvent::TurnStart {
            id: log.gen_id(),
            input: "第一个问题".into(),
        });
        log.append(SessionEvent::TurnEnd { id: log.gen_id() });
        std::thread::sleep(std::time::Duration::from_millis(40)); // 保证 mtime 有序

        // 新建会话：旧文件保留（历史可回看）。
        log.fresh(&dir);
        let file_b = log.path().unwrap();
        assert_ne!(file_a, file_b);
        assert!(file_a.exists());
        assert!(log.replay().is_empty());

        log.append(SessionEvent::TurnStart {
            id: log.gen_id(),
            input: "第二个问题".into(),
        });
        log.append(SessionEvent::TurnEnd { id: log.gen_id() });

        // 列表：mtime 倒序，B 在前，标题/项目名正确。
        let metas = list_sessions(&dir);
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].title, "第二个问题");
        assert!(metas[0].mtime >= metas[1].mtime);
        assert_eq!(metas[1].title, "第一个问题");
        assert!(!metas[0].project.is_empty());

        // 点击恢复历史会话 A，可继续追加。
        let name_a = file_a.file_name().unwrap().to_str().unwrap().to_string();
        assert!(log.switch_to_file(&dir, &name_a));
        assert!(log
            .replay()
            .iter()
            .any(|e| matches!(e, SessionEvent::TurnStart { input, .. } if input == "第一个问题")));
        log.append(SessionEvent::TurnStart {
            id: log.gen_id(),
            input: "追加消息".into(),
        });
        assert!(log.replay().iter().any(
            |e| matches!(e, SessionEvent::TurnStart { input, .. } if input == "追加消息")
        ));

        // 删除会话：活跃文件不受影响。
        let metas = list_sessions(&dir);
        let victim = metas.iter().find(|m| m.file != name_a).unwrap().file.clone();
        assert!(delete_session(&dir, &victim));
        assert_eq!(list_sessions(&dir).len(), 1);

        // 清理上限：活跃文件永不删。
        prune_sessions(&dir, 1, Some(name_a.as_str()));
        let metas = list_sessions(&dir);
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].file, name_a);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
