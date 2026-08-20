//! Application state and non-rendering session/project control behavior.

use super::*;

pub(super) struct AppState {
    pub(super) host: Arc<EguiUi>,
    pub(super) log: Arc<SessionLog>,
    pub(super) last_event: usize,
    pub(super) messages: Vec<ChatMsg>,
    pub(super) input: String,
    pub(super) busy: bool,
    pub(super) thinking: bool,
    /// 当前思考链累积文本（回合结束/正文到达时固化入气泡）。
    pub(super) thinking_text: String,
    /// 本回合开始时刻：状态栏展示「已用时 Ns」，长等待不再像假死。
    pub(super) turn_started: Option<std::time::Instant>,
    pub(super) dark: bool,
    pub(super) sidebar_expanded: bool,
    pub(super) settings_open: bool,
    pub(super) settings_page: String,
    // 模型设置表单
    pub(super) f_provider: String,
    pub(super) f_base: String,
    pub(super) f_model: String,
    pub(super) f_key: String,
    /// 思考档位 / 努力度（对齐 cc-switch thinkingLevelMap）：发给上游的 reasoning_effort 字符串。
    pub(super) f_effort: String,
    // aidops 后端连接配置表单（对应 Config.aidops；留空则仅用本地文件记忆）
    pub(super) f_aidops_base: String,
    pub(super) f_aidops_key: String,
    pub(super) f_aidops_project: String,
    pub(super) profiles: Vec<String>,
    pub(super) selected_profile: String,
    pub(super) attachment: String,
    pub(super) permission: String,
    /// 权限 chip 下拉菜单是否展开。用自定义 chip + Area 弹层代替默认 ComboBox，
    /// 保证与模型 chip 同 28px 高度 / 同圆角 / 同边框，水平基线对齐。
    pub(super) perm_menu_open: bool,
    /// 插件管理列表（内置核心项恒启用 + 用户导入的 WASM 插件）。
    pub(super) plugin_rows: Vec<PluginUiRow>,
    /// 上一帧记录的模态面板矩形：外部点击关闭时的内部误触守卫。
    pub(super) modal_panel_rect: Option<egui::Rect>,
    /// 上一帧模态是否已打开：打开当帧的 press 是触发点击，不能当作“外部点击”关闭。
    pub(super) modal_open_last_frame: bool,
    pub(super) note: String,
    // 侧栏项目列表（上下文隔离 / 快速切换）
    pub(super) projects: Vec<crate::ProjectRow>,
    pub(super) active_project: String,
    // 会话历史（侧栏「历史记录」面板，跨项目聚合）
    pub(super) history: Vec<SessionMeta>,
    /// 历史条目文件名 → 所属 sessions 目录（跨项目点击恢复定位用）。
    pub(super) history_dirs: std::collections::HashMap<String, std::path::PathBuf>,
    pub(super) history_search: String,
    /// 历史操作（精简 / 清空）反馈提示与展示起点（5 秒后自动隐去）。
    pub(super) history_note: String,
    pub(super) history_note_at: Option<std::time::Instant>,
    pub(super) current_session: String,
    /// 会话重命名：正在编辑标题的会话文件名（弹窗编辑）。
    pub(super) renaming: Option<String>,
    /// 重命名输入框缓冲。
    pub(super) rename_buf: String,
    /// 版本更新状态（后台线程写、GUI 主循环读）。
    pub(super) update_status: Arc<Mutex<UpdateStatus>>,
    /// 更新设置表单缓冲。
    pub(super) f_update_url: String,
    pub(super) f_update_channel: String,
    pub(super) f_auto_check: bool,
    pub(super) f_auto_install: bool,
    // 记忆面板状态（浏览本地原生记忆资产）
    pub(super) mem_tab: String,
    pub(super) mem_query: String,
    pub(super) mem_loaded: bool,
    pub(super) mem_items: Vec<MemItem>,
    /// 是否已对当前工作区执行过资产索引（首次打开记忆面板时自动执行一次）。
    pub(super) mem_bootstrapped: bool,
    /// 最近一次索引/操作的反馈信息。
    pub(super) mem_index_msg: String,
    /// 首次索引的异步回传通道（非阻塞轮询，避免首次点击卡顿）。
    pub(super) mem_boot_rx: Option<
        std::sync::mpsc::Receiver<
            harness_core::error::Result<(harness_capability::index::IndexStats, usize)>,
        >,
    >,
    /// 记忆面板刷新的异步回传通道。
    pub(super) mem_refresh_rx: Option<std::sync::mpsc::Receiver<Vec<MemItem>>>,
    // ── 文件预览（纯 UI 本地状态，不持久化、不进 SessionLog）──
    /// 预览窗是否展开。
    pub(super) preview_open: bool,
    /// 当前预览的文件相对路径（相对 workspace_root）。
    pub(super) preview_path: Option<String>,
    /// 预览窗内容缓存：避免每帧重读磁盘。
    pub(super) preview_content: Option<String>,
    /// 预览模式：源码 / Diff。
    pub(super) preview_mode: crate::preview::PreviewMode,
    /// diff 文本缓存（切换到 Diff 模式时按需加载）。
    pub(super) preview_diff: Option<String>,
    /// 文件是否受 git 跟踪（决定是否显示 Diff tab）。
    pub(super) preview_tracked: bool,
    /// 预览加载错误信息（文件不存在 / 超大 / 二进制）。
    pub(super) preview_error: Option<String>,
    /// 预览内容是否被截断（超过 512KB）。
    pub(super) preview_truncated: bool,
    /// 预览加载的异步回传通道（非阻塞模式：不在同帧等待结果）。
    pub(super) preview_rx: Option<std::sync::mpsc::Receiver<crate::preview::PreviewLoadResult>>,
    /// 延迟打开预览（渲染期间收集点击，渲染后处理，避免同帧布局突变闪烁）。
    pub(super) pending_preview: Option<String>,
    /// 预览内容缓存（path → (content, truncated)）：同一文件重复点击零加载、无空窗。
    pub(super) preview_cache: std::collections::HashMap<String, (String, bool)>,
    /// 当前预览的语法高亮 LayoutJob（preview_content 就绪后一次性生成，渲染零成本）。
    pub(super) preview_highlight: Option<egui::text::LayoutJob>,
    /// 文件树是否展开。
    pub(super) tree_open: bool,
    /// 文件树根节点（懒构建）。
    pub(super) tree_root: Option<crate::preview::FileTreeNode>,
    /// 文件树展开路径集合。
    pub(super) tree_expanded: std::collections::HashSet<String>,
    /// 文件树上次刷新时间（节流）。
    pub(super) tree_last_refresh: Option<std::time::Instant>,
    // ── Git 变更（统一入口：快速查看哪些文件被修改，点击审查 diff）──
    /// 有未提交变更的文件列表（含状态码）。
    pub(super) git_changes: Vec<harness_capability::git::GitChange>,
    /// 当前分支名。
    pub(super) git_branch: String,
    /// Git 变更是否已加载过（避免重复刷新）。
    pub(super) git_loaded: bool,
    /// 文件树区域当前视图：true = Git 变更列表，false = 文件树。
    pub(super) tree_show_git: bool,
}

