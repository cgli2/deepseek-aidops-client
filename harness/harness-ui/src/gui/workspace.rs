//! Workspace tree, preview side panel, and conversation message stream.

use super::*;

/// 先创建右侧分栏，使后续底部输入区自动只占中央剩余区域。
pub(super) fn show_side_panels(state: &mut AppState, ctx: &egui::Context, pal: Palette) {
    // ── 最右：文件树（独立开关）────
    // frame 边距为 0：让「文件树」头部紧贴顶部导航头，不留白色空隙；
    // 内容区的内边距改在 render_tree 主体里加。
    // 不用 show_animated：展开动画与 resizable 分隔线拖拽逐帧冲突
    // （动画把宽度往目标值拉回，拖拽又写入新宽度）→ 分隔线拼命抖动；
    // 与预览面板同一决策：开关直接 show，宽度稳定。
    if state.tree_open {
        egui::SidePanel::right("tree")
            .resizable(true)
            .default_width(240.0)
            .width_range(180.0..=360.0)
            .frame(egui::Frame::default().fill(pal.side).inner_margin(0.0))
            .show(ctx, |ui| {
                state.render_tree(ui, &pal);
            });
    }
    // ── 次右：文件预览（分隔窗口 + 宽度开关动画）─────────
    // 回到 SidePanel 分隔布局：预览占据右侧固定宽度，不遮挡文件树。
    // 闪烁缓解：面板打开/关闭时宽度都从 0↔目标值平滑过渡（约 0.15s），
    // 中央消息流宽度渐变重排，视觉上是“柔滑推开/合拢”而非瞬间跳变。
    // 动画结束后记录实际宽度供重开保持；关闭动画结束时释放面板。
    if state.preview_open || state.preview_animating {
        let target_w = state.preview_width.clamp(320.0, 600.0);
        let t = ctx.animate_bool(egui::Id::new("preview_open_anim"), state.preview_open);
        if state.preview_open {
            // 打开：宽度 0 → 目标
            let w = (target_w * t).max(4.0);
            egui::SidePanel::right("preview")
                .exact_width(w)
                .frame(egui::Frame::default().fill(pal.panel).inner_margin(0.0))
                .show(ctx, |ui| {
                    state.render_preview(ui, &pal);
                });
            if t >= 0.999 {
                state.preview_width = w;
                state.preview_animating = false;
            }
        } else {
            // 关闭：宽度 目标 → 0（动画完成前保持渲染）
            let w = (target_w * t).max(4.0);
            egui::SidePanel::right("preview")
                .exact_width(w)
                .frame(egui::Frame::default().fill(pal.panel).inner_margin(0.0))
                .show(ctx, |ui| {
                    state.render_preview(ui, &pal);
                });
            if t <= 0.001 {
                state.preview_animating = false;
            }
        }
    }
}

