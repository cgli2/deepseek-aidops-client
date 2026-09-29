//! File preview, workspace tree, and Git changes panel behavior.

use std::sync::Arc;

use super::AppState;
use super::icons::{Icon, draw_icon, draw_icon_sized, draw_smooth_spinner};
use super::theme::{Palette, palette};
use super::widgets::{
    TabOption, animate_interaction, badge_pill, close_button, lerp_color, segmented_icon_tabs,
};

fn loading_line(ui: &mut egui::Ui, pal: &Palette, label: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
        draw_smooth_spinner(
            ui.painter(),
            rect.center(),
            6.0,
            pal.accent,
            ui.input(|i| i.time),
        );
        ui.label(egui::RichText::new(label).size(12.0).color(pal.accent));
    });
    ui.ctx().request_repaint();
}

fn telemetry_status_badge(ui: &mut egui::Ui, pal: &Palette, busy: bool) {
    let (label, color) = if busy {
        ("正在执行", pal.accent)
    } else {
        ("就绪空闲", pal.success)
    };
    let width = label.chars().count() as f32 * 7.0 + 29.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 20.0), egui::Sense::hover());
    ui.painter().rect(
        rect,
        egui::Rounding::same(10.0),
        color.gamma_multiply(0.15),
        egui::Stroke::new(1.0, color.gamma_multiply(0.7)),
    );
    let icon_center = egui::pos2(rect.left() + 10.0, rect.center().y);
    if busy {
        draw_smooth_spinner(
            ui.painter(),
            icon_center,
            4.5,
            color,
            ui.input(|input| input.time),
        );
        ui.ctx().request_repaint();
    } else {
        draw_icon_sized(ui.painter(), icon_center, Icon::CircleDot, color, 10.0);
    }
    ui.painter().text(
        egui::pos2(rect.left() + 19.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(10.5),
        color,
    );
}

fn panel_tool_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: Option<Icon>,
    label: &str,
    tip: &str,
) -> bool {
    let char_w = label
        .chars()
        .map(|c| if c.is_ascii() { 6.8 } else { 11.5 })
        .sum::<f32>();
    let icon_w = if icon.is_some() { 16.0 } else { 0.0 };
    let w = char_w + icon_w + 14.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), egui::Sense::click());
    let (hov, act) = animate_interaction(ui, resp.id, &resp);
    let draw_rect = rect.shrink(0.4 * act);
    if hov > 0.001 {
        ui.painter().rect_filled(
            draw_rect,
            egui::Rounding::same(5.0),
            pal.translucent_hover(hov),
        );
    }
    let border_color = if hov > 0.05 {
        lerp_color(pal.border, pal.accent, hov * 0.4)
    } else {
        pal.border
    };
    ui.painter().rect(
        draw_rect,
        egui::Rounding::same(5.0),
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(1.0, border_color),
    );
    let mut text_x = draw_rect.left() + 7.0;
    if let Some(ic) = icon {
        let ic_c = egui::pos2(draw_rect.left() + 11.0, draw_rect.center().y);
        draw_icon(ui.painter(), ic_c, ic, lerp_color(pal.dim, pal.text, hov));
        text_x += 16.0;
    }
    ui.painter().text(
        egui::pos2(text_x, draw_rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(11.5),
        lerp_color(pal.dim, pal.text, hov * 0.8),
    );
    resp.on_hover_text(tip).clicked()
}