impl AppState {
    pub(super) fn new(host: Arc<EguiUi>, log: Arc<SessionLog>) -> Self {
        let settings = &host.settings;
        let dark = settings.get("ui.theme").as_deref() != Some("light");
        // 活跃项目：优先 settings 持久化的上次选择，回退启动时工作区根；并确保入库。
        let active_project = settings
            .get("workspace.root")
            .filter(|p| std::path::Path::new(p).is_dir())
            .unwrap_or_else(|| host.workspace_root.clone());
        let _ = settings.add_project(std::path::Path::new(&active_project));
        let projects = settings.projects();
        // 首次升级时把旧的单模型配置迁移为模型配置列表。
        if settings.model_profiles().is_empty() {
            if let Some(key) = settings.get("llm.api_key") {
                let _ = settings.save_model_profile(&crate::ModelProfile {
                    name: format!("{} · {}", host.provider, host.model),
                    provider: host.provider.clone(),
                    base_url: host.base_url.clone(),
                    model: host.model.clone(),
                    api_key: key,
                });
            }
        }
        let mut state = Self {
            profiles: settings
                .model_profiles()
                .into_iter()
                .map(|p| p.name)
                .collect(),
            f_provider: host.provider.clone(),
            f_base: host.base_url.clone(),
            f_model: host.model.clone(),
            f_key: String::new(),
            f_effort: settings.get("llm.reasoning_effort").unwrap_or_default(),
            // aidops 后端连接：从 .harness.toml 的 [aidops] 段加载（无则空，仅用本地记忆）。
            f_aidops_base: Config::load()
                .map(|c| c.aidops.base_url)
                .unwrap_or_default(),
            f_aidops_key: Config::load()
                .ok()
                .and_then(|c| c.aidops.api_key)
                .unwrap_or_default(),
            f_aidops_project: Config::load()
                .ok()
                .and_then(|c| c.aidops.project_id)
                .map(|v| v.to_string())
                .unwrap_or_default(),
            selected_profile: String::new(),
            attachment: String::new(),
            permission: settings
                .get("permission.mode")
                .unwrap_or_else(|| "工作区写入".into()),
            plugin_rows: Self::load_plugin_rows(settings, &host.wasm_plugins),
            modal_panel_rect: None,
            modal_open_last_frame: false,
            last_event: 0,
            messages: vec![ChatMsg {
                kind: "assistant".into(),
                label: "助手".into(),
                text: READY.into(),
                raw: String::new(),
            }],
            input: String::new(),
            busy: false,
            thinking: false,
            thinking_text: String::new(),
            turn_started: None,
            dark,
            sidebar_expanded: true,
            settings_open: false,
            settings_page: "模型设置".into(),
            note: String::new(),
            projects,
            active_project,
            history: Vec::new(),
            history_dirs: std::collections::HashMap::new(),
            history_search: String::new(),
            history_note: String::new(),
            history_note_at: None,
            current_session: String::new(),
            renaming: None,
            rename_buf: String::new(),
            perm_menu_open: false,
            // 版本更新：节流 24h 的后台自动检查（manifest_url 为空时跳过）。
            update_status: {
                let s = Arc::new(Mutex::new(UpdateStatus::Idle));
                let auto_check = settings
                    .get("update.auto_check")
                    .map(|v| v == "true")
                    .unwrap_or(true);
                let last = settings
                    .get("update.last_check_ts")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                if auto_check && harness_core::update::now_secs().saturating_sub(last) > 24 * 3600 {
                    let url = settings
                        .get("update.manifest_url")
                        .unwrap_or_else(|| harness_core::update::DEFAULT_MANIFEST_URL.to_string());
                    let ch = settings
                        .get("update.channel")
                        .unwrap_or_else(|| "stable".into());
                    let skip = settings.get("update.skipped_version").unwrap_or_default();
                    harness_core::update::spawn_check(s.clone(), &url, &ch, &skip, false);
                    let _ = settings.set(
                        "update.last_check_ts",
                        &harness_core::update::now_secs().to_string(),
                    );
                }
                s
            },
            f_update_url: settings
                .get("update.manifest_url")
                .unwrap_or_else(|| harness_core::update::DEFAULT_MANIFEST_URL.to_string()),
            f_update_channel: settings
                .get("update.channel")
                .unwrap_or_else(|| "stable".into()),
            f_auto_check: settings
                .get("update.auto_check")
                .map(|v| v == "true")
                .unwrap_or(true),
            f_auto_install: settings
                .get("update.auto_install")
                .map(|v| v == "true")
                .unwrap_or(false),
            // 记忆面板状态（浏览本地原生记忆资产）
            mem_tab: String::new(),
            mem_query: String::new(),
            mem_loaded: false,
            mem_items: Vec::new(),
            mem_bootstrapped: false,
            mem_index_msg: String::new(),
            mem_boot_rx: None,
            mem_refresh_rx: None,
            // 文件预览初始状态
            preview_open: false,
            preview_path: None,
            preview_content: None,
            preview_mode: crate::preview::PreviewMode::Source,
            preview_diff: None,
            preview_tracked: false,
            preview_error: None,
            preview_truncated: false,
            preview_rx: None,
            pending_preview: None,
            preview_cache: std::collections::HashMap::new(),
            preview_highlight: None,
            tree_open: false,
            tree_root: None,
            tree_expanded: std::collections::HashSet::new(),
            tree_last_refresh: None,
            // Git 变更初始状态
            git_changes: Vec::new(),
            git_branch: String::new(),
            git_loaded: false,
            tree_show_git: false,
            // host/log 放最后：上方字段仍需借用 host.settings，提前移入会报 E0505。
            host,
            log,
        };
        state.refresh_history();
        state
    }

