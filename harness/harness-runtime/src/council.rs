//! 可恢复的专家团 DAG 编排器：确定性依赖调度、并行执行、重试与质量门禁。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{StreamExt, stream::FuturesUnordered};
use harness_capability::git::{Git, GitChange};
use harness_capability::shell::{Shell, ShellRequest};
use harness_capability::subagent::Subagent;
use harness_core::error::{Error, Result};
use harness_core::{AppContext, Workspace};
use harness_llm::Chunk;
use harness_session::{
    CouncilEvent, CouncilGateResult, CouncilTaskSpec, CouncilTaskState, DeliveryCriterion,
    DeliveryOutcome, DeliveryReport, SessionEvent, SessionLog,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const COUNCIL_PREFIX: &str = "[HARNESS_EXPERT_COUNCIL]\n";

/// 并行调度通道统一 future 类型：（任务 id，执行通道，结果）。
/// 本地确定性校验与专家任务共用一条 FuturesUnordered，须统一为 trait object。
type CouncilTaskFuture = std::pin::Pin<
    Box<dyn std::future::Future<Output = (String, &'static str, Result<String>)> + Send>,
>;

#[derive(Clone)]
struct TaskRuntime {
    spec: CouncilTaskSpec,
    state: CouncilTaskState,
    attempt: u32,
    summary: String,
    local_checked: bool,
    /// 常规重试耗尽后允许一次“从失败节点续跑”。Done 节点绝不回退。
    resume_attempted: bool,
}

/// 同一轮专家团共享的只读事实快照。一次采集、各节点复用，避免多个专家重复
/// 扫描仓库和把相同状态反复塞进上下文。
#[derive(Default)]
struct CouncilEvidence {
    workspace: String,
    git: String,
}

pub struct CouncilOrchestrator {
    max_parallel: usize,
    max_attempts: u32,
}

impl Default for CouncilOrchestrator {
    fn default() -> Self {
        Self {
            max_parallel: std::env::var("HARNESS_COUNCIL_MAX_PARALLEL")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(3)
                .clamp(1, 8),
            max_attempts: 2,
        }
    }
}

impl CouncilOrchestrator {
    pub async fn run(
        &self,
        ctx: &AppContext,
        goal: String,
        cancellation: CancellationToken,
    ) -> Result<()> {
        let log = ctx.get::<SessionLog>();
        let subagent = ctx.get::<dyn Subagent>();
        let council_id = Uuid::new_v4().to_string();
        let task_class = classify_task(&goal);
        let fast_track = task_class == TaskClass::SmallChange;
        let specs = dynamic_plan_for_class(&goal, task_class);
        let evidence = collect_shared_evidence(ctx);
        validate_dag(&specs)?;

        log.append(SessionEvent::TurnStart {
            id: log.gen_id(),
            input: goal.clone(),
        });
        emit(
            &log,
            CouncilEvent::Started {
                council_id: council_id.clone(),
                goal: goal.clone(),
                max_parallel: self.max_parallel,
            },
        );
        emit(
            &log,
            CouncilEvent::PlanCreated {
                council_id: council_id.clone(),
                tasks: specs.clone(),
            },
        );
        append_answer(
            &log,
            format!(
                "专家团已启动：已创建 {} 个 DAG 节点，最多并行 {} 位专家。{}\n\n",
                specs.len(),
                self.max_parallel,
                if fast_track {
                    "已启用快速交付：一个开发专家直接完成修改，随后执行本地校验并生成交付。若 35 秒无结果会立即取消并精简重试，不会长期占用队列。"
                } else {
                    "首批专家正在分析，我会持续更新下方任务卡。"
                }
            ),
        );

        let mut tasks: HashMap<String, TaskRuntime> = specs
            .into_iter()
            .map(|spec| {
                (
                    spec.id.clone(),
                    TaskRuntime {
                        spec,
                        state: CouncilTaskState::Pending,
                        attempt: 0,
                        summary: String::new(),
                        local_checked: false,
                        resume_attempted: false,
                    },
                )
            })
            .collect();
        let mut running: FuturesUnordered<CouncilTaskFuture> = FuturesUnordered::new();
        let mut running_ids = HashSet::new();
        let mut running_started: HashMap<String, Instant> = HashMap::new();
        let mut hard_failure: Option<String> = None;
        // 本地通道（含异步完成的本地校验）推进了 DAG：跨循环体保持，
        // 确保空 running 时先重算依赖图而不是误报 Blocked。
        let mut made_local_progress = false;

        loop {
            if cancellation.is_cancelled() {
                for task in tasks
                    .values_mut()
                    .filter(|t| !matches!(t.state, CouncilTaskState::Done))
                {
                    task.state = CouncilTaskState::Cancelled;
                    task_event(&log, &council_id, task, "用户取消专家团任务");
                }
                emit(
                    &log,
                    CouncilEvent::Cancelled {
                        council_id: council_id.clone(),
                        reason: "用户取消".into(),
                    },
                );
                append_answer(
                    &log,
                    "[已停止] 专家团任务已取消；未开始的专家任务不会继续启动。".into(),
                );
                log.append(SessionEvent::Delivery {
                    id: log.gen_id(),
                    report: council_delivery_report(
                        &tasks,
                        DeliveryOutcome::Cancelled,
                        Some("用户取消专家团任务；未完成节点已标记取消".into()),
                    ),
                });
                log.append(SessionEvent::TurnEnd { id: log.gen_id() });
                return Ok(());
            }

            // 只调度依赖全部完成、且写入范围不与当前运行任务重叠的节点。
            let mut candidates: Vec<String> = tasks
                .values()
                .filter(|task| {
                    matches!(
                        task.state,
                        CouncilTaskState::Pending | CouncilTaskState::Ready
                    )
                })
                .filter(|task| {
                    task.spec.depends_on.iter().all(|id| {
                        tasks
                            .get(id)
                            .is_some_and(|d| d.state == CouncilTaskState::Done)
                    })
                })
                .map(|task| task.spec.id.clone())
                .collect();
            candidates.sort();
            for id in candidates {
                if running.len() >= self.max_parallel {
                    break;
                }
                let conflicts = tasks.get(&id).is_some_and(|candidate| {
                    running_ids.iter().any(|rid| {
                        tasks.get(rid).is_some_and(|active| {
                            scopes_overlap(&candidate.spec.write_scopes, &active.spec.write_scopes)
                        })
                    })
                });
                if conflicts {
                    continue;
                }
                // 测试和审查先走毫秒级/本地确定性通道；只有无法判定或失败时才升级 LLM。
                if !tasks.get(&id).is_some_and(|task| task.local_checked) {
                    if id == "delivery" && tasks.len() <= 3 {
                        let summary = local_delivery_summary(&tasks);
                        let task = tasks.get_mut(&id).unwrap();
                        task.local_checked = true;
                        task.state = CouncilTaskState::Done;
                        task.summary = summary;
                        task_event(
                            &log,
                            &council_id,
                            task,
                            "已生成交付总结 · 协调器复用已验收证据 · 无需调用 LLM",
                        );
                        emit(
                            &log,
                            CouncilEvent::ArtifactPublished {
                                council_id: council_id.clone(),
                                task_id: id.clone(),
                                summary: task.summary.clone(),
                                evidence: vec!["实现与本地质量门禁的结构化证据".into()],
                            },
                        );
                        made_local_progress = true;
                        continue;
                    } else if id == "review" {
                        let decision = local_review_decision(ctx);
                        let task = tasks.get_mut(&id).unwrap();
                        task.local_checked = true;
                        if let Some(summary) = decision {
                            task.state = CouncilTaskState::Done;
                            task.summary = summary;
                            task_event(
                                &log,
                                &council_id,
                                task,
                                "已通过审查 · 本地差异路由 · 无需调用 LLM",
                            );
                            emit(
                                &log,
                                CouncilEvent::ArtifactPublished {
                                    council_id: council_id.clone(),
                                    task_id: id.clone(),
                                    summary: task.summary.clone(),
                                    evidence: vec!["Git 实际改动范围与风险规则".into()],
                                },
                            );
                            made_local_progress = true;
                            continue;
                        }
                    } else if id == "testing" {
                        let task = tasks.get_mut(&id).unwrap();
                        task.local_checked = true;
                        task.state = CouncilTaskState::Running;
                        task_event(
                            &log,
                            &council_id,
                            task,
                            "正在执行本地确定性校验 · 无 Token 消耗",
                        );
                        // 本地校验并入并行调度通道：此前内联 await 会阻塞调度循环
                        // （cargo check 最长 120s），期间其它就绪节点无法启动。
                        let ctx = ctx.clone();
                        let task_id = id.clone();
                        let fast_validation = fast_track;
                        running_ids.insert(id);
                        running_started.insert(task_id.clone(), Instant::now());
                        running.push(Box::pin(async move {
                            let started = Instant::now();
                            let result = match run_local_validation(&ctx, fast_validation).await {
                                Some(Ok(summary)) => Ok(format!(
                                    "GATE: PASS\n{summary}\n[本地校验用时 {}ms]",
                                    started.elapsed().as_millis()
                                )),
                                Some(Err(detail)) => {
                                    Err(Error::Subagent(format!("本地校验未通过：{detail}")))
                                }
                                None => Err(Error::Subagent("未发现可用本地校验能力".into())),
                            };
                            (task_id, "本地确定性校验", result)
                        }));
                        continue;
                    }
                }
                let upstream = tasks
                    .get(&id)
                    .unwrap()
                    .spec
                    .depends_on
                    .iter()
                    .filter_map(|dep| {
                        tasks
                            .get(dep)
                            .map(|t| format!("- {}: {}", t.spec.title, t.summary))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let task = tasks.get_mut(&id).unwrap();
                task.state = CouncilTaskState::Running;
                task.attempt += 1;
                let task_timeout_secs =
                    council_task_timeout_secs(&task.spec.id, task.attempt, fast_track);
                let active_timeout_secs = council_active_task_timeout_secs(fast_track);
                task_event(
                    &log,
                    &council_id,
                    task,
                    &format!(
                        "专家已分配：{task_timeout_secs} 秒内必须产生文本或工具动作；持续有实际动作时最长执行 {active_timeout_secs} 秒"
                    ),
                );
                let prompt = task_prompt(&goal, &task.spec, &upstream, &evidence, task.attempt);
                let task_id = id.clone();
                let brief = matches!(
                    task_id.as_str(),
                    "analysis" | "requirements" | "risk" | "design" | "delivery"
                );
                let sub = subagent.clone();
                let progress_log = log.clone();
                let progress_council_id = council_id.clone();
                let progress_task_id = id.clone();
                let progress_attempt = task.attempt;
                let reporter = Arc::new(move |detail: String| {
                    emit(
                        &progress_log,
                        CouncilEvent::TaskStateChanged {
                            council_id: progress_council_id.clone(),
                            task_id: progress_task_id.clone(),
                            state: CouncilTaskState::Running,
                            attempt: progress_attempt,
                            detail,
                        },
                    );
                });
                running_ids.insert(id);
                running_started.insert(task_id.clone(), Instant::now());
                running.push(Box::pin(async move {
                    let (channel, result) =
                        match tokio::time::timeout(Duration::from_secs(active_timeout_secs), async {
                            if brief {
                                match sub
                                    .spawn_brief_with_timeout(
                                        &prompt,
                                        Duration::from_secs(task_timeout_secs),
                                    )
                                    .await
                                {
                                    // 部分兼容模型可能只回 reasoning/空分片。不能把这种
                                    // 协议差异当作专家失败，立刻升级到能完成 AgentLoop 的通道。
                                    Err(error)
                                        if error
                                            .to_string()
                                            .contains("brief child returned no assistant text") =>
                                    {
                                        (
                                            "完整 Agent（轻量通道空响应后升级）",
                                            sub.spawn_with_timeout(
                                                &prompt,
                                                Duration::from_secs(task_timeout_secs),
                                            )
                                            .await,
                                        )
                                    }
                                    result => ("轻量 LLM", result),
                                }
                            } else {
                                (
                                    "完整 Agent（过程可见）",
                                    sub.spawn_observed(
                                        &prompt,
                                        Duration::from_secs(task_timeout_secs),
                                        Duration::from_secs(task_timeout_secs),
                                        reporter,
                                    )
                                    .await,
                                )
                            }
                        })
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => (
                                if brief { "轻量 LLM" } else { "完整 Agent（过程可见）" },
                                Err(Error::Subagent(format!(
                                    "child exceeded the active execution limit of {active_timeout_secs} seconds"
                                ))),
                            ),
                        };
                    (task_id, channel, result)
                }));
            }

            if running.is_empty() {
                // 本地通道刚完成节点后，依赖它的下一层尚未出现在本轮 candidates 中；
                // 立即重算 DAG，不能把正常推进误报成“无可运行节点”。
                if made_local_progress {
                    made_local_progress = false;
                    continue;
                }
                if tasks.values().all(|t| t.state == CouncilTaskState::Done) {
                    break;
                }
                let reason = hard_failure
                    .unwrap_or_else(|| "任务图没有可运行节点，可能存在失败依赖或资源冲突".into());
                emit(
                    &log,
                    CouncilEvent::Blocked {
                        council_id: council_id.clone(),
                        reason: reason.clone(),
                    },
                );
                append_answer(
                    &log,
                    format!(
                        "[error] 专家团未完成：{reason}。已保留全部已完成任务与证据，可重试或调整目标。"
                    ),
                );
                log.append(SessionEvent::TurnEnd { id: log.gen_id() });
                return Ok(());
            }

            tokio::select! {
                _ = cancellation.cancelled() => continue,
                _ = tokio::time::sleep(Duration::from_secs(3)) => {
                    // 即使模型尚未返回首个分片，也持续产生会话事件驱动 UI 心跳，
                    // 用户能看到每个专家的等待时长而不是面对静止转圈。
                    for id in running_ids.iter() {
                        if let Some(task) = tasks.get(id) {
                            let elapsed = running_started.get(id).map(|at| at.elapsed().as_secs()).unwrap_or(0);
                            let detail = if elapsed >= 12 {
                                "尚未收到专家最终结果；正在保持任务并等待，超时后会自动以精简上下文重试"
                            } else {
                                "专家仍在执行，正在等待模型首响应或工具结果"
                            };
                            task_event(&log, &council_id, task, &format!("{detail} · 已用时 {elapsed} 秒"));
                        }
                    }
                }
                completed = running.next() => {
                    let Some((id, channel, result)) = completed else { continue };
                    running_ids.remove(&id);
                    let elapsed_ms = running_started.remove(&id).map(|at| at.elapsed().as_millis()).unwrap_or(0);
                    let task = tasks.get_mut(&id).unwrap();
                    if channel == "本地确定性校验" {
                        // 本地通道结果不走 LLM 验收/重试链：成功即交付，
                        // 失败回 Ready 升级测试专家（local_checked 已置位，不会再进本地分支）。
                        match result {
                            Ok(summary) => {
                                task.state = CouncilTaskState::Done;
                                task.summary = summary;
                                task_event(&log, &council_id, task, &format!("已通过测试 · 本地命令通道 · {elapsed_ms}ms"));
                                emit(&log, CouncilEvent::ArtifactPublished {
                                    council_id: council_id.clone(), task_id: id,
                                    summary: task.summary.clone(), evidence: vec!["本地命令退出码为 0".into()],
                                });
                                made_local_progress = true;
                            }
                            Err(detail) => {
                                task.state = CouncilTaskState::Ready;
                                let note = if detail.to_string().contains("未发现可用本地校验能力") {
                                    "未发现可用本地校验能力，升级测试专家".to_string()
                                } else {
                                    format!("本地校验未通过，升级测试专家诊断 · {detail}")
                                };
                                task_event(&log, &council_id, task, &note);
                                made_local_progress = true;
                            }
                        }
                        continue;
                    }
                    match result {
                        Ok(answer) if acceptable_result(&answer) => {
                            task.state = CouncilTaskState::Done;
                            task.summary = structured_summary(&answer);
                            task_event(&log, &council_id, task, &format!("已通过任务验收 · {channel} · {elapsed_ms}ms"));
                            emit(&log, CouncilEvent::ArtifactPublished {
                                council_id: council_id.clone(), task_id: id,
                                summary: task.summary.clone(), evidence: task.spec.acceptance_criteria.clone(),
                            });
                        }
                        outcome => {
                            let detail = match outcome { Ok(answer) => format!("输出未达到验收要求：{}", compact(&answer, 240)), Err(error) => error.to_string() };
                            let timed_out = detail.to_ascii_lowercase().contains("timed out");
                            // 超时是可恢复错误，不是已通过验收。旧逻辑把部分上游节点改为
                            // Done，致使后续可能在缺证据情况下交付 PASS；现在统一进入
                            // 有限重试/续跑，最终作为 Failed 进入 Blocked 交付报告。
                            if task.attempt < self.max_attempts {
                                task.state = CouncilTaskState::Ready;
                                let route = if timed_out {
                                    "首轮等待超时，已切换精简上下文重试"
                                } else {
                                    "执行失败，将自动重试"
                                };
                                task_event(&log, &council_id, task, &format!("{route}：{detail}"));
                            } else if !fast_track && !task.resume_attempted {
                                // 仅重新打开当前失败节点；依赖均已 Done，因此调度器会复用
                                // 已验收的设计/实现/测试证据，不会从头重复执行。
                                task.resume_attempted = true;
                                task.state = CouncilTaskState::Ready;
                                task_event(
                                    &log,
                                    &council_id,
                                    task,
                                    &format!(
                                        "常规重试已耗尽，正在从当前失败节点自动续跑 · 已保留上游完成证据 · {detail}"
                                    ),
                                );
                            } else {
                                task.state = CouncilTaskState::Failed;
                                task_event(&log, &council_id, task, &detail);
                                hard_failure = Some(format!("{}连续 {} 次失败：{detail}", task.spec.title, task.attempt));
                            }
                        }
                    }
                }
            }
        }

        let gates = evaluate_gates(&tasks);
        for gate in &gates {
            emit(
                &log,
                CouncilEvent::GateEvaluated {
                    council_id: council_id.clone(),
                    gate: gate.clone(),
                },
            );
        }
        if gates.iter().all(|gate| gate.passed) {
            let summary = tasks
                .get("delivery")
                .map(|t| t.summary.clone())
                .unwrap_or_else(|| "全部专家任务与质量门禁已完成".into());
            emit(
                &log,
                CouncilEvent::Completed {
                    council_id: council_id.clone(),
                    summary: summary.clone(),
                },
            );
            append_answer(
                &log,
                format!("专家团已完成全部 DAG 节点并通过质量门禁。\n\n{summary}"),
            );
            log.append(SessionEvent::Delivery {
                id: log.gen_id(),
                report: council_delivery_report(&tasks, DeliveryOutcome::Verified, None),
            });
        } else {
            let failed = gates
                .iter()
                .filter(|g| !g.passed)
                .map(|g| format!("{}：{}", g.name, g.evidence))
                .collect::<Vec<_>>()
                .join("；");
            emit(
                &log,
                CouncilEvent::Blocked {
                    council_id: council_id.clone(),
                    reason: failed.clone(),
                },
            );
            append_answer(
                &log,
                format!("[error] 专家团任务已执行完，但质量门禁未通过：{failed}"),
            );
            log.append(SessionEvent::Delivery {
                id: log.gen_id(),
                report: council_delivery_report(
                    &tasks,
                    DeliveryOutcome::SystemFailure,
                    Some(failed),
                ),
            });
        }
        log.append(SessionEvent::TurnEnd { id: log.gen_id() });
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskClass {
    Analysis,
    SmallChange,
    StandardChange,
    HighRiskChange,
}

fn classify_task(goal: &str) -> TaskClass {
    let lower = goal.to_lowercase();
    let contains_any = |terms: &[&str]| terms.iter().any(|term| lower.contains(term));
    let code_change = contains_any(&[
        "修改",
        "修复",
        "实现",
        "新增",
        "增加",
        "删除",
        "优化",
        "改进",
        "编译",
        "测试",
        "代码",
        "按钮",
        "界面",
        "重构",
        "fix",
        "implement",
        "change",
        "build",
        "test",
        "refactor",
    ]);
    if !code_change {
        return TaskClass::Analysis;
    }
    if contains_any(&[
        "安全",
        "权限",
        "认证",
        "支付",
        "数据库",
        "迁移",
        "部署",
        "生产",
        "并发",
        "架构",
        "跨模块",
        "重构",
        "security",
        "auth",
        "payment",
        "database",
        "migration",
        "deploy",
        "concurrency",
        "architecture",
    ]) {
        return TaskClass::HighRiskChange;
    }
    if goal.chars().count() <= 160
        && contains_any(&[
            "按钮",
            "宽度",
            "高度",
            "颜色",
            "字体",
            "间距",
            "文案",
            "样式",
            "布局",
            "ui",
            "css",
            "label",
            "button",
            "下拉",
            "下拉框",
            "菜单",
            "选项",
            "模式",
            "paper",
            "live",
            "sim",
        ])
    {
        return TaskClass::SmallChange;
    }
    TaskClass::StandardChange
}

#[cfg(test)]
fn dynamic_plan(goal: &str) -> Vec<CouncilTaskSpec> {
    dynamic_plan_for_class(goal, classify_task(goal))
}

fn dynamic_plan_for_class(goal: &str, task_class: TaskClass) -> Vec<CouncilTaskSpec> {
    let task =
        |id: &str, title: &str, role: &str, deps: &[&str], scopes: &[&str], criteria: &[&str]| {
            CouncilTaskSpec {
                id: id.into(),
                title: title.into(),
                objective: format!("围绕目标完成{title}：{goal}"),
                role: role.into(),
                depends_on: deps.iter().map(|v| (*v).into()).collect(),
                write_scopes: scopes.iter().map(|v| (*v).into()).collect(),
                acceptance_criteria: criteria.iter().map(|v| (*v).into()).collect(),
            }
        };
    match task_class {
        TaskClass::Analysis => vec![
            task(
                "analysis",
                "目标分析与结论",
                "分析专家",
                &[],
                &[],
                &["直接回答目标", "给出依据与限制"],
            ),
            task(
                "delivery",
                "交付总结",
                "协调者",
                &["analysis"],
                &[],
                &["结论清晰", "证据完整"],
            ),
        ],
        TaskClass::SmallChange => vec![
            task(
                "implementation",
                "快速实现",
                "快速交付开发专家",
                &[],
                &["workspace"],
                &["只定位必要文件", "完成必要修改", "执行局部验证并报告变更"],
            ),
            task(
                "testing",
                "快速本地校验",
                "确定性校验器",
                &["implementation"],
                &[],
                &["执行 git 差异校验", "给出明确门禁结论"],
            ),
            task(
                "delivery",
                "交付总结",
                "协调者",
                &["testing"],
                &[],
                &["核对变更和验证", "明确交付结果"],
            ),
        ],
        // 普通变更先由一个具备工具能力的开发专家完成；构建设计、独立审查等
        // 额外 LLM 节点只留给高风险任务。这样简单编码不必等待多轮专家串行交接。
        TaskClass::StandardChange => vec![
            task(
                "implementation",
                "实现与局部验证",
                "开发专家",
                &[],
                &["workspace"],
                &["完成必要修改", "报告变更与验证"],
            ),
            task(
                "testing",
                "构建与回归测试",
                "测试专家",
                &["implementation"],
                &[],
                &["执行相关测试", "报告通过与失败"],
            ),
            task(
                "delivery",
                "质量门禁与交付总结",
                "协调者",
                &["testing"],
                &[],
                &["核对全部证据", "明确限制和交付物"],
            ),
        ],
        TaskClass::HighRiskChange => vec![
            task(
                "requirements",
                "需求与验收分析",
                "需求分析师",
                &[],
                &[],
                &["范围明确", "验收标准明确"],
            ),
            task(
                "risk",
                "风险与现状调查",
                "系统分析师",
                &[],
                &[],
                &["列出风险", "提供现状证据"],
            ),
            task(
                "design",
                "方案与任务设计",
                "系统设计师",
                &[],
                &[],
                &["方案可执行", "依赖明确"],
            ),
            task(
                "implementation",
                "实现与局部验证",
                "开发专家",
                &["design"],
                &["workspace"],
                &["完成必要修改", "报告变更与验证"],
            ),
            task(
                "review",
                "独立代码审查",
                "代码审查员",
                &["implementation"],
                &[],
                &["检查正确性安全性", "列出阻断项"],
            ),
            task(
                "testing",
                "构建与回归测试",
                "测试专家",
                &["implementation"],
                &["tests"],
                &["执行相关测试", "报告通过与失败"],
            ),
            task(
                "delivery",
                "质量门禁与交付总结",
                "协调者",
                &["requirements", "risk", "review", "testing"],
                &[],
                &["核对全部证据", "明确限制和交付物"],
            ),
        ],
    }
}

pub fn validate_dag(tasks: &[CouncilTaskSpec]) -> Result<()> {
    let ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    if ids.len() != tasks.len() {
        return Err(Error::Runtime("专家团任务 ID 重复".into()));
    }
    if tasks.iter().any(|t| t.acceptance_criteria.is_empty()) {
        return Err(Error::Runtime("每个专家任务必须有验收标准".into()));
    }
    if tasks
        .iter()
        .flat_map(|t| &t.depends_on)
        .any(|id| !ids.contains(id.as_str()))
    {
        return Err(Error::Runtime("专家团任务依赖不存在".into()));
    }
    fn visit<'a>(
        id: &'a str,
        map: &HashMap<&'a str, &'a CouncilTaskSpec>,
        visiting: &mut HashSet<&'a str>,
        done: &mut HashSet<&'a str>,
    ) -> bool {
        if done.contains(id) {
            return true;
        }
        if !visiting.insert(id) {
            return false;
        }
        let ok = map[id]
            .depends_on
            .iter()
            .all(|dep| visit(dep, map, visiting, done));
        visiting.remove(id);
        done.insert(id);
        ok
    }
    let map: HashMap<_, _> = tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut visiting = HashSet::new();
    let mut done = HashSet::new();
    if tasks
        .iter()
        .any(|t| !visit(&t.id, &map, &mut visiting, &mut done))
    {
        return Err(Error::Runtime("专家团任务图存在环".into()));
    }
    Ok(())
}

fn evaluate_gates(tasks: &HashMap<String, TaskRuntime>) -> Vec<CouncilGateResult> {
    let complete = tasks.values().all(task_has_passed);
    let test = tasks.get("testing").is_none_or(|t| {
        t.state == CouncilTaskState::Done
            && t.summary.contains("GATE: PASS")
            && !has_blocker(&t.summary)
    });
    let review = tasks.get("review").is_none_or(|t| {
        t.state == CouncilTaskState::Done
            && t.summary.contains("GATE: PASS")
            && !has_blocker(&t.summary)
    });
    let evidence = tasks.values().all(|t| {
        !t.summary.trim().is_empty() && !t.spec.acceptance_criteria.is_empty() && task_has_passed(t)
    });
    vec![
        CouncilGateResult {
            name: "任务完整性".into(),
            passed: complete,
            evidence: format!(
                "{}/{} 个节点完成",
                tasks
                    .values()
                    .filter(|t| t.state == CouncilTaskState::Done)
                    .count(),
                tasks.len()
            ),
        },
        CouncilGateResult {
            name: "测试门禁".into(),
            passed: test,
            evidence: if !tasks.contains_key("testing") {
                "当前任务无需测试节点，已按最小 DAG 跳过"
            } else if test {
                "测试专家已提交无阻断证据"
            } else {
                "测试失败或存在阻断项"
            }
            .into(),
        },
        CouncilGateResult {
            name: "审查门禁".into(),
            passed: review,
            evidence: if !tasks.contains_key("review") {
                "当前风险等级无需独立审查，已按最小 DAG 跳过"
            } else if review {
                "审查员已提交无阻断结论"
            } else {
                "审查发现阻断问题"
            }
            .into(),
        },
        CouncilGateResult {
            name: "证据完整性".into(),
            passed: evidence,
            evidence: if evidence {
                "所有任务均有摘要与验收依据"
            } else {
                "存在缺失证据"
            }
            .into(),
        },
    ]
}

/// `Done` 仅代表调度器停止该节点，不能等价于验收通过。所有完成门禁和最终
/// Delivery 都共用此规则，避免超时降级、空摘要或带阻断项的节点混入成功交付。
fn task_has_passed(task: &TaskRuntime) -> bool {
    if task.state != CouncilTaskState::Done
        || task.summary.trim().is_empty()
        || has_blocker(&task.summary)
    {
        return false;
    }
    if matches!(task.spec.id.as_str(), "testing" | "review") {
        task.summary.contains("GATE: PASS")
    } else {
        task.summary.contains("STATUS: PASS")
    }
}

fn council_delivery_report(
    tasks: &HashMap<String, TaskRuntime>,
    outcome: DeliveryOutcome,
    reason: Option<String>,
) -> DeliveryReport {
    let mut ordered = tasks.values().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.spec.id.cmp(&right.spec.id));
    let criteria = ordered
        .into_iter()
        .map(|task| DeliveryCriterion {
            id: task.spec.id.clone(),
            description: format!(
                "{}：{}",
                task.spec.title,
                task.spec.acceptance_criteria.join("；")
            ),
            satisfied: task_has_passed(task),
            evidence: if task_has_passed(task) {
                vec![compact(&task.summary, 600)]
            } else {
                Vec::new()
            },
        })
        .collect::<Vec<_>>();
    let verification = criteria
        .iter()
        .flat_map(|criterion| criterion.evidence.clone())
        .collect();
    DeliveryReport {
        outcome,
        criteria,
        verification,
        reason,
    }
}

fn task_prompt(
    goal: &str,
    task: &CouncilTaskSpec,
    upstream: &str,
    evidence: &CouncilEvidence,
    attempt: u32,
) -> String {
    let fast_instruction = if task.role == "快速交付开发专家" {
        "\n这是 60 秒级的低风险局部修改：不要规划、不要委派、不要扫描整个仓库。先用一次精确搜索或读取定位目标，直接修改，然后仅做与改动相关的快速验证并交付。"
    } else {
        ""
    };
    let gate_instruction = if matches!(task.id.as_str(), "testing" | "review") {
        "\n这是质量门禁任务。结论必须单独包含严格标记 `GATE: PASS` 或 `GATE: FAIL - 具体阻断原因`；存在任何未解决的失败时严禁输出 PASS。"
    } else {
        ""
    };
    format!(
        "你是专家团中的{}。\n总目标：{}\n当前任务：{}\n任务说明：{}\n验收标准：{}\n允许写入范围：{}\n共享事实快照（已缓存，请勿重复扫描同类信息）：\n工作区：{}\nGit：{}\n已验收上游摘要：\n{}\n{}\n{}\n请实际完成任务并验证。最终严格使用以下短格式，字段缺失写“无”：\nSTATUS: PASS 或 FAIL\nSUMMARY: 一两句结论\nFILES: 改动路径\nCHECKS: 已执行命令/证据\nRISKS: 风险、限制或阻断\n{}",
        task.role,
        goal,
        task.title,
        task.objective,
        task.acceptance_criteria.join("；"),
        if task.write_scopes.is_empty() {
            "只读分析".into()
        } else {
            task.write_scopes.join(", ")
        },
        evidence.workspace,
        evidence.git,
        if upstream.is_empty() {
            "（无）"
        } else {
            upstream
        },
        if attempt > 1 {
            "本次为超时/失败后的精简重试：直接利用共享快照和上游证据，避免重复扫描；优先完成最小可验证交付。"
        } else {
            ""
        },
        fast_instruction,
        gate_instruction
    )
}

fn acceptable_result(answer: &str) -> bool {
    answer.trim().chars().count() >= 20
        && !answer.contains("[error]")
        && !answer.contains("子代理执行失败")
}

fn collect_shared_evidence(ctx: &AppContext) -> CouncilEvidence {
    let workspace = ctx
        .try_get::<Workspace>()
        .map(|ws| ws.root().display().to_string())
        .unwrap_or_else(|| "未提供工作区服务".into());
    let git = ctx.try_get::<dyn Git>().map_or_else(
        || "未提供 Git 服务".into(),
        |git| match (git.status(), git.changed_files()) {
            (Ok(status), Ok(files)) => format!(
                "分支 {}；{}；{} 个未提交文件：{}",
                status.branch,
                if status.dirty {
                    "工作区有改动"
                } else {
                    "工作区干净"
                },
                files.len(),
                files
                    .iter()
                    .take(8)
                    .map(|file| file.path.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => "Git 状态暂不可用".into(),
        },
    );
    CouncilEvidence { workspace, git }
}

/// 将专家输出转成有限、可交接的证据，而非把整段自由文本复制给下游节点。
fn structured_summary(answer: &str) -> String {
    let mut selected = Vec::new();
    for line in answer.lines() {
        let line = line.trim();
        if line.starts_with("STATUS:")
            || line.starts_with("SUMMARY:")
            || line.starts_with("FILES:")
            || line.starts_with("CHECKS:")
            || line.starts_with("RISKS:")
            || line.contains("GATE:")
        {
            selected.push(line);
        }
    }
    if selected.is_empty() {
        compact(answer, 900)
    } else {
        compact(&selected.join("\n"), 1_200)
    }
}

fn local_delivery_summary(tasks: &HashMap<String, TaskRuntime>) -> String {
    // 交付摘要必须覆盖全部上游节点：分析类小图（analysis -> delivery）没有
    // implementation/testing 节点，硬编码 ID 会把分析专家的真实结论整段丢弃，
    // 产出"无实现节点"的空壳 PASS。按节点 ID 排序保证输出稳定。
    let mut upstream_ids: Vec<&String> = tasks
        .keys()
        .filter(|id| id.as_str() != "delivery")
        .collect();
    upstream_ids.sort();
    let evidence = if upstream_ids.is_empty() {
        "无上游节点证据".to_string()
    } else {
        upstream_ids
            .iter()
            .map(|id| {
                let task = &tasks[*id];
                format!(
                    "## {}（{}）\n{}",
                    task.spec.title,
                    id,
                    compact(&task.summary, 700)
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };
    let testing = tasks
        .get("testing")
        .map(|task| compact(&task.summary, 400))
        .unwrap_or_else(|| "当前任务无需测试节点".into());
    let upstream_passed = tasks
        .iter()
        .filter(|(id, _)| id.as_str() != "delivery")
        .all(|(_, task)| task_has_passed(task));
    if upstream_passed {
        format!(
            "STATUS: PASS\nSUMMARY: 已基于上游节点证据和质量门禁生成交付。\nFILES: 见上游证据。\nCHECKS: {testing}\nRISKS: 无新增阻断。\n\n上游证据：\n{evidence}"
        )
    } else {
        "STATUS: FAIL\nSUMMARY: 上游节点尚未提交可验收的 PASS 证据，不能生成成功交付。\nFILES: 无\nCHECKS: 无\nRISKS: 请修复或重新验证失败/降级的上游节点。".into()
    }
}

/// 低风险、小范围的真实 Git 差异无需再等待一个审查模型；不确定时返回 None，
/// 由独立审查专家兜底，避免为了省 Token 牺牲安全性。
fn local_review_decision(ctx: &AppContext) -> Option<String> {
    let git = ctx.try_get::<dyn Git>()?;
    let files = git.changed_files().ok()?;
    let diff = git.diff().ok()?;
    if diff_requires_expert_review(&files, &diff) {
        return None;
    }
    Some(format!(
        "GATE: PASS\n本地差异路由：{} 个低风险文件、约 {} 行差异；未发现安全、数据或执行边界变更。",
        files.len(),
        diff.lines()
            .filter(|line| line.starts_with('+') || line.starts_with('-'))
            .count()
    ))
}

fn diff_requires_expert_review(files: &[GitChange], diff: &str) -> bool {
    let changed_lines = diff
        .lines()
        .filter(|line| {
            (line.starts_with('+') || line.starts_with('-'))
                && !line.starts_with("+++")
                && !line.starts_with("---")
        })
        .count();
    let sensitive = [
        "auth",
        "security",
        "permission",
        "secret",
        "token",
        "payment",
        "database",
        "migration",
        "sql",
        "shell",
        "sandbox",
        "unsafe",
        "deploy",
        "credential",
        "认证",
        "权限",
        "密钥",
        "支付",
        "数据库",
        "迁移",
        "部署",
    ];
    let lower_diff = diff.to_lowercase();
    files.len() > 3
        || changed_lines > 180
        || files.iter().any(|change| {
            let path = change.path.to_lowercase();
            sensitive.iter().any(|word| path.contains(word))
        })
        || sensitive.iter().any(|word| lower_diff.contains(word))
}

/// 返回 None 表示当前上下文没有本地执行能力，调用方应透明升级测试专家。
async fn run_local_validation(
    ctx: &AppContext,
    fast_validation: bool,
) -> Option<std::result::Result<String, String>> {
    let shell = ctx.try_get::<dyn Shell>()?;
    let root = ctx.try_get::<Workspace>()?.root();
    let (cwd, command) = if fast_validation {
        // 快速路径不再为一处样式/文案改动启动整个工作区构建。语法、类型和完整
        // 回归仍属于普通/高风险路径；这里先用确定性差异检查在几秒内放行交付。
        (root.clone(), "git diff --check".to_string())
    } else if root.join("Cargo.toml").is_file() {
        (root, "cargo check --workspace".to_string())
    } else if root.join("harness").join("Cargo.toml").is_file() {
        (root.join("harness"), "cargo check --workspace".to_string())
    } else if root.join("package.json").is_file() {
        // build 比猜测各项目的 test runner 参数稳定，且能覆盖类型与打包错误。
        (root, "npm run build --if-present".to_string())
    } else {
        (root, "git diff --check".to_string())
    };
    let output = match shell
        .run(ShellRequest {
            cmd: command.clone(),
            cwd: Some(cwd),
            timeout_ms: 120_000,
        })
        .await
    {
        Ok(output) => output,
        Err(error) => return Some(Err(compact(&error.to_string(), 280))),
    };
    if output.exit_code == 0 {
        Some(Ok(format!(
            "GATE: PASS\n本地确定性校验通过：`{command}`（exit 0）。"
        )))
    } else {
        let detail = if output.stderr.trim().is_empty() {
            output.stdout
        } else {
            output.stderr
        };
        Some(Err(format!(
            "`{command}` exit {}：{}",
            output.exit_code,
            compact(detail.trim(), 360)
        )))
    }
}
fn council_task_timeout_secs(task_id: &str, attempt: u32, fast_track: bool) -> u64 {
    if fast_track {
        let configured = std::env::var("HARNESS_COUNCIL_FAST_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(35)
            .clamp(15, 90);
        // 小任务只允许一次短重试；失败需要尽快释放专家槽，而非静默堆积数分钟。
        return if attempt > 1 {
            (configured / 2).max(15)
        } else {
            configured
        };
    }
    let (key, fallback) = match task_id {
        "requirements" | "risk" => ("HARNESS_COUNCIL_ANALYSIS_TIMEOUT_SECS", 35),
        "design" | "delivery" => ("HARNESS_COUNCIL_DESIGN_TIMEOUT_SECS", 60),
        _ => ("HARNESS_COUNCIL_WORK_TIMEOUT_SECS", 120),
    };
    let configured = std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(fallback)
        .clamp(10, 600);
    // 第二次仅携带结构化上游摘要，预期更快；缩短无首响应时的总等待，
    // 同时保留第一次完整执行所需的时间窗口。
    if attempt > 1 {
        (configured / 2).max(20)
    } else {
        configured
    }
}

/// 首个动作之后的硬上限。此前把这个上限同时当作“首个响应期限”，导致工具已经
/// 在执行的正常任务也会被误杀；现在只有没有新动作才会由子代理的 idle 计时取消。
fn council_active_task_timeout_secs(fast_track: bool) -> u64 {
    std::env::var("HARNESS_COUNCIL_ACTIVE_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(if fast_track { 120 } else { 600 })
        .clamp(60, 1_800)
}
fn has_blocker(text: &str) -> bool {
    ["阻断：", "测试失败", "未通过", "critical", "blocker"]
        .iter()
        .any(|v| text.to_lowercase().contains(&v.to_lowercase()))
}
fn scopes_overlap(a: &[String], b: &[String]) -> bool {
    !a.is_empty()
        && !b.is_empty()
        && a.iter().any(|x| {
            b.iter().any(|y| {
                x == "workspace" || y == "workspace" || x.starts_with(y) || y.starts_with(x)
            })
        })
}
fn compact(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
fn emit(log: &SessionLog, event: CouncilEvent) {
    log.append(SessionEvent::Council {
        id: log.gen_id(),
        event,
    });
}
fn task_event(log: &SessionLog, council_id: &str, task: &TaskRuntime, detail: &str) {
    emit(
        log,
        CouncilEvent::TaskStateChanged {
            council_id: council_id.into(),
            task_id: task.spec.id.clone(),
            state: task.state.clone(),
            attempt: task.attempt,
            detail: detail.into(),
        },
    );
}
fn append_answer(log: &SessionLog, text: String) {
    log.append(SessionEvent::Assistant {
        id: log.gen_id(),
        chunk: Chunk {
            text: Some(text),
            ..Default::default()
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use harness_capability::subagent::Subagent;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct TrackingSubagent {
        active: AtomicUsize,
        peak: AtomicUsize,
    }

    struct FailingSubagent;

    struct BriefEmptySubagent;

    #[async_trait]
    impl Subagent for FailingSubagent {
        async fn spawn(&self, _task: &str) -> Result<String> {
            Err(Error::Subagent("planned failure".into()))
        }

        async fn spawn_brief(&self, _task: &str) -> Result<String> {
            Err(Error::Subagent("planned failure".into()))
        }
    }

    #[async_trait]
    impl Subagent for BriefEmptySubagent {
        async fn spawn(&self, _task: &str) -> Result<String> {
            Ok(
                "STATUS: PASS\nSUMMARY: 完整 Agent 已完成结论。\nFILES: 无\nCHECKS: 无\nRISKS: 无"
                    .into(),
            )
        }

        async fn spawn_brief(&self, _task: &str) -> Result<String> {
            Err(Error::Subagent(
                "brief child returned no assistant text".into(),
            ))
        }
    }
    #[async_trait]
    impl Subagent for TrackingSubagent {
        async fn spawn(&self, task: &str) -> Result<String> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(15)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(format!(
                "STATUS: PASS\nSUMMARY: 任务已经完成并验证：{}\nFILES: 无\nCHECKS: 检查通过\nRISKS: 无\nGATE: PASS",
                task.chars().take(30).collect::<String>()
            ))
        }
    }

    #[test]
    fn default_graph_is_valid_and_parallel_at_both_fanouts() {
        let tasks = dynamic_plan("重构认证和数据库迁移架构");
        validate_dag(&tasks).unwrap();
        // 需求、风险、设计走轻量通道并行启动，避免实现前串行等待。
        assert_eq!(tasks.iter().filter(|t| t.depends_on.is_empty()).count(), 3);
        assert!(tasks.iter().any(|t| t.id == "review"));
        assert!(tasks.iter().any(|t| t.id == "testing"));
    }

    #[test]
    fn cycle_is_rejected() {
        let mut tasks = dynamic_plan("重构认证和数据库迁移架构");
        tasks[0].depends_on.push("delivery".into());
        assert!(validate_dag(&tasks).is_err());
    }

    #[test]
    fn task_router_builds_minimal_graphs() {
        assert_eq!(classify_task("解释这段设计的优缺点"), TaskClass::Analysis);
        assert_eq!(dynamic_plan("把按钮宽度改小").len(), 3);
        let dropdown = dynamic_plan("右上角模式下拉框去掉 sim，只保留 paper 和 live，代码逻辑不变");
        assert_eq!(
            classify_task("右上角模式下拉框去掉 sim，只保留 paper 和 live，代码逻辑不变"),
            TaskClass::SmallChange
        );
        assert_eq!(dropdown[0].role, "快速交付开发专家");
        assert_eq!(dropdown[1].role, "确定性校验器");
        assert_eq!(dynamic_plan("修复消息处理错误并增加测试").len(), 3);
        assert!(
            dynamic_plan("修复消息处理错误并增加测试")
                .iter()
                .all(|task| task.id != "review")
        );
        assert_eq!(dynamic_plan("重构认证和数据库迁移架构").len(), 7);
    }

    #[test]
    fn local_delivery_reuses_existing_evidence() {
        let mut tasks = HashMap::new();
        for spec in dynamic_plan("修复消息处理错误并增加测试") {
            let id = spec.id.clone();
            let summary = if matches!(id.as_str(), "testing" | "review") {
                "GATE: PASS\nSUMMARY: evidence".into()
            } else {
                "STATUS: PASS\nSUMMARY: evidence".into()
            };
            tasks.insert(
                id,
                TaskRuntime {
                    spec,
                    state: CouncilTaskState::Done,
                    attempt: 1,
                    summary,
                    local_checked: false,
                    resume_attempted: false,
                },
            );
        }
        let summary = local_delivery_summary(&tasks);
        assert!(summary.contains("STATUS: PASS"));
        assert!(summary.contains("CHECKS:"));
    }

    #[test]
    fn local_delivery_includes_analysis_evidence() {
        // 分析类小图（analysis -> delivery）没有 implementation/testing 节点，
        // 交付摘要必须携带分析专家的真实结论，不能退化为"无实现节点"的空壳 PASS。
        let mut tasks = HashMap::new();
        for spec in dynamic_plan("解释这段设计的优缺点") {
            let id = spec.id.clone();
            let summary = if id == "analysis" {
                "STATUS: PASS\nSUMMARY: 专家团运行正常：DAG 调度、本地质量门禁与重试链路均已验证。\nRISKS: 无"
                    .into()
            } else {
                String::new()
            };
            tasks.insert(
                id,
                TaskRuntime {
                    spec,
                    state: CouncilTaskState::Done,
                    attempt: 1,
                    summary,
                    local_checked: false,
                    resume_attempted: false,
                },
            );
        }
        assert_eq!(tasks.len(), 2);
        let summary = local_delivery_summary(&tasks);
        assert!(summary.contains("STATUS: PASS"));
        assert!(summary.contains("专家团运行正常"));
        assert!(!summary.contains("无实现节点"));
    }

    #[test]
    fn diff_router_skips_small_ui_change_but_escalates_sensitive_change() {
        let ui = vec![GitChange {
            path: "app/src/theme.rs".into(),
            index: "  ".into(),
            worktree: "M ".into(),
        }];
        assert!(!diff_requires_expert_review(
            &ui,
            "--- a/app/src/theme.rs\n+++ b/app/src/theme.rs\n- width: 90\n+ width: 72"
        ));

        let auth = vec![GitChange {
            path: "server/src/auth.rs".into(),
            index: "  ".into(),
            worktree: "M ".into(),
        }];
        assert!(diff_requires_expert_review(
            &auth,
            "--- a/server/src/auth.rs\n+++ b/server/src/auth.rs\n+ allow = true"
        ));
    }

    #[tokio::test]
    async fn orchestrator_runs_parallel_and_passes_gates() {
        let ctx = AppContext::new();
        let log = SessionLog::new();
        let tracker = Arc::new(TrackingSubagent {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        });
        let _a = ctx.provide(log.clone());
        let sub: Arc<dyn Subagent> = tracker.clone();
        let _b = ctx.provide(sub);
        CouncilOrchestrator {
            max_parallel: 3,
            max_attempts: 2,
        }
        .run(
            &ctx,
            "重构认证和数据库迁移架构".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(tracker.peak.load(Ordering::SeqCst) >= 2);
        let events = log.replay();
        assert!(events.iter().any(|event| matches!(
            event,
            SessionEvent::Council {
                event: CouncilEvent::Completed { .. },
                ..
            }
        )));
        assert!(
            log.replay()
                .iter()
                .filter(|event| matches!(
                    event,
                    SessionEvent::Council {
                        event: CouncilEvent::GateEvaluated { .. },
                        ..
                    }
                ))
                .count()
                >= 4
        );
        assert!(log.replay().iter().any(|event| matches!(
            event,
            SessionEvent::Delivery { report, .. }
                if report.outcome == DeliveryOutcome::Verified
                    && report.criteria.iter().all(|criterion| criterion.satisfied)
        )));
    }

    #[tokio::test]
    async fn exhausted_node_is_resumed_once_before_blocking() {
        let ctx = AppContext::new();
        let log = SessionLog::new();
        let _log = ctx.provide(log.clone());
        let subagent: Arc<dyn Subagent> = Arc::new(FailingSubagent);
        let _subagent = ctx.provide(subagent);

        CouncilOrchestrator::default()
            .run(
                &ctx,
                "解释这段设计的优缺点".into(),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let resumed = log.replay().iter().any(|event| {
            matches!(
                event,
                SessionEvent::Council { event: CouncilEvent::TaskStateChanged { detail, .. }, .. }
                    if detail.contains("自动续跑")
            )
        });
        assert!(resumed, "失败节点应只触发一次自动续跑提示");
    }

    #[tokio::test]
    async fn empty_brief_is_upgraded_without_marking_task_failed() {
        let ctx = AppContext::new();
        let log = SessionLog::new();
        let _log = ctx.provide(log.clone());
        let subagent: Arc<dyn Subagent> = Arc::new(BriefEmptySubagent);
        let _subagent = ctx.provide(subagent);

        CouncilOrchestrator::default()
            .run(
                &ctx,
                "解释这段设计的优缺点".into(),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(log.replay().iter().any(|event| matches!(
            event,
            SessionEvent::Council { event: CouncilEvent::TaskStateChanged { detail, .. }, .. }
                if detail.contains("轻量通道空响应后升级")
        )));
    }
}
