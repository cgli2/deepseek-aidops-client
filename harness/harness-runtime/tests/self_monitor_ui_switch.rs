//! 验收：UI「参数配置」页的自我监控开关（`harness_core::tuning` 进程级开关）
//! 确实控制真实 Agent 回合的采集行为。
//!
//! 链路：settings_view / settings_panel（UI）-> tuning -> monitor_adapter::MonitorTurn::start
//!      -> agent_loop::run_turn（真实入口）。
use harness_capability::hook::{Hook, HookDecision, HookPayload};
use harness_core::{tuning, AppContext, Config, UserInput, Workspace};
use harness_llm::{Chunk, LlmProvider, ReplayLlm};
use harness_session::SessionLog;
use std::{fs, path::Path, path::PathBuf, sync::Arc};

fn root() -> PathBuf {
    let p = std::env::temp_dir().join(format!("self-monitor-switch-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&p).unwrap();
    p
}

struct Allow;
impl Hook for Allow {
    fn run(&self, _: &HookPayload) -> harness_core::Result<HookDecision> {
        Ok(HookDecision::Allow)
    }
}

/// 走真实入口跑一个回合；`config.self_monitor` 一律为默认值（enabled=true, mode="protect"），
/// 因此结果只可能由进程级开关决定，用来隔离「UI 开关是否生效」这一变量。
async fn run_one_turn(p: &Path) {
    let ctx = AppContext::new();
    let mut config = Config::default();
    config.self_monitor.sidecar = false;
    let llm: Arc<dyn LlmProvider> =
        ReplayLlm::new(vec![Chunk { text: Some("你好".into()), ..Default::default() }]);
    let hook: Arc<dyn Hook> = Arc::new(Allow);
    let _regs = [
        ctx.provide(SessionLog::new()),
        ctx.provide(Workspace::new(p.to_path_buf())),
        ctx.provide(Arc::new(config)),
        ctx.provide(llm),
        ctx.provide(hook),
        ctx.provide(harness_tool::ToolRegistry::new()),
    ];
    harness_runtime::AgentLoop::new()
        .run_turn(&ctx, UserInput { text: "hi".into(), attachments: vec![] })
        .await
        .unwrap();
}

fn spool(p: &Path) -> PathBuf {
    p.join(".harness/self-monitor/spool")
}

fn turn_dirs(p: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<_> = fs::read_dir(spool(p))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|d| d.is_dir())
        .collect();
    dirs.sort();
    dirs
}

#[tokio::test]
async fn ui_switch_controls_runtime_collection() {
    // 1. 未在 UI 保存过（tuning = None）-> 回退配置文件默认（enabled=true）-> 采集。
    let p = root();
    tuning::set_self_monitor_enabled(None);
    tuning::set_self_monitor_mode(None);
    run_one_turn(&p).await;
    let dirs = turn_dirs(&p);
    assert_eq!(dirs.len(), 1, "默认配置下应记录一个回合：{dirs:?}");

    // 2. UI 关闭开关 -> 同一份配置下不再采集。
    let p = root();
    tuning::set_self_monitor_enabled(Some(false));
    run_one_turn(&p).await;
    assert!(!spool(&p).exists(), "UI 关闭后不应写入 .harness/self-monitor/spool");

    // 3. 开关打开但模式为 off -> 仍然不采集。
    let p = root();
    tuning::set_self_monitor_enabled(Some(true));
    tuning::set_self_monitor_mode(Some("off".into()));
    run_one_turn(&p).await;
    assert!(!spool(&p).exists(), "mode=off 时不应采集");

    // 4. 开关打开 + observe -> 恢复采集，并落盘可观测的 WAL。
    let p = root();
    tuning::set_self_monitor_enabled(Some(true));
    tuning::set_self_monitor_mode(Some("observe".into()));
    run_one_turn(&p).await;
    let dirs = turn_dirs(&p);
    assert_eq!(dirs.len(), 1, "开关打开后应记录一个回合：{dirs:?}");
    let wal = dirs[0].join("events-000001.jsonl");
    let mut wrote = false;
    for _ in 0..100 {
        if fs::metadata(&wal).map(|m| m.len() > 0).unwrap_or(false) {
            wrote = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(wrote, "应写入非空事件日志 {}", wal.display());

    // 复位进程级开关，避免影响后续断言。
    tuning::set_self_monitor_enabled(None);
    tuning::set_self_monitor_mode(None);
}