/// 最后创建 CentralPanel；Egui 要求中央面板在所有边缘面板之后渲染。
pub(super) fn show_main(state: &mut AppState, ctx: &egui::Context, pal: Palette) {
    // ── 主区：头部 + 消息流 ──────────────────────────────────
    egui::CentralPanel::default()
        .frame(egui::Frame::default().fill(pal.bg))
        .show(ctx, |ui| {
            ui.add_space(4.0);
            // 正文画布保留响应式左右 gutter：宽窗口更舒展，窄窗口不浪费空间。
            // 顶部导航色带仍保持满宽，消息气泡按扣除 gutter 后的宽度排版。
            let content_padding = if ui.available_width() >= 1100.0 {
                24.0
            } else if ui.available_width() >= 760.0 {
                18.0
            } else {
                12.0
            };
            egui::Frame::default()
                .inner_margin(egui::Margin {
                    left: content_padding,
                    right: content_padding,
                    top: 2.0,
                    bottom: 0.0,
                })
                .show(ui, |ui| {
                    // ── 版本更新横幅（非 Idle 时显示）──
                    state.draw_update_banner(ui, &pal);
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            let max_w = ui.available_width();
                            let messages = state.messages.clone();
                            let mut index = 0;
                            while index < messages.len() {
                                let msg = &messages[index];
                                if msg.text.is_empty() {
                                    index += 1;
                                    continue; // 纯 DSML 气泡剥离后为空，不渲染空卡片
                                }
                                // 连续的思考 / 工具 / 计划属于同一个 agent 回合：合并为一张
                                // 工作过程卡，避免每个 tool call 都占据一整行对话空间。
                                if matches!(msg.kind.as_str(), "thinking" | "tool" | "plan") {
                                    let start = index;
                                    index += 1;
                                    while index < messages.len()
                                        && matches!(messages[index].kind.as_str(), "thinking" | "tool" | "plan")
                                    {
                                        index += 1;
                                    }
                                    render_work_batch(ui, &messages[start..index], start, max_w, pal);
                                    ui.add_space(6.0);
                                    continue;
                                }
                                let (fill, text_color): (egui::Color32, egui::Color32) =
                                    match msg.kind.as_str() {
                                        "user" => (pal.user_bubble, pal.user_text),
                                        "error" => (pal.err_bubble, pal.err_text),
                                        _ => (pal.ai_bubble, pal.text),
                                    };
                                // 所有气泡统一左对齐：用户消息不右对齐，阅读动线更连贯。
                                ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                                    let bubble = egui::Frame::default()
                                        .fill(fill)
                                        .rounding(egui::Rounding::same(12.0))
                                        .inner_margin(if cfg!(target_os = "macos") {
                                            egui::Margin::symmetric(12.0, 10.0)
                                        } else {
                                            egui::Margin::symmetric(14.0, 12.0)
                                        })
                                        .stroke(egui::Stroke::new(1.0_f32, pal.border));
                                    bubble.show(ui, |ui| {
                                        // 最终回答使用接近整行的稳定阅读宽度，避免短行 Markdown
                                        // 按内容收缩成窄卡片；思考/工具过程由独立工作卡渲染，不受影响。
                                        let assistant_content_w = (max_w - 40.0).max(280.0);
                                        if msg.kind == "assistant" {
                                            ui.set_width(assistant_content_w);
                                        } else {
                                            ui.set_max_width(max_w * 0.96);
                                        }
                                        ui.label(
                                            egui::RichText::new(&msg.label)
                                                .size(10.5)
                                                .color(pal.dim),
                                        );
                                        #[cfg(target_os = "macos")]
                                        ui.add_space(2.0);
                                        // selectable(true)：正文支持鼠标拖选，选中后 Ctrl+C 复制。
                                        let resp = if msg.kind == "assistant" {
                                            // Markdown 真实渲染：标题/加粗/列表/代码块全部保留，
                                            // 段落间有换行与间距，不再因文件路径被压平成纯文本。
                                            let markdown_w = ui.available_width().max(240.0);
                                            let theme = crate::markdown::MdTheme {
                                                text: pal.text,
                                                dim: pal.dim,
                                                accent: pal.accent,
                                                code_text: pal.text,
                                                code_bg: pal.field,
                                            };
                                            let job = crate::markdown::to_job(&msg.text, &theme, markdown_w);
                                            let mut last_resp = Some(ui.add(
                                                egui::Label::new(job).selectable(true),
                                            ));
                                            // 文件路径识别：正文用完整 markdown 渲染，
                                            // 识别出的文件路径作为可点击 chip 追加在正文下方，
                                            // 不打断正文排版。
                                            let mut file_paths: Vec<String> = Vec::new();
                                            for b in crate::markdown::parse_blocks(
                                                &msg.text,
                                                &theme,
                                                markdown_w,
                                            ) {
                                                if let crate::markdown::MarkdownBlock::FilePath(p) = b {
                                                    if !file_paths.contains(&p) {
                                                        file_paths.push(p);
                                                    }
                                                }
                                            }
                                            if !file_paths.is_empty() {
                                                ui.add_space(6.0);
                                                ui.label(
                                                    egui::RichText::new("文件：").size(10.5).color(pal.dim),
                                                );
                                                for path in file_paths {
                                                    let label = egui::RichText::new(&path)
                                                        .monospace()
                                                        .color(pal.accent)
                                                        .underline()
                                                        .size(12.5);
                                                    let btn = egui::Button::new(label)
                                                        .fill(egui::Color32::TRANSPARENT)
                                                        .stroke(egui::Stroke::NONE);
                                                    let rb = ui.add(btn);
                                                    if rb.hovered() {
                                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                                    }
                                                    let rb = rb.on_hover_text("点击预览此文件");
                                                    if rb.clicked() {
                                                        state.pending_preview = Some(path);
                                                    }
                                                    last_resp = Some(rb);
                                                }
                                            }
                                            last_resp.unwrap_or_else(|| ui.label(""))
                                        } else {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(&msg.text)
                                                        .size(13.5)
                                                        .color(text_color),
                                                )
                                                .selectable(true),
                                            )
                                        };
                                        // 右键菜单：一键复制整条消息内容。
                                        resp.context_menu(|ui| {
                                            if ui.button("📋 复制全部内容").clicked() {
                                                ui.ctx().copy_text(msg.text.clone());
                                                ui.close_menu();
                                            }
                                        });
                                    });
                                });
                                ui.add_space(6.0);
                                index += 1;
                            }
                            // 标准模式只展示常规对话。保留专家团状态供用户切回团队模式
                            // 查看证据，但不让旧的失败/完成卡片继续占据主会话并制造“仍在运行”的误解。
                            if state.multi_agent {
                                for council in state.councils.values() {
                                    render_council_card(ui, council, max_w, pal);
                                    ui.add_space(8.0);
                                }
                            }
                            // 不依赖首个 SSE 分片：用户提交后的下一帧就显示在消息主区域。
                            // API 首包慢时，用户无需在底部小字里寻找运行状态。
                            if state.busy {
                                let secs = state.turn_started.map(|t| t.elapsed().as_secs()).unwrap_or(0);
                                let activity = if state.activity.is_empty() {
                                    "正在启动任务"
                                } else {
                                    state.activity.as_str()
                                };
                                // 紧凑版 loading 气泡：缩小上下内边距与字号，降低单条占用高度。
                                egui::Frame::default()
                                    .fill(pal.ai_bubble)
                                    .rounding(egui::Rounding::same(12.0))
                                    .stroke(egui::Stroke::new(1.0_f32, pal.accent.gamma_multiply(0.55)))
                                    .inner_margin(egui::Margin::symmetric(12.0, 5.0))
                                    .show(ui, |ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        ui.horizontal(|ui| {
                                            ui.add(egui::Spinner::new().size(12.0));
                                            ui.label(
                                                egui::RichText::new(format!("{activity} · {secs} 秒"))
                                                    .size(11.0)
                                                    .color(pal.text),
                                            );
                                        });
                                        if state.last_activity.map(|t| t.elapsed().as_secs() >= 10).unwrap_or(false) {
                                            ui.add_space(2.0);
                                            ui.label(
                                                egui::RichText::new("仍在等待模型或工具返回；任务没有静默停止，可随时点击停止")
                                                    .size(10.0)
                                                    .color(pal.warn),
                                            );
                                        }
                                    });
                            }
                        });
                });
        });

    // ── 处理延迟的文件预览请求（避免渲染期间布局突变闪烁）──
    if let Some(path) = state.pending_preview.take() {
        state.open_preview(path);
        ctx.request_repaint();
    }
}

