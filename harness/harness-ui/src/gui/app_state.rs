//! Application state and non-rendering session/project control behavior.

use super::*;

pub(super) struct AppState {
    pub(super) host: Arc<EguiUi>,
    pub(super) log: Arc<SessionLog>,
    pub(super) last_event: usize,
    pub(super) messages: Vec<ChatMsg>,
    /// 唯一由 Runtime Delivery 事件更新；不能由模型文本或 TurnEnd 推导成功。
    pub(super) delivery: Option<DeliveryUi>,
    /// 当前运行时阶段/白名单/证据的只读投影，供主面板在执行中显示。
    pub(super) execution_projection: Option<ExecutionProjectionUi>,
    /// 运行时遥测卡片是否展开（默认折叠为单行摘要，最大限度节省垂直空间）。
    /// 纯 UI 内存状态，不持久化；每次启动默认折叠。
    pub(super) runtime_expanded: bool,
    /// 专家团结构化投影（由持久化 CouncilEvent 重建）。
    pub(super) councils: std::collections::BTreeMap<String, CouncilUi>,
    pub(super) input: String,
    pub(super) busy: bool,
    pub(super) thinking: bool,
    /// 团队协作模式开关。启用后发送消息时自动包裹 [HARNESS_EXPERT_COUNCIL]，
    /// 由 CouncilOrchestrator 编排专家协作（并非主 Agent 的 delegate 拆分）。
    pub(super) multi_agent: bool,
    /// 当前思考链累积文本（回合结束/正文到达时固化入气泡）。
    pub(super) thinking_text: String,
    /// 本回合开始时刻：状态栏展示「已用时 Ns」，长等待不再像假死。
    pub(super) turn_started: Option<std::time::Instant>,
    /// 当前后台阶段：由会话事件驱动，明确长任务正卡在哪个环节。
    pub(super) activity: String,
    /// 最近一次收到后台事件的时刻，用于提示用户是否仍有进展。
    pub(super) last_activity: Option<std::time::Instant>,
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
    /// 上游模型列表（点「获取上游模型列表」后填充）。
    pub(super) f_models: Vec<String>,
    /// 已勾选要启用的模型集合（空 = 未选择，保存时取第一个）。
    pub(super) f_selected_models: std::collections::HashSet<String>,
    /// 正在获取模型列表（按钮 loading 态）。
    pub(super) models_loading: bool,
    /// 获取模型列表的错误/状态提示。
    pub(super) models_msg: String,
    /// 获取模型列表的异步回传通道（非阻塞轮询）。
    pub(super) models_rx:
        Option<std::sync::mpsc::Receiver<std::result::Result<Vec<String>, String>>>,
    // aidops 后端连接配置表单（对应 Config.aidops；留空则仅用本地文件记忆）
    pub(super) f_aidops_base: String,
    pub(super) f_aidops_key: String,
    pub(super) f_aidops_project: String,
    /// 已保存的模型配置列表缓存（避免每帧查库 + 解密 Key；增删改后调 refresh_profiles）。
    pub(super) profiles: Vec<crate::ModelProfile>,
    /// 正在编辑的配置名（None = 新增模式；保存时按此定位覆写 / 重命名迁移）。
    pub(super) editing_profile: Option<String>,
    /// 当前待发送附件。可由文件选择或粘贴文件路径加入。
    pub(super) attachments: Vec<harness_core::Attachment>,
    pub(super) permission: String,
    // 运行时调参（「参数配置」页）：上下文预算（字符）/ 进展检查间隔 / 最大输出 tokens。
    // 空字符串 = 未配置（回退环境变量 / 默认值）；保存时写入 settings.db 并即时生效。
    pub(super) f_context_budget: String,
    pub(super) f_max_steps: String,
    pub(super) f_max_tokens: String,
    /// 权限 chip 下拉菜单是否展开。用自定义 chip + Area 弹层代替默认 ComboBox，
    /// 保证与模型 chip 同 28px 高度 / 同圆角 / 同边框，水平基线对齐。
    pub(super) perm_menu_open: bool,
    /// 模型 chip 下拉菜单是否展开：直接在下拉列表切换模型，不再跳转设置页。
    pub(super) model_menu_open: bool,
    /// 插件管理列表（内置核心项恒启用 + 用户导入的 WASM 插件）。
    pub(super) plugin_rows: Vec<PluginUiRow>,
    /// 插件管理页当前 tab（"sys" = 系统插件，其余 = 自定义插件）。
    pub(super) plugin_tab: String,
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
    /// 代码图谱：原始符号列表（code tab 由结构化视图消费，不再压成 MemItem 文本）。
    pub(super) mem_code_symbols: Vec<harness_capability::assets::CodeSymbol>,
    /// 代码图谱：已展开的文件分组（按文件路径为键）。
    pub(super) mem_code_expanded: std::collections::HashSet<String>,
    /// 代码图谱：当前选中符号 id（点击详情/关系 chip 导航用）。
    pub(super) mem_code_sel: Option<String>,
    /// 代码图谱：跳转后需要滚动回顶的提示（下帧消费后清空）。
    pub(super) mem_code_scroll: bool,
    /// 记忆面板数据是否仍在加载（异步刷新进行中，展示轻量 loading）。
    /// 当前未消费（刷新结果经 poll_mem 直接落盘），保留供后续加载态指示。
    #[allow(dead_code)]
    pub(super) mem_loading: bool,
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
    pub(super) mem_refresh_rx: Option<std::sync::mpsc::Receiver<MemRefresh>>,
    /// 技能管理数据：当前全部技能（含状态），供技能 tab 管理界面使用。
    pub(super) skill_items: Vec<harness_capability::assets::Skill>,
    // ── 文件预览（纯 UI 本地状态，不持久化、不进 SessionLog）──
    /// 预览窗是否展开。
    pub(super) preview_open: bool,
    /// 预览面板是否仍在开关动画中（动画结束后才真正释放面板，避免关闭瞬间跳变）。
    pub(super) preview_animating: bool,
    /// 预览浮层宽度（用户可拖拽调整；浮层不占布局，消息流宽度恒定）。
    pub(super) preview_width: f32,
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
    /// 延迟剪贴板写入：渲染闭包内点击复制时先暂存文本，帧末统一写入，
    /// 避免布局期间触碰系统剪贴板产生副作用。
    pub(super) pending_copy: Option<String>,
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
    /// Git 查询错误。错误与“干净”必须是不同状态，绝不能静默降级为空列表。
    pub(super) git_error: Option<String>,
    /// 发起本次刷新时的工作区；用于拒绝项目切换后的旧异步结果。
    pub(super) git_workspace: String,
    /// 单调刷新代次，防止慢请求覆盖新项目/新请求的状态。
    pub(super) git_generation: u64,
    /// Git 刷新的非阻塞回传通道。
    pub(super) git_rx: Option<std::sync::mpsc::Receiver<GitRefreshResult>>,
    /// 文件树区域当前视图：true = Git 变更列表，false = 文件树。
    pub(super) tree_show_git: bool,
    // ── 输入优化（Transformative：将用户原始输入重写为 LLM 友好格式）──
    /// 正在优化输入（按钮 loading 态）。
    pub(super) optimizing: bool,
    /// 优化结果回传通道（非阻塞轮询）。
    pub(super) optimize_rx: Option<std::sync::mpsc::Receiver<std::result::Result<String, String>>>,
    /// 优化错误/状态提示。
    pub(super) optimize_msg: String,
}

