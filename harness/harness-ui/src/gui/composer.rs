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
            // 图片粘贴与文件上传都可作为独立消息发送，无需额外输入文字。
            let can_send = !state.input.trim().is_empty() || !state.attachments.is_empty();
            // ── 待发送队列：挂在输入框正上方（紧挨输入卡片），有队列时显示 ──
            super::workspace::render_pending_queue(ui, state, pal);
            // ── 输入卡片：圆角 + 细边框 + 阴影浮起，卡片自身提供 chrome ──
            let card_frame = egui::Frame::default()
                .fill(pal.panel)
                .rounding(egui::Rounding::same(12.0))
                .stroke(egui::Stroke::new(1.0_f32, pal.border))
                .inner_margin(egui::Margin {
                    left: 12.0,
                    right: 8.0,
                    top: 8.0,
                    bottom: 6.0,
                })
                .shadow(egui::epaint::Shadow {
                    offset: egui::vec2(0.0, 4.0),
                    blur: 14.0,
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
                    ui.add_space(4.0);
                }
                // 文本编辑区：去掉自身边框/背景，由卡片提供 chrome。
                // TextEdit 本身没有最大高度约束，需由 ScrollArea 提供固定上限。
                // 关闭滚动到光标时的补间动画，防止长文本输入时卡片位置逐帧来回变化。
                const COMPOSER_MAX_H: f32 = 96.0;
                let response = egui::ScrollArea::vertical()
                    .id_salt("composer-input-scroll")
                    .max_height(COMPOSER_MAX_H)
                    // 内容超过上限后固定滚动区尺寸，避免滚动条出现/消失或文本换行重算
                    // 反复改变底部面板高度，导致输入窗口在长文本输入时抖动。
                    .auto_shrink([false, false])
                    .animated(false)
                    .show(ui, |ui| {
                        ui.add_enabled(
                            !state.optimizing,
                            egui::TextEdit::multiline(&mut state.input)
                                .desired_width(f32::INFINITY)
                                .desired_rows(2)
                                .font(egui::FontId::proportional(13.5))
                                .frame(false)
                                .margin(egui::Margin::same(0.0))
                                .hint_text(
                                    egui::RichText::new(if state.optimizing {
                                        "正在优化输入…（加载中，请稍候）"
                                    } else {
                                        "描述任务、粘贴代码或提出问题…"
                                    })
                                    .color(pal.dim),
                                ),
                        )
                    })
                    .inner;
                // Enter 发送 / Shift+Enter 换行：egui 会先插入换行，这里去掉尾随 \n 再提交。
                let enter = response.has_focus()
                    && ctx.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                // Egui 只会把部分剪贴板内容转换为文本 Paste 事件。这里保留文本路径
                // 的兼容逻辑，同时在 Windows 上读取资源管理器复制文件使用的 CF_HDROP。
                // 普通文本粘贴仍保留在编辑器中，不会误变成附件。
                let (pasted_text_paths, clipboard_paste_event, command_v) = ctx.input(|i| {
                    let pasted_text_paths = i
                        .events
                        .iter()
                        .filter_map(|event| match event {
                            egui::Event::Paste(text) => Some(text),
                            _ => None,
                        })
                        .flat_map(|text| text.lines())
                        .map(|text| text.trim().trim_matches('"'))
                        .filter(|text| std::path::Path::new(text).is_file())
                        .map(std::path::PathBuf::from)
                        .collect();
                    let clipboard_paste_event = i
                        .events
                        .iter()
                        .any(|event| matches!(event, egui::Event::Paste(_)));
                    // 不能使用 InputState::modifiers：它是这一帧结束时的状态。若 Ctrl 与 V
                    // 在同一批原生事件中都已释放，该状态会是 false，导致截图/CF_HDROP 粘贴
                    // 被漏掉。Key 事件自带按下那一刻的修饰键状态，才是可靠的判断依据。
                    let command_v = i.events.iter().any(is_paste_shortcut);
                    (pasted_text_paths, clipboard_paste_event, command_v)
                });
                // 在原生窗口中 Ctrl+V 常会被 eframe 直接翻译为 Paste 事件，未必仍保留
                // Key::V 按键事件；图片没有文本载荷时尤其如此。因此两种事件都必须触发
                // 系统剪贴板读取，才能把截图/复制的图片转换成附件。
                // 在 Windows 原生后端中，eframe 在识别 Ctrl+V 后会先尝试读取“文本”
                // 剪贴板；资源管理器复制的文件没有文本格式时，它既不生成 Paste 事件、也不
                // 把 V 键事件交给 egui。因此只看 egui events 永远不会进入 CF_HDROP 分支。
                // 使用 GetAsyncKeyState 作为 Windows 上的兜底，直接在按键仍处于按下状态的
                // 这一帧读取文件剪贴板；普通文本仍由 TextEdit 的默认粘贴路径处理。
                let native_paste = response.has_focus() && native_paste_shortcut_pressed();
                let should_read_clipboard =
                    response.has_focus() && (command_v || clipboard_paste_event || native_paste);
                // 粘贴事件是全局输入；仅当编辑器拥有焦点时才把路径变成附件，避免用户在
                // 设置表单等其他输入控件粘贴路径时误把文件加入当前消息。
                let mut pasted_paths = if response.has_focus() {
                    pasted_text_paths
                } else {
                    Vec::new()
                };
                if should_read_clipboard {
                    pasted_paths.extend(paste_clipboard_files());
                }
                pasted_paths.retain(|path| path.is_file());
                pasted_paths.sort();
                pasted_paths.dedup();
                if !pasted_paths.is_empty() {
                    clear_input_if_it_only_contains_paths(&mut state.input, &pasted_paths);
                    for path in pasted_paths {
                        add_attachment(state, path);
                    }
                // 图片剪贴板不一定会转换成 Egui 的文本 Paste 事件。仅当剪贴板中
                // 没有文件时才将图片落为临时附件，避免一次粘贴附上无关内容。
                } else if should_read_clipboard {
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
                    let mfill = if mresp.hovered() || state.model_menu_open {
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
                        // 直接下拉切换模型，不再跳转设置页；与权限菜单互斥。
                        state.perm_menu_open = false;
                        state.model_menu_open = !state.model_menu_open;
                    }
                    mresp.clone().on_hover_text("切换模型（下拉直接选择）");

                    // 下拉菜单宽度固定，避免模型名/滚动条让弹层横向反复调整。
                    let menu_w = chip_w.max(210.0).min(260.0);

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
                        state.model_menu_open = false;
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
                        // 直接锚定在下拉触点（权限 chip）的左下角：菜单底边紧贴 chip 顶边、
                        // 左对齐 chip 左缘，向上展开。不再依赖固定高度估算，彻底消除
                        // 「弹层离触点太远」的缝隙。
                        let screen = ctx.screen_rect();
                        egui::Area::new(egui::Id::new("perm_menu_area"))
                            .anchor(
                                egui::Align2::LEFT_BOTTOM,
                                egui::vec2(
                                    prect.left() - screen.left(),
                                    prect.top() - screen.bottom(),
                                ),
                            )
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
                                                // 展示值必须与运行时 AccessPolicy 立即同步。
                                                // 旧实现要等下一次发送消息才生效，任务运行中
                                                // 切换后会出现 UI 与实际权限短暂不一致。
                                                state
                                                    .host
                                                    .sink
                                                    .set_permission(state.permission.clone());
                                                let _ = state
                                                    .host
                                                    .settings
                                                    .set("permission.mode", &state.permission);
                                            }
                                        }
                                    });
                            });
                    }

                    // 模型下拉弹层：向上展开（与权限菜单同 chrome），
                    // 列出「保存的配置」+「上游模型」+「管理配置」入口，点击直接切换。
                    if state.model_menu_open {
                        // `Area::anchor` 会先使用上一帧的尺寸定位、再按本帧的滚动区尺寸重算；
                        // 菜单在高度阈值附近时两套尺寸会互相来回纠正。固定菜单底边到 chip 顶边，
                        // 并关闭 Area/ScrollArea 动画，使内容高度变化也不会带动输入区抖动。
                        let menu_response = egui::Area::new(egui::Id::new("model_menu_area"))
                            .fixed_pos(mrect.left_top())
                            .pivot(egui::Align2::LEFT_BOTTOM)
                            .default_width(menu_w)
                            .movable(false)
                            .interactable(true)
                            .order(egui::Order::Foreground)
                            .fade_in(false)
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
                                        ui.set_min_width(menu_w);
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        // 列表区限高 300px + 内部滚动：条目较多时弹层会向上扩张，
                                        // 若无上限会超出外部点击判定区域（甚至推出屏幕顶部），
                                        // 导致顶部条目点不中（"新配置的模型选择不到"）。
                                        egui::ScrollArea::vertical()
                                            .id_salt("model-menu-list")
                                            .max_height(300.0)
                                            .auto_shrink([false, true])
                                            .animated(false)
                                            .show(ui, |ui| {
                                        // 保存的配置分组：直接复用整套连接信息并立即生效。
                                        // 仅列出已启用的条目（停用配置在「系统管理 · 模型配置」中管理）。
                                        let profiles: Vec<_> = state
                                            .host
                                            .settings
                                            .model_profiles()
                                            .into_iter()
                                            .filter(|p| p.enabled)
                                            .collect();
                                        if !profiles.is_empty() {
                                            ui.add_space(2.0);
                                            ui.label(
                                                egui::RichText::new("保存的配置")
                                                    .size(10.5)
                                                    .color(pal.dim),
                                            );
                                            for profile in profiles {
                                                let selected = profile.model == state.f_model
                                                    && profile.provider == state.f_provider;
                                                let name = profile.name.clone();
                                                let resp = ui.selectable_label(
                                                    selected,
                                                    egui::RichText::new(&name)
                                                        .size(12.0)
                                                        .color(pal.text),
                                                );
                                                if resp.clicked() {
                                                    state.load_profile(&name);
                                                    state.model_menu_open = false;
                                                }
                                            }
                                        }
                                        // 上游模型分组：该模型已存为配置则复用其连接信息，
                                        // 否则仅切换模型名（提交时按 f_model 直接配置）。
                                        if !state.f_models.is_empty() {
                                            ui.add_space(2.0);
                                            ui.label(
                                                egui::RichText::new("上游模型")
                                                    .size(10.5)
                                                    .color(pal.dim),
                                            );
                                            let models = state.f_models.clone();
                                            for model in models {
                                                let selected = model == state.f_model;
                                                let resp = ui.selectable_label(
                                                    selected,
                                                    egui::RichText::new(&model)
                                                        .size(12.0)
                                                        .color(pal.text),
                                                );
                                                if resp.clicked() {
                                                    // 优先匹配启用的配置；均停用时回退任意同名条目。
                                                    let all = state.host.settings.model_profiles();
                                                    let matched = all
                                                        .iter()
                                                        .find(|p| p.enabled && p.model == model)
                                                        .or_else(|| {
                                                            all.iter().find(|p| p.model == model)
                                                        })
                                                        .cloned();
                                                    match matched {
                                                        Some(p) => {
                                                            let pname = p.name.clone();
                                                            state.load_profile(&pname);
                                                        }
                                                        None => {
                                                            state.f_model = model.clone();
                                                            let _ = state
                                                                .host
                                                                .settings
                                                                .set("llm.model", &state.f_model);
                                                        }
                                                    }
                                                    state.model_menu_open = false;
                                                }
                                            }
                                        }
                                        }); // ── ScrollArea 列表区结束 ──
                                        ui.add_space(2.0);
                                        ui.separator();
                                        let manage = ui.selectable_label(
                                            false,
                                            egui::RichText::new("⚙ 管理模型配置")
                                                .size(12.0)
                                                .color(pal.accent),
                                        );
                                        if manage.clicked() {
                                            state.settings_open = true;
                                            state.settings_page = "模型配置".into();
                                            state.model_menu_open = false;
                                        }
                                    });
                            })
                            .response;

                        // 使用 Area 的真实响应判定外部点击；不再依赖会与实际滚动高度不一致的
                        // 预估矩形，点击菜单右侧滚动条或底部入口也不会错误关闭菜单。
                        let clicked_outside =
                            mresp.clicked_elsewhere() && menu_response.clicked_elsewhere();
                        let escape = ctx.input(|i| i.key_pressed(egui::Key::Escape));
                        if clicked_outside || escape {
                            state.model_menu_open = false;
                        }
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

                    // ── 优化按钮：在发送按钮左侧，点击后异步调用 LLM 重写输入 ──
                    let can_optimize = !state.input.trim().is_empty() && !state.optimizing;
                    let (orect, oresp) =
                        ui.allocate_exact_size(egui::vec2(34.0, 28.0), egui::Sense::click());
                    let ofill = if oresp.hovered() {
                        pal.hover
                    } else {
                        pal.field
                    };
                    ui.painter()
                        .rect_filled(orect, egui::Rounding::same(8.0), ofill);
                    ui.painter().rect(
                        orect,
                        egui::Rounding::same(8.0),
                        egui::Color32::TRANSPARENT,
                        egui::Stroke::new(
                            1.0_f32,
                            if state.optimizing {
                                pal.accent
                            } else {
                                pal.border
                            },
                        ),
                    );
                    // 魔法棒图标 ✨
                    let ocolor = if state.optimizing {
                        pal.accent
                    } else {
                        pal.text
                    };
                    ui.painter().text(
                        orect.center(),
                        egui::Align2::CENTER_CENTER,
                        "✨",
                        egui::FontId::proportional(14.0),
                        ocolor,
                    );
                    if oresp.clicked() && can_optimize {
                        state.optimize_input();
                    }
                    let tip = if state.optimizing {
                        "正在优化…"
                    } else if can_optimize {
                        "优化输入（用 LLM 重写为更友好的提示词）"
                    } else {
                        "输入内容后可优化"
                    };
                    oresp.on_hover_text(tip);

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
            // 优化输入是异步后台线程 + 非阻塞轮询：若无输入事件 egui 不会重绘，
            // poll_optimize 就永远不会执行，结果无法回填到输入框。因此优化期间也要心跳重绘。
            if state.optimizing {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
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

/// 判断原生按键事件是否代表系统粘贴快捷键。
///
/// 必须使用事件携带的 modifiers，而不能读取 `InputState::modifiers`；后者可能已经被
/// 同一帧稍后的 Ctrl/V 释放事件更新，从而漏掉截图和资源管理器文件的粘贴。
fn is_paste_shortcut(event: &egui::Event) -> bool {
    matches!(
        event,
        egui::Event::Key {
            key: egui::Key::V,
            pressed: true,
            modifiers,
            ..
        } if modifiers.command || modifiers.ctrl
    )
}

/// Windows 的 eframe 后端会在 Ctrl+V 时吞掉没有文本载荷的按键事件。
///
/// 文件从资源管理器复制后只提供 `CF_HDROP`，所以通过当前按键状态补回这个丢失的触发
/// 信号。其它平台仍沿用 egui 的原生 Paste/Key 事件。
#[cfg(windows)]
fn native_paste_shortcut_pressed() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

    const VK_CONTROL: i32 = 0x11;
    const VK_V: i32 = 0x56;
    unsafe {
        let key_is_down = |key| (GetAsyncKeyState(key) as u16 & 0x8000) != 0;
        key_is_down(VK_CONTROL) && key_is_down(VK_V)
    }
}