fn render_council_card(ui: &mut egui::Ui, council: &CouncilUi, max_w: f32, pal: Palette) {
    let total = council.tasks.len();
    let done = council
        .tasks
        .values()
        .filter(|t| t.state == CouncilTaskState::Done)
        .count();
    let running = council
        .tasks
        .values()
        .filter(|t| t.state == CouncilTaskState::Running)
        .count();
    let failed = council
        .tasks
        .values()
        .filter(|t| {
            matches!(
                t.state,
                CouncilTaskState::Failed | CouncilTaskState::Blocked
            )
        })
        .count();
    let elapsed = council
        .started_at
        .map(|t| t.elapsed().as_secs())
        .unwrap_or(0);
    // ScrollArea 的内容可被长、不换行文本反向撑宽。先在主会话可用宽度内创建
    // 一个硬边界子 Ui，再让卡片及全部标签只在该边界内布局。
    let available_w = max_w.min(ui.available_width()).max(240.0);
    let gutter = if available_w >= 520.0 { 12.0 } else { 6.0 };
    let card_w = (available_w - gutter * 2.0).max(228.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(gutter);
        ui.allocate_ui_with_layout(
            egui::vec2(card_w, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(card_w);
                egui::Frame::default()
                    .fill(pal.panel)
                    .rounding(egui::Rounding::same(12.0))
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        if failed > 0 { pal.warn } else { pal.accent },
                    ))
                    .inner_margin(egui::Margin::symmetric(14.0, 12.0))
                    .show(ui, |ui| {
                        let content_w = (card_w - 30.0).max(220.0);
                        ui.set_width(content_w);
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                egui::RichText::new("专家团 DAG")
                                    .strong()
                                    .size(13.5)
                                    .color(pal.text),
                            );
                            ui.label(
                                egui::RichText::new(&council.phase)
                                    .size(11.5)
                                    .color(if failed > 0 { pal.warn } else { pal.accent }),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "{done}/{total} · 并行 {running}/{} · {elapsed}s",
                                    council.max_parallel
                                ))
                                .size(10.5)
                                .color(pal.dim),
                            );
                        });
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&council.goal)
                                    .size(12.0)
                                    .color(pal.text),
                            )
                            .wrap(),
                        );
                        let progress = if total == 0 {
                            0.0
                        } else {
                            done as f32 / total as f32
                        };
                        ui.add(
                            egui::ProgressBar::new(progress)
                                .desired_width(content_w)
                                .show_percentage(),
                        );
                        ui.add_space(4.0);
                        for task in council.tasks.values() {
                            let (mark, color) = match task.state {
                                CouncilTaskState::Done => ("✓", pal.accent),
                                CouncilTaskState::Running => ("◐", pal.warn),
                                CouncilTaskState::Failed | CouncilTaskState::Blocked => {
                                    ("×", pal.err_text)
                                }
                                CouncilTaskState::Cancelled => ("—", pal.dim),
                                CouncilTaskState::Ready => ("○", pal.text),
                                CouncilTaskState::Pending => ("·", pal.dim),
                            };
                            let retry = if task.attempt > 1 {
                                format!(" · 第 {} 次", task.attempt)
                            } else {
                                String::new()
                            };
                            let title = one_line_summary(
                                &format!("{mark} {} · {}{retry}", task.spec.title, task.spec.role),
                                ((content_w / 8.0) as usize).max(20),
                            );
                            egui::CollapsingHeader::new(
                                egui::RichText::new(title).size(11.5).color(color),
                            )
                            .id_salt(("council-task", &council.id, &task.spec.id))
                            .default_open(false)
                            .show(ui, |ui| {
                                ui.set_max_width((content_w - 18.0).max(180.0));
                                let deps = if task.spec.depends_on.is_empty() {
                                    "无".into()
                                } else {
                                    task.spec.depends_on.join(", ")
                                };
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!("依赖：{deps}"))
                                            .size(10.5)
                                            .color(pal.dim),
                                    )
                                    .wrap(),
                                );
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&task.detail)
                                            .size(11.0)
                                            .color(pal.text),
                                    )
                                    .wrap(),
                                );
                            });
                        }
                        if !council.gates.is_empty() {
                            ui.separator();
                            ui.label(
                                egui::RichText::new("质量门禁")
                                    .strong()
                                    .size(11.5)
                                    .color(pal.text),
                            );
                            for gate in &council.gates {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(format!(
                                            "{} {} · {}",
                                            if gate.passed { "✓" } else { "×" },
                                            gate.name,
                                            gate.evidence
                                        ))
                                        .size(10.5)
                                        .color(
                                            if gate.passed {
                                                pal.accent
                                            } else {
                                                pal.err_text
                                            },
                                        ),
                                    )
                                    .wrap(),
                                );
                            }
                        }
                        if !council.detail.is_empty() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&council.detail)
                                        .size(10.5)
                                        .color(if failed > 0 { pal.warn } else { pal.dim }),
                                )
                                .wrap(),
                            );
                        }
                    });
            },
        );
        ui.add_space(gutter);
    });
}

