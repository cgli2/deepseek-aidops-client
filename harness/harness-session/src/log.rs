use std::{
    collections::HashMap,
    sync::{Arc, Mutex, RwLock, Weak},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use harness_llm::{Chunk, Message, ToolCall, ToolResult, Usage};

pub type EventId = u64;
pub type SessionId = Uuid;

/// 持久会话事件（turn/*、step/*、assistant/*、tool/*）。实时扩展点不写入日志（原 §5.6）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionEvent {
    TurnStart {
        id: EventId,
        input: String,
    },
    PreStep {
        id: EventId,
        msg: Vec<Message>,
    },
    StepStart {
        id: EventId,
        step: usize,
    },
    StepEnd {
        id: EventId,
        step: usize,
    },
    Assistant {
        id: EventId,
        chunk: Chunk,
    },
    /// 模型思考链增量：仅 UI「思考中」反馈，不进入模型上下文。
    Thinking {
        id: EventId,
        text: String,
    },
    ToolCall {
        id: EventId,
        call: ToolCall,
    },
    ToolResult {
        id: EventId,
        result: ToolResult,
    },
    /// plan 工具发布的结构化任务计划（长周期任务规划真相源）。
    PlanUpdate {
        id: EventId,
        items: Vec<PlanItem>,
    },
    /// Runtime 生成的交付判定。与模型的计划文本、最终总结分离，是 UI 显示
    /// “已交付”或“未完成”的唯一真相源。
    Delivery {
        id: EventId,
        report: DeliveryReport,
    },
    TurnStopping {
        id: EventId,
        will_stop: bool,
    },
    TurnEnd {
        id: EventId,
    },
    /// 一次请求的 token 用量（AIOps 成本计量）。由 agent 循环从 `Chunk.usage` 派生，
    /// 不进入模型上下文，不影响多轮重建（落在 `_ => {}` 分支）。
    Usage {
        id: EventId,
        usage: Usage,
    },
    /// Runtime 的执行遥测和 UI 投影来源。它不进入模型上下文；重放时可恢复
    /// 当前阶段、动态工具白名单与交付证据，而不是从模型自述推断状态。
    Telemetry {
        id: EventId,
        telemetry: ExecutionTelemetry,
    },
    /// 专家团编排事件。所有任务状态、证据与门禁结果均落盘，可恢复和审计。
    Council {
        id: EventId,
        event: CouncilEvent,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CouncilTaskState {
    Pending,
    Ready,
    Running,
    Done,
    Blocked,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouncilTaskSpec {
    pub id: String,
    pub title: String,
    pub objective: String,
    pub role: String,
    pub depends_on: Vec<String>,
    pub write_scopes: Vec<String>,
    pub acceptance_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CouncilGateResult {
    pub name: String,
    pub passed: bool,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CouncilEvent {
    Started {
        council_id: String,
        goal: String,
        max_parallel: usize,
    },
    PlanCreated {
        council_id: String,
        tasks: Vec<CouncilTaskSpec>,
    },
    TaskStateChanged {
        council_id: String,
        task_id: String,
        state: CouncilTaskState,
        attempt: u32,
        detail: String,
    },
    ArtifactPublished {
        council_id: String,
        task_id: String,
        summary: String,
        evidence: Vec<String>,
    },
    GateEvaluated {
        council_id: String,
        gate: CouncilGateResult,
    },
    Blocked {
        council_id: String,
        reason: String,
    },
    Completed {
        council_id: String,
        summary: String,
    },
    Cancelled {
        council_id: String,
        reason: String,
    },
}

/// 计划条目。`claimed_done` 只表示模型自报完成；只有 Runtime 的 Delivery
/// 验收报告可将其视为 verified，避免计划文本直接伪造交付状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanItem {
    #[serde(default)]
    pub id: String,
    pub text: String,
    #[serde(default = "default_status")]
    pub status: String,
    /// 此计划项声明要覆盖的验收 ID，仅作运行时追踪，不接受模型自报作为证据。
    #[serde(default)]
    pub criterion_ids: Vec<String>,
    /// 模型声明的证据说明；UI 会标为“待运行时验收”。
    #[serde(default)]
    pub evidence: Vec<String>,
}

/// 回合的最终交付状态。`TurnEnd` 只表示日志完整，不能推导交付成功。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeliveryOutcome {
    Verified,
    Blocked,
    Interrupted,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryCriterion {
    pub id: String,
    pub description: String,
    pub satisfied: bool,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryReport {
    pub outcome: DeliveryOutcome,
    pub criteria: Vec<DeliveryCriterion>,
    pub verification: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ExecutionTelemetry {
    pub intent: String,
    pub phase: String,
    pub allowed_tools: Vec<String>,
    pub step: usize,
    pub tool_calls: usize,
    pub evidence_count: usize,
    pub verified_count: usize,
    pub blocked_count: usize,
    pub detail: String,
}

/// 会话历史列表条目（侧栏「历史记录」面板展示用）。
#[derive(Debug, Clone)]
pub struct SessionMeta {
    /// 会话文件名（如 `{uuid}.jsonl`）。
    pub file: String,
    /// 首条用户输入提炼的简洁标题；无回合时回退文件名。
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
        | SessionEvent::Delivery { id, .. }
        | SessionEvent::TurnStopping { id, .. }
        | SessionEvent::TurnEnd { id }
        | SessionEvent::Usage { id, .. }
        | SessionEvent::Telemetry { id, .. }
        | SessionEvent::Council { id, .. } => *id,
    }
}

struct LogInner {
    events: Vec<SessionEvent>,
    next: EventId,
    path: Option<std::path::PathBuf>,
    /// 常驻追加句柄：避免每事件 open/close 的文件系统开销（惰性打开，路径变更时重建）。
    writer: Option<std::fs::File>,
    /// 尚未 flush 的流式分片计数：Assistant/Thinking 高频事件延迟批量 flush，
    /// 关键边界事件即时 flush，保持 TurnEnd 级别的崩溃恢复语义。
    pending_flush: usize,
}

/// 流式高频事件：写入但延迟 flush；其余事件（回合/步骤边界、工具结果、用量等）即时 flush。
fn is_stream_chunk(ev: &SessionEvent) -> bool {
    matches!(
        ev,
        SessionEvent::Assistant { .. } | SessionEvent::Thinking { .. }
    )
}

/// 延迟 flush 的批量上限：即使没有边界事件，累积到该数量也强制落盘。
const FLUSH_BATCH: usize = 32;

/// 会话追加日志（真相源）。仅追加写；fork/resume/replay 全从日志派生（原 §5.5）。
struct SessionState {
    id: SessionId,
    inner: Mutex<LogInner>,
}

/// 会话日志的可切换视图。UI 持有的根视图可以切换到另一条会话；正在执行的
/// 回合必须先调用 [`SessionLog::pin`]，得到固定到当时状态的句柄，因而不会把
/// 流式输出串写进用户后来打开的新会话。
pub struct SessionLog {
    state: RwLock<Arc<SessionState>>,
    /// 已被后台回合 pin 住的会话状态。UI 从历史切回该文件时复用同一状态，
    /// 既能继续看到流式输出，也能保持控制器的会话队列身份不变。
    state_cache: Arc<Mutex<HashMap<std::path::PathBuf, Weak<SessionState>>>>,
}

impl SessionLog {
    fn from_state(state: SessionState) -> Arc<Self> {
        let state = Arc::new(state);
        let log = Arc::new(Self {
            state: RwLock::new(state.clone()),
            state_cache: Arc::new(Mutex::new(HashMap::new())),
        });
        log.remember_state(&state);
        log
    }

    fn state(&self) -> Arc<SessionState> {
        self.state.read().unwrap().clone()
    }

    fn replace_state(&self, state: SessionState) {
        let state = Arc::new(state);
        self.remember_state(&state);
        *self.state.write().unwrap() = state;
    }

    /// 固定当前会话状态，供后台执行持有。随后 UI 切换/新建会话不会影响它。
    pub fn pin(&self) -> Arc<Self> {
        Arc::new(Self {
            state: RwLock::new(self.state()),
            state_cache: self.state_cache.clone(),
        })
    }

    fn session_path_key(path: &std::path::Path) -> std::path::PathBuf {
        path.parent()
            .and_then(|parent| parent.canonicalize().ok())
            .zip(path.file_name())
            .map(|(parent, name)| parent.join(name))
            .unwrap_or_else(|| path.to_path_buf())
    }

    fn remember_state(&self, state: &Arc<SessionState>) {
        let path = state.inner.lock().ok().and_then(|inner| inner.path.clone());
        let Some(path) = path else {
            return;
        };
        if let Ok(mut cache) = self.state_cache.lock() {
            cache.retain(|_, cached| cached.strong_count() > 0);
            cache.insert(Self::session_path_key(&path), Arc::downgrade(state));
        }
    }

    fn cached_state(&self, path: &std::path::Path) -> Option<Arc<SessionState>> {
        let key = Self::session_path_key(path);
        let Ok(mut cache) = self.state_cache.lock() else {
            return None;
        };
        let state = cache.get(&key).and_then(Weak::upgrade);
        if state.is_none() {
            cache.remove(&key);
        }
        state
    }
}

impl SessionLog {
    /// 恢复未正常闭合的回合时，不只补 TurnEnd；先留下用户可见的中断原因，
    /// 避免历史记录看起来像助手无缘无故写到一半便消失。
    fn close_interrupted_turn(&self) {
        let mut active_council = None;
        let mut delivery_reported = false;
        for event in self.replay() {
            match event {
                SessionEvent::TurnStart { .. } => delivery_reported = false,
                SessionEvent::Delivery { .. } => delivery_reported = true,
                SessionEvent::Council {
                    event: CouncilEvent::Started { council_id, .. },
                    ..
                } => active_council = Some(council_id),
                SessionEvent::Council {
                    event:
                        CouncilEvent::Completed { council_id, .. }
                        | CouncilEvent::Cancelled { council_id, .. }
                        | CouncilEvent::Blocked { council_id, .. },
                    ..
                } if active_council.as_deref() == Some(council_id.as_str()) => {
                    active_council = None
                }
                _ => {}
            }
        }
        if let Some(council_id) = active_council {
            self.append(SessionEvent::Council {
                id: self.gen_id(),
                event: CouncilEvent::Cancelled {
                    council_id,
                    reason: "程序退出或意外中断；恢复后未自动重启可能产生副作用的专家任务".into(),
                },
            });
        }
        if !delivery_reported {
            self.append(SessionEvent::Delivery {
                id: self.gen_id(),
                report: DeliveryReport {
                    outcome: DeliveryOutcome::Interrupted,
                    criteria: Vec::new(),
                    verification: Vec::new(),
                    reason: Some("程序退出或意外中断；本回合没有生成完整的验收报告".into()),
                },
            });
        }
        self.append(SessionEvent::Assistant {
            id: self.gen_id(),
            chunk: Chunk {
                text: Some("[error] 上次任务在完成前被程序退出或意外中断，已恢复会话。之前的输出可能不完整，请重试或要求继续。".into()),
                ..Default::default()
            },
        });
        self.append(SessionEvent::TurnEnd { id: self.gen_id() });
    }
    pub fn new() -> Arc<Self> {
        Self::from_state(SessionState {
            id: Uuid::new_v4(),
            inner: Mutex::new(LogInner {
                events: Vec::new(),
                next: 0,
                path: None,
                writer: None,
                pending_flush: 0,
            }),
        })
    }

    /// 创建带 JSONL 落盘的会话日志。内存仍用于低延迟投影，磁盘文件用于诊断和恢复。
    pub fn persistent(dir: impl AsRef<std::path::Path>) -> Arc<Self> {
        let id = Uuid::new_v4();
        let dir = dir.as_ref();
        let _ = std::fs::create_dir_all(dir);
        Self::from_state(SessionState {
            id,
            inner: Mutex::new(LogInner {
                events: Vec::new(),
                next: 0,
                path: Some(dir.join(format!("{id}.jsonl"))),
                writer: None,
                pending_flush: 0,
            }),
        })
    }

    pub fn id(&self) -> SessionId {
        self.state().id
    }

    /// 单调自增事件 id（全局唯一，跨 turn/step）。
    pub fn gen_id(&self) -> EventId {
        let state = self.state();
        let mut g = state.inner.lock().unwrap();
        let id = g.next;
        g.next += 1;
        id
    }

    /// 仅追加写一条会话事件。
    ///
    /// 性能：常驻追加句柄 + 分界 flush。流式文本/思考分片只写不刷（每 FLUSH_BATCH
    /// 条批量落盘），回合/工具结果等边界事件即时 flush——此前每个 token 分片一次
    /// 「open+write+flush」，一次中等回复数百次同步 syscall，是流式热路径的主要开销。
    pub fn append(&self, ev: SessionEvent) {
        let state = self.state();
        let mut g = state.inner.lock().unwrap();
        let deferred = is_stream_chunk(&ev);
        g.events.push(ev.clone());
        if g.path.is_none() {
            return;
        }
        if g.writer.is_none() {
            g.writer = g.path.as_ref().and_then(|path| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .ok()
            });
        }
        let Ok(line) = serde_json::to_string(&ev) else {
            return;
        };
        let force_flush = if deferred {
            g.pending_flush += 1;
            g.pending_flush >= FLUSH_BATCH
        } else {
            true
        };
        let Some(file) = g.writer.as_mut() else {
            return;
        };
        {
            use std::io::Write;
            let _ = writeln!(file, "{line}");
            if force_flush {
                let _ = file.flush();
            }
        }
        if force_flush {
            g.pending_flush = 0;
        }
    }

    /// 重放全部事件（模型可见状态只能从此重建，完成文档 §8 不变量 1）。
    pub fn replay(&self) -> Vec<SessionEvent> {
        self.state().inner.lock().unwrap().events.clone()
    }

    /// 仅复制指定下标之后的新事件，避免 GUI 高频刷新反复克隆完整会话。
    pub fn replay_from(&self, start: usize) -> (usize, Vec<SessionEvent>) {
        let state = self.state();
        let inner = state.inner.lock().unwrap();
        let start = start.min(inner.events.len());
        (inner.events.len(), inner.events[start..].to_vec())
    }

    /// 累计本会话全部 token 用量（AIOps 成本计量）。仅统计 `Usage` 事件，
    /// 不影响模型上下文重建。
    pub fn usage_total(&self) -> Usage {
        let mut total = Usage::default();
        let state = self.state();
        for ev in state.inner.lock().unwrap().events.iter() {
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
            let state = self.state();
            if let Ok(mut inner) = state.inner.lock() {
                inner.events.clear();
                inner.next = 0;
                // 先关闭常驻句柄再截断，避免 Windows 上持有句柄时 truncate 失败。
                inner.writer = None;
                inner.pending_flush = 0;
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
        let state = self.state();
        let g = state.inner.lock().unwrap();
        Self::from_state(SessionState {
            id: Uuid::new_v4(),
            inner: Mutex::new(LogInner {
                events: g.events.clone(),
                next: g.next,
                path: None,
                writer: None,
                pending_flush: 0,
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
        self.replace_state(SessionState {
            id: Uuid::new_v4(),
            inner: Mutex::new(LogInner {
                events,
                next,
                path,
                writer: None,
                pending_flush: 0,
            }),
        });
        let needs_close = matches!(
            self.state().inner.lock().unwrap().events.last(),
            Some(ev) if !matches!(ev, SessionEvent::TurnEnd { .. })
        );
        if needs_close {
            self.close_interrupted_turn();
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
        let log = Self::from_state(SessionState {
            id,
            inner: Mutex::new(LogInner {
                events,
                next,
                path,
                writer: None,
                pending_flush: 0,
            }),
        });
        let needs_close = matches!(
            log.state().inner.lock().unwrap().events.last(),
            Some(ev) if !matches!(ev, SessionEvent::TurnEnd { .. })
        );
        if needs_close {
            log.close_interrupted_turn();
        }
        log
    }

    /// 当前持久化文件路径（历史面板高亮当前会话用）。
    pub fn path(&self) -> Option<std::path::PathBuf> {
        self.state().inner.lock().unwrap().path.clone()
    }

    /// 持久化目录（历史列表扫描用）。
    pub fn dir(&self) -> Option<std::path::PathBuf> {
        self.path()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
    }

    /// 新建会话（「新建对话」）：换新 uuid 文件，旧会话文件原样保留，
    /// 成为历史记录可回看；并顺带按上限清理最旧会话防磁盘无限增长。
    pub fn fresh(&self, dir: impl AsRef<std::path::Path>) {
        let dir = dir.as_ref();
        let _ = std::fs::create_dir_all(dir);
        let file = dir.join(format!("{}.jsonl", Uuid::new_v4()));
        self.replace_state(SessionState {
            id: Uuid::new_v4(),
            inner: Mutex::new(LogInner {
                events: Vec::new(),
                next: 0,
                path: Some(file.clone()),
                writer: None,
                pending_flush: 0,
            }),
        });
        prune_dir(dir, 50, file.file_name().and_then(|f| f.to_str()));
    }

    /// 切换到指定历史会话文件（点击恢复入口）：重建该文件全部事件并复用其
    /// 继续追加；尾事件非 TurnEnd 时补闭合（与 open_latest 同一中断修复策略）。
    pub fn switch_to_file(&self, dir: impl AsRef<std::path::Path>, file: &str) -> bool {
        let path = dir.as_ref().join(file);
        if !path.is_file() {
            return false;
        }
        if let Some(state) = self.cached_state(&path) {
            *self.state.write().unwrap() = state;
            return true;
        }
        let (events, next) = load_file(&path);
        self.replace_state(SessionState {
            id: Uuid::new_v4(),
            inner: Mutex::new(LogInner {
                events,
                next,
                path: Some(path),
                writer: None,
                pending_flush: 0,
            }),
        });
        let needs_close = matches!(
            self.state().inner.lock().unwrap().events.last(),
            Some(ev) if !matches!(ev, SessionEvent::TurnEnd { .. })
        );
        if needs_close {
            self.close_interrupted_turn();
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

/// 从用户输入提炼简洁历史标题：折叠空白、去除常见控制前缀，再按句读边界截断。
/// 纯确定性文本处理（侧栏元数据读取路径），不引入模型调用。
fn summarize_title(input: &str) -> String {
    const MAX: usize = 24;
    // 折叠所有空白（含换行）为单个空格，避免多行输入撑爆侧栏标题。
    let s: String = input.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s.trim();
    // 剥离历史文件中可能残留的流程控制前缀。
    let s = s
        .strip_prefix("[HARNESS_MULTI_AGENT]")
        .map(str::trim_start)
        .unwrap_or(s);
    if s.is_empty() {
        return String::new();
    }
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    // 在 MAX 内找最近的句读/空白边界，优先保留完整语义片段而非生硬切字。
    let mut cut = MAX;
    for i in (1..=MAX).rev() {
        if matches!(
            chars[i - 1],
            '。' | '！' | '？' | '，' | '；' | '、' | '.' | '!' | '?' | ',' | ';' | ' '
        ) {
            cut = i;
            break;
        }
    }
    let head = chars[..cut].iter().collect::<String>();
    let head = head.trim_end();
    if head.is_empty() {
        return s.chars().take(MAX).collect();
    }
    let last = head.chars().last().unwrap();
    if matches!(last, '。' | '！' | '？' | '.' | '!' | '?') {
        head.to_string()
    } else {
        // 去掉收尾的轻标点再补省略号，避免“xxx，”这类残缺样式。
        let trimmed =
            head.trim_end_matches(|c: char| matches!(c, '，' | '；' | '、' | ',' | ';' | ' '));
        format!("{trimmed}…")
    }
}

/// 读会话标题：自定义标题（旁挂 `<uuid>.title`）优先，回退首条 TurnStart 输入提炼的简洁摘要。
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
            let t = summarize_title(&input);
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

/// 扫描目录取 mtime 最新的 `.jsonl` 重建事件（坏行跳过）；无文件返回空集。
fn load_latest(dir: &std::path::Path) -> (Vec<SessionEvent>, EventId, Option<std::path::PathBuf>) {
    let latest = std::fs::read_dir(dir).ok().and_then(|entries| {
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
    fn telemetry_event_survives_json_round_trip() {
        let event = SessionEvent::Telemetry {
            id: 7,
            telemetry: ExecutionTelemetry {
                phase: "verify".into(),
                allowed_tools: vec!["shell".into()],
                verified_count: 1,
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: SessionEvent = serde_json::from_str(&json).unwrap();
        assert!(matches!(decoded, SessionEvent::Telemetry { telemetry, .. }
            if telemetry.phase == "verify" && telemetry.allowed_tools == ["shell"]));
    }

    #[test]
    fn summarize_title_extracts_clean_titles() {
        // 短标题原样保留。
        assert_eq!(summarize_title("第一个问题"), "第一个问题");
        // 折叠空白（含换行）。
        assert_eq!(summarize_title("  多  行\n输入  "), "多 行 输入");
        // 剥离流程控制前缀。
        assert_eq!(
            summarize_title("[HARNESS_MULTI_AGENT] 开始协作"),
            "开始协作"
        );
        // 长标题按句读边界截断并补省略号，长度受限。
        let long = "请帮我分析一下这个项目的整体架构，然后给出优化建议";
        let t = summarize_title(long);
        assert!(t.chars().count() <= 24);
        assert!(t.ends_with('…'));
        // 以结束标点结尾时不补省略号。
        let q = "这是一个很长的问题吗？是的它真的很长很长很长很长很长很长";
        assert!(summarize_title(q).ends_with('？'));
    }

    #[test]
    fn open_latest_resumes_and_closes_interrupted_turn() {
        let dir = std::env::temp_dir().join(format!("harness-session-test-{}", Uuid::new_v4()));
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
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::Delivery { report, .. }
                if report.outcome == DeliveryOutcome::Interrupted
        )));
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
        let dir = std::env::temp_dir().join(format!("harness-history-test-{}", Uuid::new_v4()));
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
        assert!(log
            .replay()
            .iter()
            .any(|e| matches!(e, SessionEvent::TurnStart { input, .. } if input == "追加消息")));

        // 删除会话：活跃文件不受影响。
        let metas = list_sessions(&dir);
        let victim = metas
            .iter()
            .find(|m| m.file != name_a)
            .unwrap()
            .file
            .clone();
        assert!(delete_session(&dir, &victim));
        assert_eq!(list_sessions(&dir).len(), 1);

        // 清理上限：活跃文件永不删。
        prune_sessions(&dir, 1, Some(name_a.as_str()));
        let metas = list_sessions(&dir);
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].file, name_a);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pinned_log_remains_isolated_when_ui_starts_a_new_session() {
        let dir =
            std::env::temp_dir().join(format!("harness-pinned-session-test-{}", Uuid::new_v4()));
        let log = SessionLog::persistent(&dir);
        let old_file = log.path().unwrap();
        let running = log.pin();

        // 模拟 A 正在流式执行时，UI 新建 B。
        log.fresh(&dir);
        let new_file = log.path().unwrap();
        running.append(SessionEvent::Assistant {
            id: running.gen_id(),
            chunk: Chunk {
                text: Some("only A receives this".into()),
                ..Default::default()
            },
        });

        assert_ne!(old_file, new_file);
        assert!(running.replay().iter().any(|event| matches!(event,
            SessionEvent::Assistant { chunk, .. } if chunk.text.as_deref() == Some("only A receives this")
        )));
        assert!(
            log.replay().is_empty(),
            "new session must not receive old stream output"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delivery_report_is_persisted_and_replayed() {
        let dir = std::env::temp_dir().join(format!("harness-delivery-test-{}", Uuid::new_v4()));
        let log = SessionLog::persistent(&dir);
        log.append(SessionEvent::Delivery {
            id: log.gen_id(),
            report: DeliveryReport {
                outcome: DeliveryOutcome::Blocked,
                criteria: vec![DeliveryCriterion {
                    id: "user-objective".into(),
                    description: "完成变更并验证".into(),
                    satisfied: false,
                    evidence: vec![],
                }],
                verification: vec![],
                reason: Some("验证未执行".into()),
            },
        });
        drop(log);

        let replayed = SessionLog::open_latest(&dir).replay();
        assert!(matches!(
            replayed.first(),
            Some(SessionEvent::Delivery { report, .. })
                if report.outcome == DeliveryOutcome::Blocked
                    && report.criteria.len() == 1
                    && report.reason.as_deref() == Some("验证未执行")
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn switching_back_reuses_a_running_session_state() {
        let dir = std::env::temp_dir().join(format!(
            "harness-switch-running-session-test-{}",
            Uuid::new_v4()
        ));
        let log = SessionLog::persistent(&dir);
        log.append(SessionEvent::TurnStart {
            id: log.gen_id(),
            input: "会话 A".into(),
        });
        let file_a = log
            .path()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().to_string())
            })
            .unwrap();
        let running = log.pin();

        // UI 切往另一个会话，再回到仍在流式输出的 A。
        log.fresh(&dir);
        assert!(log.switch_to_file(&dir, &file_a));
        assert_eq!(log.id(), running.id());

        running.append(SessionEvent::Assistant {
            id: running.gen_id(),
            chunk: Chunk {
                text: Some("A 的实时输出".into()),
                ..Default::default()
            },
        });
        assert!(log.replay().iter().any(|event| matches!(event,
            SessionEvent::Assistant { chunk, .. } if chunk.text.as_deref() == Some("A 的实时输出")
        )));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
