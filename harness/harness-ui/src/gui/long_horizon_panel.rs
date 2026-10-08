//! Long-horizon task status, submission, and durable HITL decisions.

use super::fonts::{FONT_CAPTION, FONT_SECONDARY, FONT_TITLE, FONT_UI};
use super::theme::Palette;
use super::widgets::{accent_button_ex, compact_button};
use super::*;
use harness_runtime::{CheckpointState, TaskStatus};

fn render_capsule_progress(ui: &mut egui::Ui, pal: &Palette, pct: f32) {
    let height = 6.0;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width() - 50.0, height),
            egui::Sense::hover(),
        );
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(3.0), pal.field);
        ui.painter().rect(
            rect,
            egui::Rounding::same(3.0),
            egui::Color32::TRANSPARENT,
            egui::Stroke::new(1.0, pal.card_border),
        );
        let clamped = pct.clamp(0.0, 1.0);
        if clamped > 0.005 {
            let fill_w = (rect.width() * clamped).max(6.0);
            let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_w, height));
            let fill_col = if clamped >= 0.999 {
                pal.success
            } else {
                pal.accent
            };
            ui.painter()
                .rect_filled(fill_rect, egui::Rounding::same(3.0), fill_col);
        }
        ui.label(
            egui::RichText::new(format!("{:.0}%", clamped * 100.0))
                .size(FONT_CAPTION)
                .color(pal.dim),
        );
    });
}

pub(super) fn show(state: &mut AppState, ctx: &egui::Context, pal: Palette) {
    if !state.lha_open {
        return;
    }

    let runtime = state
        .host
        .long_horizon
        .runtime_for(std::path::Path::new(&state.active_project));
    let (runtime, mut tasks, decisions) = match runtime {
        Ok(runtime) => {
            let tasks = runtime.tasks();
            let decisions = runtime.decisions();
            match (tasks, decisions) {
                (Ok(tasks), Ok(decisions)) => (Some(runtime), tasks, decisions),
                (Err(error), _) | (_, Err(error)) => {
                    state.lha_note = format!("读取长任务状态失败: {error}");
                    (Some(runtime), Vec::new(), Vec::new())
                }
            }
        }
        Err(error) => {
            state.lha_note = format!("打开长任务控制面失败: {error}");
            (None, Vec::new(), Vec::new())
        }
    };
    tasks.reverse();

    let mut open = true;
    let mut submit_requested = false;
    let mut decision: Option<(bool, String)> = None;
    egui::Window::new("长时程任务")
        .id(egui::Id::new("long_horizon_control"))
        .open(&mut open)
        .default_size([700.0, 620.0])
        .min_width(560.0)
        .resizable(true)
        .show(ctx, |ui| {
            super::self_monitor_panel::show(ui, &state.active_project, &pal);
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("持久化执行、恢复状态与人工决策")
                    .size(FONT_SECONDARY)
                    .color(pal.dim),
            );
            ui.add_space(6.0);
            ui.add(
                egui::TextEdit::multiline(&mut state.lha_prompt)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .hint_text("输入需要长时间自主执行的目标…"),
            );
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let enabled = runtime.is_some() && !state.lha_prompt.trim().is_empty();
                if accent_button_ex(ui, &pal, "提交长任务", enabled) {
                    submit_requested = true;
                }
                if state.host.sink.any_busy() {
                    ui.label(
                        egui::RichText::new("已有任务执行中，新输入将进入队列").color(pal.warn),
                    );
                }
            });

            if !state.lha_note.is_empty() {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(&state.lha_note)
                        .size(FONT_CAPTION)
                        .color(pal.dim),
                );
            }

            ui.add_space(8.0);
            let sep_rect = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover()).0;
            ui.painter().rect_filled(sep_rect, 0.0, pal.line);
            ui.add_space(8.0);

            let pending: Vec<_> = decisions
                .iter()
                .filter(|item| item.state == CheckpointState::Pending)
                .collect();
            ui.label(
                egui::RichText::new(format!("待人工确认 ({})", pending.len()))
                    .size(FONT_TITLE)
                    .strong()
                    .color(pal.text),
            );
            ui.add_space(4.0);
            if pending.is_empty() {
                ui.label(egui::RichText::new("当前没有待确认检查点").size(FONT_SECONDARY).color(pal.dim));
            } else {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("操作人:").size(FONT_UI).color(pal.dim));
                    ui.text_edit_singleline(&mut state.lha_actor);
                });
                ui.add(
                    egui::TextEdit::singleline(&mut state.lha_decision_note)
                        .desired_width(f32::INFINITY)
                        .hint_text("审批说明（建议填写）"),
                );
                ui.add_space(4.0);
                for checkpoint in pending {
                    egui::Frame::default()
                        .fill(pal.field)
                        .rounding(egui::Rounding::same(8.0))
                        .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
                        .inner_margin(egui::Margin::same(10.0))
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(&checkpoint.subject)
                                    .strong()
                                    .size(FONT_UI)
                                    .color(pal.text),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} · {:?} · 影子工件 {}",
                                    checkpoint.checkpoint_id,
                                    checkpoint.kind,
                                    checkpoint.shadow_artifact
                                ))
                                .size(FONT_CAPTION)
                                .color(pal.dim),
                            );
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                if compact_button(ui, &pal, "批准") {
                                    decision = Some((true, checkpoint.checkpoint_id.clone()));
                                }
                                if compact_button(ui, &pal, "拒绝") {
                                    decision = Some((false, checkpoint.checkpoint_id.clone()));
                                }
                            });
                        });
                    ui.add_space(6.0);
                }
            }

            ui.add_space(8.0);
            let sep_rect2 = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover()).0;
            ui.painter().rect_filled(sep_rect2, 0.0, pal.line);
            ui.add_space(8.0);

            ui.label(
                egui::RichText::new(format!("任务状态 ({})", tasks.len()))
                    .size(FONT_TITLE)
                    .strong()
                    .color(pal.text),
            );
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("long_horizon_tasks")
                .max_height(300.0)
                .show(ui, |ui| {
                    if tasks.is_empty() {
                        ui.label(egui::RichText::new("当前项目尚无长时程任务").size(FONT_SECONDARY).color(pal.dim));
                    }
                    for task in tasks.iter().take(100) {
                        egui::Frame::default()
                            .fill(pal.field)
                            .rounding(egui::Rounding::same(8.0))
                            .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
                            .inner_margin(egui::Margin::same(10.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(&task.spec.task_id)
                                            .strong()
                                            .size(FONT_UI)
                                            .color(pal.text),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label(
                                                egui::RichText::new(status_text(&task.status))
                                                    .size(FONT_CAPTION)
                                                    .color(status_color(&task.status, pal)),
                                            );
                                        },
                                    );
                                });
                                ui.add_space(4.0);
                                render_capsule_progress(
                                    ui,
                                    &pal,
                                    (task.progress_pct / 100.0).clamp(0.0, 1.0),
                                );
                                ui.add_space(2.0);
                                let note = task.last_note.as_deref().unwrap_or("暂无进度说明");
                                ui.label(
                                    egui::RichText::new(format!(
                                        "重试 {} / {} · {note}",
                                        task.retry_count, task.spec.max_retries
                                    ))
                                    .size(FONT_CAPTION)
                                    .color(pal.dim),
                                );
                                if let Some(detail) = status_detail(&task.status) {
                                    ui.label(egui::RichText::new(detail).size(FONT_CAPTION).color(pal.dim));
                                }
                            });
                        ui.add_space(6.0);
                    }
                });
        });
    state.lha_open = open;

    if submit_requested {
        let prompt = std::mem::take(&mut state.lha_prompt);
        state.host.sink.set_permission(state.permission.clone());
        state.host.sink.submit(prompt);
        state.busy = true;
        state.lha_note = "长任务已提交；执行状态会持久化到当前项目。".into();
    }
    if let (Some(runtime), Some((approved, checkpoint_id))) = (runtime, decision) {
        let actor = state.lha_actor.trim();
        let note = state.lha_decision_note.trim();
        let result = if approved {
            runtime.approve_decision(&checkpoint_id, actor, note)
        } else {
            runtime.reject_decision(&checkpoint_id, actor, note)
        };
        match result {
            Ok(()) => {
                state.lha_note = format!(
                    "检查点 {checkpoint_id} 已{}。",
                    if approved { "批准" } else { "拒绝" }
                );
                state.lha_decision_note.clear();
            }
            Err(error) => state.lha_note = format!("保存决策失败: {error}"),
        }
    }
}

