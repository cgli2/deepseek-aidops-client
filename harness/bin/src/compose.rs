//! 编译期组合（dsh `inject` 的特化）：把核心 Provider / 工具 / UI 装配进 `AppContext`。
//!
//! 在真实产品里这会拆成多个小插件（llm / tool / ui / sandbox …），每个 `Plugin::register`
//! 贡献若干可逆 `Registration`；此处聚合为单一 `HarnessPlugin` 以端到端跑通骨架，并体现
//! "换 Provider 不改 Consumer"（不变量 2）：`BashTool` 只见到 `Arc<dyn Shell>`。

use std::path::PathBuf;
use std::sync::Arc;

use harness_capability::assets::{CodeGraph, ConversationMemory, SkillLibrary, WikiStore};
use harness_capability::editor::Editor;
use harness_capability::fs::Fs;
use harness_capability::git::Git;
use harness_capability::hook::Hook;
use harness_capability::lsp::Lsp;
use harness_capability::memory::Memory;
use harness_capability::shell::Shell;
use harness_capability::subagent::Subagent;
use harness_capability::watcher::FileWatcher;
use harness_core::config::Config;
use harness_core::extension::{ExtensionPoint, ExtensionRegistry};
use harness_core::plugin::Plugin;
use harness_core::types::Profile;
use harness_core::ui_input::UiInputSink;
use harness_core::AccessPolicy;
use harness_core::AppContext;
use harness_core::LlmControl;
use harness_core::Registration;
use harness_llm::LlmProvider;
use harness_provider_git::GitCli;
use harness_provider_hook::ShellHook;
use harness_provider_local::{LocalBash, LocalEditor, LocalFs, LocalLsp, PollingFileWatcher};
use harness_provider_memory::FileMemory;
#[cfg(feature = "aidops")]
use harness_provider_aidops::AidopsBackend;
use harness_provider_sandbox::Sandbox;
use harness_session::SessionLog;
use harness_tool::{BashTool, DelegateTool, EditTool, FsTool, MemoryTool, PlanTool, ToolRegistry};
use harness_ui::Ui;

use harness_runtime::{InProcessSubagent, SessionController};

use harness_ui::ConsoleUi;
#[cfg(feature = "gui")]
use harness_ui::EguiUi;
#[cfg(feature = "tui")]
use harness_ui::TuiUi;

/// 承载整条核心装配链的单一插件。
pub struct HarnessPlugin {
    pub profile: Profile,
    pub config: Config,
    pub settings: Arc<harness_ui::SettingsDb>,
}

