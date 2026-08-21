//! Top-level eframe layout orchestration.

use super::*;

impl eframe::App for AppState {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_log();
        self.poll_preview();
        self.poll_mem();
        self.poll_models();
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
        visuals.widgets.inactive.bg_fill = pal.field;
        visuals.selection.bg_fill = pal.user_bubble;
        // 下拉 / 选择控件统一主题化：按钮底色、描边、悬停、弹出菜单背景与圆角全部跟主题走，
        // 不再使用 egui 默认灰块风格。
        visuals.menu_rounding = egui::Rounding::same(8.0);
        visuals.popup_shadow = egui::epaint::Shadow {
            offset: egui::vec2(0.0, 6.0),
            blur: 16.0,
            spread: 0.0,
            color: egui::Color32::from_black_alpha(90),
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

        let chrome_colors = crate::window_chrome::ChromeColors {
            fill: pal.head_fill,
            border: pal.head_border,
            text: pal.text,
            dim: pal.dim,
            accent: pal.accent,
            #[cfg(target_os = "windows")]
            hover: pal.hover,
        };
        let integrated_titlebar_setting = self.host.settings.get("ui.integrated_titlebar");
        let integrated_titlebar = crate::window_chrome::integrated_titlebar_enabled(
            integrated_titlebar_setting.as_deref(),
        );
        let sidebar_width = if self.sidebar_expanded { 220.0 } else { 56.0 };
        let chrome_actions = crate::window_chrome::show(
            ctx,
            chrome_colors,
            self.dark,
            &self.host.llm_control.status(),
            integrated_titlebar,
            sidebar_width,
            self.tree_open,
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
            if self.tree_open && self.tree_root.is_none() {
                self.build_tree();
            }
        }
        sidebar::show(self, ctx, pal, sidebar_width);
        // 边缘分栏必须先创建；这样输入区与中央会话区共享同一块剩余矩形，
        // 文件树/预览也能自然延伸到窗口底部。
        workspace::show_side_panels(self, ctx, pal);
        let send_now = composer::show(self, ctx, pal);
        workspace::show_main(self, ctx, pal);

        settings_view::show(self, ctx, pal);

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
                        if ui.button("确定").clicked() {
                            if let Some(dir) = self.history_dirs.get(&file).cloned() {
                                harness_session::rename_session(&dir, &file, &self.rename_buf);
                                self.refresh_history();
                            }
                            self.renaming = None;
                        }
                        if ui.button("取消").clicked() {
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
