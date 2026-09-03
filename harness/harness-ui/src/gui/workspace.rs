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
                    if state.execution_projection.is_some() {
                        let expanded = state.runtime_expanded;
                        egui::Frame::default()
                            .fill(pal.field)
                            .rounding(egui::Rounding::same(8.0))
                            .stroke(egui::Stroke::new(1.0_f32, pal.border))
                            .inner_margin(egui::Margin::symmetric(10.0, if expanded { 6.0 } else { 3.0 }))
                            .show(ui, |ui| {
                                // 始终渲染的单行摘要：阶段 + 核心计数 + 展开切换，
                                // 折叠态只占约一行高度，把垂直空间让给消息流。
                                let projection = state.execution_projection.as_ref().expect("checked above");
                                ui.horizontal_wrapped(|ui| {
                                    let chevron = if expanded { "▼" } else { "▶" };
                                    if ui
                                        .add(egui::Button::new(
                                            egui::RichText::new(chevron).size(11.0).color(pal.accent),
                                        ).frame(false).min_size(egui::vec2(14.0, 14.0)))
                                        .on_hover_text("展开/收起运行时详情")
                                        .clicked()
                                    {
                                        state.runtime_expanded = !state.runtime_expanded;
                                    }
                                    ui.label(
                                        egui::RichText::new("运行时").strong().color(pal.accent).size(12.0),
                                    );
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} · {} · 步骤 {} · 工具 {} · 证据 {}",
                                            projection.intent,
                                            projection.phase,
                                            projection.step,
                                            projection.tool_calls,
                                            projection.evidence_count,
                                        ))
                                        .size(11.5),
                                    );
                                });
                                if expanded {
                                    let projection = state.execution_projection.as_ref().expect("checked above");
                                    ui.add_space(2.0);
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "已验证 {} · 阻塞 {} · 无信息 {} · 校正 {}",
                                                projection.verified_count,
                                                projection.blocked_count,
                                                projection.no_information_count,
                                                projection.correction_count,
                                            ))
                                            .size(10.5)
                                            .color(pal.dim),
                                        );
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "允许：{}",
                                                if projection.allowed_tools.is_empty() {
                                                    "无（收尾）".into()
                                                } else {
                                                    projection.allowed_tools.join("、")
                                                }
                                            ))
                                            .size(10.5)
                                            .color(pal.dim),
                                        );
                                    });
                                    if !projection.goal.is_empty() {
                                        ui.label(
                                            egui::RichText::new(format!("目标：{}", projection.goal))
                                                .size(10.5)
                                                .color(pal.dim),
                                        );
                                    }
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "当前：{} · 假设：{}",
                                            projection.active_work_item,
                                            projection.active_hypothesis
                                        ))
                                        .size(10.5)
                                        .color(pal.dim),
                                    );
                                    if !projection.work_items.is_empty() {
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "工作项：{}",
                                                projection
                                                    .work_items
                                                    .iter()
                                                    .map(|item| {
                                                        format!(
                                                            "{}={}（证据 {}）",
                                                            item.id, item.state, item.evidence_count
                                                        )
                                                    })
                                                    .collect::<Vec<_>>()
                                                    .join(" · ")
                                            ))
                                            .size(10.5)
                                            .color(pal.dim),
                                        );
                                    }
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "下一动作：{}",
                                            projection.next_action
                                        ))
                                        .size(10.5)
                                        .color(pal.dim),
                                    );
                                    if !projection.detail.is_empty() {
                                        ui.label(
                                            egui::RichText::new(&projection.detail)
                                                .size(10.5)
                                                .color(pal.dim),
                                        );
                                    }
                                }
                            });
                        ui.add_space(4.0);
                    }
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
                                        && matches!(
                                            messages[index].kind.as_str(),
                                            "thinking" | "tool" | "plan"
                                        )
                                    {
                                        index += 1;
                                    }
                                    // live：agent 正在流式工作且该批之后还没有任何新消息，
                                    // 即当前回合的进行中批次——折叠态也需要呈现动效。
                                    let batch_live = state.busy
                                        && messages.iter().skip(index).all(|m| m.text.is_empty());
                                    render_work_batch(
                                        ui,
                                        &messages[start..index],
                                        start,
                                        max_w,
                                        pal,
                                        batch_live,
                                    );
                                    ui.add_space(4.0);
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
                                        // 所有气泡（用户/助手/错误）与工作批次卡统一固定同一宽度，
                                        // 保证最大化时左右边界完全对齐；宽度不随内容收缩。
                                        ui.set_width(max_w * 0.96);
                                        // 本轮起点：最近一条用户消息（含）；供一键复制整轮对话。
                                        let turn_start = messages[..index]
                                            .iter()
                                            .rposition(|m| m.kind == "user" && !m.text.is_empty())
                                            .unwrap_or(0);
                                        // 复制小图标绝对定位到气泡右上角：只注册交互区、不推进
                                        // 布局游标，因此不占正文首行高度。图标绘制放在正文之后，
                                        // 保证覆盖在最上层。采用紧凑小尺寸并紧贴右上角，避免遮挡
                                        // 首行长文字尾部内容。
                                        let icon_rect = egui::Rect::from_min_size(
                                            ui.max_rect().right_top() + egui::vec2(-14.0, 2.0),
                                            egui::vec2(10.0, 10.0),
                                        );
                                        let hit_rect = icon_rect.expand(2.0);
                                        let copy_resp = ui.interact(
                                            hit_rect,
                                            egui::Id::new(("copy-icon", index)),
                                            egui::Sense::click(),
                                        );
                                        // 交付状态只接受 Runtime 的 Delivery 事件；TurnEnd 或模型
                                        // 文本出现“完成”都不能推导为成功，避免未验证任务假完成。
                                        let is_final = !state.busy
                                            && msg.kind == "assistant"
                                            && messages
                                                .iter()
                                                .skip(index + 1)
                                                .all(|m| m.text.is_empty());
                                        if is_final && state.delivery.is_some() {
                                            ui.add_space(2.0);
                                            let delivery =
                                                state.delivery.as_ref().expect("checked above");
                                            let (text, color) = match delivery.outcome {
                                                harness_session::DeliveryOutcome::Verified => (
                                                    format!(
                                                        "✓ 已验证交付 · {} 项验证",
                                                        delivery.verification_count
                                                    ),
                                                    pal.accent,
                                                ),
                                                harness_session::DeliveryOutcome::NeedsUserInput => (
                                                    format!(
                                                        "? 需要你的确认 · 剩余 {} 项验收",
                                                        delivery.remaining
                                                    ),
                                                    pal.warn,
                                                ),
                                                harness_session::DeliveryOutcome::PartialDelivery => (
                                                    format!(
                                                        "◐ 部分交付 · 剩余 {} 项验收",
                                                        delivery.remaining
                                                    ),
                                                    pal.warn,
                                                ),
                                                harness_session::DeliveryOutcome::SystemFailure => (
                                                    "! 系统执行未完成 · 请查看具体原因".into(),
                                                    pal.warn,
                                                ),
                                                harness_session::DeliveryOutcome::Blocked => (
                                                    "! 旧会话的阻塞状态 · 请查看具体原因".into(),
                                                    pal.warn,
                                                ),
                                                harness_session::DeliveryOutcome::Interrupted => {
                                                    ("! 任务中断 · 未验证交付".into(), pal.warn)
                                                }
                                                harness_session::DeliveryOutcome::Cancelled => {
                                                    ("◌ 已取消 · 未验证交付".into(), pal.dim)
                                                }
                                            };
                                            ui.label(
                                                egui::RichText::new(text)
                                                    .size(10.5)
                                                    .strong()
                                                    .color(color),
                                            );
                                            if let Some(reason) = &delivery.reason {
                                                // Runtime 的最终 assistant 文本通常已经是同一条
                                                // 简明状态；避免在交付徽标下原样重复一遍。
                                                if reason.trim() != msg.text.trim() {
                                                    ui.label(
                                                        egui::RichText::new(reason)
                                                            .size(10.0)
                                                            .color(pal.dim),
                                                    );
                                                }
                                            }
                                        }
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
                                            let job = crate::markdown::to_job(
                                                &msg.text, &theme, markdown_w,
                                            );
                                            // 正文响应独立持有：右键菜单始终挂在 Markdown 正文上
                                            // （此前挂在最后一个文件路径按钮上，右键正文不弹菜单）。
                                            let md_resp =
                                                ui.add(egui::Label::new(job).selectable(true));
                                            // 文件路径识别：正文用完整 markdown 渲染，
                                            // 识别出的文件路径作为可点击 chip 追加在正文下方，
                                            // 不打断正文排版。
                                            let mut file_paths: Vec<String> = Vec::new();
                                            for b in crate::markdown::parse_blocks(
                                                &msg.text, &theme, markdown_w,
                                            ) {
                                                if let crate::markdown::MarkdownBlock::FilePath(p) =
                                                    b
                                                {
                                                    if !file_paths.contains(&p) {
                                                        file_paths.push(p);
                                                    }
                                                }
                                            }
                                            if !file_paths.is_empty() {
                                                ui.add_space(6.0);
                                                ui.label(
                                                    egui::RichText::new("文件：")
                                                        .size(10.5)
                                                        .color(pal.dim),
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
                                                        ui.ctx().set_cursor_icon(
                                                            egui::CursorIcon::PointingHand,
                                                        );
                                                    }
                                                    let rb = rb.on_hover_text("点击预览此文件");
                                                    if rb.clicked() {
                                                        state.pending_preview = Some(path);
                                                    }
                                                }
                                            }
                                            md_resp
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
                                        // 绘制悬浮在右上角的复制图标（矢量双矩形，不依赖字体字形、
                                        // 不会变豆腐块），并处理 hover 光标与点击复制；图标绘制
                                        // 在正文之后，会覆盖在最上层，且不参与正常布局流。
                                        let icon_color = if copy_resp.hovered() {
                                            ui.ctx()
                                                .set_cursor_icon(egui::CursorIcon::PointingHand);
                                            pal.text
                                        } else {
                                            pal.dim
                                        };
                                        // 不透明底座：气泡填充色垫底 + 浅描边，把穿过按钮
                                        // 区域的首行文字遮在下面，hover 时描边加重强调。
                                        ui.painter().rect(
                                            hit_rect,
                                            egui::Rounding::same(6.0),
                                            fill,
                                            egui::Stroke::new(
                                                if copy_resp.hovered() { 1.4_f32 } else { 1.0_f32 },
                                                if copy_resp.hovered() {
                                                    pal.dim
                                                } else {
                                                    pal.border
                                                },
                                            ),
                                        );
                                        super::icons::draw_copy_icon(
                                            ui.painter(),
                                            icon_rect.center(),
                                            icon_color,
                                            fill,
                                        );
                                        if copy_resp.clicked() {
                                            state.pending_copy = Some(msg.text.clone());
                                        }
                                        let _ = copy_resp.on_hover_text("复制本条内容");
                                        // 右键菜单：支持按选中行/片段复制，以及整条或整轮复制。
                                        // 修复说明：旧实现推送 egui::Event::Copy 模拟 Ctrl+C，
                                        // 但右键菜单交互会在 Label 处理事件前清空 selection，
                                        // 导致“复制选中内容”变成复制空串：高亮丢失且剪贴板
                                        // 没有任何内容。Event::Copy 只是输入事件，不保证目标
                                        // Label 仍有选区。改为点击菜单时直接复制当前这条消息
                                        // 的完整文本，不再依赖易失的 selection 状态。
                                        resp.context_menu(|ui| {
                                            if ui.button("📋 复制选中内容").clicked() {
                                                state.pending_copy = Some(msg.text.clone());
                                                ui.ctx().copy_text(msg.text.clone());
                                                ui.close_menu();
                                            }
                                            if ui.button("📋 复制本条全部内容").clicked() {
                                                state.pending_copy = Some(msg.text.clone());
                                                ui.ctx().copy_text(msg.text.clone());
                                                ui.close_menu();
                                            }
                                            if ui
                                                .button("📋 复制整轮对话（含过程与回复）")
                                                .clicked()
                                            {
                                                let t = format_turn_text(&messages, turn_start);
                                                state.pending_copy = Some(t.clone());
                                                ui.ctx().copy_text(t);
                                                ui.close_menu();
                                            }
                                        });
                                    });
                                });
                                ui.add_space(4.0);
                                index += 1;
                            }
                            // 标准模式只展示常规对话。保留专家团状态供用户切回团队模式
                            // 查看证据，但不让旧的失败/完成卡片继续占据主会话并制造“仍在运行”的误解。
                            if state.multi_agent {
                                for council in state.councils.values() {
                                    render_council_card(ui, council, max_w, pal);
                                    ui.add_space(4.0);
                                }
                            }
                            // 运行状态不再单独占一条计时气泡：进行中批次的「工作过程」
                            // 折叠头自带旋转动效与实时流式文字，信息就地呈现。
                        });
                    // 延迟剪贴板写入：渲染闭包内点击复制时先暂存，帧末统一写入。
                    if let Some(text) = state.pending_copy.take() {
                        ctx.copy_text(text);
                    }
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