impl Plugin for HarnessPlugin {
    fn name(&self) -> &'static str {
        "harness-core"
    }

    fn register(self: Arc<Self>, ctx: &AppContext) -> Vec<Registration> {
        let mut regs = Vec::new();

        // 扩展点登记表（审计用，不参与调度）。
        let ext = Arc::new(ExtensionRegistry::new());
        regs.push(ctx.provide(ext.clone()));

        // 沙箱按平台选择：Windows → JobObject（进程树回收），Linux → landlock+seccomp，
        // 其它平台回退 NullSandbox（原 §9 / §16）。
        // 工作区根优先恢复用户上次在侧栏选中的项目（目录仍存在时），否则回退启动默认。
        let cwd_default = workspace_root();
        let cwd = self
            .settings
            .get("workspace.root")
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or(cwd_default);
        let sandbox: Arc<dyn Sandbox> = platform_sandbox(&cwd);
        regs.push(ctx.provide(sandbox.clone()));
        let access = Arc::new(AccessPolicy::new(
            self.settings
                .get("permission.mode")
                .unwrap_or_else(|| "工作区写入".into()),
        ));
        regs.push(ctx.provide(access));

        // 本地 Provider（Consumer 永不直接依赖它们）。
        // 共享 Workspace：GUI 侧栏切换项目后，shell/fs/editor 立即落在新工作区。
        let workspace = harness_core::Workspace::new(cwd.clone());
        regs.push(ctx.provide(workspace.clone()));
        let shell: Arc<dyn Shell> = LocalBash::with_workspace(sandbox.clone(), workspace.clone());
        regs.push(ctx.provide(shell.clone()));
        let fs: Arc<dyn Fs> = LocalFs::with_workspace(workspace.clone());
        regs.push(ctx.provide(fs.clone()));
        let editor: Arc<dyn Editor> = LocalEditor::with_workspace(workspace.clone());
        regs.push(ctx.provide(editor.clone()));

        // LLM Provider（feature / 环境变量决定 DeepSeek / local / replay）。
        let initial: Arc<dyn LlmProvider> = make_llm(&self.config);
        let key_configured = std::env::var(&self.config.llm.api_key_env)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
            || self
                .config
                .llm
                .api_key
                .as_deref()
                .map(str::trim)
                .is_some_and(|v| !v.is_empty());
        let initial_status = if key_configured {
            format!("DeepSeek / {} / 已配置 Key", self.config.llm.model)
        } else {
            format!("演示模式 / 未配置 API Key（当前不会调用真实模型）")
        };
        let managed = harness_llm::ManagedLlm::new(initial, initial_status);
        let llm: Arc<dyn LlmProvider> = managed.clone();
        regs.push(ctx.provide(llm.clone()));
        let llm_control: Arc<dyn LlmControl> = managed;
        regs.push(ctx.provide(llm_control));

        // 默认会话日志（真相源）：重启自动恢复最近会话（open_latest），
        // 被中断的回合补 TurnEnd 闭合以保证 replay 一致。须在工具注册前创建（PlanTool 依赖）。
        let log = SessionLog::open_latest(cwd.join(".harness").join("sessions"));
        regs.push(ctx.provide(log.clone()));

        // 进程内子代理 Provider（DelegateTool 依赖，先于工具注册创建）。
        let subagent: Arc<dyn Subagent> =
            InProcessSubagent::new(ctx.clone(), 4, std::time::Duration::from_secs(180));
        regs.push(ctx.provide(subagent.clone()));

        // 工具注册表 + 注册工具（仅依赖 capability trait，零 Provider 耦合）。
        // plan/delegate 为长周期任务规划与子代理委托的模型可见入口。
        let tools = ToolRegistry::new();
        tools.register(BashTool::new(shell.clone()));
        tools.register(FsTool::new(fs.clone()));
        tools.register(EditTool::new(editor.clone()));
        tools.register(PlanTool::new(log.clone()));
        tools.register(DelegateTool::new(subagent.clone()));
        regs.push(ctx.provide(tools.clone()));

        // 会话控制器（UI → 运行时 的反向输入通道）。GUI 经 `Arc<dyn UiInputSink>` 注入，
        // 解耦 harness-ui 与 harness-runtime。需要当前 tokio 运行时句柄以在后台 spawn turn。
        let controller = SessionController::new(ctx.clone(), tokio::runtime::Handle::current());
        // 以 trait object 形式注册（解耦 UI 与运行时）：包装为 `Arc<SessionController>` 后 coerce 为 `Arc<dyn UiInputSink>`。
        let sink: Arc<dyn UiInputSink> = Arc::new(controller);
        regs.push(ctx.provide(sink));

        // UI（事件总线消费者；headless 用 NullUi）在资产服务注册后再构建（见下方），
        // 以便把四类记忆资产服务注入 GUI 记忆面板（后端实时查询）。

        // Codex 借鉴能力（记忆 / 钩子 / git）。均以"能力接缝"形式注入，Consumer 零耦合。
        let memory: Arc<dyn Memory> = FileMemory::new(cwd.clone());
        regs.push(ctx.provide(memory.clone()));
        let hook: Arc<dyn Hook> = ShellHook::from_config(&self.config.hooks);
        regs.push(ctx.provide(hook.clone()));
        let git: Arc<dyn Git> = GitCli::new(cwd.clone());
        regs.push(ctx.provide(git.clone()));

        // ---- 四类记忆资产（ChatMemory L0~L3 / Skill / Wiki / CodeGraph）----
        // 原生实现始终构建（dsh 不接入 aidops 也能工作 → 约束 1）。
        let native_conv = harness_provider_memory::NativeConversationMemory::new(cwd.clone());
        let native_skill = harness_provider_memory::NativeSkillLibrary::new(cwd.clone());
        let native_wiki = harness_provider_memory::NativeWikiStore::new(cwd.clone());
        let native_code = harness_provider_memory::NativeCodeGraph::new(cwd.clone());

        // 若配置了 aidops 后端（且编译期启用 `aidops` feature），用远程 Provider 包裹原生兜底；
        // 否则直接注册原生 Provider。Consumer（MemoryTool / runtime）只见到 `Arc<dyn ...>`，零改动。
        #[cfg(feature = "aidops")]
        let (conv, skill, wiki, code) = {
            let cfg = &self.config.aidops;
            if cfg.enabled() {
                let api_key = std::env::var(&cfg.api_key_env)
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .or_else(|| cfg.api_key.clone())
                    .unwrap_or_default();
                let backend = AidopsBackend::new(
                    harness_provider_aidops::AidopsConfig {
                        base_url: cfg.base_url.clone(),
                        api_key_env: cfg.api_key_env.clone(),
                        api_key: if api_key.is_empty() { None } else { Some(api_key) },
                        project_id: cfg.project_id,
                    },
                    native_conv.clone(),
                    native_skill.clone(),
                    native_wiki.clone(),
                    native_code.clone(),
                );
                (
                    backend.clone() as Arc<dyn ConversationMemory>,
                    backend.clone() as Arc<dyn SkillLibrary>,
                    backend.clone() as Arc<dyn WikiStore>,
                    backend as Arc<dyn CodeGraph>,
                )
            } else {
                (
                    native_conv.clone() as Arc<dyn ConversationMemory>,
                    native_skill.clone() as Arc<dyn SkillLibrary>,
                    native_wiki.clone() as Arc<dyn WikiStore>,
                    native_code.clone() as Arc<dyn CodeGraph>,
                )
            }
        };
        #[cfg(not(feature = "aidops"))]
        let (conv, skill, wiki, code) = (
            native_conv.clone() as Arc<dyn ConversationMemory>,
            native_skill.clone() as Arc<dyn SkillLibrary>,
            native_wiki.clone() as Arc<dyn WikiStore>,
            native_code.clone() as Arc<dyn CodeGraph>,
        );

        regs.push(ctx.provide(conv.clone()));
        regs.push(ctx.provide(skill.clone()));
        regs.push(ctx.provide(wiki.clone()));
        regs.push(ctx.provide(code.clone()));

        // UI（事件总线消费者；headless 用 NullUi）。资产服务已注册，一并注入 GUI
        // 供记忆面板做后端实时查询（不连后端时服务即原生文件实现，面板展示本地）。
        let ui: Arc<dyn Ui> = make_ui(
            self.profile,
            ctx,
            &self.config,
            &cwd,
            self.settings.clone(),
            conv.clone(),
            skill.clone(),
            wiki.clone(),
            code.clone(),
        );
        regs.push(ctx.provide(ui.clone()));

        // 记忆工具（Consumer）：让模型可显式查询/沉淀记忆与知识。
        tools.register(MemoryTool::new(conv, skill, wiki, code));

        // M6：可替换的本地 LSP、文件监听与进程内子代理 Provider。
        let lsp_command =
            std::env::var("HARNESS_LSP_COMMAND").unwrap_or_else(|_| "rust-analyzer".into());
        let lsp: Arc<dyn Lsp> = Arc::new(LocalLsp::new(lsp_command));
        regs.push(ctx.provide(lsp));
        let watcher: Arc<dyn FileWatcher> = Arc::new(PollingFileWatcher::new(
            std::time::Duration::from_millis(350),
        ));
        regs.push(ctx.provide(watcher));

        // 声明扩展点归属（cookbook 校验用）。
        for p in [
            ExtensionPoint::Llm,
            ExtensionPoint::Shell,
            ExtensionPoint::Fs,
            ExtensionPoint::Editor,
            ExtensionPoint::Memory,
            ExtensionPoint::Hook,
            ExtensionPoint::Git,
            ExtensionPoint::Lsp,
            ExtensionPoint::Subagent,
            ExtensionPoint::FileWatcher,
            ExtensionPoint::PreStep,
            ExtensionPoint::TurnStopping,
        ] {
            ext.declare(p, "harness-core");
        }

        regs
    }
}

