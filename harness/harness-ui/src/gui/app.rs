//! Top-level eframe layout orchestration.

use super::*;

impl eframe::App for AppState {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_log();
        self.poll_preview();
        self.poll_git_changes();
        self.poll_mem();
        self.poll_models();
        self.poll_optimize();
        self.busy = self.host.sink.busy();

        let pal = palette(self.dark);
        let mut visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = pal.bg;
        visuals.window_fill = pal.panel;
        visuals.extreme_bg_color = pal.field;
        visuals.widgets.noninteractive.bg_fill = pal.panel;
        // 关键：非交互分割线（ui.separator() 等）强制统一为主题 pal.line 发丝线，
        // 根治默认退化为纯灰色 #3c3c3c 的暗黑生硬边框问题。
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, pal.line);
        visuals.window_stroke = egui::Stroke::new(1.0_f32, pal.border);
        visuals.window_rounding = egui::Rounding::same(8.0);
        visuals.widgets.inactive.bg_fill = pal.field;
        visuals.selection.bg_fill = pal.user_bubble;
        // 下拉 / 选择控件统一主题化：按钮底色、描边、悬停、弹出菜单背景与圆角全部跟主题走，
        // 不再使用 egui 默认灰块风格，严格对齐 macOS 规范。
        visuals.menu_rounding = egui::Rounding::same(5.0);
        visuals.widgets.inactive.rounding = egui::Rounding::same(5.0);
        visuals.widgets.hovered.rounding = egui::Rounding::same(5.0);
        visuals.widgets.active.rounding = egui::Rounding::same(5.0);
        visuals.widgets.open.rounding = egui::Rounding::same(5.0);
        visuals.popup_shadow = egui::epaint::Shadow {
            offset: egui::vec2(0.0, 4.0),
            blur: 16.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(if pal.is_dark { 100 } else { 35 }),
        };
        visuals.window_shadow = egui::epaint::Shadow {
            offset: egui::vec2(0.0, 10.0),
            blur: 28.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(if pal.is_dark { 120 } else { 45 }),
        };
        let w_stroke = egui::Stroke::new(1.0_f32, pal.border);
        let w_text = egui::Stroke::new(1.0_f32, pal.text);
        visuals.widgets.inactive.bg_stroke = w_stroke;
        visuals.widgets.inactive.fg_stroke = w_text;
        visuals.widgets.hovered.bg_fill = pal.hover;
        visuals.widgets.hovered.bg_stroke = w_stroke;
        visuals.widgets.hovered.fg_stroke = w_text;
        visuals.widgets.active.bg_fill = pal.hover;
        visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, pal.accent);
        visuals.widgets.active.fg_stroke = w_text;
        visuals.widgets.open.bg_fill = pal.field;
        visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, pal.accent);
        visuals.widgets.open.fg_stroke = w_text;
        ctx.set_visuals(visuals);
        ctx.style_mut(|s| {
            s.spacing.scroll = egui::style::ScrollStyle::floating();
            s.spacing.menu_margin = egui::Margin::symmetric(8.0, 5.0);
        });

        let chrome_colors = crate::window_chrome::ChromeColors {
            fill: pal.head_fill,
            border: pal.head_border,
            line: pal.line,
            text: pal.text,
            dim: pal.dim,
            accent: pal.accent,
            card: pal.card_bg,
            success: pal.success,
            warn: pal.warn,
            #[cfg(target_os = "windows")]
            hover: pal.hover,
            is_dark: pal.is_dark,
        };
        let integrated_titlebar_setting = self.host.settings.get("ui.integrated_titlebar");
        let integrated_titlebar = crate::window_chrome::integrated_titlebar_enabled(
            integrated_titlebar_setting.as_deref(),
        );

        // ── 跟随系统外观动态自适应 ───────────────────────────────
        if let Some(sys_theme) = ctx.system_theme() {
            let theme_pref = self.host.settings.get("ui.theme");
            if theme_pref.as_deref() == Some("system") || theme_pref.is_none() {
                let sys_dark = sys_theme == egui::Theme::Dark;
                if self.dark != sys_dark {
                    self.dark = sys_dark;
                    self.rehighlight_preview();
                }
            }
        }

        // ── macOS 工业级全域快捷键网络 ──────────────────────────
        let (cmd_n, cmd_comma, cmd_b, cmd_w, cmd_f, cmd_1, cmd_2, cmd_3) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::N),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Comma),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::B)
                    || i.consume_key(egui::Modifiers::COMMAND, egui::Key::Backslash),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::W),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::F),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num1),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num2),
                i.consume_key(egui::Modifiers::COMMAND, egui::Key::Num3),
            )
        });

        if cmd_n {
            self.new_session();
        }
        if cmd_comma {
            self.settings_open = !self.settings_open;
        }
        if cmd_b {
            self.sidebar_expanded = !self.sidebar_expanded;
        }
        if cmd_w {
            if self.settings_open {
                self.settings_open = false;
            } else if self.renaming.is_some() {
                self.renaming = None;
            } else if self.lha_open {
                self.lha_open = false;
            } else if self.preview_open {
                self.preview_open = false;
                self.preview_animating = true;
            } else if self.tree_open {
                self.tree_open = false;
            }
        }
        if cmd_f {
            if !self.sidebar_expanded {
                self.sidebar_expanded = true;
            }
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("history_search_input")));
        }
        if cmd_1 {
            self.inspector_tab = 0;
            if !self.preview_open {
                self.preview_open = true;
            }
        }
        if cmd_2 {
            self.inspector_tab = 1;
            if !self.preview_open {
                self.preview_open = true;
            }
        }
        if cmd_3 {
            self.inspector_tab = 2;
            if !self.preview_open {
                self.preview_open = true;
            }
        }

        // ── 原生文件拖拽添加附件支持 (Finder Drag & Drop) ───────
        let dropped_files = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped_files.is_empty() {
            let mut added_names = Vec::new();
            for file in dropped_files {
                if let Some(path) = file.path {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("文件")
                        .to_string();
                    composer::add_attachment(self, path);
                    added_names.push(name);
                }
            }
            if !added_names.is_empty() {
                self.note = format!("已添加拖拽附件：{}", added_names.join(", "));
            }
        }

        let target_sidebar_width = if self.sidebar_expanded { 230.0 } else { 56.0 };
        let sidebar_width = ctx.animate_value_with_time(
            egui::Id::new("sidebar_width_anim"),
            target_sidebar_width,
            0.16,
        );

        let active_project_name = self
            .projects
            .iter()
            .find(|p| p.path == self.active_project)
            .map(|p| p.name.as_str())
            .unwrap_or("默认工作区");
        let active_session_title = self
            .history
            .iter()
            .find(|s| s.file == self.current_session)
            .map(|s| s.title.as_str())
            .unwrap_or("新对话");
        let llm_status = self.host.llm_control.status();
        let wb_ctx = crate::window_chrome::WorkbenchContext {
            project_name: active_project_name,
            session_title: active_session_title,
            model_name: &self.f_model,
            status: &llm_status,
            busy: self.busy,
        };

        let chrome_actions = crate::window_chrome::show(
            ctx,
            chrome_colors,
            self.dark,
            &wb_ctx,
            integrated_titlebar,
            sidebar_width,
            self.tree_open,
            self.preview_open,
            self.sidebar_expanded,
        );
        if chrome_actions.toggle_sidebar {
            self.sidebar_expanded = !self.sidebar_expanded;
        }
        if chrome_actions.toggle_theme {
            self.dark = !self.dark;
            let _ = self
                .host
                .settings
                .set("ui.theme", if self.dark { "dark" } else { "light" });
            // 主题切换后重生成高亮（旧 job 还是旧主题色）。
            self.rehighlight_preview();
        }
        if chrome_actions.toggle_tree {
            self.tree_open = !self.tree_open;
            if self.tree_open && self.tree_needs_reload() {
                self.build_tree();
            }
        }
        if chrome_actions.toggle_inspector {
            self.preview_open = !self.preview_open;
            if !self.preview_open {
                self.preview_animating = true;
            }
        }
        sidebar::show(self, ctx, pal, sidebar_width);
        // 边缘分栏必须先创建；这样输入区与中央会话区共享同一块剩余矩形，
        // 文件树/预览也能自然延伸到窗口底部。
        workspace::show_side_panels(self, ctx, pal);
        let send_now = composer::show(self, ctx, pal);
        workspace::show_main(self, ctx, pal);

        settings_view::show(self, ctx, pal);
        long_horizon_panel::show(self, ctx, pal);

        // ── 会话重命名弹窗 ───────────────────────────────────────
        if let Some(file) = self.renaming.clone() {
            egui::Window::new("重命名会话")
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(
                        "为这个会话设置便于识别的名称（写入旁挂 .title 文件，不影响日志内容）：",
                    );
                    ui.add_space(6.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.rename_buf)
                            .desired_width(300.0)
                            .hint_text("如：发布前的压测排障"),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if widgets::accent_button(ui, &pal, "确定") {
                            if let Some(dir) = self.history_dirs.get(&file).cloned() {
                                harness_session::rename_session(&dir, &file, &self.rename_buf);
                                self.refresh_history();
                            }
                            self.renaming = None;
                        }
                        if widgets::ghost_button(ui, &pal, "取消") {
                            self.renaming = None;
                        }
                    });
                });
        }

        if send_now {
            self.submit();
        }

        crate::window_chrome::handle_resize(ctx, integrated_titlebar);

        // 轮询 SessionLog 需要周期重绘（egui 默认按需重绘）。
        ctx.request_repaint_after(std::time::Duration::from_millis(80));
    }
}
