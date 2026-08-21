//! Bottom message composer and model/permission controls.

use super::*;

pub(super) fn show(state: &mut AppState, ctx: &egui::Context, pal: Palette) -> bool {
    let mut send_now = false;
    // 右侧分栏已由 app 在本面板前创建，因此这里得到的就是中央剩余区域。
    let main_width = ctx.available_rect().width().max(0.0);
    let gutter = if main_width >= 1100.0 {
        24.0
    } else if main_width >= 760.0 {
        18.0
    } else {
        12.0
    };
    egui::TopBottomPanel::bottom("composer")
        .show_separator_line(false)
        .frame(
            egui::Frame::default()
                .fill(pal.bg)
                .inner_margin(egui::Margin {
                    left: gutter,
                    right: gutter,
                    top: 4.0,
                    bottom: 6.0,
                }),
        )
        .show(ctx, |ui| {
            // 忙碌时仍允许发送：控制器会把输入放入 FIFO 任务队列。
            let can_send = !state.input.trim().is_empty();
            // ── 待发送队列：挂在输入框正上方（紧挨输入卡片），有队列时显示 ──
            super::workspace::render_pending_queue(ui, state, pal);
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
                // 附件占输入框左上方独立一行，文件名与删除入口始终可见。
                if !state.attachments.is_empty() {
                    let mut remove = None;
                    ui.horizontal_wrapped(|ui| {
                        for (index, attachment) in state.attachments.iter().enumerate() {
                            let name = attachment
                                .path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("附件");
                            let response = ui.add(
                                egui::Button::new(
                                    egui::RichText::new(format!("📎 {name}  ×"))
                                        .size(11.5)
                                        .color(pal.text),
                                )
                                .fill(pal.field),
                            );
                            if response.clicked() {
                                remove = Some(index);
                            }
                            response.on_hover_text("点击删除此附件");
                        }
                    });
                    if let Some(index) = remove {
                        state.attachments.remove(index);
                    }
                    ui.add_space(6.0);
                }
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
                // 从资源管理器复制文件路径后直接粘贴：识别真实文件并按上传处理。
                // 普通文本粘贴仍保留在编辑器中，不会误变成附件。
                let pasted_paths: Vec<std::path::PathBuf> = ctx.input(|i| {
                    i.events
                        .iter()
                        .filter_map(|event| match event {
                            egui::Event::Paste(text) => Some(text),
                            _ => None,
                        })
                        .flat_map(|text| text.lines())
                        .map(|text| text.trim().trim_matches('"'))
                        .filter(|text| std::path::Path::new(text).is_file())
                        .map(std::path::PathBuf::from)
                        .collect()
                });
                if !pasted_paths.is_empty() {
                    for path in pasted_paths {
                        let rendered = path.display().to_string();
                        if state.input.trim() == rendered {
                            state.input.clear();
                        }
                        add_attachment(state, path);
                    }
                }
                // 图片剪贴板不一定会转换成 Egui 的文本 Paste 事件。检测 Ctrl+V 后
                // 直接读取系统图像剪贴板，落为临时 PNG，再与普通上传走同一附件链路。
                let image_paste = response.has_focus()
                    && ctx.input(|i| i.key_pressed(egui::Key::V) && i.modifiers.command);
                if image_paste {
                    if let Some(path) = paste_clipboard_image() {
                        add_attachment(state, path);
                    }
                }
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

                    let mode_text = if state.multi_agent {
                        "团队"
                    } else {
                        "标准"
                    };
                    // 模式 chip：与模型/权限控件共用同一套 28px chrome，
                    // 仅以状态圆点和强调色描边表达启用，避免标准 Button 的突兀块感。
                    let mode_w = 64.0_f32;
                    let (mode_rect, mode_resp) =
                        ui.allocate_exact_size(egui::vec2(mode_w, 28.0), egui::Sense::click());
                    let mode_fill = if mode_resp.hovered() {
                        pal.hover
                    } else {
                        pal.field
                    };
                    ui.painter()
                        .rect_filled(mode_rect, egui::Rounding::same(8.0), mode_fill);
                    ui.painter().rect(
                        mode_rect,
                        egui::Rounding::same(8.0),
                        egui::Color32::TRANSPARENT,
                        egui::Stroke::new(
                            1.0_f32,
                            if state.multi_agent {
                                pal.accent
                            } else {
                                pal.border
                            },
                        ),
                    );
                    ui.painter().circle_filled(
                        mode_rect.left_center() + egui::vec2(11.0, 0.0),
                        3.0,
                        if state.multi_agent {
                            pal.accent
                        } else {
                            pal.dim
                        },
                    );
                    ui.painter().text(
                        mode_rect.left_center() + egui::vec2(20.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        mode_text,
                        egui::FontId::proportional(12.0),
                        pal.text,
                    );
                    if mode_resp.clicked() {
                        state.multi_agent = !state.multi_agent;
                        let _ = state.host.settings.set(
                            "ui.multi_agent",
                            if state.multi_agent { "true" } else { "false" },
                        );
                        state.note = if state.multi_agent {
                            "已开启专家团 DAG：任务状态、证据和质量门禁会实时显示并持久化".into()
                        } else {
                            "已切换为标准模式".into()
                        };
                    }
                    mode_resp.on_hover_text(if state.multi_agent {
                        "已启用：依赖 DAG、最多 3 个并行专家、自动重试和质量门禁"
                    } else {
                        "切换到专家团：需求、风险、设计、实现、测试、审查和交付门禁"
                    });

                    // ── 模型 chip（点击 → 打开设置页） ──
                    let model_label = format!("{} · {}", state.f_provider, state.f_model);
                    let text_w: f32 = model_label
                        .chars()
                        .map(|c| if c.is_ascii() { 7.0 } else { 12.0 })
                        .sum();
                    let chip_w = (text_w + 30.0).max(92.0).min(190.0);
                    let text_limit = chip_w - 34.0;
                    let mut used_w = 0.0_f32;
                    let mut model_display = String::new();
                    for c in model_label.chars() {
                        let char_w = if c.is_ascii() { 7.0 } else { 12.0 };
                        if used_w + char_w > text_limit {
                            model_display.push('…');
                            break;
                        }
                        model_display.push(c);
                        used_w += char_w;
                    }
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
                        &model_display,
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
                        state.settings_page = "模型配置".into();
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
                    let has_att = !state.attachments.is_empty();
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
                    let tip = "添加附件";
                    if aresp.on_hover_text(tip).clicked() {
                        let picked = if state.settings_page == "新建项目" {
                            rfd::FileDialog::new().pick_folder()
                        } else {
                            rfd::FileDialog::new().pick_file()
                        };
                        if let Some(path) = picked {
                            add_attachment(state, path);
                        }
                    }

                    // 弹性空间 → 圆形发送/停止按钮
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn_size = 34.0;
                        let (brect, bresp) = ui.allocate_exact_size(
                            egui::vec2(btn_size, btn_size),
                            egui::Sense::click(),
                        );
                        let center = brect.center();
                        let bfill = if can_send {
                            pal.btn_fill
                        } else if state.busy {
                            egui::Color32::from_rgb(0xfb, 0xbf, 0x24)
                        } else {
                            pal.field
                        };
                        ui.painter().circle_filled(center, btn_size / 2.0, bfill);
                        if state.busy && !can_send {
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
                            if can_send {
                                send_now = true;
                            } else if state.busy {
                                trace("[cancel] requested");
                                state.host.sink.cancel();
                            }
                        }
                        if can_send && state.busy {
                            bresp.on_hover_text("加入任务队列 (Enter)");
                        } else if state.busy {
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
                    let queued = state.host.sink.queued_count();
                    let (dot, text): (egui::Color32, String) = if state.busy {
                        let secs = state
                            .turn_started
                            .map(|t| t.elapsed().as_secs())
                            .unwrap_or(0);
                        let quiet_secs = state
                            .last_activity
                            .map(|t| t.elapsed().as_secs())
                            .unwrap_or(0);
                        let spinner = ["◐", "◓", "◑", "◒"][(secs % 4) as usize];
                        let activity = if state.activity.is_empty() {
                            "正在启动任务"
                        } else {
                            &state.activity
                        };
                        let freshness = if quiet_secs >= 10 {
                            format!(" · 最近反馈 {quiet_secs} 秒前")
                        } else {
                            String::new()
                        };
                        (
                            if quiet_secs >= 10 {
                                egui::Color32::from_rgb(0xfb, 0xbf, 0x24)
                            } else {
                                egui::Color32::from_rgb(0x81, 0x8d, 0xf8)
                            },
                            format!(
                                "{spinner} {activity} · 已用时 {secs} 秒{freshness}{} · 可随时停止",
                                if queued > 0 {
                                    format!(" · 队列中 {queued} 条")
                                } else {
                                    String::new()
                                },
                            ),
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

fn add_attachment(state: &mut AppState, path: std::path::PathBuf) {
    if state
        .attachments
        .iter()
        .any(|attachment| attachment.path == path)
    {
        return;
    }
    let mime = match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => "image/*",
        "txt" | "md" | "csv" | "json" | "toml" | "yaml" | "yml" => "text/plain",
        "doc" | "docx" => "application/msword",
        "xls" | "xlsx" => "application/vnd.ms-excel",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .into();
    state
        .attachments
        .push(harness_core::Attachment { path, mime });
}

fn paste_clipboard_image() -> Option<std::path::PathBuf> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let image = clipboard.get_image().ok()?;
    let dir = std::env::temp_dir().join("deepseek-aidops-attachments");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("clipboard-{}.bmp", uuid_like_suffix()));
    save_rgba_as_bmp(&path, image.width as u32, image.height as u32, &image.bytes).ok()?;
    Some(path)
}

/// 无额外图像依赖地写出 32-bit BGRA BMP；用于把系统剪贴板图片转成普通附件文件。
fn save_rgba_as_bmp(
    path: &std::path::Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> std::io::Result<()> {
    let pixels = width as usize * height as usize;
    if width == 0 || height == 0 || rgba.len() < pixels * 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid clipboard image",
        ));
    }
    let image_bytes = pixels * 4;
    let file_size = 54 + image_bytes;
    let mut out = Vec::with_capacity(file_size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(54u32).to_le_bytes());
    out.extend_from_slice(&(40u32).to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(-(height as i32)).to_le_bytes()); // top-down
    out.extend_from_slice(&(1u16).to_le_bytes());
    out.extend_from_slice(&(32u16).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(image_bytes as u32).to_le_bytes());
    out.extend_from_slice(&[0; 16]);
    for pixel in rgba.chunks_exact(4).take(pixels) {
        out.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
    }
    std::fs::write(path, out)
}

fn uuid_like_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().to_string())
        .unwrap_or_else(|_| "image".into())
}