/// 选择 LLM Provider：env `HARNESS_REPLAY=1` 走 headless 回放闭环；否则按 feature 选真实 Provider。
///
/// DeepSeek 在 M4 已接入真实 HTTP（reqwest → OpenAI 兼容 `/chat/completions`）。
/// key 解析优先级：环境变量 `api_key_env` 名 > 配置字面量 `api_key`；二者皆空则回退 `ReplayLlm`。
fn make_llm(cfg: &Config) -> Arc<dyn LlmProvider> {
    if std::env::var("HARNESS_REPLAY").is_ok() {
        return harness_llm::ReplayLlm::new(vec![harness_llm::Chunk {
            text: Some("Hello! I am the harness skeleton (replay).".into()),
            tool_calls: vec![],
            reasoning: None,
            usage: None,
        }]);
    }
    if cfg!(feature = "deepseek") {
        // key：环境变量（api_key_env 指定名）优先，其次配置字面量 api_key。
        let env_key = std::env::var(&cfg.llm.api_key_env).unwrap_or_default();
        let key = if !env_key.trim().is_empty() {
            env_key
        } else {
            cfg.llm.api_key.clone().unwrap_or_default()
        };
        if key.trim().is_empty() {
            eprintln!(
                "[harness] 未配置 DeepSeek API key（环境变量 {} 与 default.toml 的 [llm].api_key 均缺失），\n\
                 [harness] 已回退到 ReplayLlm 以便可见地跑通一回合。配置 key 后即为真实模型。",
                cfg.llm.api_key_env
            );
            return harness_llm::ReplayLlm::new(vec![harness_llm::Chunk {
                text: Some("Hello! I am the harness skeleton (replay fallback — configure a DeepSeek API key for a real model).".into()),
                tool_calls: vec![],
                reasoning: None,
                usage: None,
            }]);
        }
        return harness_llm::DeepSeek::new(
            cfg.llm.base_url.clone(),
            key,
            cfg.llm.model.clone(),
            None,
        );
    }
    if cfg!(feature = "local-llm") {
        return harness_llm::LocalLlm::new(cfg.llm.base_url.clone());
    }
    harness_llm::ReplayLlm::new(vec![])
}

