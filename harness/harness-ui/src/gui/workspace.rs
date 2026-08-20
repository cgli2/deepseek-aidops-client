//! Workspace tree, preview side panel, and conversation message stream.

use super::*;

pub(super) fn show(state: &mut AppState, ctx: &egui::Context, pal: Palette) {
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
                                        ui.set_max_width(max_w * 0.96);
                                        ui.label(
                                            egui::RichText::new(&msg.label)
                                                .size(10.5)
                                                .color(pal.dim),
                                        );
                                        #[cfg(target_os = "macos")]
                                        ui.add_space(2.0);
                                        // selectable(true)：正文支持鼠标拖选，选中后 Ctrl+C 复制。
                                        let resp = if msg.kind == "assistant" {
                                            // Markdown 富文本渲染：标题/加粗/列表/代码块转 LayoutJob；
                                            // 行内代码中的文件路径识别为可点击 chip。
                                            let blocks = crate::markdown::parse_blocks(
                                                &msg.text,
                                                &crate::markdown::MdTheme {
                                                    text: pal.text,
                                                    dim: pal.dim,
                                                    accent: pal.accent,
                                                    code_text: pal.text,
                                                    code_bg: pal.field,
                                                },
                                                max_w * 0.96 - 20.0,
                                            );
                                            let mut last_resp = None;
                                            for block in blocks {
                                                match block {
                                                    crate::markdown::MarkdownBlock::Job(job) => {
                                                        last_resp = Some(ui.add(
                                                            egui::Label::new(job).selectable(true),
                                                        ));
                                                    }
                                                    crate::markdown::MarkdownBlock::FilePath(
                                                        path,
                                                    ) => {
                                                        // 用透明背景 Button 实现可点击文件路径：
                                                        // - Button 悬停时自动变手型指针（Label+Sense::click 不会）
                                                        // - 透明背景避免方块感，保持行内文本外观
                                                        let label = egui::RichText::new(&path)
                                                            .monospace()
                                                            .color(pal.accent)
                                                            .underline()
                                                            .size(12.5);
                                                        let btn = egui::Button::new(label)
                                                            .fill(egui::Color32::TRANSPARENT)
                                                            .stroke(egui::Stroke::NONE);
                                                        let r = ui.add(btn);
                                                        if r.hovered() {
                                                            ui.ctx().set_cursor_icon(
                                                                egui::CursorIcon::PointingHand,
                                                            );
                                                        }
                                                        let r = r.on_hover_text("点击预览此文件");
                                                        if r.clicked() {
                                                            state.pending_preview = Some(path);
                                                        }
                                                        last_resp = Some(r);
                                                    }
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
                        });
                });
        });

    // ── 处理延迟的文件预览请求（避免渲染期间布局突变闪烁）──
    if let Some(path) = state.pending_preview.take() {
        state.open_preview(path);
        ctx.request_repaint();
    }
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
    let queue_fill = pal.warn.gamma_multiply(if state.dark { 0.16 } else { 0.10 });
    egui::Frame::default()
        .fill(queue_fill)
        .rounding(egui::Rounding::same(9.0))
        .stroke(egui::Stroke::new(1.0_f32, pal.warn))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // 左侧 accent 竖条：强化「待处理队列」识别。
                let (bar, _) = ui.allocate_exact_size(egui::vec2(3.0, 18.0), egui::Sense::hover());
                ui.painter().rect_filled(
                    bar,
                    egui::Rounding::same(2.0),
                    pal.warn,
                );
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
                        egui::RichText::new(format!("{}. {}", position + 1, one_line_summary(&item.text, 68)))
                            .size(12.0)
                            .color(pal.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let delete = ui
                            .add(egui::Button::new(egui::RichText::new("移除").size(11.0).color(pal.err_text)))
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
                    ui.label(egui::RichText::new(label).size(10.5).color(pal.dim));
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&msg.text)
                                .monospace()
                                .size(11.5)
                                .color(pal.text),
                        )
                        .selectable(true),
                    );
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