pub(super) struct GitRefreshResult {
    pub(super) generation: u64,
    pub(super) workspace: String,
    pub(super) result: std::result::Result<GitRefreshData, String>,
}

pub(super) struct GitRefreshData {
    pub(super) repo_root: String,
    pub(super) branch: String,
    pub(super) changes: Vec<harness_capability::git::GitChange>,
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
                    enabled: true,
                });
            }
        }
        let mut state = Self {
            profiles: settings.model_profiles(),
            f_provider: host.provider.clone(),
            f_base: host.base_url.clone(),
            f_model: host.model.clone(),
            f_key: String::new(),
            f_effort: settings.get("llm.reasoning_effort").unwrap_or_default(),
            f_models: Vec::new(),
            f_selected_models: std::collections::HashSet::new(),
            models_loading: false,
            models_msg: String::new(),
            models_rx: None,
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
            editing_profile: None,
            attachments: Vec::new(),
            permission: settings
                .get("permission.mode")
                .unwrap_or_else(|| "工作区写入".into()),
            f_context_budget: settings.get("runtime.context_budget").unwrap_or_default(),
            f_max_steps: settings.get("runtime.max_steps").unwrap_or_default(),
            f_max_tokens: settings.get("runtime.max_tokens").unwrap_or_default(),
            plugin_rows: Self::load_plugin_rows(settings, &host.wasm_plugins, &host.trellis),
            plugin_tab: "sys".into(),
            modal_panel_rect: None,
            modal_open_last_frame: false,
            last_event: 0,
            messages: vec![ChatMsg {
                kind: "assistant".into(),
                label: "助手".into(),
                text: READY.into(),
                raw: String::new(),
            }],
            delivery: None,
            execution_projection: None,
            runtime_expanded: false,
            councils: std::collections::BTreeMap::new(),
            input: String::new(),
            busy: false,
            thinking: false,
            multi_agent: settings.get("ui.multi_agent").as_deref() == Some("true"),
            thinking_text: String::new(),
            turn_started: None,
            activity: String::new(),
            last_activity: None,
            dark,
            sidebar_expanded: true,
            settings_open: false,
            settings_page: "模型配置".into(),
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
            model_menu_open: false,
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
            mem_code_symbols: Vec::new(),
            mem_code_expanded: std::collections::HashSet::new(),
            mem_code_sel: None,
            mem_code_scroll: false,
            mem_loading: false,
            mem_bootstrapped: false,
            mem_index_msg: String::new(),
            mem_boot_rx: None,
            mem_refresh_rx: None,
            skill_items: Vec::new(),
            // 文件预览初始状态
            preview_open: false,
            preview_animating: false,
            preview_width: 420.0,
            preview_path: None,
            preview_content: None,
            preview_mode: crate::preview::PreviewMode::Source,
            preview_diff: None,
            preview_tracked: false,
            preview_error: None,
            preview_truncated: false,
            preview_rx: None,
            pending_preview: None,
            pending_copy: None,
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
            git_error: None,
            git_workspace: String::new(),
            git_generation: 0,
            git_rx: None,
            tree_show_git: false,
            optimizing: false,
            optimize_rx: None,
            optimize_msg: String::new(),
            // host/log 放最后：上方字段仍需借用 host.settings，提前移入会报 E0505。
            host,
            log,
        };
        // 运行时调参：把持久化的参数配置载入进程级开关（agent 循环 / LLM 客户端读取）。
        // 空字符串解析失败 → None → 回退环境变量 / 默认值。
        harness_core::tuning::set_context_budget_chars(state.f_context_budget.parse().ok());
        harness_core::tuning::set_max_steps(state.f_max_steps.parse().ok());
        harness_core::tuning::set_max_output_tokens(state.f_max_tokens.parse().ok());
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
        // trace 摘要：类型分布 + 关键细节（回合边界/步号/工具名与参数/结果成败/
        // token 用量），取代旧版无信息的「+N events processed」计数行。
        let mut assistant_chunks = 0usize;
        let mut assistant_chars = 0usize;
        let mut thinking_chars = 0usize;
        let mut prompt_tokens = 0u64;
        let mut completion_tokens = 0u64;
        let mut details: Vec<String> = Vec::new();
        for event in &events {
            match event {
                SessionEvent::TurnStart { input, .. } => {
                    details.push(format!("turn_start「{}」", brief(input, 40)));
                    self.push("user", "你", input);
                    self.delivery = None;
                    self.execution_projection = None;
                    // 队列中的任务真正开始执行时重新计时，不能沿用前一个任务的耗时。
                    self.turn_started = Some(std::time::Instant::now());
                    self.record_activity("正在准备上下文");
                }
                SessionEvent::StepStart { step, .. } => {
                    details.push(format!("step {step}"));
                    self.record_activity(&format!("正在请求模型（第 {step} 步）"));
                }
                SessionEvent::Assistant { chunk, .. } => {
                    if let Some(text) = &chunk.text {
                        assistant_chunks += 1;
                        assistant_chars += text.chars().count();
                        self.finalize_thinking();
                        self.append_assistant(text);
                        self.record_activity("正在生成回复");
                    }
                }
                SessionEvent::Thinking { text, .. } => {
                    thinking_chars += text.chars().count();
                    // 思考链增量：累积全文并实时覆盖尾部「思考」气泡（只展示最近几十字，
                    // 不刷屏），长推理期用户能看到内容在滚动，而不是只剩状态栏一个标志。
                    self.thinking = true;
                    self.record_activity("模型正在思考");
                    self.thinking_text.push_str(text);
                    if self.thinking_text.chars().count() > 400 {
                        let total = self.thinking_text.chars().count();
                        let keep: String = self.thinking_text.chars().skip(total - 400).collect();
                        self.thinking_text = keep;
                    }
                    self.update_thinking_bubble();
                }
                SessionEvent::ToolCall { call, .. } => {
                    details.push(format!("tool {} {}", call.name, args_brief(&call.args)));
                    self.finalize_thinking();
                    self.record_activity(&format!("正在执行工具：{}", call.name));
                    // 参数摘要（≤120 字）：agent 行为全程可见。
                    let summary: String = call.args.to_string().chars().take(120).collect();
                    self.push("tool", "工具", &format!("调用 {}: {}", call.name, summary));
                }
                SessionEvent::ToolResult { result, .. } => {
                    details.push(if result.ok {
                        format!("tool_result ok {}ch", result.content.chars().count())
                    } else {
                        format!("tool_result FAIL「{}」", brief(&result.content, 80))
                    });
                    self.record_activity(if result.ok {
                        "已收到工具结果，继续分析"
                    } else {
                        "工具执行未成功，正在调整"
                    });
                    let preview: String = result.content.chars().take(400).collect();
                    self.push(
                        "tool",
                        "工具",
                        &format!("{} 返回: {}", if result.ok { "->" } else { "X" }, preview),
                    );
                }
                SessionEvent::PlanUpdate { items, .. } => {
                    details.push(format!("plan {}项", items.len()));
                    let mut s = String::from("[计划]\n");
                    for (i, item) in items.iter().enumerate() {
                        let mark = match item.status.as_str() {
                            "claimed_done" => "!",
                            "doing" => "…",
                            "blocked" => "×",
                            _ => "·",
                        };
                        s.push_str(&format!("{}. {} {}\n", i + 1, mark, item.text));
                    }
                    self.push("plan", "计划", s.trim_end());
                }
                SessionEvent::Delivery { report, .. } => {
                    let remaining = report
                        .criteria
                        .iter()
                        .filter(|item| !item.satisfied)
                        .count();
                    details.push(format!(
                        "delivery {:?} remaining={remaining}",
                        report.outcome
                    ));
                    self.delivery = Some(DeliveryUi {
                        outcome: report.outcome.clone(),
                        remaining,
                        verification_count: report.verification.len(),
                        reason: report.reason.clone(),
                    });
                }
                SessionEvent::Usage { usage, .. } => {
                    prompt_tokens += usage.prompt_tokens;
                    completion_tokens += usage.completion_tokens;
                }
                SessionEvent::Telemetry { telemetry, .. } => {
                    details.push(format!("telemetry {}", telemetry.phase));
                    self.execution_projection = Some(ExecutionProjectionUi {
                        executor: telemetry.executor.clone(),
                        goal: telemetry.goal.clone(),
                        intent: telemetry.intent.clone(),
                        phase: telemetry.phase.clone(),
                        allowed_tools: telemetry.allowed_tools.clone(),
                        step: telemetry.step,
                        tool_calls: telemetry.tool_calls,
                        evidence_count: telemetry.evidence_count,
                        verified_count: telemetry.verified_count,
                        blocked_count: telemetry.blocked_count,
                        active_work_item: telemetry.active_work_item.clone(),
                        work_items: telemetry.work_items.clone(),
                        next_action: telemetry.next_action.clone(),
                        active_hypothesis: telemetry.active_hypothesis.clone(),
                        no_information_count: telemetry.no_information_count,
                        correction_count: telemetry.correction_count,
                        detail: telemetry.detail.clone(),
                    });
                }
                SessionEvent::Council { event, .. } => {
                    details.push("council".into());
                    self.apply_council_event(event);
                }
                SessionEvent::TurnEnd { .. } => {
                    details.push("turn_end".into());
                    self.finalize_thinking();
                    self.turn_started = None;
                    let queued = self.host.sink.queued_count();
                    if queued > 0 {
                        self.record_activity(&format!("当前任务完成，等待队列中 {queued} 条任务"));
                    } else {
                        self.activity.clear();
                        self.last_activity = None;
                    }
                    // 回合已完整落盘：刷新历史列表（mtime / 标题可能变化）。
                    self.refresh_history();
                }
                _ => {}
            }
        }
        self.last_event = next;
        let mut line = format!("[log] +{} events", events.len());
        if assistant_chunks > 0 {
            line.push_str(&format!(
                " | assistant×{assistant_chunks}({assistant_chars}ch)"
            ));
        }
        if thinking_chars > 0 {
            line.push_str(&format!(" | thinking({thinking_chars}ch)"));
        }
        for detail in &details {
            line.push_str(&format!(" | {detail}"));
        }
        if prompt_tokens > 0 || completion_tokens > 0 {
            line.push_str(&format!(" | tokens p{prompt_tokens}/c{completion_tokens}"));
        }
        trace(&brief(&line, 800));
    }

    fn apply_council_event(&mut self, event: &CouncilEvent) {
        match event {
            CouncilEvent::Started {
                council_id,
                goal,
                max_parallel,
            } => {
                self.councils.insert(
                    council_id.clone(),
                    CouncilUi {
                        id: council_id.clone(),
                        goal: goal.clone(),
                        phase: "规划中".into(),
                        max_parallel: *max_parallel,
                        started_at: Some(std::time::Instant::now()),
                        ..Default::default()
                    },
                );
                self.record_activity("专家团正在创建任务图");
            }
            CouncilEvent::PlanCreated { council_id, tasks } => {
                if let Some(run) = self.councils.get_mut(council_id) {
                    run.phase = "执行中".into();
                    run.tasks = tasks
                        .iter()
                        .map(|spec| {
                            (
                                spec.id.clone(),
                                CouncilTaskUi {
                                    spec: spec.clone(),
                                    state: CouncilTaskState::Pending,
                                    attempt: 0,
                                    detail: "等待依赖".into(),
                                },
                            )
                        })
                        .collect();
                }
                self.record_activity("专家团任务图已创建，开始调度");
            }
            CouncilEvent::TaskStateChanged {
                council_id,
                task_id,
                state,
                attempt,
                detail,
            } => {
                if let Some(run) = self.councils.get_mut(council_id) {
                    if let Some(task) = run.tasks.get_mut(task_id) {
                        task.state = state.clone();
                        task.attempt = *attempt;
                        task.detail = detail.clone();
                    }
                }
                self.record_activity(match state {
                    CouncilTaskState::Running => "专家正在并行执行任务",
                    CouncilTaskState::Done => "专家任务完成，正在解锁下游",
                    CouncilTaskState::Failed | CouncilTaskState::Blocked => "专家任务遇到阻塞",
                    _ => "专家团正在更新任务状态",
                });
            }
            CouncilEvent::ArtifactPublished {
                council_id,
                task_id,
                summary,
                ..
            } => {
                if let Some(task) = self
                    .councils
                    .get_mut(council_id)
                    .and_then(|r| r.tasks.get_mut(task_id))
                {
                    task.detail = summary.clone();
                }
            }
            CouncilEvent::GateEvaluated { council_id, gate } => {
                if let Some(run) = self.councils.get_mut(council_id) {
                    run.phase = "质量门禁".into();
                    run.gates.push(gate.clone());
                }
                self.record_activity("正在评估质量门禁");
            }
            CouncilEvent::Blocked { council_id, reason } => {
                if let Some(run) = self.councils.get_mut(council_id) {
                    run.phase = "已阻塞".into();
                    run.detail = reason.clone();
                }
            }
            CouncilEvent::Completed {
                council_id,
                summary,
            } => {
                if let Some(run) = self.councils.get_mut(council_id) {
                    run.phase = "已完成".into();
                    run.detail = summary.clone();
                }
            }
            CouncilEvent::Cancelled { council_id, reason } => {
                if let Some(run) = self.councils.get_mut(council_id) {
                    run.phase = "已取消".into();
                    run.detail = reason.clone();
                }
            }
        }
    }

    /// 记录一个用户可见的后台阶段；时间戳只存在 UI 内存，不写入会话历史。
    pub(super) fn record_activity(&mut self, activity: &str) {
        self.activity = activity.to_string();
        self.last_activity = Some(std::time::Instant::now());
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
        let t = std::mem::take(&mut self.thinking_text);
        // 收拢：思考过程只保留一行简短摘要，不占对话屏。
        // 完整思考不属于对话正文；需要时可通过摘要引导上下文，不再铺满屏幕。
        let compact = t.split_whitespace().collect::<Vec<_>>().join(" ");
        let summary: String = compact.chars().take(60).collect();
        if let Some(last) = self.messages.last_mut() {
            if last.kind == "thinking" {
                last.text = summary.clone();
                return;
            }
        }
        self.push("thinking", "思考", &summary);
    }
    pub(super) fn submit(&mut self) {
        let mut text = self.input.trim().to_string();
        // 允许仅发送附件：粘贴图片与通过上传按钮选中的图片共用同一发送链路。
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        let attachments = std::mem::take(&mut self.attachments);
        let sink = self.host.sink.clone();
        sink.set_permission(self.permission.clone());
        let settings = &self.host.settings;
        let _ = settings.set("permission.mode", &self.permission);
        let _ = settings.set("llm.model", &self.f_model);
        let _ = settings.set("llm.provider", &self.f_provider);
        // 直接使用用户当前选择的模型：界面选哪个就用哪个，不做任何自动切换。
        // 优先匹配启用的配置；当前模型被停用时回退任意同名条目，避免发送直接断链。
        let all_profiles = self.host.settings.model_profiles();
        let selected = all_profiles
            .iter()
            .find(|p| p.enabled && p.model == self.f_model)
            .or_else(|| all_profiles.iter().find(|p| p.model == self.f_model))
            .cloned();
        if let Some(profile) = selected {
            let _ = self.host.llm_control.configure_provider(
                profile.provider.clone(),
                profile.base_url.clone(),
                profile.model.clone(),
                profile.api_key.clone(),
                self.effort(),
            );
            self.f_provider = profile.provider;
            self.f_base = profile.base_url;
            self.f_model = profile.model;
        } else if let (Some(base), Some(key)) =
            (settings.get("llm.base_url"), settings.get("llm.api_key"))
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
        if self.multi_agent {
            // 控制标记只存在于 UI→运行时通道；编排器在写日志前剥离。
            text = format!("[HARNESS_EXPERT_COUNCIL]\n{text}");
        }
        let queued = self.busy;
        self.input.clear();
        self.busy = true;
        if !queued {
            self.thinking_text.clear();
            self.turn_started = Some(std::time::Instant::now());
            self.record_activity("正在提交任务");
        }
        sink.submit_with_attachments(text, attachments);
        if queued {
            self.note = format!("新任务已加入队列（当前待执行 {} 条）", sink.queued_count());
        }
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
        self.councils.clear();
        self.thinking_text.clear();
        self.turn_started = None;
        self.activity.clear();
        self.last_activity = None;
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
    /// poll_log 下帧从 0 重放全部消息流。运行中的回合已持有 pin 后的固定日志，
    /// 因而切换 UI 视图不会串写，可让不同会话并行执行。
    pub(super) fn switch_session(&mut self, file: &str) {
        trace(&format!(
            "[session] restore attempt {file} busy={}",
            self.busy
        ));
        let Some(dir) = self.history_dirs.get(file).cloned() else {
            return;
        };
        // 跨项目会话：先切工作区根（工具上下文与项目列表同步），
        // switch_workspace 会重载该目录最近会话，随后再精确定位到目标文件。
        if self.log.dir().as_ref() != Some(&dir) {
            // 文件工具共享同一工作区根；不同项目之间不能在任一后台回合执行时切换，
            // 否则运行中的工具调用可能落到错误项目。同项目内的历史会话不受此限制。
            if self.host.sink.any_busy() {
                self.note =
                    "当前有后台任务运行：可并行切换同项目会话，跨项目切换请等待任务结束".into();
                return;
            }
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
        self.councils.clear();
        self.input.clear();
        self.thinking = false;
        self.thinking_text.clear();
        self.turn_started = None;
        self.activity.clear();
        self.last_activity = None;
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
        self.preview_animating = false;
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
        // 旧 Git 请求结果不能在切换后覆盖新项目；下一次打开面板会按新根刷新。
        self.git_generation = self.git_generation.wrapping_add(1);
        self.git_rx = None;
        self.git_loaded = false;
        self.git_error = None;
        self.git_changes.clear();
        self.git_branch.clear();
        self.git_workspace.clear();
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

    /// 从上游拉取模型列表（非阻塞：后台线程请求，主线程每帧轮询结果，不卡 UI）。
    pub(super) fn fetch_models_from_upstream(&mut self) {
        if self.models_loading || self.models_rx.is_some() {
            return;
        }
        self.models_loading = true;
        self.models_msg = "正在获取上游模型列表…".into();
        let base = self.f_base.clone();
        let key = self.f_key.trim().to_string();
        let llm = self.host.llm_control.clone();
        let (tx, rx) = std::sync::mpsc::channel::<std::result::Result<Vec<String>, String>>();
        self.models_rx = Some(rx);
        std::thread::spawn(move || {
            let res = llm.fetch_models(base, key);
            let _ = tx.send(res);
        });
    }

    /// 每帧轮询上游模型列表结果（非阻塞）。
    pub(super) fn poll_models(&mut self) {
        if let Some(rx) = &self.models_rx {
            match rx.try_recv() {
                Ok(Ok(models)) => {
                    self.models_rx = None;
                    self.models_loading = false;
                    self.f_models = models;
                    // 预勾选：当前填写的模型若在列表中则选中；否则默认全不选（保存取第一个）。
                    if !self.f_model.trim().is_empty() {
                        if self.f_models.iter().any(|m| m == &self.f_model) {
                            self.f_selected_models.insert(self.f_model.clone());
                        }
                    }
                    self.models_msg =
                        format!("共获取 {} 个模型，可勾选多个启用", self.f_models.len());
                }
                Ok(Err(e)) => {
                    self.models_rx = None;
                    self.models_loading = false;
                    self.f_models.clear();
                    self.models_msg = format!("获取失败：{e}");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.models_rx = None;
                    self.models_loading = false;
                    self.models_msg = "获取模型列表失败：后台任务异常退出".into();
                }
            }
        }
    }

    pub(super) fn apply_model(&mut self) {
        let settings = &self.host.settings;
        let editing = self.editing_profile.clone();
        // Key 解析：未重填时编辑沿用该条目自身 Key，新增回退全局 Key。
        let key = if self.f_key.trim().is_empty() {
            editing
                .as_deref()
                .and_then(|n| self.profiles.iter().find(|p| p.name == n))
                .map(|p| p.api_key.clone())
                .filter(|k| !k.is_empty())
                .unwrap_or_else(|| settings.get("llm.api_key").unwrap_or_default())
        } else {
            self.f_key.trim().to_string()
        };
        let provider = self.f_provider.trim().to_string();
        let base = self.f_base.trim().to_string();

        if editing.is_some() {
            // ── 编辑模式：单条覆写（含重命名迁移），保留原启用状态 ──
            let enabled = editing
                .as_deref()
                .and_then(|n| self.profiles.iter().find(|p| p.name == n))
                .map(|p| p.enabled)
                .unwrap_or(true);
            if self.f_model.trim().is_empty() {
                self.note = "请填写模型名称".into();
                return;
            }
            let model = self.f_model.trim().to_string();
            if let Err(error) = self.host.llm_control.configure_provider(
                provider.clone(),
                base.clone(),
                model.clone(),
                key.clone(),
                self.effort(),
            ) {
                self.note = format!("配置错误: {error}");
                trace(&format!("[config] rejected: {error}"));
                return;
            }
            let _ = settings.set("llm.base_url", &base);
            let _ = settings.set("llm.model", &model);
            let _ = settings.set("llm.provider", &provider);
            let _ = settings.set_secret("llm.api_key", &key);
            let _ = settings.set("llm.reasoning_effort", &self.f_effort);
            let name = format!("{provider} · {model}");
            let _ = settings.save_model_profile(&crate::ModelProfile {
                name: name.clone(),
                provider,
                base_url: base,
                model,
                api_key: key,
                enabled,
            });
            // 编辑导致重命名（厂商 / 模型名变更）：删除旧条目避免残留重复配置。
            if let Some(old) = editing {
                if old != name {
                    let _ = settings.delete_model_profile(&old);
                }
            }
            self.refresh_profiles();
            self.reset_model_form();
            self.note = format!("模型配置「{name}」已保存并应用");
            trace("[config] model profile updated");
            return;
        }

        // ── 新增模式：批量写入勾选的模型（无勾选则取表单单个模型）──
        // 按上游列表顺序稳定排序，保证「第一个为当前模型」可预期。
        let mut targets: Vec<String> = self
            .f_models
            .iter()
            .filter(|m| self.f_selected_models.contains(*m))
            .cloned()
            .collect();
        // 勾选了但不在上游列表的条目（防御性兜底）。
        for m in &self.f_selected_models {
            if !targets.contains(m) {
                targets.push(m.clone());
            }
        }
        if targets.is_empty() && !self.f_model.trim().is_empty() {
            targets.push(self.f_model.trim().to_string());
        }
        if targets.is_empty() {
            self.note = "请填写模型名称，或获取上游模型列表后勾选".into();
            return;
        }
        let primary = targets[0].clone();
        if let Err(error) = self.host.llm_control.configure_provider(
            provider.clone(),
            base.clone(),
            primary.clone(),
            key.clone(),
            self.effort(),
        ) {
            self.note = format!("配置错误: {error}");
            trace(&format!("[config] rejected: {error}"));
            return;
        }
        let _ = settings.set("llm.base_url", &base);
        let _ = settings.set("llm.model", &primary);
        let _ = settings.set("llm.provider", &provider);
        let _ = settings.set_secret("llm.api_key", &key);
        let _ = settings.set("llm.reasoning_effort", &self.f_effort);
        // 持久化多选模型集合（逗号分隔），供下次打开恢复勾选。
        let mut sel: Vec<&str> = self.f_selected_models.iter().map(|s| s.as_str()).collect();
        sel.sort_unstable();
        let _ = settings.set("llm.selected_models", &sel.join(","));
        // 批量写入：逐条去重（同名 或 同 厂商+地址+模型 组合），仅插入新模型。
        let existing = settings.model_profiles();
        let mut added = 0usize;
        let mut skipped = 0usize;
        for model in &targets {
            let name = format!("{provider} · {model}");
            let dup = existing.iter().any(|p| {
                p.name == name
                    || (p.provider == provider && p.base_url == base && p.model == *model)
            });
            if dup {
                skipped += 1;
                continue;
            }
            let _ = settings.save_model_profile(&crate::ModelProfile {
                name,
                provider: provider.clone(),
                base_url: base.clone(),
                model: model.clone(),
                api_key: key.clone(),
                enabled: true,
            });
            added += 1;
        }
        self.refresh_profiles();
        self.reset_model_form();
        self.note = if added > 0 && skipped > 0 {
            format!("已新增 {added} 个模型（跳过 {skipped} 个重复），当前模型：{primary}")
        } else if added > 0 {
            format!("已新增 {added} 个模型配置并应用，当前模型：{primary}")
        } else {
            format!("所选模型均已存在，未新增；当前模型：{primary}")
        };
        trace(&format!(
            "[config] batch save applied added={added} skipped={skipped}"
        ));
    }

    /// 保存成功后复位表单：清空输入与勾选、退出编辑模式（厂商 / 地址保留，便于连续添加）。
    fn reset_model_form(&mut self) {
        self.editing_profile = None;
        self.f_model.clear();
        self.f_key.clear();
        self.f_selected_models.clear();
        self.f_models.clear();
        self.models_msg.clear();
    }

    /// 刷新模型配置列表缓存（增删 / 启停后调用，避免每帧查库解密）。
    pub(super) fn refresh_profiles(&mut self) {
        self.profiles = self.host.settings.model_profiles();
    }
}

/// 折叠空白并截断到 n 字符（超长补 …），保证 trace 单行且长度可控。
fn brief(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = flat.chars().count();
    if count <= n {
        flat
    } else {
        let head: String = flat.chars().take(n).collect();
        format!("{head}…")
    }
}

/// 提取工具参数中最有诊断价值的字段（path/command/query），无命中回退原始 JSON。
fn args_brief(args: &serde_json::Value) -> String {
    let get = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or_default();
    let raw = if !get("path").is_empty() {
        format!("{} {}", get("op"), get("path")).trim().to_string()
    } else if !get("command").is_empty() {
        get("command").to_string()
    } else if !get("query").is_empty() {
        format!("query={}", get("query"))
    } else {
        args.to_string()
    };
    brief(&raw, 60)
}