    pub(super) fn push(&mut self, kind: &str, label: &str, text: &str) {
        self.messages.push(ChatMsg {
            kind: kind.into(),
            label: label.into(),
            text: text.into(),
            raw: String::new(),
        });
    }

    pub(super) fn append_assistant(&mut self, text: &str) {
        if text.starts_with("[error]") || text.starts_with("[已停止]") {
            self.push("error", "系统", text);
            return;
        }
        // 旧日志防御：按气泡「累积原文 + 整体重算剥离」。SSE 分片可能把 DSML 块
        // 切成多段，逐片 strip 无法移除跨片块；整体重算保证块完整后即被剥离。
        if let Some(last) = self.messages.last_mut() {
            if last.kind == "assistant" {
                last.raw.push_str(text);
                last.text = harness_llm::dsml::strip_dsml(&last.raw);
                return;
            }
        }
        let raw = text.to_string();
        let stripped = harness_llm::dsml::strip_dsml(&raw);
        self.messages.push(ChatMsg {
            kind: "assistant".into(),
            label: "助手".into(),
            text: stripped,
            raw,
        });
    }

    /// 当前思考档位（reasoning_effort）的 Option 形态：空串视作 None。
    pub(super) fn effort(&self) -> Option<String> {
        let e = self.f_effort.trim();
        if e.is_empty() {
            None
        } else {
            Some(e.to_string())
        }
    }

