//! Real disk/process regression for a single user request. The model is scripted;
//! file changes and Python test exits are real, not mocked success messages.
use std::sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}};
use std::path::PathBuf;
use async_trait::async_trait;
use harness_core::{AppContext, Workspace, error::Result, types::UserInput};
use harness_capability::hook::Hook;
use harness_provider_hook::NullHook;
use harness_llm::{Chunk, ChunkStream, LlmProvider, Message, RequestOptions, ToolCall, ToolResult};
use harness_runtime::AgentLoop;
use harness_session::{SessionLog, SessionEvent, DeliveryOutcome};
use harness_tool::{DynTool, ToolRegistry};

struct Script {
    calls: Vec<ToolCall>,
    index: AtomicUsize,
    options: Mutex<Vec<RequestOptions>>,
}
#[async_trait]
impl LlmProvider for Script {
    fn name(&self) -> &'static str { "disk-workflow-regression" }
    fn tools(&self) -> Vec<harness_llm::ToolSchema> { vec![] }
    fn stream(&self, messages: Vec<Message>) -> ChunkStream {
        self.stream_with_options(messages, RequestOptions::default())
    }
    fn stream_with_options(&self, _: Vec<Message>, options: RequestOptions) -> ChunkStream {
        self.options.lock().unwrap().push(options);
        let index = self.index.fetch_add(1, Ordering::SeqCst);
        let chunk = match self.calls.get(index) {
            Some(call) => Chunk { tool_calls: vec![call.clone()], ..Default::default() },
            None => Chunk { text: Some("修复完成，测试通过。".into()), ..Default::default() },
        };
        Box::pin(futures::stream::iter([Ok(chunk)]))
    }
}

struct DiskTool { name: &'static str, root: PathBuf, no_op: bool }

struct ReadOnlyEvidenceTool { name: &'static str }

struct UiEditAndBuildTool { name: &'static str, root: PathBuf }

#[async_trait]
impl DynTool for UiEditAndBuildTool {
    fn name(&self) -> &'static str { self.name }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let content = match self.name {
            "search" => "共 1 条命中（格式：相对路径:行号: 内容）：\nsrc/settings_view.rs:1: ui.button(\"确定\")".to_owned(),
            "fs" => std::fs::read_to_string(self.root.join("src/settings_view.rs"))?,
            "edit" => {
                let path = self.root.join("src/settings_view.rs");
                let before = std::fs::read_to_string(&path)?;
                std::fs::write(path, before.replace("ui.button(\"确定\")", "ui.add_sized([80.0, 28.0], Button::new(\"确定\"))"))?;
                "edit applied".to_owned()
            }
            "shell" => "Finished `dev` profile [unoptimized + debuginfo] target(s)".to_owned(),
            _ => unreachable!(),
        };
        Ok(ToolResult { call_id: call.id.clone(), ok: true, content, continuation_debt: 0 })
    }
}

#[async_trait]
impl DynTool for ReadOnlyEvidenceTool {
    fn name(&self) -> &'static str { self.name }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let content = if self.name == "search" {
            "共 1 条命中（格式：相对路径:行号: 内容）：\nsrc/settings_view.rs:1: ui.button(\"确定\")".to_owned()
        } else {
            "[文件共 1 行，当前显示 1-1 行]\nui.button(\"确定\")".to_owned()
        };
        Ok(ToolResult { call_id: call.id.clone(), ok: true, content, continuation_debt: 0 })
    }
}
#[async_trait]
impl DynTool for DiskTool {
    fn name(&self) -> &'static str { self.name }
    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let mut ok = true;
        let path = self.root.join(call.args["path"].as_str().unwrap_or(""));
        let content = match self.name {
            "fs" if call.args["op"] == "read" => std::fs::read_to_string(path)?,
            "fs" => {
                if !self.no_op { std::fs::write(path, call.args["content"].as_str().unwrap())?; }
                "written".into()
            }
            "edit" => {
                let source = std::fs::read_to_string(&path)?;
                let old = call.args["old_text"].as_str().unwrap();
                let new = call.args["new_text"].as_str().unwrap();
                ok = source.contains(old);
                if ok && !self.no_op { std::fs::write(path, source.replacen(old, new, 1))?; }
                "edit result".into()
            }
            "shell" => {
                let output = tokio::process::Command::new("python")
                    .env("PYTHONDONTWRITEBYTECODE", "1")
                    .args(["-m", "unittest", "test_save.py"]).current_dir(&self.root)
                    .output().await?;
                ok = output.status.success();
                format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
            }
            _ => unreachable!(),
        };
        Ok(ToolResult { call_id: call.id.clone(), ok, content, continuation_debt: 0 })
    }
}

