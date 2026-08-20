//! Workspace tree, preview side panel, and conversation message stream.

use super::*;

pub(super) fn show(state: &mut AppState, ctx: &egui::Context, pal: Palette) {
    // ── 最右：文件树（独立开关，show_animated 平滑展开/收起）────
    egui::SidePanel::right("tree")
        .resizable(true)
        .default_width(240.0)
        .width_range(180.0..=360.0)
        .frame(egui::Frame::default().fill(pal.side).inner_margin(8.0))
        .show_animated(ctx, state.tree_open, |ui| {
            state.render_tree(ui, &pal);
        });
    // ── 次右：文件预览 ──────────────────────────────────────
    // 内容加载完成后直接以稳定宽度显示。不要使用 show_animated：侧栏展开动画
    // 会让 CentralPanel 连续重排，预览内容也会在变化的宽度下逐帧重新布局，
    // 点击文件时视觉上表现为整个窗口闪烁。
    if state.preview_open {
        egui::SidePanel::right("preview")
            .resizable(true)
            .default_width(380.0)
            .width_range(320.0..=600.0)
            .frame(egui::Frame::default().fill(pal.panel).inner_margin(0.0))
            .show(ctx, |ui| {
                state.render_preview(ui, &pal);
            });
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
                            for msg in state.messages.clone() {
                                if msg.text.is_empty() {
                                    continue; // 纯 DSML 气泡剥离后为空，不渲染空卡片
                                }
                                let (fill, text_color): (egui::Color32, egui::Color32) =
                                    match msg.kind.as_str() {
                                        "user" => (pal.user_bubble, pal.user_text),
                                        "error" => (pal.err_bubble, pal.err_text),
                                        "tool" | "plan" => (pal.tool_bubble, pal.dim),
                                        "thinking" => (pal.field, pal.dim),
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
