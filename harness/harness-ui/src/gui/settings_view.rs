//! Settings modal layout and page routing.

use super::*;

pub(super) fn show(state: &mut AppState, ctx: &egui::Context, pal: Palette) {
    // ── 设置弹层 ─────────────────────────────────────────────
    // ── 设置模态：全屏半透明遮罩 + 居中圆角面板（替代默认 Window 标题栏样式）──
    if state.settings_open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        state.settings_open = false;
    }
    if !state.settings_open {
        // 关闭后清空面板矩形，避免残留矩形影响后续遮罩误触防护。
        state.modal_panel_rect = None;
    }
    if state.settings_open {
        let page = state.settings_page.clone();
        let system_page = matches!(
            page.as_str(),
            "模型配置" | "模型设置" | "技能管理" | "记忆系统" | "记忆" | "参数配置"
                | "系统配置" | "系统更新" | "更新"
        );
        let display_title = if system_page { "系统管理" } else { page.as_str() };
        let screen = ctx.screen_rect();
        // 系统管理采用紧凑、稳定的双栏尺寸；内容多少不再改变弹窗大小。
        let panel_w = if system_page {
            (screen.width() - 180.0).clamp(700.0, 860.0)
        } else {
            (screen.width() - 320.0).clamp(520.0, 660.0)
        };
        let panel_h = if system_page {
            (screen.height() - 100.0).clamp(520.0, 680.0)
        } else {
            (screen.height() - 56.0).clamp(534.0, 914.0)
        };
        let scroll_h = panel_h - 94.0;
        // 内容变化（插件行、提示文字、滚动条）不能影响面板位置；否则居中锚点会
        // 和自动尺寸互相反馈，在 Windows 上表现为持续抖动。
        let panel_pos = egui::pos2(
            screen.left() + (screen.width() - panel_w) * 0.5,
            screen.top() + ((screen.height() - panel_h) * 0.5).max(20.0),
        );

        // 蒙层：纯装饰压暗（直接画到 Background 层，不注册任何交互控件）。
        // ⚠️ 不能用带 Sense 的 Area 做蒙层：egui 0.30 会给 interactable Area 自动注册
        // 覆盖整个区域的“置顶点击”控件（area.rs move_response），抢占面板交互并自动
        // 把蒙层提到 Foreground 最前，表现为弹窗被蒙层挡住/点不动。
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Middle,
            egui::Id::new("modal_dim"),
        ))
        .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(130));

        // 点面板外关闭：原始输入判定（本帧不注册任何全屏交互控件，不与面板抢事件）。
        // modal_open_last_frame 守卫：打开当帧的 press 是侧栏触发点击，不得误关。
        if state.modal_open_last_frame {
            let pressed = ctx.input(|i| i.pointer.any_pressed());
            let origin = ctx.input(|i| i.pointer.press_origin());
            if pressed {
                if let Some(pos) = origin {
                    let on_panel = state.modal_panel_rect.is_some_and(|r| r.contains(pos));
                    if !on_panel {
                        state.settings_open = false;
                    }
                }
            }
        }

        // 面板层：Foreground 层。蒙层已无交互控件，本层是前景唯一可交互层，
        // 不会被抬到更前；层内 ComboBox 下拉注册更晚 → 盖住面板。
        // 切勿用 Tooltip：下拉菜单开在 Foreground 层，面板若更高会把菜单整个盖住。
        egui::Area::new("settings_panel".into())
                .order(egui::Order::Foreground)
                .fixed_pos(panel_pos)
                .show(ctx, |ui| {
                    egui::Frame::default()
                        .fill(pal.panel)
                        .rounding(egui::Rounding::same(14.0))
                        .stroke(egui::Stroke::new(1.0_f32, pal.border))
                        .shadow(egui::epaint::Shadow {
                            offset: egui::vec2(0.0, 10.0),
                            blur: 28.0,
                            spread: 0.0,
                            color: egui::Color32::from_black_alpha(120),
                        })
                        .inner_margin(egui::Margin::symmetric(20.0, 16.0))
                        .show(ui, |ui| {
                            ui.set_width(panel_w);
                            ui.set_height(panel_h);
                            // 头部：标题 + 关闭按钮
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(display_title)
                                        .size(16.0)
                                        .strong()
                                        .color(pal.text),
                                );
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if close_button(ui, &pal) {
                                        state.settings_open = false;
                                    }
                                });
                            });
                            let sep = ui
                                .allocate_exact_size(
                                    egui::vec2(ui.available_width(), 1.0),
                                    egui::Sense::hover(),
                                )
                                .0;
                            ui.painter().rect_filled(sep, 0.0, pal.border);
                            ui.add_space(10.0);
                            // 固定反馈区高度，保存提示出现/消失时弹窗与内容区均不跳动。
                            ui.allocate_ui_with_layout(
                                egui::vec2(ui.available_width(), 24.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    if !state.note.is_empty() {
                                        ui.label(
                                            egui::RichText::new(&state.note)
                                                .size(12.0)
                                                .color(pal.accent),
                                        );
                                    }
                                },
                            );
                            ui.horizontal(|ui| {
                                if system_page {
                                    ui.vertical(|ui| {
                                        ui.set_width(156.0);
                                        ui.add_space(2.0);
                                        for (target, label) in [
                                            ("模型配置", "模型配置"),
                                            ("技能管理", "技能管理"),
                                            ("记忆系统", "记忆系统"),
                                            ("参数配置", "参数配置"),
                                            ("系统更新", "系统更新"),
                                        ] {
                                            let selected = match target {
                                                "模型配置" => matches!(page.as_str(), "模型配置" | "模型设置"),
                                                "记忆系统" => matches!(page.as_str(), "记忆系统" | "记忆"),
                                                "参数配置" => matches!(page.as_str(), "参数配置" | "系统配置"),
                                                "系统更新" => matches!(page.as_str(), "系统更新" | "更新"),
                                                _ => page == target,
                                            };
                                            if ui
                                                .add_sized(
                                                    [148.0, 36.0],
                                                    egui::SelectableLabel::new(selected, label),
                                                )
                                                .clicked()
                                            {
                                                state.settings_page = target.into();
                                                state.note.clear();
                                            }
                                            ui.add_space(4.0);
                                        }
                                    });
                                    ui.separator();
                                    ui.add_space(10.0);
                                }
                                egui::ScrollArea::vertical()
                                .min_scrolled_height(scroll_h)
                                .max_height(scroll_h)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    // 外层是系统管理的左右分栏；右侧内容必须重新建立纵向布局，
                                    // 否则会继承 horizontal，把所有表单项排成一整行并撑宽弹窗。
                                    ui.vertical(|ui| match page.as_str() {
                                    "模型配置" | "模型设置" => {
                                        ui.set_min_width(if system_page { panel_w - 220.0 } else { panel_w - 12.0 });
                                        field_label(ui, &pal, "已保存的配置");
                                        ui.horizontal(|ui| {
                                            egui::ComboBox::from_id_salt("profiles")
                                                .width(
                                                    (panel_w
                                                        - if system_page { 304.0 } else { 96.0 })
                                                    .max(260.0),
                                                )
                                                .selected_text(if state.selected_profile.is_empty() {
                                                    "选择已保存的配置…"
                                                } else {
                                                    state.selected_profile.as_str()
                                                })
                                                .show_ui(ui, |ui| {
                                                    for name in state.profiles.clone() {
                                                        if ui
                                                            .selectable_value(&mut state.selected_profile, name.clone(), name.as_str())
                                                            .clicked()
                                                        {
                                                            state.load_profile(&name);
                                                        }
                                                    }
                                                });
                                            if !state.selected_profile.is_empty()
                                                && ghost_button(ui, &pal, "删除")
                                            {
                                                let _ = state
                                                    .host
                                                    .settings
                                                    .delete_model_profile(&state.selected_profile);
                                                state.profiles = state
                                                    .host
                                                    .settings
                                                    .model_profiles()
                                                    .into_iter()
                                                    .map(|p| p.name)
                                                    .collect();
                                                state.selected_profile.clear();
                                            }
                                        });
                                        field_label(ui, &pal, "模型厂商");
                                        ui.add(egui::TextEdit::singleline(&mut state.f_provider).desired_width(f32::INFINITY));
                                        field_label(ui, &pal, "API 地址");
                                        ui.add(egui::TextEdit::singleline(&mut state.f_base).desired_width(f32::INFINITY));
                                        field_label(ui, &pal, "模型名称（可自由填写；留空则保存时取上游列表第一个）");
                                        ui.horizontal(|ui| {
                                            ui.add(egui::TextEdit::singleline(&mut state.f_model).desired_width(ui.available_width() - 150.0));
                                            if state.models_loading {
                                                ui.spinner();
                                                ui.label(egui::RichText::new("获取中…").size(11.0).color(pal.dim));
                                            } else if ghost_button(ui, &pal, "获取上游模型列表") {
                                                state.fetch_models_from_upstream();
                                            }
                                        });
                                        if !state.models_msg.is_empty() {
                                            ui.label(
                                                egui::RichText::new(&state.models_msg)
                                                    .size(11.0)
                                                    .color(if state.f_models.is_empty() { pal.err_text } else { pal.accent }),
                                            );
                                        }
                                        if !state.f_models.is_empty() {
                                            ui.add_space(4.0);
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "勾选要启用的模型（共 {} 个）：",
                                                    state.f_models.len()
                                                ))
                                                .size(11.5)
                                                .color(pal.dim),
                                            );
                                            egui::ScrollArea::vertical()
                                                .id_salt("model_list_scroll")
                                                .max_height(120.0)
                                                .show(ui, |ui| {
                                                    for m in state.f_models.clone() {
                                                        let mut checked = state.f_selected_models.contains(&m);
                                                        if ui.checkbox(&mut checked, &m).changed() {
                                                            if checked {
                                                                state.f_selected_models.insert(m.clone());
                                                            } else {
                                                                state.f_selected_models.remove(&m);
                                                            }
                                                        }
                                                    }
                                                });
                                            ui.add_space(4.0);
                                            let n = state.f_selected_models.len();
                                            if n > 0 {
                                                ui.label(
                                                    egui::RichText::new(format!("已选 {n} 个 · 保存后以第一个为当前模型，其余作为可用模型"))
                                                        .size(11.0)
                                                        .color(pal.accent),
                                                );
                                            }
                                        }
                                        field_label(ui, &pal, "API Key（AES-256-GCM 加密后保存至 SQLite，跨操作系统通用）");
                                        ui.add(egui::TextEdit::singleline(&mut state.f_key).password(true).desired_width(f32::INFINITY));
                                        field_label(ui, &pal, "思考档位 reasoning_effort（可选：off/low/medium/high/xhigh/max/auto，留空=默认）");
                                        ui.add(egui::TextEdit::singleline(&mut state.f_effort).desired_width(f32::INFINITY));
                                        ui.add_space(14.0);
                                        if accent_button(ui, &pal, "添加 / 更新并应用") {
                                            state.apply_model();
                                        }
                                        ui.add_space(8.0);
                                        ui.label(
                                            egui::RichText::new(
                                                "支持采用 OpenAI Chat Completions 协议的服务；保存时以“厂商 · 模型名”建立或更新配置。",
                                            )
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                    }
                                    "新建项目" => {
                                        ui.label(
                                            egui::RichText::new("选择项目目录后立即切换到该项目，并保存到侧栏项目列表。")
                                                .size(12.5)
                                                .color(pal.text),
                                        );
                                        ui.add_space(12.0);
                                        if accent_button(ui, &pal, "选择项目目录") {
                                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                                let s = path.display().to_string();
                                                let _ = state.host.settings.add_project(&path);
                                                state.switch_project(&s);
                                            }
                                        }
                                        ui.add_space(8.0);
                                        ui.label(
                                            egui::RichText::new(format!("当前项目: {}", state.active_project))
                                                .size(12.0)
                                                .color(pal.dim),
                                        );
                                    }
                                    "插件管理" => {
                                        field_label(ui, &pal, "系统插件（随应用发布，默认启用且不可移除）");
                                        for i in 0..state.plugin_rows.len() {
                                            if state.plugin_rows[i].core {
                                                let _ = plugin_row_ui(ui, &pal, &mut state.plugin_rows[i]);
                                            }
                                        }
                                        ui.add_space(6.0);
                                        field_label(ui, &pal, "扩展插件（WASM · wasmtime 沙箱隔离，可自由启用 / 禁用或移除）");
                                        let active_plugins = state.host.wasm_plugins.active_ids();
                                        ui.label(
                                            egui::RichText::new(if active_plugins.is_empty() {
                                                "运行时：当前没有已加载的 WASM 插件".to_string()
                                            } else {
                                                format!("运行时：已加载 {}", active_plugins.join("、"))
                                            })
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                        ui.add_space(4.0);
                                        let mut remove_ids: Vec<String> = Vec::new();
                                        let mut wasm_count = 0;
                                        for i in 0..state.plugin_rows.len() {
                                            if state.plugin_rows[i].core {
                                                continue;
                                            }
                                            wasm_count += 1;
                                            let (remove, changed) = plugin_row_ui(ui, &pal, &mut state.plugin_rows[i]);
                                            if changed {
                                                let row = &mut state.plugin_rows[i];
                                                let result = if row.enabled {
                                                    state.host.wasm_plugins.activate(&row.id, std::path::Path::new(&row.desc))
                                                } else {
                                                    state.host.wasm_plugins.deactivate(&row.id)
                                                };
                                                match result {
                                                    Ok(()) => {
                                                        row.active = row.enabled;
                                                        let _ = state.host.settings.set_plugin_enabled(&row.id, &row.name, row.enabled);
                                                        state.note = format!("插件「{}」{}", row.name, if row.enabled { "已启用并开始运行" } else { "已禁用并卸载" });
                                                    }
                                                    Err(error) => {
                                                        row.enabled = !row.enabled;
                                                        state.note = format!("插件状态未变更: {error}");
                                                    }
                                                }
                                            }
                                            if remove {
                                                remove_ids.push(state.plugin_rows[i].id.clone());
                                            }
                                        }
                                        if wasm_count == 0 {
                                            ui.label(
                                                egui::RichText::new("尚未导入 WASM 插件，点下方「＋ 添加新插件」导入 .wasm / .wat 产物。")
                                                    .size(12.0)
                                                    .color(pal.dim),
                                            );
                                            ui.add_space(6.0);
                                        }
                                        if !remove_ids.is_empty() {
                                            for id in &remove_ids {
                                                let _ = state.host.wasm_plugins.deactivate(id);
                                                let _ = state.host.settings.remove_plugin(id);
                                            }
                                            state.plugin_rows.retain(|r| !remove_ids.contains(&r.id));
                                            state.note = format!("已移除 {} 个插件", remove_ids.len());
                                        }
                                        ui.add_space(8.0);
                                        ui.horizontal(|ui| {
                                            if accent_button(ui, &pal, "保存插件设置") {
                                                state.save_preferences();
                                            }
                                            if ghost_button(ui, &pal, "＋ 添加新插件") {
                                                state.import_wasm_plugin();
                                            }
                                        });
                                        ui.add_space(6.0);
                                        ui.label(
                                            egui::RichText::new(
                                                "系统插件包含基础工具和 Superpowers 工作流扩展。自定义插件可导入 .wasm/.wat；勾选即立即加载并执行可选 on_load，取消勾选即卸载。插件仅获得 host_log，默认没有 Shell、文件或网络权限；「移除」只删除登记，不删除你的原始文件。",
                                            )
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                    }
                                    "技能管理" => {
                                        if state.skill_items.is_empty() {
                                            state.refresh_skill_items();
                                        }
                                        ui.label(
                                            egui::RichText::new(
                                                "技能是可导入的 SKILL.md 指令资产。启用的自定义技能会在每个回合按任务匹配后注入模型上下文；禁用或删除后立即停止注入。",
                                            )
                                            .size(12.0)
                                            .color(pal.dim),
                                        );
                                        ui.add_space(8.0);
                                        ui.horizontal(|ui| {
                                            if accent_button(ui, &pal, "导入 SKILL.md") {
                                                state.import_skill_file();
                                            }
                                            if ghost_button(ui, &pal, "刷新") {
                                                state.refresh_skill_items();
                                            }
                                        });
                                        ui.add_space(8.0);
                                        egui::ScrollArea::vertical().show(ui, |ui| {
                                            if state.skill_items.is_empty() {
                                                ui.label(egui::RichText::new("暂无自定义技能。导入一个 SKILL.md 即可开始管理。").size(12.0).color(pal.dim));
                                            }
                                            for sk in state.skill_items.clone() {
                                                let system_skill = sk.id.starts_with("sp-");
                                                ui.group(|ui| {
                                                    ui.horizontal(|ui| {
                                                        let mut enabled = sk.enabled;
                                                        ui.add_enabled(
                                                            !system_skill,
                                                            egui::Checkbox::new(&mut enabled, ""),
                                                        )
                                                        .on_hover_text(if system_skill { "由 Superpowers 系统插件管理" } else { "启用 / 禁用此技能" });
                                                        if !system_skill && enabled != sk.enabled {
                                                            state.toggle_skill(&sk.id, enabled);
                                                        }
                                                        ui.label(egui::RichText::new(&sk.name).size(13.0).strong().color(pal.text));
                                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                            if system_skill {
                                                                ui.label(egui::RichText::new("系统插件提供").size(10.5).color(pal.accent));
                                                            } else if ghost_button(ui, &pal, "删除") {
                                                                state.delete_skill_ui(&sk.id);
                                                            }
                                                        });
                                                    });
                                                    ui.label(egui::RichText::new(&sk.trigger_boundary).size(11.5).color(pal.dim));
                                                    if !sk.steps.is_empty() {
                                                        ui.label(egui::RichText::new(format!("步骤: {}", sk.steps.join(" → "))).size(11.0).color(pal.dim));
                                                    }
                                                });
                                                ui.add_space(6.0);
                                            }
                                        });
                                    }
                                    "记忆系统" | "记忆" => {
                                        // 标签切换：对话记忆 / 技能 / 知识库 / 代码图谱
                                        ui.horizontal_wrapped(|ui| {
                                            for (t, label) in [
                                                ("chat", "对话记忆"),
                                                ("skill", "技能"),
                                                ("wiki", "知识库"),
                                                ("code", "代码图谱"),
                                            ] {
                                                if ui
                                                    .selectable_value(
                                                        &mut state.mem_tab,
                                                        t.to_string(),
                                                        label,
                                                    )
                                                    .clicked()
                                                {
                                                    state.mem_loaded = false;
                                                }
                                            }
                                        });
                                        ui.add_space(6.0);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new("搜索").size(12.0).color(pal.dim),
                                            );
                                            if ui
                                                .text_edit_singleline(&mut state.mem_query)
                                                .changed()
                                            {
                                                state.mem_loaded = false;
                                            }
                                        });
                                        ui.add_space(6.0);
                                        if state.mem_tab.is_empty() {
                                            state.mem_tab = "chat".into();
                                        }
                                        if !state.mem_loaded {
                                            // 首次打开：自动对当前工作区做一次资产索引
                                            //（扫描 SKILL.md / *.md / 源码 → Skill/Wiki/CodeGraph），
                                            // 之后记忆面板才有真实内容可见。
                                            if !state.mem_bootstrapped {
                                                state.bootstrap_mem();
                                            }
                                            state.refresh_mem();
                                            state.refresh_skill_items();
                                            state.mem_loaded = true;
                                        }
                                        ui.horizontal(|ui| {
                                            if ghost_button(ui, &pal, "重新索引资产") {
                                                state.bootstrap_mem();
                                            }
                                            if !state.mem_index_msg.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(&state.mem_index_msg)
                                                        .size(11.0)
                                                        .color(pal.dim),
                                                );
                                            }
                                        });
                                        ui.add_space(4.0);
                                        // 代码图谱用结构化视图，展示符号数；其余 tab 展示条目数。
                                        let mem_count = if state.mem_tab == "code" {
                                            state.mem_code_symbols.len()
                                        } else {
                                            state.mem_items.len()
                                        };
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "共 {mem_count} 条 · 本地原生记忆（若已连接 aidops 后端，以远端为准）"
                                            ))
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                        ui.add_space(6.0);
if state.mem_tab == "code" {
                                            // 代码图谱：结构化视图（统计卡 + 按文件分组折叠 + 调用关系导航）。
                                            super::code_graph::render(
                                                ui,
                                                &pal,
                                                &state.mem_code_symbols,
                                                &mut state.mem_code_expanded,
                                                &mut state.mem_code_sel,
                                                &mut state.mem_code_scroll,
                                            );
                                        } else if state.mem_tab == "skill" {
                                            // 兼容旧“记忆 → 技能”入口；完整管理入口在侧栏“技能管理”。
                                            ui.horizontal(|ui| {
                                                if ghost_button(ui, &pal, "导入 SKILL.md") {
                                                    state.import_skill_file();
                                                }
                                            });
                                            ui.add_space(4.0);
                                            if state.skill_items.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(
                                                        "暂无技能。点击「导入 SKILL.md」导入自定义技能，或「重新索引资产」扫描工作区中的 SKILL.md。",
                                                    )
                                                    .size(12.0)
                                                    .color(pal.dim),
                                                );
                                            } else {
                                                egui::ScrollArea::vertical().show(ui, |ui| {
                                                    for sk in state.skill_items.clone() {
                                                        ui.group(|ui| {
                                                            ui.horizontal(|ui| {
                                                                let mut enabled = sk.enabled;
                                                                if ui
                                                                    .checkbox(&mut enabled, "")
                                                                    .on_hover_text("启用 / 禁用此技能")
                                                                    .changed()
                                                                {
                                                                    state.toggle_skill(&sk.id, enabled);
                                                                }
                                                                ui.label(
                                                                    egui::RichText::new(&sk.name)
                                                                        .size(13.0)
                                                                        .color(pal.text)
                                                                        .strong(),
                                                                );
                                                                ui.label(
                                                                    egui::RichText::new(format!("v{}", sk.version))
                                                                        .size(10.5)
                                                                        .color(pal.dim),
                                                                );
                                                                ui.with_layout(
                                                                    egui::Layout::right_to_left(egui::Align::Center),
                                                                    |ui| {
                                                                        if ghost_button(ui, &pal, "删除") {
                                                                            state.delete_skill_ui(&sk.id);
                                                                        }
                                                                    },
                                                                );
                                                            });
                                                            ui.label(
                                                                egui::RichText::new(&sk.trigger_boundary)
                                                                    .size(11.5)
                                                                    .color(if sk.enabled { pal.dim } else { pal.err_text }),
                                                            );
                                                            ui.label(
                                                                egui::RichText::new(format!(
                                                                    "步骤: {}",
                                                                    sk.steps.join(" → ")
                                                                ))
                                                                .size(11.0)
                                                                .color(pal.dim),
                                                            );
                                                        });
                                                        ui.add_space(6.0);
                                                    }
                                                });
                                            }
                                        } else {
                                                                                egui::ScrollArea::vertical().show(ui, |ui| {
                                            if state.mem_items.is_empty() {
                                                ui.label(
                                                    egui::RichText::new(
                                                        "暂无记忆。点击「重新索引资产」扫描工作区的 SKILL.md / 文档 / 源码，自动沉淀技能、知识库与代码图谱；对话中也会逐步沉淀对话记忆（L0~L3）。",
                                                    )
                                                    .size(12.0)
                                                    .color(pal.dim),
                                                );
                                            }
                                            for it in &state.mem_items {
                                                ui.group(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(&it.title)
                                                            .size(13.0)
                                                            .color(pal.text)
                                                            .strong(),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(&it.meta)
                                                            .size(10.5)
                                                            .color(pal.dim),
                                                    );
                                                    ui.label(
                                                        egui::RichText::new(&it.body)
                                                            .size(12.0)
                                                            .color(pal.text),
                                                    );
                                                });
                                                ui.add_space(6.0);
                                            }
                                        });
                                    }
                                    }

                                    "系统更新" | "更新" => {
                                        state.draw_update_settings(ui, &pal);
                                    }
                                    "Git 变更" => {
                                        // 分支 + 统计 + 刷新
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "分支：{}",
                                                    if state.git_branch.is_empty() {
                                                        "未知"
                                                    } else {
                                                        &state.git_branch
                                                    }
                                                ))
                                                .size(13.0)
                                                .strong()
                                                .color(pal.text),
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    if ghost_button(ui, &pal, "刷新") {
                                                        state.refresh_git_changes();
                                                    }
                                                },
                                            );
                                        });
                                        ui.add_space(4.0);
                                        let n = state.git_changes.len();
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "共 {} 个文件有未提交变更",
                                                n
                                            ))
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                        ui.add_space(8.0);
                                        if n == 0 && state.git_loaded {
                                            ui.label(
                                                egui::RichText::new("✨ 工作区干净，无未提交变更")
                                                    .size(12.0)
                                                    .color(pal.accent),
                                            );
                                        } else {
                                            egui::ScrollArea::vertical()
                                                .max_height(scroll_h - 120.0)
                                                .auto_shrink(false)
                                                .show(ui, |ui| {
                                                    let mut open_now: Option<String> = None;
                                                    for ch in state.git_changes.clone() {
                                                        // 状态色块
                                                        let (mark, mcolor) = match ch.marker() {
                                                            "M" => ("M", pal.warn),
                                                            "A" => ("A", pal.accent),
                                                            "D" => ("D", pal.err_text),
                                                            "R" => ("R", pal.accent),
                                                            "U" | "??" => ("?", pal.dim),
                                                            _ => ("*", pal.dim),
                                                        };
                                                        let row_h = 30.0;
                                                        let (rect, resp) = ui.allocate_at_least(
                                                            egui::vec2(ui.available_width(), row_h),
                                                            egui::Sense::click(),
                                                        );
                                                        if resp.hovered() {
                                                            ui.painter().rect_filled(
                                                                rect.shrink(1.0),
                                                                egui::Rounding::same(6.0),
                                                                pal.hover,
                                                            );
                                                        }
                                                        // 状态标记小方块
                                                        let badge = egui::Rect::from_center_size(
                                                            egui::pos2(rect.min.x + 14.0, rect.center().y),
                                                            egui::vec2(22.0, 18.0),
                                                        );
                                                        ui.painter().rect_filled(
                                                            badge,
                                                            egui::Rounding::same(4.0),
                                                            mcolor.gamma_multiply(0.22),
                                                        );
                                                        ui.painter().text(
                                                            badge.center(),
                                                            egui::Align2::CENTER_CENTER,
                                                            mark,
                                                            egui::FontId::monospace(11.0),
                                                            mcolor,
                                                        );
                                                        // 路径
                                                        ui.painter().text(
                                                            egui::pos2(rect.min.x + 44.0, rect.center().y),
                                                            egui::Align2::LEFT_CENTER,
                                                            &ch.path,
                                                            egui::FontId::monospace(11.5),
                                                            pal.text,
                                                        );
                                                        if resp.clicked() {
                                                            open_now = Some(ch.path.clone());
                                                        }
                                                        ui.add_space(2.0);
                                                    }
                                                    if let Some(path) = open_now {
                                                        // 关闭弹层，打开该文件的 Diff 预览
                                                        state.settings_open = false;
                                                        state.open_preview(path);
                                                        state.preview_mode =
                                                            crate::preview::PreviewMode::Diff;
                                                    }
                                                });
                                        }
                                    }
                                    _ => {
                                        field_label(ui, &pal, "默认访问权限");
                                        egui::ComboBox::from_id_salt("sys-perm")
                                            .width(260.0)
                                            .selected_text(&state.permission)
                                            .show_ui(ui, |ui| {
                                                for mode in ["只读", "工作区写入", "完全访问"] {
                                                    ui.selectable_value(&mut state.permission, mode.to_string(), mode);
                                                }
                                            });
                                        ui.add_space(14.0);
                                        field_label(ui, &pal, "窗口外观");
                                        let stored_titlebar = state
                                            .host
                                            .settings
                                            .get("ui.integrated_titlebar");
                                        let mut integrated_titlebar =
                                            crate::window_chrome::integrated_titlebar_enabled(
                                                stored_titlebar.as_deref(),
                                            );
                                        if ui
                                            .checkbox(
                                                &mut integrated_titlebar,
                                                "融合工作台与系统标题栏",
                                            )
                                            .changed()
                                        {
                                            let _ = state.host.settings.set(
                                                "ui.integrated_titlebar",
                                                if integrated_titlebar { "true" } else { "false" },
                                            );
                                            state.note = "窗口外观已保存，重启应用后生效".into();
                                        }
                                        ui.label(
                                            egui::RichText::new(
                                                "macOS 保留原生交通灯；Windows 使用应用窗口控制按钮。异常时关闭可恢复系统标题栏。",
                                            )
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                        ui.add_space(14.0);
                                        if accent_button(ui, &pal, "保存参数配置") {
                                            state.save_preferences();
                                        }
                                        ui.add_space(10.0);
                                        field_label(ui, &pal, "aidops 后端连接（可选）");
                                        ui.label(
                                            egui::RichText::new(
                                                "配置后 dsh 把四类记忆资产同步到智程平台；留空则仅用本地文件记忆，桌面可独立工作。",
                                            )
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                        egui::Frame::default()
                                            .fill(pal.bg)
                                            .rounding(egui::Rounding::same(10.0))
                                            .stroke(egui::Stroke::new(1.0_f32, pal.border))
                                            .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                                            .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.add_sized([110.0, 22.0], egui::Label::new(
                                                egui::RichText::new("后端地址").size(12.0).color(pal.text),
                                            ));
                                            ui.add_space(8.0);

                                        ui.add(
                                            egui::TextEdit::singleline(&mut state.f_aidops_base)
                                                .desired_width(f32::INFINITY)
                                                .hint_text("后端地址，如 http://localhost:8000"),
                                        );
                                        });
                                        ui.horizontal(|ui| {
                                            ui.add_sized([110.0, 22.0], egui::Label::new(
                                                egui::RichText::new("API Key").size(12.0).color(pal.text),
                                            ));
                                            ui.add_space(8.0);

                                        ui.add(
                                            egui::TextEdit::singleline(&mut state.f_aidops_key)
                                                .desired_width(f32::INFINITY)
                                                .hint_text("API Key（可选；亦可用环境变量 AIDOPS_API_KEY）")
                                                .password(true),
                                        );
                                        });
                                        ui.horizontal(|ui| {
                                            ui.add_sized([110.0, 22.0], egui::Label::new(
                                                egui::RichText::new("项目 ID").size(12.0).color(pal.text),
                                            ));
                                            ui.add_space(8.0);

                                        ui.add(
                                            egui::TextEdit::singleline(&mut state.f_aidops_project)
                                                .desired_width(f32::INFINITY)
                                                .hint_text("项目 ID（可选，整数）"),
                                        );
                                        });
                                            });
                                        ui.add_space(10.0);
                                        field_label(ui, &pal, "配置文件 .harness.toml");
                                        ui.horizontal(|ui| {
                                            if ghost_button(ui, &pal, "重新加载") {
                                                match Config::load() {
                                                    Ok(cfg) => {
                                                        let _ = state.host.llm_control.reload_config(&cfg);
                                                        state.f_aidops_base = cfg.aidops.base_url;
                                                        state.f_aidops_key =
                                                            cfg.aidops.api_key.unwrap_or_default();
                                                        state.f_aidops_project = cfg
                                                            .aidops
                                                            .project_id
                                                            .map(|v| v.to_string())
                                                            .unwrap_or_default();
                                                        state.note = "已从 .harness.toml 重新加载并应用配置".into();
                                                    }
                                                    Err(e) => state.note = format!("加载失败: {e}"),
                                                }
                                            }
                                            if ghost_button(ui, &pal, "原子写入") {
                                                let mut cfg = Config::default();
                                                cfg.llm.provider = state.f_provider.clone();
                                                cfg.llm.base_url = state.f_base.clone();
                                                cfg.llm.model = state.f_model.clone();
                                                // 不写入 api_key：密钥经 AES-256-GCM 加密存储，明文落盘会泄露；
                                                // 热重载（reload_config）会回退到运行时缓存的 key。
                                                cfg.llm.reasoning_effort = state.effort();
                                                // aidops 后端连接（可选插件入口）：留空 base_url 即不启用。
                                                cfg.aidops.base_url = state.f_aidops_base.trim().to_string();
                                                cfg.aidops.api_key = if state.f_aidops_key.trim().is_empty() {
                                                    None
                                                } else {
                                                    Some(state.f_aidops_key.trim().to_string())
                                                };
                                                cfg.aidops.project_id =
                                                    state.f_aidops_project.trim().parse::<i64>().ok();
                                                match cfg.save_atomic(".harness.toml") {
                                                    Ok(()) => {
                                                        state.note = "配置已原子写入 .harness.toml（含 [aidops]，临时文件 + rename）".into()
                                                    }
                                                    Err(e) => state.note = format!("写入失败: {e}"),
                                                }
                                            }
                                        });
                                        ui.add_space(8.0);
                                        ui.label(
                                            egui::RichText::new(
                                                "「原子写入」先写临时文件再 rename，崩溃不会损坏原配置；「重新加载」把文件 [llm] 段热重载进运行时，无需重启。",
                                            )
                                            .size(11.0)
                                            .color(pal.dim),
                                        );
                                    }
                                    });
                                });
                            });
                            // 记录面板矩形：供下一帧“点外部关闭”守卫判定。
                            state.modal_panel_rect = Some(ui.min_rect());
                        });
                });
        state.modal_open_last_frame = true;
    } else {
        state.modal_open_last_frame = false;
    }
}