/// A clean porcelain status is the terminal evidence for a commit request when
/// there is nothing to commit.  Keep this tool intentionally narrow so the
/// regression proves the Agent does not inspect source files or run tests.
struct CleanGitStatusTool {
    seen: Arc<Mutex<Vec<String>>>,
}

struct CommitGitTool {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl DynTool for CommitGitTool {
    fn name(&self) -> &'static str { "shell" }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let command = call.args["command"].as_str().unwrap_or_default().to_owned();
        let index = {
            let mut seen = self.seen.lock().unwrap();
            seen.push(command.clone());
            seen.len() - 1
        };
        let content = match index {
            0 => {
                assert_eq!(command, "git status --porcelain --untracked-files=all");
                " M src/lib.rs".into()
            }
            1 => {
                assert_eq!(command, "git add -A && git commit -m \"chore: save changes\"");
                "[main abc123] chore: save changes".into()
            }
            _ => panic!("unexpected repository command: {command}"),
        };
        Ok(ToolResult { call_id: call.id.clone(), ok: true, content, continuation_debt: 0 })
    }
}

#[async_trait]
impl DynTool for CleanGitStatusTool {
    fn name(&self) -> &'static str { "shell" }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let command = call.args["command"].as_str().unwrap_or_default().to_owned();
        self.seen.lock().unwrap().push(command.clone());
        assert_eq!(command, "git status --porcelain --untracked-files=all");
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content: String::new(),
            continuation_debt: 0,
        })
    }
}

