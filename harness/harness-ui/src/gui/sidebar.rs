//! Left navigation, project list, and session history.

use super::*;

fn time_group_label(t: &std::time::SystemTime) -> &'static str {
    let Ok(ago) = t.elapsed() else {
        return "今天";
    };
    let secs = ago.as_secs();
    if secs < 86400 {
        "今天"
    } else if secs < 86400 * 2 {
        "昨天"
    } else if secs < 86400 * 7 {
        "过去 7 天"
    } else {
        "更早"
    }
}

pub(super) fn show(state: &mut AppState, ctx: &egui::Context, pal: Palette, sidebar_width: f32) {
    // ── 侧栏导航 ─────────────────────────────────────────────
    egui::SidePanel::left("nav")
        .exact_width(sidebar_width)
        .frame(egui::Frame::default().fill(pal.side).inner_margin(8.0))
        .show(ctx, |ui| {
            ui.add_space(4.0);
            let logo_height = 28.0;
            let (logo_rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), logo_height),
                egui::Sense::hover(),
            );
            draw_brand_logo(ui, logo_rect, state.sidebar_expanded, &pal);
            ui.add_space(8.0);

            // ── Codex 式醒目主操作：➕ 新建会话 ──
            if state.sidebar_expanded {
                let (btn_rect, btn_resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 32.0),
                    egui::Sense::click(),
                );
                let btn_fill = if btn_resp.hovered() {
                    pal.btn_hover
                } else {
                    pal.btn_fill
                };
                ui.painter().rect(
                    btn_rect,
                    egui::Rounding::same(8.0),
                    btn_fill,
                    egui::Stroke::new(1.0_f32, pal.btn_border),
                );
                // + 图标
                let ic = btn_rect.left_center() + egui::vec2(16.0, 0.0);
                let is = egui::Stroke::new(1.6_f32, pal.btn_text);
                ui.painter().line_segment([ic + egui::vec2(-4.5, 0.0), ic + egui::vec2(4.5, 0.0)], is);
                ui.painter().line_segment([ic + egui::vec2(0.0, -4.5), ic + egui::vec2(0.0, 4.5)], is);

                ui.painter().text(
                    btn_rect.left_center() + egui::vec2(28.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    "新建对话",
                    egui::FontId::proportional(12.5),
                    pal.btn_text,
                );
                ui.painter().text(
                    btn_rect.right_center() + egui::vec2(-10.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    "⌘N",
                    egui::FontId::proportional(10.5),
                    pal.dim,
                );
                if btn_resp.clicked() {
                    state.new_session();
                }
            } else {
                let (btn_rect, btn_resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 32.0),
                    egui::Sense::click(),
                );
                let btn_fill = if btn_resp.hovered() {
                    pal.btn_hover
                } else {
                    pal.btn_fill
                };
                ui.painter().rect(
                    btn_rect,
                    egui::Rounding::same(8.0),
                    btn_fill,
                    egui::Stroke::new(1.0_f32, pal.btn_border),
                );
                let ic = btn_rect.center();
                let is = egui::Stroke::new(1.6_f32, pal.btn_text);
                ui.painter().line_segment([ic + egui::vec2(-4.5, 0.0), ic + egui::vec2(4.5, 0.0)], is);
                ui.painter().line_segment([ic + egui::vec2(0.0, -4.5), ic + egui::vec2(0.0, 4.5)], is);
                if btn_resp.on_hover_text("新建对话 (⌘N)").clicked() {
                    state.new_session();
                }
            }

            ui.add_space(6.0);

            // ── 快捷检索历史（展开时常驻）──
            if state.sidebar_expanded {
                sidebar_search_field(ui, &pal, &mut state.history_search);
                ui.add_space(4.0);
            }

            // ── 快捷功能入口 ──
            if nav_item(
                ui,
                &pal,
                Icon::Folder,
                "新建项目",
                state.sidebar_expanded,
                true,
                false,
            ) {
                state.settings_page = "新建项目".into();
                state.settings_open = true;
            }
            if nav_item(
                ui,
                &pal,
                Icon::Chip,
                "长时程任务",
                state.sidebar_expanded,
                true,
                state.lha_open,
            ) {
                state.lha_open = true;
            }
            if nav_item(
                ui,
                &pal,
                Icon::Layers,
                "插件管理",
                state.sidebar_expanded,
                true,
                false,
            ) {
                state.settings_page = "插件管理".into();
                state.settings_open = true;
            }
            if nav_item(
                ui,
                &pal,
                Icon::Gear,
                "系统管理",
                state.sidebar_expanded,
                true,
                false,
            ) {
                state.settings_page = "模型配置".into();
                state.settings_open = true;
            }

            // ── 项目列表（Codex/Cursor 式：点击即切上下文）────────────
            if state.sidebar_expanded {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("项目").size(11.0).color(pal.dim));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if sidebar_icon_button(ui, &pal, SidebarActionIcon::Add, "添加新项目")
                        {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                let s = path.display().to_string();
                                super::settings_view::stage_pending_project_dir(ctx, &s);
                                state.settings_page = "新建项目".into();
                                state.settings_open = true;
                                ctx.request_repaint();
                            }
                        }
                    });
                });
                ui.add_space(2.0);
                egui::ScrollArea::vertical()
                    .id_salt("project_list")
                    .max_height(110.0)
                    .auto_shrink(true)
                    .show(ui, |ui| {
                        let mut archive_now: Option<String> = None;
                        let mut switch_now: Option<String> = None;
                        for proj in state.projects.clone() {
                            if proj.archived {
                                continue;
                            }
                            let is_active = proj.path == state.active_project;
                            let (rect, resp) = ui.allocate_at_least(
                                egui::vec2(ui.available_width(), 26.0),
                                egui::Sense::click(),
                            );
                            let hovered = resp.hovered()
                                || (ctx.input(|i| i.pointer.has_pointer())
                                    && rect.contains(
                                        ctx.input(|i| i.pointer.hover_pos())
                                            .unwrap_or(egui::pos2(-1.0, -1.0)),
                                    ));
                            if is_active || hovered {
                                ui.painter().rect_filled(
                                    rect.shrink(1.0),
                                    egui::Rounding::same(6.0),
                                    pal.hover,
                                );
                            }
                            if is_active {
                                let bar = egui::Rect::from_min_size(
                                    egui::pos2(rect.min.x + 2.0, rect.min.y + 5.0),
                                    egui::vec2(2.5, rect.height() - 10.0),
                                );
                                ui.painter().rect_filled(
                                    bar,
                                    egui::Rounding::same(2.0),
                                    pal.accent,
                                );
                            }
                            draw_icon(
                                ui.painter(),
                                egui::pos2(rect.min.x + 14.0, rect.center().y),
                                Icon::Folder,
                                if is_active { pal.accent } else { pal.dim },
                            );
                            ui.painter().text(
                                egui::pos2(rect.min.x + 26.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                &proj.name,
                                egui::FontId::proportional(12.0),
                                if is_active { pal.text } else { pal.dim },
                            );
                            if hovered {
                                let control_h = sidebar_control_height();
                                let arch_rect = egui::Rect::from_min_size(
                                    egui::pos2(
                                        rect.max.x - control_h - 2.0,
                                        rect.center().y - control_h / 2.0,
                                    ),
                                    egui::vec2(control_h, control_h),
                                );
                                #[allow(deprecated)]
                                let archive_clicked = ui
                                    .allocate_ui_at_rect(arch_rect, |ui| {
                                        sidebar_icon_button(
                                            ui,
                                            &pal,
                                            SidebarActionIcon::Archive,
                                            "归档项目",
                                        )
                                    })
                                    .inner;
                                if archive_clicked {
                                    archive_now = Some(proj.path.clone());
                                }
                            }
                            if resp.clicked() {
                                switch_now = Some(proj.path.clone());
                            }
                        }
                        if let Some(path) = archive_now {
                            let _ = state.host.settings.archive_project(&path, true);
                            state.projects = state.host.settings.projects();
                            trace(&format!("[project] archived {path}"));
                        }
                        if let Some(path) = switch_now {
                            state.switch_project(&path);
                        }
                    });

                // ── 历史记录（Codex 时间分组智能展示）────────────────
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("历史 ({})", state.history.len()))
                            .size(11.0)
                            .color(pal.dim),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if sidebar_text_button(ui, &pal, "清空", "删除全部历史会话（保留当前对话）")
                        {
                            state.clear_history();
                        }
                        if sidebar_text_button(
                            ui,
                            &pal,
                            "精简",
                            "仅保留最近 30 个会话（当前对话不删）",
                        ) {
                            state.prune_history();
                        }
                    });
                });
                if let Some(at) = state.history_note_at {
                    if at.elapsed() < std::time::Duration::from_secs(5) {
                        ui.label(
                            egui::RichText::new(&state.history_note)
                                .size(10.5)
                                .color(pal.accent),
                        );
                    } else {
                        state.history_note_at = None;
                    }
                }
                ui.add_space(4.0);

                let history_height = (ui.available_height() - 40.0).max(90.0);
                egui::ScrollArea::vertical()
                    .id_salt("history_list")
                    .max_height(history_height)
                    .show(ui, |ui| {
                        let kw = state.history_search.trim().to_lowercase();
                        let mut open_now: Option<String> = None;
                        let mut delete_now: Option<String> = None;

                        // 过滤匹配列表
                        let filtered: Vec<SessionMeta> = state
                            .history
                            .iter()
                            .filter(|m| kw.is_empty() || m.title.to_lowercase().contains(&kw))
                            .cloned()
                            .collect();

                        if !kw.is_empty() {
                            // 搜索模式下平铺展示
                            for meta in &filtered {
                                render_history_row(
                                    ui,
                                    ctx,
                                    &pal,
                                    meta,
                                    meta.file == state.current_session,
                                    &mut state.renaming,
                                    &mut state.rename_buf,
                                    &mut delete_now,
                                    &mut open_now,
                                );
                            }
                        } else {
                            // 默认分组展示：今天、昨天、过去 7 天、更早
                            let groups = ["今天", "昨天", "过去 7 天", "更早"];
                            for &group in &groups {
                                let in_group: Vec<&SessionMeta> = filtered
                                    .iter()
                                    .filter(|m| time_group_label(&m.mtime) == group)
                                    .collect();
                                if in_group.is_empty() {
                                    continue;
                                }
                                ui.add_space(6.0);
                                ui.label(
                                    egui::RichText::new(group)
                                        .size(10.5)
                                        .strong()
                                        .color(pal.dim),
                                );
                                ui.add_space(2.0);
                                for meta in in_group {
                                    render_history_row(
                                        ui,
                                        ctx,
                                        &pal,
                                        meta,
                                        meta.file == state.current_session,
                                        &mut state.renaming,
                                        &mut state.rename_buf,
                                        &mut delete_now,
                                        &mut open_now,
                                    );
                                }
                            }
                        }

                        if let Some(file) = open_now {
                            state.switch_session(&file);
                        }
                        if let Some(file) = delete_now {
                            state.delete_session_entry(&file);
                        }
                    });
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                if state.sidebar_expanded {
                    let ws_display = if state.active_project.len() > 22 {
                        let mut s: String = state.active_project.chars().take(22).collect();
                        s.push('…');
                        s
                    } else {
                        state.active_project.clone()
                    };
                    ui.label(
                        egui::RichText::new(format!("工作区: {ws_display}"))
                            .size(10.5)
                            .color(pal.dim),
                    );
                }
            });
        });
}