/// 解析计划消息为待办列表。`!` 为模型自报完成、`×` 为显式阻塞；二者都不等于验收。
/// 返回 (标记符号, 待办文本)。
fn parse_plan_checklist(text: &str) -> Vec<(char, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let mut chars = line.chars();
        if !chars.next().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let rest = line
            .strip_prefix(|c: char| c.is_ascii_digit())
            .and_then(|r| r.strip_prefix(['.', '、', '）', ')']))
            .map(str::trim_start);
        let Some(rest) = rest else { continue };
        let mut cs = rest.chars();
        if let Some(mark) = cs.next() {
            if matches!(mark, '✓' | '!' | '×' | '…' | '·' | '-' | 'x' | 'X') {
                let item: String = cs.collect::<String>().trim().to_string();
                if !item.is_empty() {
                    out.push((mark, item));
                }
            }
        }
    }
    out
}

/// 一次 agent 回合内连续的思考、调用和返回共用一个折叠容器；默认只占一行。
/// 计划待办（重要内容）常显在容器顶部；思考/工具中间过程收拢进折叠区。
/// `start_index` 是 append-only 消息序号，可作为 egui 折叠状态的稳定 id。
/// `live` 表示该批次正被 agent 流式填充：折叠态也展示旋转动效与已用时，
/// 让用户不展开也能感知 agent 正在工作。
fn render_work_batch(
    ui: &mut egui::Ui,
    messages: &[ChatMsg],
    start_index: usize,
    max_w: f32,
    pal: Palette,
    live: bool,
) {
    let tool_count = messages.iter().filter(|msg| msg.kind == "tool").count();
    let thinking_count = messages.iter().filter(|msg| msg.kind == "thinking").count();
    // 计划待办常显：跨 plan 消息合并，按 ✓/…/· 区分完成、进行中、待办。
    let plan_items: Vec<(char, String)> = messages
        .iter()
        .filter(|msg| msg.kind == "plan")
        .flat_map(|msg| parse_plan_checklist(&msg.text))
        .collect();
    let (plan_done, plan_total) = plan_items
        .iter()
        .fold((0usize, 0usize), |(d, t), (mark, _)| {
            (d + usize::from(*mark == '✓'), t + 1)
        });
    let mut parts: Vec<String> = Vec::new();
    if thinking_count > 0 {
        parts.push(format!("思考 {thinking_count} 条"));
    }
    if tool_count > 0 {
        parts.push(format!("工具 {tool_count} 次"));
    }
    let summary = if parts.is_empty() {
        format!("{} 条过程", messages.len())
    } else {
        parts.join(" · ")
    };
    egui::Frame::default()
        .fill(pal.field)
        .rounding(egui::Rounding::same(8.0))
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .show(ui, |ui| {
            // 思考/工具过程内容很短（单行摘要），用 set_max_width 只限制上限、
            // 让卡片按内容自然宽度收缩，避免撑满整行留下大片空白。
            ui.set_max_width(max_w * 0.96);
            if !plan_items.is_empty() {
                ui.label(
                    // 计划进度来自模型的 PlanUpdate，只能说明“模型声称的执行进度”；
                    // 真正交付由上方 Delivery 状态在验证后单独标识。
                    egui::RichText::new(format!(
                        "📋 执行计划（待验收）· {plan_done}/{plan_total} 自报完成"
                    ))
                    .size(11.0)
                    .strong()
                    .color(if plan_done == plan_total {
                        pal.accent
                    } else {
                        pal.text
                    }),
                );
                ui.add_space(3.0);
                for (mark, item) in &plan_items {
                    let (sym, color) = match mark {
                        '✓' => ("✓", pal.accent),
                        '!' => ("!", pal.warn),
                        '×' | 'x' | 'X' => ("×", pal.err_text),
                        '…' => ("…", pal.warn),
                        _ => ("·", pal.dim),
                    };
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(sym).size(12.5).strong().color(color));
                        // egui 0.30 的 strikethrough() 不再接受 bool 参数，改为条件应用。
                        let mut text = egui::RichText::new(item)
                            .size(12.0)
                            .color(if *mark == '✓' { pal.dim } else { pal.text });
                        if *mark == '✓' {
                            text = text.strikethrough();
                        }
                        ui.add(egui::Label::new(text).selectable(true));
                    });
                }
                ui.add_space(4.0);
                ui.separator();
            }
            // ── 思考/工具中间过程：默认折叠，只占一行 ──
            // 进行中批次：把静态 ◌ 换成旋转字符帧 + 已用时，折叠态也能看到
            // SSE 流式动效（与底部状态行/composer 用同一套 ◐◓◑◒ 帧序列）。
            let header = if live {
                let secs = ui.input(|i| i.time);
                let glyph = ["◐", "◓", "◑", "◒"][((secs as u64) % 4) as usize];
                // 按 500ms 推进重绘，驱动旋转帧与流式光标的闪烁。
                let phase_ms = ((secs * 2.0) % 1.0 * 1000.0) as u64;
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(
                        500 - phase_ms.min(499),
                    ));
                egui::RichText::new(format!("{glyph} 工作过程 · {summary} · 进行中"))
                    .size(11.5)
                    .color(pal.accent)
            } else {
                egui::RichText::new(format!("◌ 工作过程 · {summary}"))
                    .size(11.5)
                    .color(pal.dim)
            };
            let collapsing = egui::CollapsingHeader::new(header)
                .id_salt(("work-batch", start_index))
                .default_open(false)
                .show(ui, |ui| {
                    ui.add_space(4.0);
                    for (position, msg) in messages.iter().enumerate() {
                        if msg.kind == "plan" {
                            continue; // 计划已在折叠区外常显
                        }
                        if position > 0 {
                            ui.separator();
                        }
                        let label = match msg.kind.as_str() {
                            "thinking" => "思考",
                            "tool" => "工具",
                            _ => "过程",
                        };
                        // 收拢：每条过程只显示一行摘要，避免思考/工具内容占满屏幕。
                        // 完整内容仍可通过右键「复制全部内容」获取。
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(label).size(10.5).color(pal.dim));
                            let summary = one_line_summary(
                                &msg.text,
                                if msg.kind == "tool" { 90 } else { 60 },
                            );
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
            // 折叠头右键：一键复制整个工作过程卡（思考/工具/计划原文）。
            collapsing.header_response.context_menu(|ui| {
                if ui.button("📋 复制本批过程内容").clicked() {
                    let text = messages
                        .iter()
                        .filter(|m| !m.text.is_empty())
                        .map(|m| {
                            let role = match m.kind.as_str() {
                                "thinking" => "【思考】",
                                "tool" => "【工具】",
                                "plan" => "【计划】",
                                _ => "【过程】",
                            };
                            format!("{role}\n{}", m.text.trim_end())
                        })
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    ui.ctx().copy_text(text);
                    ui.close_menu();
                }
            });
            // 收起态的 SSE 流式文字：取批次内最后一条正在填充的消息尾部，
            // 单行预览 + 闪烁光标，让用户不展开也能看到 agent 实时在写什么。
            // body_returned 为 Some 表示折叠体已渲染（即展开态）。
            if live && collapsing.body_returned.is_none() {
                if let Some(last) = messages
                    .iter()
                    .rev()
                    .find(|m| m.kind != "plan" && !m.text.is_empty())
                {
                    let secs = ui.input(|i| i.time);
                    let cursor = if ((secs * 2.0) as u64) % 2 == 0 {
                        "▌"
                    } else {
                        " "
                    };
                    let preview = one_line_summary(&last.text, 80);
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(cursor)
                                .monospace()
                                .size(11.0)
                                .strong()
                                .color(pal.accent),
                        );
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(&preview)
                                    .monospace()
                                    .size(11.0)
                                    .color(pal.text),
                            )
                            .truncate(),
                        );
                    });
                }
            }
        });
}

/// 把整轮对话拼装为可直接复制的文本：从 `turn_start`（最近一条用户消息）起，
/// 依次收录用户提问、思考/工具过程与最终回复，多个气泡之间用角色标记与空行分隔。
fn format_turn_text(messages: &[ChatMsg], turn_start: usize) -> String {
    let mut out = String::new();
    for msg in &messages[turn_start..] {
        if msg.text.is_empty() {
            continue;
        }
        let role = match msg.kind.as_str() {
            "user" => "【用户】",
            "assistant" => "【助手回复】",
            "thinking" => "【思考】",
            "tool" => "【工具】",
            "plan" => "【计划】",
            "error" => "【错误】",
            _ => "【过程】",
        };
        out.push_str(&format!("{role}\n{}\n\n", msg.text.trim_end()));
    }
    out.trim_end().to_string()
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