fn status_text(status: &TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "等待依赖",
        TaskStatus::Ready => "就绪",
        TaskStatus::Scheduled { .. } => "已调度",
        TaskStatus::Running { .. } => "执行中",
        TaskStatus::Validating => "验证中",
        TaskStatus::Succeeded { .. } => "已完成",
        TaskStatus::Failed { .. } => "失败",
        TaskStatus::Cancelled { .. } => "已取消",
        TaskStatus::BudgetExhausted { .. } => "预算耗尽",
    }
}

fn status_color(status: &TaskStatus, pal: Palette) -> egui::Color32 {
    match status {
        TaskStatus::Succeeded { .. } => pal.accent,
        TaskStatus::Failed { .. } | TaskStatus::Cancelled { .. } => pal.err_text,
        TaskStatus::BudgetExhausted { .. } => pal.warn,
        _ => pal.dim,
    }
}

fn status_detail(status: &TaskStatus) -> Option<String> {
    match status {
        TaskStatus::Scheduled { worker_id } | TaskStatus::Running { worker_id, .. } => {
            Some(format!("执行者: {worker_id}"))
        }
        TaskStatus::Succeeded { artifact_uri } => Some(format!("工件: {artifact_uri}")),
        TaskStatus::Failed { reason } | TaskStatus::Cancelled { reason } => {
            Some(format!("原因: {reason}"))
        }
        TaskStatus::BudgetExhausted { report_uri } => Some(format!("部分交付: {report_uri}")),
        _ => None,
    }
}
