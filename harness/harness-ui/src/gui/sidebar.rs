//! Left navigation, project list, and session history.

use egui::Color32;

use super::widgets::{animate_interaction, lerp_color, subtle_text_action};
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

            // ── Codex 式醒目主操作：新建会话 ──
            if state.sidebar_expanded {
                let (btn_rect, btn_resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 30.0),
                    egui::Sense::click(),
                );
                let (hover_t, active_t) = animate_interaction(ui, btn_resp.id, &btn_resp);
                let draw_rect = btn_rect.shrink(0.6 * active_t);
                let btn_fill = lerp_color(pal.btn_fill, pal.btn_hover, hover_t);
                let btn_border = lerp_color(pal.btn_border, pal.accent, hover_t * 0.6);
                ui.painter().rect(
                    draw_rect,
                    egui::Rounding::same(5.0),
                    btn_fill,
                    egui::Stroke::new(1.0_f32, btn_border),
                );
                // + 矢量图标
                let ic = draw_rect.left_center() + egui::vec2(16.0, 0.0);
                draw_icon(ui.painter(), ic, Icon::Plus, pal.btn_text);

                ui.painter().text(
                    draw_rect.left_center() + egui::vec2(28.0, 0.0),
                    egui::Align2::LEFT_CENTER,
                    "新建对话",
                    egui::FontId::proportional(12.0),
                    pal.btn_text,
                );
                ui.painter().text(
                    draw_rect.right_center() + egui::vec2(-10.0, 0.0),
                    egui::Align2::RIGHT_CENTER,
                    "⌘N",
                    egui::FontId::proportional(10.0),
                    lerp_color(pal.dim, pal.btn_text, hover_t * 0.5),
                );
                if btn_resp.clicked() {
                    state.new_session();
                }
            } else {
                let (btn_rect, btn_resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), 30.0),
                    egui::Sense::click(),
                );
                let (hover_t, active_t) = animate_interaction(ui, btn_resp.id, &btn_resp);
                let draw_rect = btn_rect.shrink(0.6 * active_t);
                let btn_fill = lerp_color(pal.btn_fill, pal.btn_hover, hover_t);
                let btn_border = lerp_color(pal.btn_border, pal.accent, hover_t * 0.6);
                ui.painter().rect(
                    draw_rect,
                    egui::Rounding::same(5.0),
                    btn_fill,
                    egui::Stroke::new(1.0_f32, btn_border),
                );
                draw_icon(ui.painter(), draw_rect.center(), Icon::Plus, pal.btn_text);
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
                                egui::vec2(ui.available_width(), 28.0),
                                egui::Sense::click(),
                            );
                            let hovered = resp.hovered()
                                || (ctx.input(|i| i.pointer.has_pointer())
                                    && rect.contains(
                                        ctx.input(|i| i.pointer.hover_pos())
                                            .unwrap_or(egui::pos2(-1.0, -1.0)),
                                    ));
                            let proj_id = ui.id().with(("proj_row", &proj.path));
                            let hover_t = ui
                                .ctx()
                                .animate_bool_responsive(proj_id.with("hov"), hovered);
                            let active_t = ui.ctx().animate_bool_responsive(
                                proj_id.with("act"),
                                resp.is_pointer_button_down_on(),
                            );
                            let draw_rect = rect.shrink(0.4 * active_t);

                            // 背景四态：激活底色与半透明悬停自然叠层
                            if is_active {
                                let sel_bg = if pal.is_dark {
                                    Color32::from_white_alpha(18)
                                } else {
                                    Color32::from_black_alpha(12)
                                };
                                ui.painter().rect_filled(draw_rect, 5.0, sel_bg);
                            }
                            if hover_t > 0.001 {
                                ui.painter().rect_filled(
                                    draw_rect,
                                    5.0,
                                    pal.translucent_hover(hover_t),
                                );
                            }
                            if is_active {
                                let bar_h = (draw_rect.height() - 12.0).max(12.0);
                                let bar = egui::Rect::from_min_size(
                                    egui::pos2(
                                        draw_rect.min.x + 2.0,
                                        draw_rect.center().y - bar_h / 2.0,
                                    ),
                                    egui::vec2(2.5, bar_h),
                                );
                                ui.painter().rect_filled(
                                    bar,
                                    egui::Rounding::same(2.0),
                                    pal.accent,
                                );
                            }
                            draw_icon(
                                ui.painter(),
                                egui::pos2(draw_rect.min.x + 14.0, draw_rect.center().y),
                                Icon::Folder,
                                if is_active {
                                    pal.accent
                                } else {
                                    lerp_color(pal.dim, pal.text, hover_t * 0.7)
                                },
                            );
                            ui.painter().text(
                                egui::pos2(draw_rect.min.x + 26.0, draw_rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                &proj.name,
                                egui::FontId::proportional(12.0),
                                if is_active {
                                    pal.text
                                } else {
                                    lerp_color(pal.dim, pal.text, hover_t * 0.8 + 0.2)
                                },
                            );
                            if hover_t > 0.05 {
                                let control_h = sidebar_control_height();
                                let arch_rect = egui::Rect::from_min_size(
                                    egui::pos2(
                                        draw_rect.max.x - control_h - 2.0,
                                        draw_rect.center().y - control_h / 2.0,
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
                        if subtle_text_action(ui, &pal, "清空", "删除全部历史会话（保留当前对话）")
                        {
                            state.clear_history();
                        }
                        if subtle_text_action(
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
                                .size(11.0)
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
                                        .size(11.0)
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
                            .size(11.0)
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
    let (rect, resp) =
        ui.allocate_at_least(egui::vec2(ui.available_width(), 36.0), egui::Sense::click());
    let is_hovered = resp.hovered()
        || (ctx.input(|i| i.pointer.has_pointer())
            && rect.contains(
                ctx.input(|i| i.pointer.hover_pos())
                    .unwrap_or(egui::pos2(-1.0, -1.0)),
            ));
    let row_id = ui.id().with(("hist_row", &meta.file));
    let hover_t = ui
        .ctx()
        .animate_bool_responsive(row_id.with("hov"), is_hovered);
    let active_t = ui
        .ctx()
        .animate_bool_responsive(row_id.with("act"), resp.is_pointer_button_down_on());
    let draw_rect = rect.shrink(0.4 * active_t);

    // 四态半透明无缝叠加（彻底消除同色套娃）
    if is_active {
        let sel_bg = if pal.is_dark {
            Color32::from_white_alpha(18)
        } else {
            Color32::from_black_alpha(12)
        };
        ui.painter().rect_filled(draw_rect, 5.0, sel_bg);
    }
    if hover_t > 0.001 {
        ui.painter()
            .rect_filled(draw_rect, 5.0, pal.translucent_hover(hover_t));
    }
    if is_active {
        let bar_h = (draw_rect.height() - 14.0).max(12.0);
        let bar = egui::Rect::from_min_size(
            egui::pos2(draw_rect.min.x + 2.0, draw_rect.center().y - bar_h / 2.0),
            egui::vec2(2.5, bar_h),
        );
        ui.painter()
            .rect_filled(bar, egui::Rounding::same(2.0), pal.accent);
    }

    // 标题截断
    let mut title: String = meta.title.chars().take(16).collect();
    if meta.title.chars().count() > 16 {
        title.push('…');
    }
    let title_color = if is_active {
        pal.text
    } else {
        lerp_color(pal.dim, pal.text, hover_t * 0.8 + 0.2)
    };
    ui.painter().text(
        egui::pos2(draw_rect.min.x + 10.0, draw_rect.min.y + 11.0),
        egui::Align2::LEFT_CENTER,
        &title,
        egui::FontId::proportional(12.0),
        title_color,
    );
    ui.painter().text(
        egui::pos2(draw_rect.min.x + 10.0, draw_rect.max.y - 10.0),
        egui::Align2::LEFT_CENTER,
        relative_time(&meta.mtime),
        egui::FontId::proportional(10.0),
        pal.dim,
    );

    // 快捷按钮跟随 hover_t 平滑透明度与微滑入
    if hover_t > 0.05 {
        let slide = (1.0 - hover_t) * 4.0;
        let rename_rect = egui::Rect::from_min_size(
            egui::pos2(draw_rect.max.x - 42.0 + slide, draw_rect.center().y - 9.0),
            egui::vec2(18.0, 18.0),
        );
        let rb_id = row_id.with("rename");
        let rb = ui.interact(rename_rect, rb_id, egui::Sense::click());
        let (rb_hov, rb_act) = animate_interaction(ui, rb_id, &rb);
        if rb_hov > 0.01 {
            ui.painter().rect_filled(
                rename_rect.shrink(0.5 * rb_act),
                egui::Rounding::same(4.0),
                pal.translucent_hover(rb_hov * 1.5),
            );
        }
        draw_icon(
            ui.painter(),
            rename_rect.center(),
            Icon::Pencil,
            lerp_color(pal.dim, pal.text, rb_hov),
        );
        if rb.on_hover_text("重命名此会话").clicked() {
            *renaming = Some(meta.file.clone());
            *rename_buf = meta.title.clone();
        }

        let del_rect = egui::Rect::from_min_size(
            egui::pos2(draw_rect.max.x - 22.0 + slide, draw_rect.center().y - 9.0),
            egui::vec2(18.0, 18.0),
        );
        let db_id = row_id.with("delete");
        let db = ui.interact(del_rect, db_id, egui::Sense::click());
        let (db_hov, db_act) = animate_interaction(ui, db_id, &db);
        if db_hov > 0.01 {
            let warn_tint = Color32::from_rgb(0xdc, 0x26, 0x26).gamma_multiply(0.25);
            let hover_bg = lerp_color(pal.translucent_hover(db_hov), warn_tint, db_hov);
            ui.painter().rect_filled(
                del_rect.shrink(0.5 * db_act),
                egui::Rounding::same(4.0),
                hover_bg,
            );
        }
        let del_icon_color = if db.hovered() {
            Color32::from_rgb(0xf8, 0x71, 0x71)
        } else {
            lerp_color(pal.dim, pal.text, db_hov)
        };
        draw_icon(ui.painter(), del_rect.center(), Icon::Trash, del_icon_color);
        if db.on_hover_text("删除此会话").clicked() {
            *delete_now = Some(meta.file.clone());
        }
    }

    if resp.clicked() && !is_active {
        *open_now = Some(meta.file.clone());
    }
}