impl AppState {
    /// 打开文件预览窗并加载指定文件。
    ///
    /// 命中缓存时立即打开；未命中时也**立即打开面板**（面板内显示"加载中…"），
    /// 内容就绪后原地更新——避免面板在异步返回后"空降"，导致中央消息流宽度突变闪烁。
    pub(super) fn open_preview(&mut self, path: String) {
        self.inspector_tab = 0;
        self.preview_path = Some(path.clone());
        self.preview_mode = if crate::preview::is_markdown_path(&path) {
            crate::preview::PreviewMode::Markdown
        } else {
            crate::preview::PreviewMode::Source
        };
        self.preview_diff = None;
        self.preview_tracked = false;
        // 面板立即打开：内容未就绪前渲染"加载中…"占位，稳定面板宽度。
        self.preview_open = true;
        if let Some((content, truncated)) = self.preview_cache.get(&path).cloned() {
            // 缓存命中也要重建语法高亮：否则沿用上一个文件的高亮 job，内容与高亮错乱。
            let file_name = std::path::Path::new(&path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file.txt");
            self.preview_highlight = Some(crate::highlight::highlight_to_job(
                &content,
                file_name,
                self.dark,
                egui::Color32::TRANSPARENT,
                palette(self.dark).dim,
                f32::INFINITY,
            ));
            self.preview_content = Some(content);
            self.preview_truncated = truncated;
            self.preview_error = None;
            // 缓存只存了内容；diff / tracked 仍需异步加载（已跟踪/未跟踪都算）。
            self.load_preview(path);
            return;
        }
        self.preview_content = None;
        self.preview_error = None;
        self.preview_truncated = false;
        self.preview_highlight = None;
        // 面板已立即打开；等 poll_preview 内容就绪后原地更新。
        self.load_preview(path);
    }

    /// 异步加载文件内容 + git 跟踪状态 + diff（复用 UiRuntime 独立线程模式）。
    ///
    /// 路径探测：气泡里模型写的路径通常相对「仓库根」，而 fs 沙箱根（Workspace）
    /// 可能落在仓库子目录（如 exe 位于 `harness/dist` 时根是 `.../harness`）。
    /// 因此从沙箱根开始逐级向父目录拼接候选绝对路径，第一个读得动的即命中。
    pub(super) fn load_preview(&mut self, path: String) {
        let fs = self.host.fs.clone();
        let git = self.host.git.clone();
        // 回传请求路径：poll_preview 据此丢弃过期结果（快速切换文件时防污染）。
        let req_path = path.clone();
        // 当前选中的项目是 UI 文件浏览的唯一事实源。settings 只负责持久化，
        // 不能在切换路径中反向充当运行时状态，否则写入异常时会回读旧项目。
        let ws_root = self.active_workspace_root();
        let handle = self.host.rt.handle();
        let (tx, rx) = std::sync::mpsc::channel::<crate::preview::PreviewLoadResult>();
        self.preview_rx = Some(rx);
        std::thread::spawn(move || {
            let res = handle.block_on(async move {
                let candidates = crate::preview::candidate_abs_paths(&ws_root, &path);
                let mut content: Option<harness_core::error::Result<String>> = None;
                let mut resolved: Option<std::path::PathBuf> = None;
                for cand in &candidates {
                    match fs.read(cand).await {
                        Ok(c) => {
                            content = Some(Ok(c));
                            resolved = Some(cand.clone());
                            break;
                        }
                        Err(e) => content = Some(Err(e)),
                    }
                }
                // 相对文件名（如 `memory_panel.rs`）直接拼接工作区根找不到时，
                // 在工作区内按文件名受限搜索，命中即作为最终候选读取。
                // 仅当路径是「裸文件名或很浅的相对路径」时搜索，避免对深层路径误搜。
                if resolved.is_none() {
                    let basename = std::path::Path::new(&path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    if !basename.is_empty() && !path.contains('/') && !path.contains('\\') {
                        if let Some(found) = crate::preview::find_by_filename(&ws_root, basename) {
                            match fs.read(&found).await {
                                Ok(c) => {
                                    content = Some(Ok(c));
                                    resolved = Some(found);
                                }
                                Err(e) => content = Some(Err(e)),
                            }
                        }
                    }
                }
                let tracked = resolved
                    .as_ref()
                    .map(|p| git.is_tracked(&p.display().to_string()).unwrap_or(false))
                    .unwrap_or(false);
                // diff 内容：
                // - 已跟踪文件 → git diff（可能有实际修改，也可能为空）
                // - 未跟踪文件（is_tracked=false 但读取成功）→ 整文件作为新增行
                //   （git diff 对未跟踪文件恒为空，全新增展示才符合预期）
                let diff = if tracked {
                    resolved
                        .as_ref()
                        .and_then(|p| git.diff_path(&p.display().to_string()).ok())
                        .filter(|d| !d.trim().is_empty())
                } else {
                    // content: Option<Result<String>>，取 Ok 分支的内容。
                    content.as_ref().and_then(|r| r.as_ref().ok()).map(|c| {
                        c.lines()
                            .map(|l| format!("+{l}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                };
                let has_diff = diff.is_some();
                // tracked 语义 =「有 diff 可看」：未跟踪文件的"全新增 diff"也算，
                // 这样预览窗会显示 Diff tab（源码 / Diff 切换可审查新增内容）。
                crate::preview::PreviewLoadResult {
                    path: req_path,
                    content: content.unwrap_or_else(|| {
                        Err(harness_core::error::Error::Io(std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            format!("file not found after probing {candidates:?}"),
                        )))
                    }),
                    diff,
                    tracked: has_diff,
                }
            });
            let _ = tx.send(res);
        });
    }

    /// 每帧轮询预览加载结果（非阻塞：try_recv 不等待）。
    pub(super) fn poll_preview(&mut self) {
        let path = self.preview_path.clone();
        if let Some(rx) = &self.preview_rx {
            match rx.try_recv() {
                Ok(res) => {
                    // 过期结果守卫：快速连续点击不同文件时，旧请求可能晚到。
                    // 只应用与当前预览路径一致的结果，其余直接丢弃（并清空 rx 防残留）。
                    if self.preview_path.as_ref() != Some(&res.path) {
                        self.preview_rx = None;
                        return;
                    }
                    self.preview_rx = None;
                    let cur_path = self.preview_path.clone();
                    match res.content {
                        Ok(content) => {
                            if crate::preview::is_binary(&content) {
                                self.preview_error = Some("二进制文件，无法预览".into());
                                self.preview_content = None;
                            } else {
                                let (text, truncated) = crate::preview::truncate_content(&content);
                                self.preview_content = Some(text.clone());
                                self.preview_truncated = truncated;
                                self.preview_error = None;
                                // 生成语法高亮 LayoutJob（一次性，渲染零成本）。
                                let file_name = cur_path
                                    .as_ref()
                                    .and_then(|p| std::path::Path::new(p).file_name())
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("file.txt");
                                self.preview_highlight = Some(crate::highlight::highlight_to_job(
                                    &text,
                                    file_name,
                                    self.dark,
                                    egui::Color32::TRANSPARENT,
                                    palette(self.dark).dim,
                                    f32::INFINITY,
                                ));
                                // 写入缓存：同一文件重复点击秒开，不重新加载。
                                if let Some(p) = &cur_path {
                                    self.preview_cache.insert(p.clone(), (text, truncated));
                                }
                            }
                        }
                        Err(e) => {
                            self.preview_error = Some(format!("{e}"));
                            self.preview_content = None;
                        }
                    }
                    self.preview_tracked = res.tracked;
                    self.preview_diff = res.diff;
                    // 面板已在 open_preview 时立即打开；这里只更新内容，不再触发打开，
                    // 避免面板"空降"导致中央消息流宽度突变闪烁。
                    // 若用户已主动关闭（切到别处），则不强开。
                    let _ = self.preview_open;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // 仍在加载中，下一帧再查
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.preview_rx = None;
                    self.preview_error = Some("加载失败：后台任务异常退出".into());
                }
            }
        }
        let _ = path;
    }

    /// 渲染协同检查器与预览窗（右侧 SidePanel 分隔面板：自绘头部 + 内容滚动区）。
    pub(super) fn render_preview(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        // 闪烁缓解：面板打开瞬间用透明度淡入（约 0.15s），
        // 中央消息流宽度突变被淡入柔化，减轻视觉冲击。
        let fade = ui
            .ctx()
            .animate_bool(egui::Id::new("preview_fade"), self.preview_open);
        if fade < 0.98 {
            ui.set_opacity(fade);
        }

        // ── 顶部栏：分段式 Tab 切换器 + 关闭按钮 ──
        let head_h = 36.0;
        egui::Frame::default()
            .fill(pal.head_fill)
            .inner_margin(egui::Margin::symmetric(8.0, 5.0))
            .show(ui, |ui| {
                ui.set_min_height(head_h - 10.0);
                ui.horizontal(|ui| {
                    let tabs = [
                        TabOption::new(Some(Icon::FileText), "文件预览"),
                        TabOption::new(Some(Icon::GitBranch), "代码变更"),
                        TabOption::new(Some(Icon::Activity), "运行时遥测"),
                    ];
                    if let Some(new_tab) = segmented_icon_tabs(ui, pal, &tabs, self.inspector_tab) {
                        self.inspector_tab = new_tab;
                        if new_tab == 1 && !self.git_loaded {
                            self.refresh_git_changes();
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if close_button(ui, pal) {
                            self.preview_open = false;
                            // 触发关闭滑出动画（面板继续渲染直到宽度缩回 0）。
                            self.preview_animating = true;
                        }
                    });
                });
            });

        // 头部下方分隔线
        let sep = ui
            .allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover())
            .0;
        ui.painter().rect_filled(sep, 0.0, pal.border);

        // 内容区按激活 Tab 分发
        match self.inspector_tab {
            0 => self.render_file_preview_tab(ui, pal),
            1 => self.render_git_diff_tab(ui, pal),
            _ => self.render_telemetry_tab(ui, pal),
        }
    }

    /// 选项卡 1：专业级代码与文档预览
    fn render_file_preview_tab(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let is_markdown = self
            .preview_path
            .as_deref()
            .map(crate::preview::is_markdown_path)
            .unwrap_or(false);
        let cur_path = self.preview_path.clone();
        let cur_content = self.preview_content.clone();
        let ws_root = self.active_workspace_root();

        // 工具栏
        egui::Frame::default()
            .fill(pal.head_fill.gamma_multiply(0.5))
            .inner_margin(egui::Margin::symmetric(10.0, 5.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(path) = &cur_path {
                        let name = std::path::Path::new(path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(path.as_str());
                        let lang = file_lang_label(path);
                        badge_pill(
                            ui,
                            lang,
                            pal.accent,
                            pal.accent.gamma_multiply(0.12),
                            pal.accent.gamma_multiply(0.35),
                        );
                        let name_trunc: String = name.chars().take(22).collect();
                        let name_disp = if name.chars().count() > 22 {
                            format!("{name_trunc}…")
                        } else {
                            name_trunc
                        };
                        ui.label(
                            egui::RichText::new(&name_disp)
                                .size(12.0)
                                .strong()
                                .color(pal.text),
                        );

                        if let Some(content) = &cur_content {
                            let line_count = content.lines().count();
                            let kb = content.len() as f32 / 1024.0;
                            ui.label(
                                egui::RichText::new(format!("{line_count} 行 · {kb:.1} KB"))
                                    .size(11.0)
                                    .color(pal.dim),
                            );
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let abs = tree_attachment_path(&ws_root, path);
                            if panel_tool_button(
                                ui,
                                pal,
                                Some(Icon::ExternalLink),
                                "打开",
                                "在操作系统默认编辑器中打开该文件",
                            ) {
                                open_in_system_editor(&abs);
                                self.note = format!("已在系统编辑器中打开 {name}");
                            }

                            if let Some(content) = &cur_content {
                                if panel_tool_button(
                                    ui,
                                    pal,
                                    Some(Icon::Copy),
                                    "复制全部",
                                    "复制当前文件全部源码到剪贴板",
                                ) {
                                    ui.ctx().copy_text(content.clone());
                                    self.note = "已复制文件全部内容到剪贴板".into();
                                }
                            }

                            if is_markdown {
                                if ui
                                    .add(egui::SelectableLabel::new(
                                        self.preview_mode == crate::preview::PreviewMode::Source,
                                        egui::RichText::new("源码").size(11.0),
                                    ))
                                    .clicked()
                                {
                                    self.preview_mode = crate::preview::PreviewMode::Source;
                                }
                                if ui
                                    .add(egui::SelectableLabel::new(
                                        self.preview_mode == crate::preview::PreviewMode::Markdown,
                                        egui::RichText::new("预览").size(11.0),
                                    ))
                                    .clicked()
                                {
                                    self.preview_mode = crate::preview::PreviewMode::Markdown;
                                }
                            }
                        });
                    } else {
                        ui.label(egui::RichText::new("未选择文件").size(11.5).color(pal.dim));
                    }
                });
            });

        let sep = ui
            .allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover())
            .0;
        ui.painter().rect_filled(sep, 0.0, pal.border);

        // 内容区
        let avail_h = ui.available_height().max(120.0);
        egui::ScrollArea::both()
            .id_salt("file_preview_scroll")
            .auto_shrink(false)
            .max_height(avail_h)
            .show(ui, |ui| {
                if self.preview_path.is_none() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(50.0);
                        let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
                        draw_icon_sized(ui.painter(), icon_rect.center(), Icon::Code, pal.dim, 22.0);
                        ui.add_space(12.0);
                        ui.label(egui::RichText::new("暂无打开的文件预览").size(13.0).strong().color(pal.text));
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("在左侧项目文件树、Git 变更或对话消息中点击文件，\n即可在此进行带行号与语法高亮的代码审查。")
                                .size(11.5)
                                .color(pal.dim),
                        );
                    });
                    return;
                }

                if self.preview_content.is_none()
                    && self.preview_error.is_none()
                    && self.preview_rx.is_some()
                {
                    ui.add_space(30.0);
                    loading_line(ui, pal, "正在加载文件内容...");
                    return;
                }

                if let Some(err) = &self.preview_error {
                    ui.add_space(20.0);
                    ui.horizontal(|ui| {
                        let (icon_rect, _) =
                            ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                        draw_icon_sized(
                            ui.painter(),
                            icon_rect.center(),
                            Icon::AlertTriangle,
                            pal.err_text,
                            14.0,
                        );
                        ui.label(
                            egui::RichText::new(err)
                                .size(12.0)
                                .color(pal.err_text),
                        );
                    });
                    return;
                }

                match self.preview_mode {
                    crate::preview::PreviewMode::Markdown => {
                        if let Some(content) = &self.preview_content {
                            if self.preview_truncated {
                                ui.label(
                                    egui::RichText::new("文件过大，仅显示前 512KB")
                                        .size(11.0)
                                        .color(pal.warn),
                                );
                                ui.add_space(4.0);
                            }
                            let width = (ui.available_width() - 24.0).max(80.0);
                            let job = crate::markdown::to_job(
                                content,
                                &crate::markdown::MdTheme {
                                    text: pal.text,
                                    dim: pal.dim,
                                    accent: pal.accent,
                                    code_text: pal.text,
                                    code_bg: pal.field,
                                },
                                width,
                            );
                            egui::Frame::default()
                                .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                                .show(ui, |ui| {
                                    ui.add(egui::Label::new(job).selectable(true));
                                });
                        }
                    }
                    _ => {
                        if self.preview_content.is_some() {
                            if self.preview_truncated {
                                ui.label(
                                    egui::RichText::new("文件过大，仅显示前 512KB")
                                        .size(11.0)
                                        .color(pal.warn),
                                );
                                ui.add_space(4.0);
                            }
                            let job = self
                                .preview_highlight
                                .clone()
                                .unwrap_or_else(|| egui::text::LayoutJob::default());
                            let _ = ui.add(egui::Label::new(job).selectable(true));
                        }
                    }
                }
            });
    }

    /// 选项卡 2：全功能 Git Diff 变更集检查器
    fn render_git_diff_tab(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        // 工具栏
        egui::Frame::default()
            .fill(pal.head_fill.gamma_multiply(0.5))
            .inner_margin(egui::Margin::symmetric(10.0, 5.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let branch = if self.git_branch.is_empty() {
                        "HEAD"
                    } else {
                        &self.git_branch
                    };
                    let branch_w = branch.len() as f32 * 6.8 + 30.0;
                    let (b_rect, _) =
                        ui.allocate_exact_size(egui::vec2(branch_w, 20.0), egui::Sense::hover());
                    ui.painter().rect(
                        b_rect,
                        egui::Rounding::same(4.0),
                        pal.warn.gamma_multiply(0.12),
                        egui::Stroke::new(1.0, pal.warn.gamma_multiply(0.35)),
                    );
                    let ic_c = egui::pos2(b_rect.left() + 10.0, b_rect.center().y);
                    draw_icon(ui.painter(), ic_c, Icon::GitBranch, pal.warn);
                    ui.painter().text(
                        egui::pos2(b_rect.left() + 20.0, b_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        branch,
                        egui::FontId::proportional(11.0),
                        pal.warn,
                    );
                    badge_pill(
                        ui,
                        &format!("{} 处变更", self.git_changes.len()),
                        pal.text,
                        pal.field,
                        pal.border,
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if panel_tool_button(
                            ui,
                            pal,
                            Some(Icon::RefreshCw),
                            "刷新",
                            "重新读取 Git 工作区与未暂存变更",
                        ) {
                            self.refresh_git_changes();
                        }
                    });
                });
            });

        let sep = ui
            .allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover())
            .0;
        ui.painter().rect_filled(sep, 0.0, pal.border);

        let avail_h = ui.available_height().max(120.0);
        egui::ScrollArea::both()
            .id_salt("git_diff_inspector_scroll")
            .auto_shrink(false)
            .max_height(avail_h)
            .show(ui, |ui| {
                if !self.git_loaded {
                    ui.add_space(30.0);
                    loading_line(ui, pal, "正在查询 Git 状态...");
                    return;
                }

                if let Some(err) = &self.git_error {
                    ui.add_space(20.0);
                    if err.contains("not a git repository") {
                        ui.label(
                            egui::RichText::new("当前目录不是 Git 仓库或尚未初始化")
                                .size(12.0)
                                .color(pal.dim),
                        );
                    } else {
                        ui.label(
                            egui::RichText::new(format!("Git 状态读取失败：{err}"))
                                .size(12.0)
                                .color(pal.err_text),
                        );
                    }
                    return;
                }

                if self.git_changes.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(50.0);
                        let (icon_rect, _) =
                            ui.allocate_exact_size(egui::vec2(40.0, 40.0), egui::Sense::hover());
                        draw_icon(
                            &ui.painter(),
                            icon_rect.center(),
                            Icon::CheckCircle,
                            pal.success,
                        );
                        ui.add_space(12.0);
                        ui.label(
                            egui::RichText::new("工作区代码整洁")
                                .size(13.0)
                                .strong()
                                .color(pal.success),
                        );
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("暂无任何未提交或已修改的代码变更。")
                                .size(11.5)
                                .color(pal.dim),
                        );
                    });
                    return;
                }

                // 变更文件列表
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("变更文件列表（点击切换查看 Diff）:")
                        .size(11.0)
                        .strong()
                        .color(pal.dim),
                );
                ui.add_space(4.0);

                let mut open_diff = None;
                for ch in &self.git_changes {
                    let (mark, mcolor) = match ch.marker() {
                        "M" => ("M 修改", pal.warn),
                        "A" => ("A 新增", pal.success),
                        "D" => ("D 删除", pal.err_text),
                        "R" => ("R 重命名", pal.accent),
                        _ => ("? 未跟踪", pal.dim),
                    };
                    let is_active = self.preview_path.as_deref() == Some(&ch.path);
                    let row_h = 24.0;
                    let (rect, resp) = ui.allocate_at_least(
                        egui::vec2(ui.available_width(), row_h),
                        egui::Sense::click(),
                    );
                    if resp.hovered() || is_active {
                        ui.painter().rect_filled(
                            rect.shrink(1.0),
                            egui::Rounding::same(4.0),
                            pal.hover,
                        );
                    }
                    if is_active {
                        let bar = egui::Rect::from_min_size(
                            egui::pos2(rect.min.x + 2.0, rect.min.y + 4.0),
                            egui::vec2(2.5, rect.height() - 8.0),
                        );
                        ui.painter()
                            .rect_filled(bar, egui::Rounding::same(2.0), pal.accent);
                    }

                    // 标记 pill
                    let mark_rect = egui::Rect::from_min_size(
                        egui::pos2(rect.min.x + 8.0, rect.min.y + 3.0),
                        egui::vec2(48.0, 18.0),
                    );
                    ui.painter().rect_filled(
                        mark_rect,
                        egui::Rounding::same(4.0),
                        mcolor.gamma_multiply(0.18),
                    );
                    ui.painter().text(
                        mark_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        mark,
                        egui::FontId::monospace(10.0),
                        mcolor,
                    );

                    // 路径文本
                    ui.painter().text(
                        egui::pos2(rect.min.x + 62.0, rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        &ch.path,
                        egui::FontId::monospace(11.0),
                        if is_active { pal.text } else { pal.dim },
                    );

                    if resp.clicked() {
                        open_diff = Some(ch.path.clone());
                    }
                    ui.add_space(2.0);
                }

                if let Some(path) = open_diff {
                    self.open_preview(path);
                    self.preview_mode = crate::preview::PreviewMode::Diff;
                }

                // Diff 详情区
                ui.add_space(10.0);
                let sep2 = ui
                    .allocate_exact_size(
                        egui::vec2(ui.available_width(), 1.0),
                        egui::Sense::hover(),
                    )
                    .0;
                ui.painter().rect_filled(sep2, 0.0, pal.border);
                ui.add_space(6.0);

                if let Some(diff) = &self.preview_diff {
                    let active_name = self.preview_path.as_deref().unwrap_or("Diff");
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("差异对比: {active_name}"))
                                .size(11.5)
                                .strong()
                                .color(pal.text),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if panel_tool_button(
                                ui,
                                pal,
                                Some(Icon::Copy),
                                "复制 Diff",
                                "复制当前文件的 Unified Diff 补丁内容",
                            ) {
                                ui.ctx().copy_text(diff.clone());
                                self.note = "已复制 Diff 内容到剪贴板".into();
                            }
                        });
                    });
                    ui.add_space(4.0);

                    render_diff_viewer(ui, pal, diff);
                } else if self.preview_rx.is_some() {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("正在加载 Diff 对比...")
                            .size(11.5)
                            .color(pal.dim),
                    );
                } else {
                    ui.add_space(16.0);
                    ui.label(
                        egui::RichText::new("请点击上方变更文件列表，查看详细代码增删对比。")
                            .size(11.5)
                            .color(pal.dim),
                    );
                }
            });
    }

    /// 选项卡 3：运行时状态全景与遥测 HUD (Runtime Telemetry HUD)
    fn render_telemetry_tab(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        // 工具栏
        egui::Frame::default()
            .fill(pal.head_fill.gamma_multiply(0.5))
            .inner_margin(egui::Margin::symmetric(10.0, 5.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    badge_pill(
                        ui,
                        "DAG 运行时遥测",
                        pal.accent,
                        pal.accent.gamma_multiply(0.12),
                        pal.accent.gamma_multiply(0.35),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        telemetry_status_badge(ui, pal, self.busy);
                    });
                });
            });

        let sep = ui
            .allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover())
            .0;
        ui.painter().rect_filled(sep, 0.0, pal.border);

        let avail_h = ui.available_height().max(120.0);
        egui::ScrollArea::vertical()
            .id_salt("telemetry_inspector_scroll")
            .auto_shrink(false)
            .max_height(avail_h)
            .show(ui, |ui| {
                ui.add_space(8.0);

                if let Some(projection) = &self.execution_projection {
                    // 1. 意图与执行阶段
                    egui::Frame::default()
                        .fill(pal.card_bg)
                        .rounding(egui::Rounding::same(8.0))
                        .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
                        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                draw_icon(ui.painter(), icon_rect.center(), Icon::Target, pal.accent);
                                ui.label(
                                    egui::RichText::new("执行意图与阶段目标")
                                        .size(12.0)
                                        .strong()
                                        .color(pal.text),
                                );
                            });
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                badge_pill(
                                    ui,
                                    &projection.intent,
                                    pal.accent,
                                    pal.accent.gamma_multiply(0.12),
                                    pal.accent.gamma_multiply(0.35),
                                );
                                ui.label(
                                    egui::RichText::new(format!(
                                        "阶段: {} · 第 {} 步",
                                        projection.phase, projection.step
                                    ))
                                    .size(11.5)
                                    .color(pal.text),
                                );
                            });
                            if !projection.goal.is_empty() {
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(format!("目标: {}", projection.goal))
                                        .size(11.0)
                                        .color(pal.dim),
                                );
                            }
                        });

                    ui.add_space(8.0);

                    // 2. 门禁与验证指标
                    egui::Frame::default()
                        .fill(pal.card_bg)
                        .rounding(egui::Rounding::same(8.0))
                        .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
                        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                draw_icon(ui.painter(), icon_rect.center(), Icon::ShieldCheck, pal.accent);
                                ui.label(
                                    egui::RichText::new("门禁与验证指标")
                                        .size(12.0)
                                        .strong()
                                        .color(pal.text),
                                );
                            });
                            ui.add_space(6.0);
                            ui.horizontal_wrapped(|ui| {
                                badge_pill(
                                    ui,
                                    &format!("已验证: {}", projection.verified_count),
                                    pal.success,
                                    pal.success.gamma_multiply(0.12),
                                    pal.success.gamma_multiply(0.35),
                                );
                                badge_pill(
                                    ui,
                                    &format!("阻塞中: {}", projection.blocked_count),
                                    pal.warn,
                                    pal.warn.gamma_multiply(0.12),
                                    pal.warn.gamma_multiply(0.35),
                                );
                                badge_pill(
                                    ui,
                                    &format!("无信息: {}", projection.no_information_count),
                                    pal.dim,
                                    pal.hover,
                                    pal.border,
                                );
                                badge_pill(
                                    ui,
                                    &format!("校正中: {}", projection.correction_count),
                                    pal.purple,
                                    pal.purple.gamma_multiply(0.12),
                                    pal.purple.gamma_multiply(0.35),
                                );
                            });

                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "工具调用: {} 次 · 收集证据: {} 项",
                                        projection.tool_calls, projection.evidence_count
                                    ))
                                    .size(11.0)
                                    .color(pal.dim),
                                );
                            });
                        });

                    ui.add_space(8.0);

                    // 3. 当前假设与活跃工作项
                    egui::Frame::default()
                        .fill(pal.card_bg)
                        .rounding(egui::Rounding::same(8.0))
                        .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
                        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                draw_icon(ui.painter(), icon_rect.center(), Icon::Sparkles, pal.accent);
                                ui.label(
                                    egui::RichText::new("当前工作项与假设")
                                        .size(12.0)
                                        .strong()
                                        .color(pal.text),
                                );
                            });
                            ui.add_space(6.0);
                            ui.horizontal(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                                draw_icon(ui.painter(), icon_rect.center(), Icon::Wrench, pal.text);
                                ui.label(
                                    egui::RichText::new(format!("当前工作: {}", projection.active_work_item))
                                        .size(11.5)
                                        .color(pal.text),
                                );
                            });
                            ui.add_space(2.0);
                            ui.horizontal(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                                draw_icon(ui.painter(), icon_rect.center(), Icon::Brain, pal.dim);
                                ui.label(
                                    egui::RichText::new(format!("待验假设: {}", projection.active_hypothesis))
                                        .size(11.0)
                                        .color(pal.dim),
                                );
                            });
                        });

                    ui.add_space(8.0);

                    // 4. 工作项清单分解
                    if !projection.work_items.is_empty() {
                        egui::Frame::default()
                            .fill(pal.card_bg)
                            .rounding(egui::Rounding::same(8.0))
                            .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
                            .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                    draw_icon(ui.painter(), icon_rect.center(), Icon::ListTree, pal.accent);
                                    ui.label(
                                        egui::RichText::new("DAG 工作项分解")
                                            .size(12.0)
                                            .strong()
                                            .color(pal.text),
                                    );
                                });
                                ui.add_space(4.0);
                                for item in &projection.work_items {
                                    ui.horizontal(|ui| {
                                        badge_pill(
                                            ui,
                                            &item.state,
                                            pal.accent,
                                            pal.accent.gamma_multiply(0.12),
                                            pal.accent.gamma_multiply(0.35),
                                        );
                                        ui.label(
                                            egui::RichText::new(&item.id)
                                                .size(11.5)
                                                .strong()
                                                .color(pal.text),
                                        );
                                        ui.label(
                                            egui::RichText::new(format!("(证据 {})", item.evidence_count))
                                                .size(11.0)
                                                .color(pal.dim),
                                        );
                                    });
                                    ui.add_space(2.0);
                                }
                            });
                        ui.add_space(8.0);
                    }

                    // 5. 允许调用的工具
                    egui::Frame::default()
                        .fill(pal.card_bg)
                        .rounding(egui::Rounding::same(8.0))
                        .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
                        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                draw_icon(ui.painter(), icon_rect.center(), Icon::Key, pal.accent);
                                ui.label(
                                    egui::RichText::new("门禁允许工具")
                                        .size(12.0)
                                        .strong()
                                        .color(pal.text),
                                );
                            });
                            ui.add_space(4.0);
                            if projection.allowed_tools.is_empty() {
                                ui.label(egui::RichText::new("无（任务进入收尾验收阶段）").size(11.0).color(pal.dim));
                            } else {
                                ui.horizontal_wrapped(|ui| {
                                    for tool in &projection.allowed_tools {
                                        badge_pill(
                                            ui,
                                            tool,
                                            pal.text,
                                            pal.field,
                                            pal.border,
                                        );
                                    }
                                });
                            }
                        });
                } else {
                    // 空闲或普通对话状态
                    let usage = self.log.usage_total();
                    egui::Frame::default()
                        .fill(pal.card_bg)
                        .rounding(egui::Rounding::same(8.0))
                        .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
                        .inner_margin(egui::Margin::symmetric(12.0, 12.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                draw_icon(ui.painter(), icon_rect.center(), Icon::BarChart, pal.accent);
                                ui.label(
                                    egui::RichText::new("会话资源消耗统计")
                                        .size(12.0)
                                        .strong()
                                        .color(pal.text),
                                );
                            });
                            ui.add_space(8.0);
                            ui.horizontal_wrapped(|ui| {
                                badge_pill(
                                    ui,
                                    &format!("提示 Tokens: {}", usage.prompt_tokens),
                                    pal.accent,
                                    pal.accent.gamma_multiply(0.12),
                                    pal.accent.gamma_multiply(0.35),
                                );
                                badge_pill(
                                    ui,
                                    &format!("补全 Tokens: {}", usage.completion_tokens),
                                    pal.purple,
                                    pal.purple.gamma_multiply(0.12),
                                    pal.purple.gamma_multiply(0.35),
                                );
                                badge_pill(
                                    ui,
                                    &format!("总计: {}", usage.prompt_tokens + usage.completion_tokens),
                                    pal.text,
                                    pal.field,
                                    pal.border,
                                );
                            });

                            ui.add_space(14.0);
                            ui.horizontal(|ui| {
                                let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                                draw_icon(ui.painter(), icon_rect.center(), Icon::Sparkles, pal.dim);
                                ui.label(
                                    egui::RichText::new("智能体遥测提示")
                                        .size(11.5)
                                        .strong()
                                        .color(pal.dim),
                                );
                            });
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(
                                    "当前会话处于标准对话交互就绪状态。当 Agent 执行复杂自主排障、\n长时程决策或代码重构时，DAG 任务编排、假设验证与运行时门禁\n将在此面板实时投射呈现，避免垂直空间挤占对话流。"
                                )
                                .size(11.0)
                                .color(pal.dim),
                            );
                        });
                }
            });
    }

    // ── 文件树 ──────────────────────────────────────────────────

    /// 重新生成预览高亮（主题切换时调用）。
    pub(super) fn rehighlight_preview(&mut self) {
        let Some(content) = self.preview_content.clone() else {
            return;
        };
        let file_name = self
            .preview_path
            .as_ref()
            .and_then(|p| std::path::Path::new(p).file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("file.txt");
        self.preview_highlight = Some(crate::highlight::highlight_to_job(
            &content,
            file_name,
            self.dark,
            egui::Color32::TRANSPARENT,
            palette(self.dark).dim,
            f32::INFINITY,
        ));
    }

    /// 异步刷新 Git 变更（分支名 + 变更文件列表，含状态码）。
    pub(super) fn refresh_git_changes(&mut self) {
        let git = self.host.git.clone();
        let workspace = self.active_workspace_root();
        self.git_generation = self.git_generation.wrapping_add(1);
        let generation = self.git_generation;
        self.git_workspace = workspace.clone();
        self.git_loaded = false;
        self.git_error = None;
        let (tx, rx) = std::sync::mpsc::channel::<super::app_state::GitRefreshResult>();
        self.git_rx = Some(rx);
        std::thread::spawn(move || {
            let result = (|| {
                let repo_root = git.repository_root().map_err(|error| error.to_string())?;
                let branch = git.current_branch().map_err(|error| error.to_string())?;
                let changes = git.changed_files().map_err(|error| error.to_string())?;
                Ok(super::app_state::GitRefreshData {
                    repo_root: repo_root.display().to_string(),
                    branch,
                    changes,
                })
            })();
            let _ = tx.send(super::app_state::GitRefreshResult {
                generation,
                workspace,
                result,
            });
        });
    }

    /// GUI 帧中非阻塞消费 Git 查询结果。回包必须同时匹配刷新代次和当前工作区，
    /// 否则是切项目前的旧结果，直接丢弃。
    pub(super) fn poll_git_changes(&mut self) {
        let Some(rx) = self.git_rx.as_ref() else {
            return;
        };
        let Ok(update) = rx.try_recv() else { return };
        self.git_rx = None;
        let current_workspace = self.active_workspace_root();
        if update.generation != self.git_generation || update.workspace != current_workspace {
            return;
        }
        self.git_loaded = true;
        match update.result {
            Ok(data) => {
                self.git_branch = data.branch;
                self.git_workspace = data.repo_root;
                self.git_changes = data.changes;
                self.git_error = None;
            }
            Err(error) => {
                self.git_branch.clear();
                self.git_changes.clear();
                self.git_error = Some(error);
            }
        }
    }

    /// 异步优化输入：后台线程调用 LLM 重写用户输入，非阻塞回传结果。
    pub(super) fn optimize_input(&mut self) {
        if self.optimizing {
            return;
        }
        let text = self.input.trim().to_string();
        if text.is_empty() {
            self.optimize_msg = "请先输入内容再优化".into();
            return;
        }
        self.optimizing = true;
        self.optimize_msg.clear();
        let llm = self.host.llm_control.clone();
        let (tx, rx) = std::sync::mpsc::channel::<std::result::Result<String, String>>();
        self.optimize_rx = Some(rx);
        std::thread::spawn(move || {
            let result = llm.complete_one_shot(text);
            let _ = tx.send(result);
        });
    }

    /// 非阻塞消费优化结果：成功则替换输入框内容。
    pub(super) fn poll_optimize(&mut self) {
        let Some(rx) = self.optimize_rx.as_ref() else {
            return;
        };
        let Ok(result) = rx.try_recv() else { return };
        self.optimize_rx = None;
        self.optimizing = false;
        match result {
            Ok(optimized) => {
                self.input = optimized;
                self.optimize_msg.clear();
            }
            Err(error) => {
                self.optimize_msg = format!("优化失败：{error}");
            }
        }
    }

    /// 构建文件树（懒构建 2 层）。
    pub(super) fn build_tree(&mut self) {
        let fs = self.host.fs.clone();
        let git = self.host.git.clone();
        let root = self.active_workspace_root();
        let root_for_name = root.clone();
        let handle = self.host.rt.handle();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<crate::preview::FileTreeNode>>();
        std::thread::spawn(move || {
            let nodes = handle.block_on(async move {
                // git 有未提交变化的文件集合（文件树标记用）；非 git 仓库为空。
                let dirty: std::collections::HashSet<String> = git
                    .changed_files()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|c| c.path.replace('\\', "/"))
                    .collect();
                list_dir_recursive(&fs, std::path::Path::new(&root), "", &dirty, 10).await
            });
            let _ = tx.send(nodes);
        });
        if let Ok(nodes) = rx.recv() {
            self.tree_root = Some(crate::preview::FileTreeNode {
                name: root_for_name.clone(),
                path: String::new(),
                is_dir: true,
                dirty: false,
                children: nodes,
            });
            self.tree_workspace = root_for_name;
            self.tree_last_refresh = Some(std::time::Instant::now());
        }
    }

    /// 渲染文件树。
    pub(super) fn render_tree(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        // 防御性校验：即使未来新增项目切换入口忘了手工清缓存，只要树记录的
        // workspace 与当前项目不一致，就必须在展示前重建，绝不显示旧项目内容。
        if self.tree_needs_reload() {
            self.build_tree();
        }
        // 标题栏
        let head_h = if cfg!(target_os = "macos") {
            32.0
        } else {
            28.0
        };
        egui::TopBottomPanel::top("tree_head")
            .exact_height(head_h)
            .frame(
                egui::Frame::default()
                    .fill(pal.head_fill)
                    .inner_margin(egui::Margin::symmetric(10.0, 4.0)),
            )
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    // 标题随视图切换：Git 变更视图显示统计，文件树视图显示"文件树"。
                    let title = if self.tree_show_git {
                        format!("Git 变更 ({})", self.git_changes.len())
                    } else {
                        "文件树".to_string()
                    };
                    ui.label(egui::RichText::new(title).size(12.0).color(pal.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if close_button(ui, pal) {
                            self.tree_open = false;
                        }
                        // R 按钮：Git 变更视图 = 切回文件树；文件树视图 = 刷新文件树。
                        if ui
                            .add(egui::Button::new(egui::RichText::new("R").size(12.0)))
                            .on_hover_text(if self.tree_show_git {
                                "切回文件树"
                            } else {
                                "刷新文件树"
                            })
                            .clicked()
                        {
                            if self.tree_show_git {
                                self.tree_show_git = false;
                            } else {
                                let need_refresh = self.tree_needs_reload()
                                    || self
                                        .tree_last_refresh
                                        .map(|t| t.elapsed().as_secs() > 5)
                                        .unwrap_or(true);
                                if need_refresh {
                                    self.build_tree();
                                }
                            }
                        }
                        // Git 变更入口（刷新按钮左边）：矢量图标，点击切换 Git 变更视图。
                        ui.add_space(4.0);
                        let git_btn = ui
                            .add_sized(
                                [20.0, 20.0],
                                egui::Button::new(egui::RichText::new("").size(10.0)),
                            )
                            .on_hover_text("Git 变更（查看未提交文件）");
                        draw_icon(
                            &ui.painter(),
                            git_btn.rect.center(),
                            Icon::GitBranch,
                            if self.tree_show_git {
                                pal.accent
                            } else if self.git_changes.is_empty() {
                                pal.dim
                            } else {
                                pal.warn
                            },
                        );
                        if git_btn.clicked() {
                            // 直接切换树区域视图，不弹窗。
                            self.tree_show_git = true;
                            self.refresh_git_changes();
                        }
                    });
                });
            });

        // 内容区：按视图分支（面板 frame 边距为 0 以让头部贴顶，内边距在这里补）
        egui::Frame::default()
            .inner_margin(egui::Margin {
                left: 8.0,
                right: 8.0,
                top: 4.0,
                bottom: 8.0,
            })
            .show(ui, |ui| {
                egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                    if self.tree_show_git {
                        self.render_git_changes_list(ui, pal);
                    } else if let Some(root) = &self.tree_root.clone() {
                        let mut clicked_path: Option<String> = None;
                        let mut toggle_path: Option<String> = None;
                        let mut attachment_path: Option<String> = None;
                        self.render_tree_node(
                            ui,
                            root,
                            0,
                            pal,
                            &mut clicked_path,
                            &mut toggle_path,
                            &mut attachment_path,
                        );
                        if let Some(path) = clicked_path {
                            self.pending_preview = Some(path);
                        }
                        if let Some(path) = toggle_path {
                            if self.tree_expanded.contains(&path) {
                                self.tree_expanded.remove(&path);
                            } else {
                                self.tree_expanded.insert(path);
                                // 懒加载子节点
                                self.expand_tree_node();
                            }
                        }
                        if let Some(path) = attachment_path {
                            let absolute =
                                tree_attachment_path(&self.active_workspace_root(), &path);
                            let display_name = absolute
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or("文件")
                                .to_string();
                            super::composer::add_attachment(self, absolute);
                            self.note = format!("已添加附件：{display_name}");
                        }
                    } else {
                        ui.add_space(20.0);
                        ui.label(egui::RichText::new("加载中...").size(12.0).color(pal.dim));
                    }
                });
            });
    }

    /// 渲染 Git 变更文件列表（状态色块 + 路径；点击在预览窗打开 Diff）。
    pub(super) fn render_git_changes_list(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        if !self.git_loaded {
            ui.add_space(16.0);
            ui.label(egui::RichText::new("加载中...").size(12.0).color(pal.dim));
            ui.ctx().request_repaint();
            return;
        }
        if self.git_changes.is_empty() {
            if let Some(error) = &self.git_error {
                ui.add_space(16.0);
                if error.contains("not a git repository") {
                    ui.label(
                        egui::RichText::new("当前目录不是 Git 仓库或未初始化")
                            .size(12.0)
                            .color(pal.dim),
                    );
                } else {
                    ui.label(
                        egui::RichText::new(format!("无法读取 Git 状态：{error}"))
                            .size(12.0)
                            .color(pal.err_text),
                    );
                }
                ui.label(
                    egui::RichText::new(format!("查询工作区：{}", self.git_workspace))
                        .size(11.0)
                        .color(pal.dim),
                );
                return;
            }
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                draw_icon(ui.painter(), rect.center(), Icon::Sparkles, pal.accent);
                ui.label(
                    egui::RichText::new(format!(
                        "工作区干净，无未提交变更 · {}",
                        self.git_workspace
                    ))
                    .size(12.0)
                    .color(pal.accent),
                );
            });
            return;
        }
        let mut open_diff: Option<String> = None;
        for ch in self.git_changes.clone() {
            let (mark, mcolor) = match ch.marker() {
                "M" => ("M", pal.warn),
                "A" => ("A", pal.accent),
                "D" => ("D", pal.err_text),
                "R" => ("R", pal.accent),
                "U" | "??" => ("?", pal.dim),
                _ => ("*", pal.dim),
            };
            let row_h = 26.0;
            let (rect, resp) = ui.allocate_at_least(
                egui::vec2(ui.available_width(), row_h),
                egui::Sense::click(),
            );
            let hovered = resp.hovered();
            let is_active = self.preview_path.as_ref() == Some(&ch.path);
            if hovered || is_active {
                ui.painter()
                    .rect_filled(rect.shrink(1.0), egui::Rounding::same(5.0), pal.hover);
            }
            if is_active {
                let bar = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + 2.0, rect.min.y + 5.0),
                    egui::vec2(2.5, rect.height() - 10.0),
                );
                ui.painter()
                    .rect_filled(bar, egui::Rounding::same(2.0), pal.accent);
            }
            // 状态标记小方块
            let badge = egui::Rect::from_center_size(
                egui::pos2(rect.min.x + 13.0, rect.center().y),
                egui::vec2(20.0, 16.0),
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
                egui::FontId::monospace(10.5),
                mcolor,
            );
            // 路径
            ui.painter().text(
                egui::pos2(rect.min.x + 40.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &ch.path,
                egui::FontId::monospace(11.5),
                if is_active { pal.text } else { pal.dim },
            );
            if resp.clicked() {
                open_diff = Some(ch.path.clone());
            }
            ui.add_space(2.0);
        }
        if let Some(path) = open_diff {
            // 点击变更文件：预览窗直接打开 Diff 模式。
            self.open_preview(path);
            self.preview_mode = crate::preview::PreviewMode::Diff;
        }
    }

    /// 递归渲染文件树节点。
    pub(super) fn render_tree_node(
        &self,
        ui: &mut egui::Ui,
        node: &crate::preview::FileTreeNode,
        depth: usize,
        pal: &Palette,
        clicked_path: &mut Option<String>,
        toggle_path: &mut Option<String>,
        attachment_path: &mut Option<String>,
    ) {
        let row_h = 24.0;
        let indent = depth as f32 * 14.0;
        let (rect, resp) = ui.allocate_at_least(
            egui::vec2(ui.available_width(), row_h),
            egui::Sense::click(),
        );
        let hovered = resp.hovered();
        let is_active = self.preview_path.as_ref() == Some(&node.path) && !node.is_dir;
        let expanded = node.is_dir && self.tree_expanded.contains(&node.path);

        // 行背景
        if is_active || hovered {
            ui.painter()
                .rect_filled(rect.shrink(1.0), egui::Rounding::same(4.0), pal.hover);
        }
        if is_active {
            let bar = egui::Rect::from_min_size(
                egui::pos2(rect.min.x + 2.0, rect.min.y + 4.0),
                egui::vec2(2.5, rect.height() - 8.0),
            );
            ui.painter()
                .rect_filled(bar, egui::Rounding::same(2.0), pal.accent);
        }

        // 树形连接线
        if depth > 0 {
            let line_color = pal.border;
            let line_x = rect.min.x + indent - 7.0;
            let center_y = rect.center().y;
            ui.painter().line_segment(
                [egui::pos2(line_x, rect.min.y), egui::pos2(line_x, center_y)],
                egui::Stroke::new(1.0_f32, line_color),
            );
            ui.painter().line_segment(
                [
                    egui::pos2(line_x, center_y),
                    egui::pos2(line_x + 7.0, center_y),
                ],
                egui::Stroke::new(1.0_f32, line_color),
            );
        }

        let icon_x = rect.min.x + indent + 2.0;
        let center_y = rect.center().y;
        let text_x = icon_x + 16.0;

        if node.is_dir {
            draw_icon_sized(
                ui.painter(),
                egui::pos2(icon_x + 4.0, center_y),
                if expanded {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronRight
                },
                if hovered || expanded {
                    pal.text
                } else {
                    pal.dim
                },
                10.0,
            );

            // 目录名
            ui.painter().text(
                egui::pos2(text_x, center_y),
                egui::Align2::LEFT_CENTER,
                &node.name,
                egui::FontId::proportional(12.5),
                pal.text,
            );
            // 子节点数量提示
            if !node.children.is_empty() && !expanded {
                let name_w = node.name.chars().count() as f32 * 7.5;
                ui.painter().text(
                    egui::pos2(text_x + name_w + 8.0, center_y),
                    egui::Align2::LEFT_CENTER,
                    format!("({})", node.children.len()),
                    egui::FontId::proportional(10.0),
                    pal.dim,
                );
            }
            if resp.clicked() {
                *toggle_path = Some(node.path.clone());
            }
            if expanded {
                for child in &node.children {
                    self.render_tree_node(
                        ui,
                        child,
                        depth + 1,
                        pal,
                        clicked_path,
                        toggle_path,
                        attachment_path,
                    );
                }
            }
        } else {
            if node.dirty {
                draw_icon_sized(
                    ui.painter(),
                    egui::pos2(text_x - 7.0, center_y),
                    Icon::CircleDot,
                    pal.warn,
                    8.0,
                );
            }
            ui.painter().text(
                egui::pos2(text_x, center_y),
                egui::Align2::LEFT_CENTER,
                &node.name,
                egui::FontId::proportional(12.5),
                if is_active { pal.text } else { pal.dim },
            );
            if resp.clicked() {
                *clicked_path = Some(node.path.clone());
            }
            resp.context_menu(|ui| {
                if ui.button("添加到对话框附件").clicked() {
                    *attachment_path = Some(node.path.clone());
                    ui.close_menu();
                }
            });
        }
    }

    /// 懒加载展开的目录节点子项。
    pub(super) fn expand_tree_node(&mut self) {
        // 简化：直接重建树（对中小仓库足够快）。
        self.build_tree();
    }

    /// 当前项目根。active_project 属于当前帧的运行时状态；持久化设置和启动根
    /// 只在它失效时作为兼容回退。
    fn active_workspace_root(&self) -> String {
        if std::path::Path::new(&self.active_project).is_dir() {
            self.active_project.clone()
        } else {
            self.host.workspace_root.clone()
        }
    }

    pub(super) fn tree_needs_reload(&self) -> bool {
        let active = self.active_workspace_root();
        self.tree_root.is_none() || !same_workspace(&self.tree_workspace, &active)
    }
}