    /// 轮询 SessionLog 真相源，把新事件转成气泡。
    pub(super) fn poll_log(&mut self) {
        let (next, events) = self.log.replay_from(self.last_event);
        if events.is_empty() {
            return;
        }
        for event in &events {
            match event {
                SessionEvent::TurnStart { input, .. } => self.push("user", "你", input),
                SessionEvent::Assistant { chunk, .. } => {
                    if let Some(text) = &chunk.text {
                        self.finalize_thinking();
                        self.append_assistant(text);
                    }
                }
                SessionEvent::Thinking { text, .. } => {
                    // 思考链增量：累积全文并实时覆盖尾部「思考」气泡（只展示最近几十字，
                    // 不刷屏），长推理期用户能看到内容在滚动，而不是只剩状态栏一个标志。
                    self.thinking = true;
                    self.thinking_text.push_str(text);
                    if self.thinking_text.chars().count() > 400 {
                        let total = self.thinking_text.chars().count();
                        let keep: String = self.thinking_text.chars().skip(total - 400).collect();
                        self.thinking_text = keep;
                    }
                    self.update_thinking_bubble();
                }
                SessionEvent::ToolCall { call, .. } => {
                    self.finalize_thinking();
                    // 参数摘要（≤120 字）：agent 行为全程可见。
                    let summary: String = call.args.to_string().chars().take(120).collect();
                    self.push("tool", "工具", &format!("调用 {}: {}", call.name, summary));
                }
                SessionEvent::ToolResult { result, .. } => {
                    let preview: String = result.content.chars().take(400).collect();
                    self.push(
                        "tool",
                        "工具",
                        &format!("{} 返回: {}", if result.ok { "->" } else { "X" }, preview),
                    );
                }
                SessionEvent::PlanUpdate { items, .. } => {
                    let mut s = String::from("[计划]\n");
                    for (i, item) in items.iter().enumerate() {
                        let mark = match item.status.as_str() {
                            "done" => "✓",
                            "doing" => "…",
                            _ => "·",
                        };
                        s.push_str(&format!("{}. {} {}\n", i + 1, mark, item.text));
                    }
                    self.push("plan", "计划", s.trim_end());
                }
                SessionEvent::TurnEnd { .. } => {
                    self.finalize_thinking();
                    self.turn_started = None;
                    // 回合已完整落盘：刷新历史列表（mtime / 标题可能变化）。
                    self.refresh_history();
                }
                _ => {}
            }
        }
        self.last_event = next;
        trace(&format!("[log] +{} events processed", events.len()));
    }