fn render_history_row(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    pal: &Palette,
    meta: &SessionMeta,
    is_active: bool,
    renaming: &mut Option<String>,
    rename_buf: &mut String,
    delete_now: &mut Option<String>,
    open_now: &mut Option<String>,
) {
    let (rect, resp) = ui.allocate_at_least(
        egui::vec2(ui.available_width(), 34.0),
        egui::Sense::click(),
    );
    let hovered = resp.hovered()
        || (ctx.input(|i| i.pointer.has_pointer())
            && rect.contains(
                ctx.input(|i| i.pointer.hover_pos())
                    .unwrap_or(egui::pos2(-1.0, -1.0)),
            ));
    if is_active || hovered {
        ui.painter().rect_filled(
            rect.shrink(1.0),
            egui::Rounding::same(6.0),
            pal.hover,
        );
    }
    if is_active {
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + 2.0, rect.min.y + 6.0),
            egui::vec2(2.5, rect.height() - 12.0),
        );
        ui.painter().rect_filled(
            bar,
            egui::Rounding::same(2.0),
            pal.accent,
        );
    }
    // 标题截断
    let mut title: String = meta.title.chars().take(16).collect();
    if meta.title.chars().count() > 16 {
        title.push('…');
    }
    ui.painter().text(
        egui::pos2(rect.min.x + 10.0, rect.min.y + 11.0),
        egui::Align2::LEFT_CENTER,
        &title,
        egui::FontId::proportional(12.0),
        if is_active { pal.text } else { pal.dim },
    );
    ui.painter().text(
        egui::pos2(rect.min.x + 10.0, rect.max.y - 9.0),
        egui::Align2::LEFT_CENTER,
        relative_time(&meta.mtime),
        egui::FontId::proportional(9.5),
        pal.dim,
    );
    if hovered {
        let rename_rect = egui::Rect::from_min_size(
            egui::pos2(rect.max.x - 40.0, rect.center().y - 8.0),
            egui::vec2(16.0, 16.0),
        );
        let rb = ui.interact(
            rename_rect,
            egui::Id::new(("hist_rename", &meta.file)),
            egui::Sense::click(),
        );
        if rb.hovered() {
            ui.painter().rect_filled(
                rename_rect.shrink(1.0),
                egui::Rounding::same(4.0),
                pal.hover,
            );
        }
        draw_pencil_icon(
            ui.painter(),
            rename_rect.center(),
            if rb.hovered() { pal.text } else { pal.dim },
        );
        let rb = rb.on_hover_text("重命名此会话");
        if rb.clicked() {
            *renaming = Some(meta.file.clone());
            *rename_buf = meta.title.clone();
        }
        let del_rect = egui::Rect::from_min_size(
            egui::pos2(rect.max.x - 22.0, rect.center().y - 8.0),
            egui::vec2(16.0, 16.0),
        );
        let b = ui.interact(
            del_rect,
            egui::Id::new(("hist_delete", &meta.file)),
            egui::Sense::click(),
        );
        if b.hovered() {
            ui.painter().rect_filled(
                del_rect.shrink(1.0),
                egui::Rounding::same(4.0),
                pal.hover,
            );
        }
        draw_trash_icon(
            ui.painter(),
            del_rect.center(),
            if b.hovered() { pal.text } else { pal.dim },
        );
        let b = b.on_hover_text("删除此会话");
        if b.clicked() {
            *delete_now = Some(meta.file.clone());
        }
    }
    if resp.clicked() && !is_active {
        *open_now = Some(meta.file.clone());
    }
}