#[cfg(not(windows))]
fn native_paste_shortcut_pressed() -> bool {
    false
}

pub(super) fn add_attachment(state: &mut AppState, path: std::path::PathBuf) {
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
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "ico" => "image/*",
        "txt" | "md" | "csv" | "log" => "text/plain",
        "xml" => "application/xml",
        "json" => "application/json",
        "toml" | "yaml" | "yml" => "text/plain",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .into();
    state
        .attachments
        .push(harness_core::Attachment { path, mime });
}

/// 仅当输入框内容完全是本次粘贴的文件路径时清除它，避免污染待发送的消息正文。
fn clear_input_if_it_only_contains_paths(input: &mut String, paths: &[std::path::PathBuf]) {
    let pasted = input
        .lines()
        .map(|line| line.trim().trim_matches('"'))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if !pasted.is_empty()
        && pasted.iter().all(|line| {
            paths
                .iter()
                .any(|path| path.as_os_str().to_string_lossy() == *line)
        })
    {
        input.clear();
    }
}

/// 资源管理器复制文件时将路径写入 Windows 的 `CF_HDROP`，而非文本剪贴板。
/// Egui/winit 不会把该格式转为 `Event::Paste`，因此需要在 Ctrl+V 时直接读取。
#[cfg(windows)]
fn paste_clipboard_files() -> Vec<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{
        System::{
            DataExchange::{
                CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
            },
            Ole::CF_HDROP,
        },
        UI::Shell::DragQueryFileW,
    };

    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP as u32) == 0 || OpenClipboard(0) == 0 {
            return Vec::new();
        }
        struct ClipboardGuard;
        impl Drop for ClipboardGuard {
            fn drop(&mut self) {
                unsafe {
                    CloseClipboard();
                }
            }
        }
        let _clipboard = ClipboardGuard;
        let hdrop = GetClipboardData(CF_HDROP as u32);
        if hdrop == 0 {
            return Vec::new();
        }
        let count = DragQueryFileW(hdrop, u32::MAX, std::ptr::null_mut(), 0);
        (0..count)
            .filter_map(|index| {
                let len = DragQueryFileW(hdrop, index, std::ptr::null_mut(), 0);
                if len == 0 {
                    return None;
                }
                let mut name = vec![0_u16; len as usize + 1];
                let copied = DragQueryFileW(hdrop, index, name.as_mut_ptr(), name.len() as u32);
                (copied > 0).then(|| {
                    std::path::PathBuf::from(std::ffi::OsString::from_wide(
                        &name[..copied as usize],
                    ))
                })
            })
            .collect()
    }
}