    /// 思考直播气泡：消息流尾部增量覆盖，只展示最近几十字；
    /// 正文/工具事件到达后由 finalize_thinking 固化为完整内容。
    pub(super) fn update_thinking_bubble(&mut self) {
        let preview: String = self
            .thinking_text
            .chars()
            .rev()
            .take(60)
            .collect::<Vec<char>>()
            .into_iter()
            .rev()
            .collect();
        let text = format!("💭 {preview}…");
        if let Some(last) = self.messages.last_mut() {
            if last.kind == "thinking" {
                last.text = text;
                return;
            }
        }
        self.push("thinking", "思考", &text);
    }

    /// 思考结束：把完整思考内容固化为灰色气泡（保留尾部 ≤500 字），
    /// 直接替换直播气泡避免重复；供正文/工具调用/回合结束调用。
    pub(super) fn finalize_thinking(&mut self) {
        self.thinking = false;
        if self.thinking_text.is_empty() {
            return;
        }
        let mut t = std::mem::take(&mut self.thinking_text);
        let total = t.chars().count();
        if total > 500 {
            let tail: String = t.chars().skip(total - 500).collect();
            t = format!("…{tail}");
        }
        if let Some(last) = self.messages.last_mut() {
            if last.kind == "thinking" {
                last.text = t;
                return;
            }
        }
        self.push("thinking", "思考", &t);
    }

    pub(super) fn submit(&mut self) {
        let mut text = self.input.trim().to_string();
        if text.is_empty() || self.busy {
            return;
        }
        if !self.attachment.trim().is_empty() {
            text.push_str(&format!("\n\n[附件: {}]", self.attachment));
        }
        let sink = self.host.sink.clone();
        sink.set_permission(self.permission.clone());
        let settings = &self.host.settings;
        let _ = settings.set("permission.mode", &self.permission);
        let _ = settings.set("llm.model", &self.f_model);
        let _ = settings.set("llm.provider", &self.f_provider);
        if let (Some(base), Some(key)) = (settings.get("llm.base_url"), settings.get("llm.api_key"))
        {
            let _ = self.host.llm_control.configure_provider(
                self.f_provider.clone(),
                base,
                self.f_model.clone(),
                key,
                self.effort(),
            );
        }
        // 用户气泡不在此本地推入：TurnStart 事件是真相源，poll_log 会渲染，
        // 本地再推一条会导致同一问题显示两次。
        trace(&format!("[send] you: {text}"));
        self.input.clear();
        self.busy = true;
        self.thinking_text.clear();
        self.turn_started = Some(std::time::Instant::now());
        sink.submit(text);
    }