fn tree_attachment_path(workspace_root: &str, node_path: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(node_path);
    if path.is_absolute() {
        path
    } else {
        std::path::Path::new(workspace_root).join(path)
    }
}

/// Windows 路径大小写不敏感；同时统一分隔符，避免同一目录仅因展示形式不同
/// 被误判为另一个项目。其他平台保持大小写敏感语义。
fn same_workspace(left: &str, right: &str) -> bool {
    let normalize = |value: &str| value.trim_end_matches(['/', '\\']).replace('\\', "/");
    let left = normalize(left);
    let right = normalize(right);
    if cfg!(windows) {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

/// 递归列出目录（深度限制），构建文件树节点。忽略 .git/target/node_modules 等。
async fn list_dir_recursive(
    fs: &Arc<dyn harness_capability::fs::Fs>,
    dir: &std::path::Path,
    rel_dir: &str,
    dirty_files: &std::collections::HashSet<String>,
    max_depth: usize,
) -> Vec<crate::preview::FileTreeNode> {
    if max_depth == 0 {
        return Vec::new();
    }
    let mut nodes = Vec::new();
    if let Ok(entries) = fs.list(dir).await {
        let mut sorted: Vec<_> = entries.into_iter().collect();
        sorted.sort_by(|a, b| {
            let a_dir = a.is_dir();
            let b_dir = b.is_dir();
            b_dir.cmp(&a_dir).then_with(|| {
                a.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_lowercase()
                    .cmp(
                        &b.file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_lowercase(),
                    )
            })
        });
        for entry in sorted {
            let name = entry
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if crate::preview::TREE_IGNORED_DIRS.contains(&name.as_str()) {
                continue;
            }
            let is_dir = entry.is_dir();
            // 相对路径：手动拼接（不依赖 strip_prefix，避免 Windows canonicalize
            // 返回 \?\ verbatim 前缀导致前缀不匹配、回退为纯文件名的 bug）。
            let rel_path = if rel_dir.is_empty() {
                name.clone()
            } else {
                format!("{}/{}", rel_dir, name)
            };
            let dirty = !is_dir && dirty_files.contains(&rel_path);
            let children = if is_dir {
                Box::pin(list_dir_recursive(
                    fs,
                    &entry,
                    &rel_path,
                    dirty_files,
                    max_depth - 1,
                ))
                .await
            } else {
                Vec::new()
            };
            nodes.push(crate::preview::FileTreeNode {
                name,
                path: rel_path,
                is_dir,
                dirty,
                children,
            });
        }
    }
    nodes
}

/// 渲染全宽 Unified Diff 视图行
fn render_diff_viewer(ui: &mut egui::Ui, pal: &Palette, diff: &str) {
    let diff_lines = crate::preview::parse_diff(diff);
    ui.spacing_mut().item_spacing.x = 0.0;
    let row_h = 20.0;
    for dl in &diff_lines {
        let (bg, fg, sign, sign_color) = match dl.kind {
            crate::preview::DiffLineKind::Add => {
                (pal.diff_add_bg, pal.text, "+", pal.diff_sign_add)
            }
            crate::preview::DiffLineKind::Del => {
                (pal.diff_del_bg, pal.text, "-", pal.diff_sign_del)
            }
            crate::preview::DiffLineKind::Hunk => (pal.diff_hunk_bg, pal.accent, "@", pal.accent),
            crate::preview::DiffLineKind::Meta => {
                (egui::Color32::TRANSPARENT, pal.dim, "", pal.dim)
            }
            crate::preview::DiffLineKind::Context => {
                (egui::Color32::TRANSPARENT, pal.dim, " ", pal.dim)
            }
        };
        let (row_rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), row_h),
            egui::Sense::hover(),
        );
        if bg != egui::Color32::TRANSPARENT {
            ui.painter().rect_filled(row_rect, 0.0, bg);
        }
        let cy = row_rect.center().y;
        if !sign.is_empty() {
            ui.painter().text(
                egui::pos2(row_rect.min.x + 8.0, cy),
                egui::Align2::LEFT_CENTER,
                sign,
                egui::FontId::monospace(11.5),
                sign_color,
            );
        }
        let text_content = dl.text.trim_start_matches(['+', '-', '@']).trim_start();
        ui.painter().text(
            egui::pos2(row_rect.min.x + 22.0, cy),
            egui::Align2::LEFT_CENTER,
            text_content,
            egui::FontId::monospace(11.5),
            fg,
        );
    }
}