/// 待发送队列：渲染在输入框上方（紧挨输入卡片），与输入区同底色系但背景更醒目。
/// 项目仍在控制器 FIFO 中，只有轮到执行时才会进入会话事件流；因此此处可以安全撤回。
pub(super) fn render_pending_queue(ui: &mut egui::Ui, state: &mut AppState, pal: Palette) {
    let queued = state.host.sink.queued_inputs();
    if queued.is_empty() {
        return;
    }

    let mut remove_id = None;
    // 用强调底色 + accent 左边条，与输入卡片（panel）和消息流（bg）形成明显区别。
    let queue_fill = pal
        .warn
        .gamma_multiply(if state.dark { 0.16 } else { 0.10 });
    egui::Frame::default()
        .fill(queue_fill)
        .rounding(egui::Rounding::same(9.0))
        .stroke(egui::Stroke::new(1.0_f32, pal.warn))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // 左侧 accent 竖条：强化「待处理队列」识别。
                let (bar, _) = ui.allocate_exact_size(egui::vec2(3.0, 18.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(bar, egui::Rounding::same(2.0), pal.warn);
                ui.label(
                    egui::RichText::new(format!("待发送任务 · {} 条", queued.len()))
                        .size(11.5)
                        .strong()
                        .color(pal.warn),
                );
            });
            for (position, item) in queued.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}. {}",
                            position + 1,
                            one_line_summary(&item.text, 68)
                        ))
                        .size(12.0)
                        .color(pal.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let delete = ui
                            .add(egui::Button::new(
                                egui::RichText::new("移除").size(11.0).color(pal.err_text),
                            ))
                            .on_hover_text("撤回此待发送任务");
                        if delete.clicked() {
                            remove_id = Some(item.id);
                        }
                    });
                });
            }
        });
    if let Some(id) = remove_id {
        if state.host.sink.remove_queued(id) {
            state.note = "已移除待发送任务".into();
        }
    }
    ui.add_space(8.0);
}