    pub(super) fn new_session(&mut self) {
        self.host.sink.new_session();
        self.current_session = self
            .log
            .path()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()))
            .unwrap_or_default();
        self.last_event = 0;
        self.messages = vec![ChatMsg {
            kind: "assistant".into(),
            label: "助手".into(),
            text: READY.into(),
            raw: String::new(),
        }];
        self.thinking_text.clear();
        self.turn_started = None;
        self.refresh_history();
        trace("[session] new session");
    }

    /// 历史面板数据刷新：跨项目聚合全部会话（各项目 sessions 目录 +
    /// 当前日志目录），mtime 倒序；并记录每条所属目录与活跃会话文件名。
    pub(super) fn refresh_history(&mut self) {
        let mut seen_dirs: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();
        let mut dirs: Vec<std::path::PathBuf> = Vec::new();
        for proj in self.host.settings.projects() {
            if proj.archived {
                continue;
            }
            let d = std::path::Path::new(&proj.path)
                .join(".harness")
                .join("sessions");
            if d.is_dir() {
                if let Ok(c) = d.canonicalize() {
                    if seen_dirs.insert(c) {
                        dirs.push(d);
                    }
                }
            }
        }
        if let Some(d) = self.log.dir() {
            if let Ok(c) = d.canonicalize() {
                if seen_dirs.insert(c) {
                    dirs.push(d);
                }
            }
        }
        let mut all: Vec<SessionMeta> = Vec::new();
        let mut map = std::collections::HashMap::new();
        for d in &dirs {
            for m in harness_session::list_sessions(d) {
                map.insert(m.file.clone(), d.clone());
                all.push(m);
            }
        }
        all.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        self.history = all;
        self.history_dirs = map;
        self.current_session = self
            .log
            .path()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().to_string()))
            .unwrap_or_default();
    }

    /// 点击恢复历史会话：支持跨项目——目标会话属于其他项目时先切工作区根
    /// （对齐侧栏项目切换），再 SessionLog 切到该文件继续追加；
    /// poll_log 下帧从 0 重放全部消息流。忙碌时拒绝避免回合穿插。
    pub(super) fn switch_session(&mut self, file: &str) {
        trace(&format!(
            "[session] restore attempt {file} busy={}",
            self.busy
        ));
        if self.busy {
            return;
        }
        let Some(dir) = self.history_dirs.get(file).cloned() else {
            return;
        };
        // 跨项目会话：先切工作区根（工具上下文与项目列表同步），
        // switch_workspace 会重载该目录最近会话，随后再精确定位到目标文件。
        if self.log.dir().as_ref() != Some(&dir) {
            if let Some(root) = dir.parent().and_then(|p| p.parent()) {
                let path = root.display().to_string();
                let _ = self.host.settings.set("workspace.root", &path);
                self.host.sink.switch_workspace(root);
                self.active_project = path;
                self.projects = self.host.settings.projects();
            }
        }
        if !self.log.switch_to_file(&dir, file) {
            return;
        }
        self.current_session = file.to_string();
        self.last_event = 0;
        self.messages = vec![ChatMsg {
            kind: "assistant".into(),
            label: "助手".into(),
            text: READY.into(),
            raw: String::new(),
        }];
        self.input.clear();
        self.thinking = false;
        self.thinking_text.clear();
        self.turn_started = None;
        trace(&format!("[session] restored {file}"));
    }

    /// 删除单条历史会话；删当前活跃会话时先新建空会话接替。
    pub(super) fn delete_session_entry(&mut self, file: &str) {
        if self.busy {
            return;
        }
        let Some(dir) = self.history_dirs.get(file).cloned() else {
            return;
        };
        if file == self.current_session {
            self.new_session();
        }
        harness_session::delete_session(&dir, file);
        self.refresh_history();
        trace(&format!("[session] deleted {file}"));
    }

    /// 清空全部历史：删除活跃会话之外的全部会话文件（跨项目）。
    pub(super) fn clear_history(&mut self) {
        if self.busy {
            return;
        }
        if self.current_session.is_empty() {
            self.new_session();
        }
        let victims: Vec<(String, std::path::PathBuf)> = self
            .history
            .iter()
            .filter(|m| m.file != self.current_session)
            .filter_map(|m| {
                self.history_dirs
                    .get(&m.file)
                    .map(|d| (m.file.clone(), d.clone()))
            })
            .collect();
        for (f, dir) in &victims {
            harness_session::delete_session(dir, f);
        }
        self.refresh_history();
        self.history_note = if victims.is_empty() {
            "没有可清空的历史会话".into()
        } else {
            format!("已清空：删除 {} 个历史会话", victims.len())
        };
        self.history_note_at = Some(std::time::Instant::now());
        trace("[session] history cleared");
    }

    /// 精简历史：每个 sessions 目录仅保留最近 30 个会话（按 mtime），当前活跃会话永不删。
    pub(super) fn prune_history(&mut self) {
        if self.busy {
            return;
        }
        let keep: usize = 30;
        let before = self.history.len();
        let active = if self.current_session.is_empty() {
            None
        } else {
            Some(self.current_session.as_str())
        };
        let dirs: std::collections::HashSet<std::path::PathBuf> =
            self.history_dirs.values().cloned().collect();
        for d in &dirs {
            harness_session::prune_sessions(d, keep, active);
        }
        self.refresh_history();
        let removed = before.saturating_sub(self.history.len());
        // 明确反馈：会话数未超上限时也要告知「无需精简」，避免「点了没反应」的观感。
        self.history_note = if removed == 0 {
            "无需精简：每个项目会话均未超过 30 个".into()
        } else {
            format!("已精简：删除 {removed} 个旧会话（当前对话保留）")
        };
        self.history_note_at = Some(std::time::Instant::now());
        trace("[session] history pruned");
    }

    /// 侧栏项目切换：换工作区根 + 重载该项目会话历史 + 清空输入框 + 气泡提示
    ///（对齐 Codex/Cursor 的上下文切换交互）。忙碌时拒绝以避免回合穿插写错日志。
    pub(super) fn switch_project(&mut self, path: &str) {
        if path == self.active_project || self.busy {
            return;
        }
        let p = std::path::PathBuf::from(path);
        let name = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("项目")
            .to_string();
        let _ = self.host.settings.set("workspace.root", path);
        let _ = self.host.settings.add_project(&p);
        // 反向通道：Workspace 换根 + SessionLog 重载新项目目录的最近会话。
        self.host.sink.switch_workspace(&p);
        // 清空旧项目的预览与文件树缓存（基准根统一从 settings 读取）。
        self.preview_open = false;
        self.preview_path = None;
        self.preview_content = None;
        self.preview_diff = None;
        self.preview_error = None;
        self.preview_rx = None;
        self.preview_cache.clear();
        self.tree_open = false;
        self.tree_root = None;
        self.tree_expanded.clear();
        self.tree_show_git = false;
        self.active_project = path.to_string();
        self.projects = self.host.settings.projects();
        // 视图复位：poll_log 下帧从 0 重放，右侧消息流刷新为该项目历史。
        self.last_event = 0;
        self.messages = vec![ChatMsg {
            kind: "assistant".into(),
            label: "助手".into(),
            text: READY.into(),
            raw: String::new(),
        }];
        self.input.clear();
        self.thinking = false;
        self.thinking_text.clear();
        self.push("tool", "系统", &format!("已切换到项目 {name}"));
        self.refresh_history();
        trace(&format!("[project] switched to {path}"));
    }

    pub(super) fn apply_model(&mut self) {
        let settings = &self.host.settings;
        let key = if self.f_key.trim().is_empty() {
            settings.get("llm.api_key").unwrap_or_default()
        } else {
            std::mem::take(&mut self.f_key)
        };
        let result = self.host.llm_control.configure_provider(
            self.f_provider.clone(),
            self.f_base.clone(),
            self.f_model.clone(),
            key.clone(),
            self.effort(),
        );
        match result {
            Ok(()) => {
                let _ = settings.set("llm.base_url", &self.f_base);
                let _ = settings.set("llm.model", &self.f_model);
                let _ = settings.set("llm.provider", &self.f_provider);
                let _ = settings.set_secret("llm.api_key", &key);
                let _ = settings.set("llm.reasoning_effort", &self.f_effort);
                let name = format!("{} · {}", self.f_provider, self.f_model);
                let _ = settings.save_model_profile(&crate::ModelProfile {
                    name: name.clone(),
                    provider: self.f_provider.clone(),
                    base_url: self.f_base.clone(),
                    model: self.f_model.clone(),
                    api_key: key,
                });
                self.profiles = settings
                    .model_profiles()
                    .into_iter()
                    .map(|p| p.name)
                    .collect();
                self.selected_profile = name;
                self.note = "模型配置已保存并应用".into();
                // 不自动关闭：note 在弹窗内可见，给用户明确的保存反馈。
                trace("[config] model configuration applied");
            }
            Err(error) => {
                self.note = format!("配置错误: {error}");
                trace(&format!("[config] rejected: {error}"));
            }
        }
    }
}