/// 识别文件语言标签
fn file_lang_label(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "rs" => "Rust",
        "toml" => "TOML",
        "md" | "markdown" => "Markdown",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "ts" => "TypeScript",
        "tsx" => "TSX",
        "js" => "JavaScript",
        "jsx" => "JSX",
        "py" => "Python",
        "go" => "Go",
        "java" => "Java",
        "c" => "C",
        "cpp" | "cc" | "cxx" => "C++",
        "h" | "hpp" => "Header",
        "sh" | "bash" | "zsh" => "Shell",
        "sql" => "SQL",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "xml" => "XML",
        "txt" => "Plain Text",
        _ => "Code",
    }
}

/// 在操作系统默认编辑器中打开文件
fn open_in_system_editor(path: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/c", "start", "", path.to_str().unwrap_or("")])
        .spawn();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

#[cfg(test)]
mod tests {
    use super::{same_workspace, tree_attachment_path};

    #[test]
    fn tree_attachment_uses_the_active_workspace_for_relative_nodes() {
        let path = tree_attachment_path("C:/workspace/project-a", "src/main.rs");
        assert_eq!(
            path,
            std::path::Path::new("C:/workspace/project-a").join("src/main.rs")
        );
    }

    #[test]
    fn workspace_identity_normalizes_separators_and_trailing_slashes() {
        assert!(same_workspace(
            "F:\\workspace\\project-a\\",
            "F:/workspace/project-a"
        ));
    }

    #[test]
    fn workspace_identity_rejects_a_previous_project() {
        assert!(!same_workspace(
            "F:/workspace/project-a",
            "F:/workspace/project-b"
        ));
    }

    #[cfg(windows)]
    #[test]
    fn workspace_identity_is_case_insensitive_on_windows() {
        assert!(same_workspace(
            "F:/Workspace/Project-A",
            "f:/workspace/project-a"
        ));
    }
}