#[cfg(not(windows))]
fn paste_clipboard_files() -> Vec<std::path::PathBuf> {
    Vec::new()
}

fn paste_clipboard_image() -> Option<std::path::PathBuf> {
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let image = clipboard.get_image().ok()?;
    let dir = std::env::temp_dir().join("deepseek-aidops-attachments");
    std::fs::create_dir_all(&dir).ok()?;
    // 视觉 API 普遍支持 PNG/JPEG/WebP，但不保证接受 BMP。将系统剪贴板的 RGBA
    // 统一编码为 PNG，确保粘贴截图可直接进入多模态请求。
    let path = dir.join(format!("clipboard-{}.png", uuid_like_suffix()));
    save_rgba_as_png(&path, image.width as u32, image.height as u32, &image.bytes).ok()?;
    Some(path)
}

fn save_rgba_as_png(
    path: &std::path::Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> std::io::Result<()> {
    if width == 0 || height == 0 || rgba.len() != width as usize * height as usize * 4 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid clipboard image",
        ));
    }
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
    writer.write_image_data(rgba).map_err(std::io::Error::other)
}

fn uuid_like_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos().to_string())
        .unwrap_or_else(|_| "image".into())
}

#[cfg(test)]
mod tests {
    use super::{
        clear_input_if_it_only_contains_paths, is_paste_shortcut, save_rgba_as_png,
    };
    use std::path::PathBuf;