async fn run_repair(no_op: bool) {
    let root = std::env::temp_dir().join(format!("delivery-workflow-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("save.py"), "def save(value):\n    return value\n").unwrap();
    std::fs::write(root.join("test_save.py"), "import unittest\nfrom save import save\nclass SaveTest(unittest.TestCase):\n    def test_save(self):\n        self.assertEqual(save('42'), 42)\n").unwrap();
    let mut calls = Vec::new();
    let mut add = |name: &str, args| calls.push(ToolCall {
        id: format!("call-{}", calls.len()), name: name.into(), args,
    });
    // Reproduction is available immediately, before any write/search.
    add("shell", serde_json::json!({"command":"python -m unittest test_save.py"}));
    add("fs", serde_json::json!({"op":"read","path":"save.py"}));
    // Longer cross-file inspection must not reset phases or hide editing.
    for index in 0..7 {
        let path = format!("dependency_{index}.py");
        std::fs::write(root.join(&path), format!("dependency = {index}\n")).unwrap();
        add("fs", serde_json::json!({"op":"read","path":path}));
    }
    // A new reproduction artifact cannot require reading a nonexistent file first.
    add("fs", serde_json::json!({"op":"write","path":"repro.py","content":"from save import save\nprint(save('42'))\n"}));
    add("edit", serde_json::json!({"path":"save.py","old_text":"return value","new_text":"return str(value)"}));
    add("shell", serde_json::json!({"command":"python -m unittest test_save.py"}));
    add("fs", serde_json::json!({"op":"read","path":"save.py"}));
    add("edit", serde_json::json!({"path":"save.py","old_text":"return str(value)","new_text":"return int(value)"}));
    add("shell", serde_json::json!({"command":"python -m unittest test_save.py"}));
    let expected_calls = calls.len();
    let model = Arc::new(Script { calls, index: AtomicUsize::new(0), options: Mutex::new(vec![]) });
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let _log = ctx.provide(log.clone());
    let _workspace = ctx.provide(Workspace::new(root.clone()));
    let provider: Arc<dyn LlmProvider> = model.clone();
    let _model = ctx.provide(provider);
    let tools = ToolRegistry::new();
    for name in ["fs", "edit", "shell"] {
        tools.register(Arc::new(DiskTool { name, root: root.clone(), no_op }));
    }
    let _tools = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(NullHook);
    let _hook = ctx.provide(hook);
    AgentLoop::new().run_turn(&ctx, UserInput {
        text: "策略页，编辑运行实例，修改某个字段，保存报错".into(), attachments: vec![],
    }).await.unwrap();
    let events = log.replay();
    let verified = events.iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. } if report.outcome == DeliveryOutcome::Verified));
    assert_eq!(verified, !no_op, "{events:#?}");
    if !no_op {
        assert!(std::fs::read_to_string(root.join("save.py")).unwrap().contains("return int(value)"));
        assert!(root.join("repro.py").is_file());
        assert_eq!(model.index.load(Ordering::SeqCst), expected_calls + 1);
        assert!(!events.iter().any(|event| matches!(event,
            SessionEvent::ToolResult { result, .. } if result.content.contains("gate]"))));
        let outputs: Vec<_> = events.iter().filter_map(|event| match event {
            SessionEvent::ToolResult { result, .. } if result.content.contains("Ran 1 test") => Some(result.ok),
            _ => None,
        }).collect();
        assert_eq!(outputs, vec![false, false, true]);
    } else {
        assert_eq!(std::fs::read_to_string(root.join("save.py")).unwrap(), "def save(value):\n    return value\n");
    }
    for options in model.options.lock().unwrap().iter() {
        let tools = options.allowed_tools.as_ref().unwrap();
        // 范围受限修复改由阶段化执行器提供最小工具面；每一轮不再同时暴露全部
        // fs/edit/shell；收尾轮允许为空，但运行时仍会把必要动作派发并完成闭环。
        assert!(!tools.iter().any(|tool| tool == "plan" || tool == "delegate"));
    }
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn repair_reproduces_edits_retries_and_verifies_in_one_request() { run_repair(false).await; }

#[tokio::test]
async fn successful_tool_messages_without_disk_changes_cannot_deliver() { run_repair(true).await; }

