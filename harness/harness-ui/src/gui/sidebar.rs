//! Left navigation, project list, and session history.

use super::*;

pub(super) fn show(state: &mut AppState, ctx: &egui::Context, pal: Palette, sidebar_width: f32) {
    // ── 侧栏导航 ─────────────────────────────────────────────
    egui::SidePanel::left("nav")
        .exact_width(sidebar_width)
        .frame(egui::Frame::default().fill(pal.side).inner_margin(8.0))
        .show(ctx, |ui| {
            ui.add_space(4.0);
            let logo_height = 30.0;
            let (logo_rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), logo_height),
                egui::Sense::hover(),
            );
            draw_brand_logo(ui, logo_rect, state.sidebar_expanded, &pal);
            ui.add_space(6.0);
            if nav_item(
                ui,
                &pal,
                Icon::Chat,
                "新建对话",
                state.sidebar_expanded,
                !state.busy,
                true,
            ) {
                state.new_session();
            }
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
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("项目").size(11.0).color(pal.dim));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if sidebar_icon_button(ui, &pal, SidebarActionIcon::Add, "添加新项目")
                        {
                            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                let s = path.display().to_string();
                                let _ = state.host.settings.add_project(&path);
                                state.switch_project(&s);
                            }
                        }
                    });
                });
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("project_list")
                    .max_height(140.0)
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
                                egui::vec2(ui.available_width(), 30.0),
                                egui::Sense::click(),
                            );
                            // 指针在本行内即保持悬停态：否则浮出的「归档」按钮会抢走
                            // hover 导致逐帧振荡抖动（与历史面板同一修复）。
                            let hovered = resp.hovered()
                                || (ctx.input(|i| i.pointer.has_pointer())
                                    && rect.contains(
                                        ctx.input(|i| i.pointer.hover_pos())
                                            .unwrap_or(egui::pos2(-1.0, -1.0)),
                                    ));
                            if is_active || hovered {
                                ui.painter().rect_filled(
                                    rect.shrink(1.0),
                                    egui::Rounding::same(7.0),
                                    pal.hover,
                                );
                            }
                            if is_active {
                                // 左侧 accent 竖条标识当前激活项目。
                                let bar = egui::Rect::from_min_size(
                                    egui::pos2(rect.min.x + 2.0, rect.min.y + 7.0),
                                    egui::vec2(2.5, rect.height() - 14.0),
                                );
                                ui.painter().rect_filled(
                                    bar,
                                    egui::Rounding::same(2.0),
                                    pal.accent,
                                );
                            }
                            draw_icon(
                                &ui.painter(),
                                egui::pos2(rect.min.x + 16.0, rect.center().y),
                                Icon::Folder,
                                if is_active { pal.accent } else { pal.dim },
                            );
                            ui.painter().text(
                                egui::pos2(rect.min.x + 30.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                &proj.name,
                                egui::FontId::proportional(12.5),
                                if is_active { pal.text } else { pal.dim },
                            );
                            if hovered {
                                // 悬停时右侧浮现固定尺寸的矢量归档按钮。
                                let control_h = sidebar_control_height();
                                let arch_rect = egui::Rect::from_min_size(
                                    egui::pos2(
                                        rect.max.x - control_h - 3.0,
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
                // ── 历史记录（点击恢复过往会话）────────────────
                ui.add_space(12.0);
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
                // 精简 / 清空操作的即时反馈（5 秒后隐去）。
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
                sidebar_search_field(ui, &pal, &mut state.history_search);
                ui.add_space(4.0);
                let history_height = (ui.available_height() - 40.0).max(90.0);
                egui::ScrollArea::vertical()
                    .id_salt("history_list")
                    .max_height(history_height)
                    .show(ui, |ui| {
                        let kw = state.history_search.trim().to_lowercase();
                        let mut open_now: Option<String> = None;
                        let mut delete_now: Option<String> = None;
                        for meta in state.history.clone() {
                            if !kw.is_empty() && !meta.title.to_lowercase().contains(&kw) {
                                continue;
                            }
                            let is_active = meta.file == state.current_session;
                            let (rect, resp) = ui.allocate_at_least(
                                egui::vec2(ui.available_width(), 40.0),
                                egui::Sense::click(),
                            );
                            // 指针在本行内即保持悬停态：否则浮出的按钮会抢走 hover
                            // 导致行失焦→按钮消失→再悬停→再浮出，逐帧振荡抖动。
                            let hovered = resp.hovered()
                                || (ctx.input(|i| i.pointer.has_pointer())
                                    && rect.contains(
                                        ctx.input(|i| i.pointer.hover_pos())
                                            .unwrap_or(egui::pos2(-1.0, -1.0)),
                                    ));
                            if is_active || hovered {
                                ui.painter().rect_filled(
                                    rect.shrink(1.0),
                                    egui::Rounding::same(7.0),
                                    pal.hover,
                                );
                            }
                            if is_active {
                                let bar = egui::Rect::from_min_size(
                                    egui::pos2(rect.min.x + 2.0, rect.min.y + 8.0),
                                    egui::vec2(2.5, rect.height() - 16.0),
                                );
                                ui.painter().rect_filled(
                                    bar,
                                    egui::Rounding::same(2.0),
                                    pal.accent,
                                );
                            }
                            // 标题截断适配窄侧栏。
                            let mut title: String = meta.title.chars().take(15).collect();
                            if meta.title.chars().count() > 15 {
                                title.push('…');
                            }
                            ui.painter().text(
                                egui::pos2(rect.min.x + 12.0, rect.min.y + 12.0),
                                egui::Align2::LEFT_CENTER,
                                &title,
                                egui::FontId::proportional(12.0),
                                if is_active { pal.text } else { pal.dim },
                            );
                            ui.painter().text(
                                egui::pos2(rect.min.x + 12.0, rect.max.y - 11.0),
                                egui::Align2::LEFT_CENTER,
                                format!("{} · {}", meta.project, relative_time(&meta.mtime)),
                                egui::FontId::proportional(10.0),
                                pal.dim,
                            );
                            if hovered {
                                // 悬停时右侧浮现「重命名」与「删除」按钮：
                                // 矢量图标（不依赖字体字形，避免豆腐块）+ 悬停提示说明功能。
                                let rename_rect = egui::Rect::from_min_size(
                                    egui::pos2(rect.max.x - 44.0, rect.center().y - 9.0),
                                    egui::vec2(18.0, 18.0),
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
                                    &ui.painter(),
                                    rename_rect.center(),
                                    if rb.hovered() { pal.text } else { pal.dim },
                                );
                                let rb = rb.on_hover_text("重命名此会话");
                                if rb.clicked() {
                                    state.renaming = Some(meta.file.clone());
                                    state.rename_buf = meta.title.clone();
                                }
                                let del_rect = egui::Rect::from_min_size(
                                    egui::pos2(rect.max.x - 24.0, rect.center().y - 9.0),
                                    egui::vec2(18.0, 18.0),
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
                                    &ui.painter(),
                                    del_rect.center(),
                                    if b.hovered() { pal.text } else { pal.dim },
                                );
                                let b = b.on_hover_text("删除此会话");
                                if b.clicked() {
                                    delete_now = Some(meta.file.clone());
                                }
                            }
                            if resp.clicked() && !is_active {
                                open_now = Some(meta.file.clone());
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
                    ui.label(
                        egui::RichText::new(format!("工作区: {}", state.active_project))
                            .size(11.0)
                            .color(pal.dim),
                    );
                }
            });
        });

    // ── 底部输入区：圆角卡片 + 紧凑工具栏 + 圆形发送按钮（现代输入范式） ──
}