    #[test]
    fn clears_only_a_path_only_paste() {
        let path = PathBuf::from(r"C:\\work\\report.pdf");
        let mut input = format!("\"{}\"\n", path.display());

        clear_input_if_it_only_contains_paths(&mut input, std::slice::from_ref(&path));

        assert!(input.is_empty());
    }

    #[test]
    fn retains_message_text_when_it_includes_a_path() {
        let path = PathBuf::from(r"C:\\work\\report.pdf");
        let mut input = format!("请分析这个文件：{}", path.display());

        clear_input_if_it_only_contains_paths(&mut input, std::slice::from_ref(&path));

        assert_eq!(input, format!("请分析这个文件：{}", path.display()));
    }

    #[test]
    fn detects_paste_from_the_key_event_modifiers() {
        let event = egui::Event::Key {
            key: egui::Key::V,
            physical_key: Some(egui::Key::V),
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                ctrl: true,
                ..Default::default()
            },
        };

        assert!(is_paste_shortcut(&event));
    }

    #[test]
    fn clipboard_rgba_is_encoded_as_png() {
        let path = std::env::temp_dir().join(format!(
            "harness-clipboard-image-{}.png",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        save_rgba_as_png(&path, 1, 1, &[0x11, 0x22, 0x33, 0xff]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let _ = std::fs::remove_file(path);
    }
}
