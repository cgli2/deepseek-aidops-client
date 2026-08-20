//! Bottom message composer and model/permission controls.

use super::*;

pub(super) fn show(state: &mut AppState, ctx: &egui::Context, pal: Palette) -> bool {
    let mut send_now = false;
    egui::TopBottomPanel::bottom("composer")
        .show_separator_line(false)
        .frame(
            egui::Frame::default()
                .fill(pal.bg)
                .inner_margin(egui::Margin {
                    left: 10.0,
                    right: 10.0,
                    top: 4.0,
                    bottom: 6.0,
                }),
        )
        .show(ctx, |ui| {
            let can_send = !state.busy && !state.input.trim().is_empty();
            // ── 输入卡片：圆角 + 细边框 + 阴影浮起，卡片自身提供 chrome ──
            let card_frame = egui::Frame::default()
                .fill(pal.panel)
                .rounding(egui::Rounding::same(14.0))
                .stroke(egui::Stroke::new(1.0_f32, pal.border))
                .inner_margin(egui::Margin {
                    left: 14.0,
                    right: 10.0,
                    top: 12.0,
                    bottom: 8.0,
                })
                .shadow(egui::epaint::Shadow {
                    offset: egui::vec2(0.0, 6.0),
                    blur: 18.0,
                    spread: 0.0,
                    color: egui::Color32::from_black_alpha(if state.dark { 0x44 } else { 0x14 }),
                });
            card_frame.show(ui, |ui| {
                // 文本编辑区：去掉自身边框/背景，由卡片提供 chrome。
                let response = ui.add(
                    egui::TextEdit::multiline(&mut state.input)
                        .desired_width(f32::INFINITY)
                        .desired_rows(3)
                        .font(egui::FontId::proportional(13.5))
                        .frame(false)
                        .margin(egui::Margin::same(0.0))
                        .hint_text(
                            egui::RichText::new("描述任务、粘贴代码或提出问题…").color(pal.dim),
                        ),
                );
                // Enter 发送 / Shift+Enter 换行：egui 会先插入换行，这里去掉尾随 \n 再提交。
                let enter = response.has_focus()
                    && ctx.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                if enter {
                    while state.input.ends_with('\n') {
                        state.input.pop();
                    }
                    send_now = true;
                }
                ui.add_space(6.0);

                // ── 底部工具栏 ──
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;

                    // ── 模型 chip（点击 → 打开设置页） ──
                    let model_label = format!("{} · {}", state.f_provider, state.f_model);
                    let text_w: f32 = model_label
                        .chars()
                        .map(|c| if c.is_ascii() { 7.0 } else { 12.0 })
                        .sum();
                    let chip_w = (text_w + 40.0).max(110.0).min(240.0);
                    let (mrect, mresp) =
                        ui.allocate_exact_size(egui::vec2(chip_w, 28.0), egui::Sense::click());
                    let mfill = if mresp.hovered() {
                        pal.hover
                    } else {
                        pal.field
                    };
                    ui.painter()
                        .rect_filled(mrect, egui::Rounding::same(8.0), mfill);
                    ui.painter().rect(
                        mrect,
                        egui::Rounding::same(8.0),
                        egui::Color32::TRANSPARENT,
                        egui::Stroke::new(1.0_f32, pal.border),
                    );
                    ui.painter().text(
                        mrect.left_center() + egui::vec2(10.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        &model_label,
                        egui::FontId::proportional(12.0),
                        pal.text,
                    );
                    // 右侧 chevron
                    let cx = mrect.right() - 11.0;
                    let cy = mrect.center().y;
                    ui.painter().add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(cx - 3.5, cy - 1.6),
                            egui::pos2(cx + 3.5, cy - 1.6),
                            egui::pos2(cx, cy + 1.8),
                        ],
                        pal.dim,
                        egui::Stroke::NONE,
                    ));
                    if mresp.clicked() {
                        state.settings_page = "模型设置".into();
                        state.settings_open = true;
                    }
                    mresp.on_hover_text("切换模型 / API 设置");

                    // ── 权限 chip（自定义 28px，与左侧模型 chip 同高/同 chrome；默认 ComboBox 不可控高度） ──
                    let label_max_w: f32 = ["只读", "工作区写入", "完全访问"]
                        .iter()
                        .map(|s| {
                            s.chars()
                                .map(|c| if c.is_ascii() { 7.0 } else { 12.0 })
                                .sum::<f32>()
                        })
                        .fold(0.0_f32, f32::max);
                    let perm_w = (label_max_w + 40.0).max(96.0).min(160.0);
                    let (prect, presp) =
                        ui.allocate_exact_size(egui::vec2(perm_w, 28.0), egui::Sense::click());
                    let pfill = if presp.hovered() || state.perm_menu_open {
                        pal.hover
                    } else {
                        pal.field
                    };
                    ui.painter()
                        .rect_filled(prect, egui::Rounding::same(8.0), pfill);
                    ui.painter().rect(
                        prect,
                        egui::Rounding::same(8.0),
                        egui::Color32::TRANSPARENT,
                        egui::Stroke::new(1.0_f32, pal.border),
                    );
                    ui.painter().text(
                        prect.left_center() + egui::vec2(10.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        &state.permission,
                        egui::FontId::proportional(12.0),
                        pal.text,
                    );
                    // 右侧 chevron：关闭 ▼，打开 ▲（与模型 chip 关闭态一致；打开态翻转）
                    let pcx = prect.right() - 11.0;
                    let pcy = prect.center().y;
                    if state.perm_menu_open {
                        ui.painter().add(egui::Shape::convex_polygon(
                            vec![
                                egui::pos2(pcx - 3.5, pcy + 1.6),
                                egui::pos2(pcx + 3.5, pcy + 1.6),
                                egui::pos2(pcx, pcy - 1.8),
                            ],
                            pal.dim,
                            egui::Stroke::NONE,
                        ));
                    } else {
                        ui.painter().add(egui::Shape::convex_polygon(
                            vec![
                                egui::pos2(pcx - 3.5, pcy - 1.6),
                                egui::pos2(pcx + 3.5, pcy - 1.6),
                                egui::pos2(pcx, pcy + 1.8),
                            ],
                            pal.dim,
                            egui::Stroke::NONE,
                        ));
                    }
                    if presp.clicked() {
                        state.perm_menu_open = !state.perm_menu_open;
                    }
                    presp.on_hover_text("切换工具权限范围");

                    // 权限下拉弹层：向上展开（chip 靠近屏幕底部，向下会被裁），
                    // 圆角面板 + 阴影，浮于前景（与 composer 卡片同款 chrome）。
                    if state.perm_menu_open {
                        // 外部点击关闭（在渲染 Area 之前判定，避免一帧闪烁）
                        let pressed = ctx.input(|i| i.pointer.any_pressed());
                        let press_origin = ctx.input(|i| i.pointer.press_origin());
                        if pressed {
                            if let Some(pos) = press_origin {
                                let menu_rect = egui::Rect::from_min_max(
                                    egui::pos2(prect.left(), prect.top() - 96.0),
                                    egui::pos2(prect.left() + perm_w, prect.top()),
                                );
                                if !prect.contains(pos) && !menu_rect.contains(pos) {
                                    state.perm_menu_open = false;
                                }
                            }
                        }
                    }
                    if state.perm_menu_open {
                        egui::Area::new(egui::Id::new("perm_menu_area"))
                            .fixed_pos(egui::pos2(prect.left(), prect.top() - 96.0))
                            .movable(false)
                            .interactable(true)
                            .order(egui::Order::Foreground)
                            .show(ctx, |ui| {
                                egui::Frame::none()
                                    .fill(pal.panel)
                                    .rounding(egui::Rounding::same(8.0))
                                    .inner_margin(egui::Margin::same(4.0))
                                    .stroke(egui::Stroke::new(1.0_f32, pal.border))
                                    .shadow(egui::epaint::Shadow {
                                        offset: egui::vec2(0.0, 6.0),
                                        blur: 18.0,
                                        spread: 0.0,
                                        color: egui::Color32::from_black_alpha(if state.dark {
                                            0x44
                                        } else {
                                            0x14
                                        }),
                                    })
                                    .show(ui, |ui| {
                                        ui.set_min_width(perm_w);
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        for mode in ["只读", "工作区写入", "完全访问"] {
                                            let selected = state.permission == mode;
                                            let r = ui.selectable_label(
                                                selected,
                                                egui::RichText::new(mode)
                                                    .size(12.0)
                                                    .color(pal.text),
                                            );
                                            if r.clicked() {
                                                state.permission = mode.to_string();
                                                state.perm_menu_open = false;
                                                let _ = state
                                                    .host
                                                    .settings
                                                    .set("permission.mode", &state.permission);
                                            }
                                        }
                                    });
                            });
                    }

                    // ── 附件按钮：与左侧权限 chip 同高(28)/同 chrome，图标更醒目 ──
                    let (arect, aresp) =
                        ui.allocate_exact_size(egui::vec2(34.0, 28.0), egui::Sense::click());
                    let has_att = !state.attachment.is_empty();
                    let afill = if aresp.hovered() {
                        pal.hover
                    } else {
                        pal.field
                    };
                    ui.painter()
                        .rect_filled(arect, egui::Rounding::same(8.0), afill);
                    ui.painter().rect(
                        arect,
                        egui::Rounding::same(8.0),
                        egui::Color32::TRANSPARENT,
                        egui::Stroke::new(1.0_f32, if has_att { pal.accent } else { pal.border }),
                    );
                    let acolor = if has_att { pal.accent } else { pal.text };
                    draw_paperclip_icon(ui.painter(), arect.center(), acolor);
                    // 右上角「+」角标：强化「添加」语义。
                    let plus_c = egui::pos2(arect.max.x - 7.0, arect.min.y + 7.0);
                    let ps = egui::Stroke::new(1.5_f32, acolor);
                    ui.painter().line_segment(
                        [
                            egui::pos2(plus_c.x - 2.6, plus_c.y),
                            egui::pos2(plus_c.x + 2.6, plus_c.y),
                        ],
                        ps,
                    );
                    ui.painter().line_segment(
                        [
                            egui::pos2(plus_c.x, plus_c.y - 2.6),
                            egui::pos2(plus_c.x, plus_c.y + 2.6),
                        ],
                        ps,
                    );
                    let tip = if state.attachment.is_empty() {
                        "添加附件".to_string()
                    } else {
                        format!("附件: {}\n（点击重新选择）", state.attachment)
                    };
                    if aresp.on_hover_text(tip).clicked() {
                        let picked = if state.settings_page == "新建项目" {
                            rfd::FileDialog::new().pick_folder()
                        } else {
                            rfd::FileDialog::new().pick_file()
                        };
                        if let Some(path) = picked {
                            state.attachment = path.display().to_string();
                        }
                    }

                    // 若已有附件，紧随其后放一个紧凑的清除 ✕
                    if !state.attachment.is_empty() {
                        let (xrect, xresp) =
                            ui.allocate_exact_size(egui::vec2(20.0, 28.0), egui::Sense::click());
                        let xcolor = if xresp.hovered() { pal.accent } else { pal.dim };
                        ui.painter().text(
                            xrect.center(),
                            egui::Align2::CENTER_CENTER,
                            "✕",
                            egui::FontId::proportional(12.0),
                            xcolor,
                        );
                        if xresp.clicked() {
                            state.attachment.clear();
                        }
                        xresp.on_hover_text("清除附件");
                    }

                    // 弹性空间 → 圆形发送/停止按钮
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn_size = 34.0;
                        let (brect, bresp) = ui.allocate_exact_size(
                            egui::vec2(btn_size, btn_size),
                            egui::Sense::click(),
                        );
                        let center = brect.center();
                        let bfill = if state.busy {
                            egui::Color32::from_rgb(0xfb, 0xbf, 0x24)
                        } else if can_send {
                            pal.btn_fill
                        } else {
                            pal.field
                        };
                        ui.painter().circle_filled(center, btn_size / 2.0, bfill);
                        if state.busy {
                            // 停止：实心方块
                            ui.painter().rect_filled(
                                egui::Rect::from_center_size(center, egui::vec2(8.0, 8.0)),
                                egui::Rounding::same(1.2),
                                egui::Color32::from_rgb(0x1a, 0x24, 0x30),
                            );
                        } else {
                            let icon_color = if can_send { pal.btn_text } else { pal.dim };
                            // 实心三角箭头
                            ui.painter().add(egui::Shape::convex_polygon(
                                vec![
                                    egui::pos2(center.x, center.y - 4.6),
                                    egui::pos2(center.x + 4.2, center.y - 0.9),
                                    egui::pos2(center.x - 4.2, center.y - 0.9),
                                ],
                                icon_color,
                                egui::Stroke::NONE,
                            ));
                            // 箭头柄
                            ui.painter().line_segment(
                                [
                                    egui::pos2(center.x, center.y - 0.9),
                                    egui::pos2(center.x, center.y + 4.6),
                                ],
                                egui::Stroke::new(2.0_f32, icon_color),
                            );
                        }
                        if bresp.hovered() {
                            ui.painter().circle_stroke(
                                center,
                                btn_size / 2.0,
                                egui::Stroke::new(1.5_f32, pal.accent),
                            );
                        }
                        if bresp.clicked() {
                            if state.busy {
                                trace("[cancel] requested");
                                state.host.sink.cancel();
                            } else if can_send {
                                send_now = true;
                            }
                        }
                        if state.busy {
                            bresp.on_hover_text("停止生成");
                        } else if can_send {
                            bresp.on_hover_text("发送 (Enter)");
                        } else {
                            bresp.on_hover_text("输入内容后可发送");
                        }
                    });
                });
            });

            // 忙碌时每秒心跳重绘：egui 无输入事件不自动刷新，已用时计数需心跳驱动。
            if state.busy {
                ctx.request_repaint_after(std::time::Duration::from_secs(1));
            }
            ui.add_space(4.0);
            // 卡片下方的提示行与状态行（轻量 footer）
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Enter 发送 · Shift+Enter 换行")
                        .size(11.0)
                        .color(pal.dim),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let usage = state.log.usage_total();
                    let (dot, text): (egui::Color32, String) = if state.busy && state.thinking {
                        let secs = state
                            .turn_started
                            .map(|t| t.elapsed().as_secs())
                            .unwrap_or(0);
                        (
                            egui::Color32::from_rgb(0x81, 0x8d, 0xf8),
                            format!("● 模型思考中 · 已用时 {secs} 秒"),
                        )
                    } else if state.busy {
                        let secs = state
                            .turn_started
                            .map(|t| t.elapsed().as_secs())
                            .unwrap_or(0);
                        (
                            egui::Color32::from_rgb(0xfb, 0xbf, 0x24),
                            format!("● 正在处理 · 已用时 {secs} 秒，可随时停止"),
                        )
                    } else {
                        (
                            pal.accent,
                            format!(
                                "● 就绪  ·  Tokens {}/{}",
                                usage.prompt_tokens, usage.completion_tokens
                            ),
                        )
                    };
                    ui.label(egui::RichText::new(text).size(11.0).color(dot));
                });
            });
        });
    send_now
}