/// 一次 agent 回合内连续的思考、调用和返回共用一个折叠容器；默认只占一行。
/// `start_index` 是 append-only 消息序号，可作为 egui 折叠状态的稳定 id。
fn render_work_batch(
    ui: &mut egui::Ui,
    messages: &[ChatMsg],
    start_index: usize,
    max_w: f32,
    pal: Palette,
) {
    let tool_count = messages.iter().filter(|msg| msg.kind == "tool").count();
    let thinking_count = messages.iter().filter(|msg| msg.kind == "thinking").count();
    let latest = messages
        .last()
        .map(|msg| one_line_summary(&msg.text, 54))
        .unwrap_or_default();
    let summary = match (thinking_count, tool_count) {
        (0, 0) => format!("{} 条过程", messages.len()),
        (0, tools) => format!("{tools} 次工具调用 · {latest}"),
        (thoughts, 0) => format!("思考中 · {thoughts} 条 · {latest}"),
        (thoughts, tools) => format!("思考 {thoughts} 条 · 工具 {tools} 次 · {latest}"),
    };
    egui::Frame::default()
        .fill(pal.field)
        .rounding(egui::Rounding::same(8.0))
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .inner_margin(egui::Margin::symmetric(10.0, 5.0))
        .show(ui, |ui| {
            ui.set_max_width(max_w * 0.96);
            egui::CollapsingHeader::new(
                egui::RichText::new(format!("◌ 工作过程 · {summary}"))
                    .size(11.5)
                    .color(pal.dim),
            )
            .id_salt(("work-batch", start_index))
            .default_open(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                for (position, msg) in messages.iter().enumerate() {
                    if position > 0 {
                        ui.separator();
                    }
                    let label = match msg.kind.as_str() {
                        "thinking" => "思考",
                        "tool" => "工具",
                        "plan" => "计划",
                        _ => "过程",
                    };
                    // 收拢：每条过程只显示一行摘要，避免思考/工具内容占满屏幕。
                    // 完整内容仍可通过右键「复制全部内容」获取。
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(label).size(10.5).color(pal.dim));
                        let summary =
                            one_line_summary(&msg.text, if msg.kind == "tool" { 90 } else { 60 });
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&summary).monospace().size(11.5).color(
                                    if msg.kind == "thinking" {
                                        pal.dim
                                    } else {
                                        pal.text
                                    },
                                ),
                            )
                            .selectable(true),
                        );
                    });
                }
            });
        });
}

fn one_line_summary(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = compact.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}