#[tokio::test]
async fn clean_git_commit_request_finishes_without_source_workflow() {
    let root = std::env::temp_dir().join(format!("git-operation-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let model = Arc::new(Script {
        calls: vec![ToolCall {
            id: "status".into(),
            name: "shell".into(),
            args: serde_json::json!({"command":"git status --porcelain --untracked-files=all"}),
        }],
        index: AtomicUsize::new(0),
        options: Mutex::new(vec![]),
    });
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let _log = ctx.provide(log.clone());
    let _workspace = ctx.provide(Workspace::new(root.clone()));
    let provider: Arc<dyn LlmProvider> = model.clone();
    let _model = ctx.provide(provider);
    let tools = ToolRegistry::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    tools.register(Arc::new(CleanGitStatusTool { seen: seen.clone() }));
    let _tools = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(NullHook);
    let _hook = ctx.provide(hook);

    AgentLoop::new().run_turn(&ctx, UserInput {
        text: "提交一下代码".into(), attachments: vec![],
    }).await.unwrap();

    assert_eq!(*seen.lock().unwrap(), vec!["git status --porcelain --untracked-files=all"]);
    assert!(log.replay().iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. } if report.outcome == DeliveryOutcome::Verified
    )));
    assert!(model.options.lock().unwrap().iter().all(|options| {
        options.allowed_tools.as_ref().is_none_or(|tools| {
            tools.is_empty() || tools == &["shell".to_string()]
        })
    }));
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn dirty_git_commit_request_commits_instead_of_entering_repair_workflow() {
    let root = std::env::temp_dir().join(format!("git-operation-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let model = Arc::new(Script {
        calls: vec![
            ToolCall {
                id: "status".into(),
                name: "shell".into(),
                args: serde_json::json!({"command":"git status --porcelain --untracked-files=all"}),
            },
            ToolCall {
                id: "commit".into(),
                name: "shell".into(),
                args: serde_json::json!({"command":"git add -A && git commit -m \"chore: save changes\""}),
            },
        ],
        index: AtomicUsize::new(0),
        options: Mutex::new(vec![]),
    });
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let _log = ctx.provide(log.clone());
    let _workspace = ctx.provide(Workspace::new(root.clone()));
    let provider: Arc<dyn LlmProvider> = model.clone();
    let _model = ctx.provide(provider);
    let tools = ToolRegistry::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    tools.register(Arc::new(CommitGitTool { seen: seen.clone() }));
    let _tools = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(NullHook);
    let _hook = ctx.provide(hook);

    AgentLoop::new().run_turn(&ctx, UserInput {
        text: "提交一下代码".into(), attachments: vec![],
    }).await.unwrap();

    assert_eq!(*seen.lock().unwrap(), vec![
        "git status --porcelain --untracked-files=all",
        "git add -A && git commit -m \"chore: save changes\"",
    ]);
    assert!(log.replay().iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. } if report.outcome == DeliveryOutcome::Verified
    )));
    assert!(model.options.lock().unwrap().iter().all(|options| {
        options.allowed_tools.as_ref().is_none_or(|tools| {
            tools.is_empty() || tools == &["shell".to_string()]
        })
    }));
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn explanatory_warning_question_completes_without_tools_or_stall_message() {
    let root = std::env::temp_dir().join(format!("direct-explanation-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let model = Arc::new(Script {
        calls: vec![],
        index: AtomicUsize::new(0),
        options: Mutex::new(vec![]),
    });
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let _log = ctx.provide(log.clone());
    let _workspace = ctx.provide(Workspace::new(root.clone()));
    let provider: Arc<dyn LlmProvider> = model;
    let _model = ctx.provide(provider);
    let _tools = ctx.provide(ToolRegistry::new());
    let hook: Arc<dyn Hook> = Arc::new(NullHook);
    let _hook = ctx.provide(hook);

    AgentLoop::new().run_turn(&ctx, UserInput {
        text: "这个提示是什么意思？仅剩 Git 提示的 LF will be replaced by CRLF（既有 core.autocrlf 行为，未改动配置）。".into(),
        attachments: vec![],
    }).await.unwrap();

    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. } if report.outcome == DeliveryOutcome::Verified
    )), "{events:#?}");
    assert!(!events.iter().any(|event| matches!(event, SessionEvent::ToolCall { .. })));
    assert!(!events.iter().any(|event| matches!(event,
        SessionEvent::Assistant { chunk, .. }
            if chunk.text.as_deref().is_some_and(|text| text.contains("已停止空转"))
    )));
    std::fs::remove_dir_all(&root).unwrap();
}