/// 选择 UI：默认 headless 用 `ConsoleUi`（终端可见 transcript）；`--tui` / `--gui` 经对应 feature 启用真实实现。
///
/// GUI 从 `AppContext` 取出 `Arc<dyn UiInputSink>`（即 `SessionController`）注入 `EguiUi`，
/// 使输入框的提交能驱动后台 agent turn。
#[allow(unused_variables)]
fn make_ui(
    profile: Profile,
    ctx: &AppContext,
    config: &Config,
    workspace: &std::path::Path,
    settings: Arc<harness_ui::SettingsDb>,
    conv: Arc<dyn ConversationMemory>,
    skill: Arc<dyn SkillLibrary>,
    wiki: Arc<dyn WikiStore>,
    code: Arc<dyn CodeGraph>,
) -> Arc<dyn Ui> {
    #[cfg(feature = "tui")]
    if profile == Profile::Tui {
        // TUI 与 GUI 同样经反向输入通道驱动回合（多轮交互式）。
        let sink: Arc<dyn UiInputSink> = ctx.get::<dyn UiInputSink>();
        return Arc::new(TuiUi::new(sink));
    }
    #[cfg(feature = "gui")]
    if profile == Profile::Gui {
        // 取出反向输入通道：`get` 的类型参数必须是与 `provide` 一致的「裸 trait object」
        // `dyn UiInputSink`（而非 `Arc<dyn UiInputSink>`），否则 `ServiceCell` 的 TypeId 不匹配会 panic。
        let sink: Arc<dyn UiInputSink> = ctx.get::<dyn UiInputSink>();
        let llm_control: Arc<dyn LlmControl> = ctx.get::<dyn LlmControl>();
        return Arc::new(EguiUi::new(
            sink,
            llm_control,
            workspace.display().to_string(),
            config.llm.provider.clone(),
            config.llm.base_url.clone(),
            config.llm.model.clone(),
            settings,
            conv,
            skill,
            wiki,
            code,
        ));
    }
    Arc::new(ConsoleUi)
}

fn workspace_root() -> PathBuf {
    if let Some(path) = std::env::var_os("HARNESS_WORKSPACE").map(PathBuf::from) {
        return path;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if dir
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("dist"))
            {
                if let Some(project) = dir.parent() {
                    return project.to_path_buf();
                }
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// 按平台选择沙箱实现（原 §9 / §16）。
#[cfg(target_os = "windows")]
fn platform_sandbox(_cwd: &std::path::Path) -> Arc<dyn Sandbox> {
    harness_provider_sandbox::JobObject::new()
}

#[cfg(target_os = "linux")]
fn platform_sandbox(cwd: &std::path::Path) -> Arc<dyn Sandbox> {
    harness_provider_sandbox::LandlockSeccomp::new(cwd.to_path_buf())
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn platform_sandbox(_cwd: &std::path::Path) -> Arc<dyn Sandbox> {
    harness_provider_sandbox::NullSandbox::new()
}