#[tokio::test]
async fn invisible_button_is_not_verified_by_search_and_read_alone() {
    let root = std::env::temp_dir().join(format!("invisible-button-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/settings_view.rs"), "ui.button(\"确定\")\n").unwrap();
    let model = Arc::new(Script {
        calls: vec![
            ToolCall { id: "locate".into(), name: "search".into(), args: serde_json::json!({"pattern":"确定"}) },
            ToolCall { id: "inspect".into(), name: "fs".into(), args: serde_json::json!({"op":"read","path":"src/settings_view.rs"}) },
        ],
        index: AtomicUsize::new(0),
        options: Mutex::new(vec![]),
    });
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let _log = ctx.provide(log.clone());
    let _workspace = ctx.provide(Workspace::new(root.clone()));
    let provider: Arc<dyn LlmProvider> = model;
    let _model = ctx.provide(provider);
    let tools = ToolRegistry::new();
    tools.register(Arc::new(ReadOnlyEvidenceTool { name: "search" }));
    tools.register(Arc::new(ReadOnlyEvidenceTool { name: "fs" }));
    let _tools = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(NullHook);
    let _hook = ctx.provide(hook);

    AgentLoop::new().run_turn(&ctx, UserInput {
        text: "新建项目弹出窗口，确定按钮看不到！".into(),
        attachments: vec![],
    }).await.unwrap();

    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. } if report.outcome != DeliveryOutcome::Verified
    )), "search/read are not visual repair evidence: {events:#?}");
    assert!(!events.iter().any(|event| matches!(event,
        SessionEvent::ToolCall { call, .. } if call.name == "edit"
    )));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn invisible_button_edit_and_build_are_not_visual_verification() {
    let root = std::env::temp_dir().join(format!("button-build-only-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/settings_view.rs"), "ui.button(\"确定\")\n").unwrap();
    let model = Arc::new(Script {
        calls: vec![
            ToolCall { id: "locate".into(), name: "search".into(), args: serde_json::json!({"pattern":"确定"}) },
            ToolCall { id: "inspect".into(), name: "fs".into(), args: serde_json::json!({"op":"read","path":"src/settings_view.rs"}) },
            ToolCall { id: "edit".into(), name: "edit".into(), args: serde_json::json!({"path":"src/settings_view.rs","old_text":"ui.button(\"确定\")","new_text":"ui.add_sized([80.0, 28.0], Button::new(\"确定\"))"}) },
            ToolCall { id: "build".into(), name: "shell".into(), args: serde_json::json!({"command":"cargo check"}) },
        ],
        index: AtomicUsize::new(0),
        options: Mutex::new(vec![]),
    });
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let _log = ctx.provide(log.clone());
    let _workspace = ctx.provide(Workspace::new(root.clone()));
    let provider: Arc<dyn LlmProvider> = model;
    let _model = ctx.provide(provider);
    let tools = ToolRegistry::new();
    for name in ["search", "fs", "edit", "shell"] {
        tools.register(Arc::new(UiEditAndBuildTool { name, root: root.clone() }));
    }
    let _tools = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(NullHook);
    let _hook = ctx.provide(hook);

    AgentLoop::new().run_turn(&ctx, UserInput {
        text: "新建项目弹出窗口，确定按钮看不到！".into(),
        attachments: vec![],
    }).await.unwrap();

    let events = log.replay();
    assert!(events.iter().any(|event| matches!(event,
        SessionEvent::Delivery { report, .. } if report.outcome != DeliveryOutcome::Verified
    )), "build success is not proof that the button is visible: {events:#?}");
    std::fs::remove_dir_all(root).unwrap();
}

/// 文档类交付物端到端回归（取证：真实 Codeloop 任务）。
///
/// 旧行为：`……梳理分析，找出根因……输出md报告……评审后再动手改造。` 被判为
/// Change/Behavior，steering 一直催“对已确认文件执行一次最小编辑”，与“评审后再动手
/// 改造”的延后约束冲突，只读探索烧尽预算却零写入（部分交付·验收 0/1）。
///
/// 新行为：契约把交付物锚定为磁盘上的报告文档（Static 证据 + `document-artifact`
/// 验收项），steering 指向“单次 fs write 写报告”，报告落盘即 Verified——且全程不改
/// 任何业务代码。模型是脚本化的，但报告写盘是真实的。
struct DocReportTool {
    name: &'static str,
    root: PathBuf,
}

#[async_trait]
impl DynTool for DocReportTool {
    fn name(&self) -> &'static str {
        self.name
    }

    async fn call(&self, call: &ToolCall) -> Result<ToolResult> {
        let content = match self.name {
            "search" => {
                "共 2 条命中（格式：相对路径:行号: 内容）：\nsrc/codeloop.rs:1: fn run_codeloop() {}\nsrc/agent.rs:2: fn step() {}"
                    .to_owned()
            }
            "fs" => {
                let path = self.root.join(call.args["path"].as_str().unwrap_or(""));
                if call.args["op"] == "read" {
                    std::fs::read_to_string(path)?
                } else {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(path, call.args["content"].as_str().unwrap_or(""))?;
                    "written".into()
                }
            }
            // 文档任务在评审前不得改动业务代码；脚本也不会调用 edit，这里兜底断言。
            "edit" => panic!("document deliverable must not edit business code before review"),
            _ => unreachable!(),
        };
        Ok(ToolResult {
            call_id: call.id.clone(),
            ok: true,
            content,
            continuation_debt: 0,
        })
    }
}

#[tokio::test]
async fn research_task_converges_by_writing_the_requested_report() {
    let root = std::env::temp_dir().join(format!("doc-deliverable-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    let codeloop_before = "fn run_codeloop() {}\n";
    let agent_before = "fn step() {}\n";
    std::fs::write(root.join("src/codeloop.rs"), codeloop_before).unwrap();
    std::fs::write(root.join("src/agent.rs"), agent_before).unwrap();

    let mut calls = Vec::new();
    let mut add = |name: &str, args| {
        calls.push(ToolCall {
            id: format!("call-{}", calls.len()),
            name: name.into(),
            args,
        })
    };
    add("search", serde_json::json!({"dir":"src","pattern":"codeloop"}));
    add("fs", serde_json::json!({"op":"read","path":"src/codeloop.rs"}));
    add("fs", serde_json::json!({"op":"read","path":"src/agent.rs"}));
    add(
        "fs",
        serde_json::json!({"op":"write","path":"Codeloop根因分析报告.md","content":"# 根因\n预算烧尽零写入。\n# 解决方案\n文档交付物收敛。\n"}),
    );

    let model = Arc::new(Script {
        calls,
        index: AtomicUsize::new(0),
        options: Mutex::new(vec![]),
    });
    let ctx = AppContext::new();
    let log = SessionLog::new();
    let _log = ctx.provide(log.clone());
    let _workspace = ctx.provide(Workspace::new(root.clone()));
    let provider: Arc<dyn LlmProvider> = model;
    let _model = ctx.provide(provider);
    let tools = ToolRegistry::new();
    for name in ["search", "fs"] {
        tools.register(Arc::new(DocReportTool {
            name,
            root: root.clone(),
        }));
    }
    let _tools = ctx.provide(tools);
    let hook: Arc<dyn Hook> = Arc::new(NullHook);
    let _hook = ctx.provide(hook);

    AgentLoop::new()
        .run_turn(
            &ctx,
            UserInput {
                text: "目前系统Agent工作台Codeloop工作机制存在严重问题，一运行就报错，请对项目代码梳理分析找出根因，输出md报告与解决方案，评审后再动手改造。".into(),
                attachments: vec![],
            },
        )
        .await
        .unwrap();

    let events = log.replay();
    // 报告落盘即交付：单一裁决判 Verified。
    assert!(
        events.iter().any(|event| matches!(event,
            SessionEvent::Delivery { report, .. } if report.outcome == DeliveryOutcome::Verified)),
        "写报告任务应交付成功：{events:#?}"
    );
    // 报告文件真实存在于磁盘，且内容非空。
    let report = root.join("Codeloop根因分析报告.md");
    assert!(report.is_file(), "报告文档必须真实写盘");
    assert!(
        std::fs::read_to_string(&report).unwrap().contains("解决方案"),
        "报告应包含实质内容"
    );
    // 评审前不得改动任何业务代码。
    assert_eq!(
        std::fs::read_to_string(root.join("src/codeloop.rs")).unwrap(),
        codeloop_before
    );
    assert_eq!(
        std::fs::read_to_string(root.join("src/agent.rs")).unwrap(),
        agent_before
    );
    // document-artifact 验收项被判满足。
    assert!(
        events.iter().any(|event| matches!(event,
            SessionEvent::Delivery { report, .. } if report.criteria.iter()
                .any(|c| c.id == "document-artifact" && c.satisfied))),
        "document-artifact 验收项应满足：{events:#?}"
    );
    std::fs::remove_dir_all(root).unwrap();
}
