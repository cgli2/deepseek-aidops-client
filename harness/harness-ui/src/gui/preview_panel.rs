//! File preview, workspace tree, and Git changes panel behavior.

use std::sync::Arc;

use super::icons::{draw_icon, Icon};
use super::theme::{palette, Palette};
use super::widgets::close_button;
use super::AppState;

impl AppState {
    /// 打开文件预览窗并加载指定文件。
    ///
    /// 命中缓存时立即打开；未命中时也**立即打开面板**（面板内显示"加载中…"），
    /// 内容就绪后原地更新——避免面板在异步返回后"空降"，导致中央消息流宽度突变闪烁。
    pub(super) fn open_preview(&mut self, path: String) {
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
        // 基准根统一从 settings 读取（switch_project 已更新），避免 Arc 字段不可变。
        let ws_root = self
            .host
            .settings
            .get("workspace.root")
            .filter(|p| std::path::Path::new(p).is_dir())
            .unwrap_or_else(|| self.host.workspace_root.clone());
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

    /// 渲染文件预览窗（右侧 SidePanel 分隔面板：自绘头部 + 内容滚动区）。
    pub(super) fn render_preview(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        // 闪烁缓解：面板打开瞬间用透明度淡入（约 0.15s），
        // 中央消息流宽度突变被淡入柔化，减轻视觉冲击。
        let fade = ui
            .ctx()
            .animate_bool(egui::Id::new("preview_fade"), self.preview_open);
        if fade < 0.98 {
            ui.set_opacity(fade);
        }
        // 自绘头部：文件名 + 模式切换 + 关闭。
        let head_h = 34.0;
        egui::Frame::default()
            .fill(pal.head_fill)
            .inner_margin(egui::Margin::symmetric(10.0, 5.0))
            .show(ui, |ui| {
                ui.set_min_height(head_h - 10.0);
                ui.horizontal(|ui| {
                    let name = self
                        .preview_path
                        .as_ref()
                        .and_then(|p| std::path::Path::new(p).file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("预览");
                    // 文件名截断，防止超长文件名把关闭按钮挤出。
                    let name_trunc: String = name.chars().take(24).collect();
                    let name_disp = if name.chars().count() > 24 {
                        format!("{name_trunc}…")
                    } else {
                        name_trunc
                    };
                    ui.label(
                        egui::RichText::new(&name_disp)
                            .size(12.5)
                            .color(pal.text),
                    );
                    let is_markdown = self
                        .preview_path
                        .as_deref()
                        .map(crate::preview::is_markdown_path)
                        .unwrap_or(false);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if close_button(ui, pal) {
                            self.preview_open = false;
                            // 触发关闭滑出动画（面板继续渲染直到宽度缩回 0）。
                            self.preview_animating = true;
                            self.preview_path = None;
                            self.preview_content = None;
                        }
                        ui.add_space(6.0);
                        if self.preview_tracked
                            && ui
                                .add(egui::SelectableLabel::new(
                                    self.preview_mode == crate::preview::PreviewMode::Diff,
                                    egui::RichText::new("Diff").size(11.0),
                                ))
                                .clicked()
                        {
                            self.preview_mode = crate::preview::PreviewMode::Diff;
                        }
                        if ui
                            .add(egui::SelectableLabel::new(
                                self.preview_mode == crate::preview::PreviewMode::Source,
                                egui::RichText::new(if is_markdown { "原文" } else { "源码" })
                                    .size(11.0),
                            ))
                            .clicked()
                        {
                            self.preview_mode = crate::preview::PreviewMode::Source;
                        }
                        if is_markdown
                            && ui
                                .add(egui::SelectableLabel::new(
                                    self.preview_mode == crate::preview::PreviewMode::Markdown,
                                    egui::RichText::new("预览").size(11.0),
                                ))
                                .clicked()
                        {
                            self.preview_mode = crate::preview::PreviewMode::Markdown;
                        }
                    });
                });
            });
        // 头部下方分隔线
        let sep = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover()).0;
        ui.painter().rect_filled(sep, 0.0, pal.border);

        // 内容区（滚动区填满面板可用高）
        let avail_h = ui.available_height().max(120.0);
        egui::ScrollArea::both()
            .id_salt("preview_scroll")
            .auto_shrink(false)
            .max_height(avail_h)
            .show(ui, |ui| {
            if self.preview_content.is_none()
                && self.preview_error.is_none()
                && self.preview_rx.is_some()
            {
                ui.add_space(20.0);
                ui.label(egui::RichText::new("加载中...").size(12.0).color(pal.dim));
                // 不在此逐帧 request_repaint：app.rs 已按 80ms 周期重绘并轮询 poll_preview，
                // 内容就绪后自然更新，避免点击后 CPU 满载空转。
                return;
            }
            if let Some(err) = &self.preview_error {
                ui.add_space(20.0);
                ui.label(
                    egui::RichText::new(format!("! {err}"))
                        .size(12.0)
                        .color(pal.err_text),
                );
                return;
            }
            match self.preview_mode {
                crate::preview::PreviewMode::Markdown => {
                    if let Some(content) = &self.preview_content {
                        if self.preview_truncated {
                            ui.label(
                                egui::RichText::new("文件过大，仅显示前 512KB")
                                    .size(10.5)
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
                crate::preview::PreviewMode::Source => {
                    if self.preview_content.is_some() {
                        if self.preview_truncated {
                            ui.label(
                                egui::RichText::new("文件过大，仅显示前 512KB")
                                    .size(10.5)
                                    .color(pal.warn),
                            );
                            ui.add_space(4.0);
                        }
                        // 语法高亮渲染：行号 + 高亮 token 统一在 LayoutJob 里，
                        // egui 按文本哈希缓存 galley，tokenize 只做一次。
                        // 水平滚动：无限宽度 + 横向滚动区，长行不换行，按中键拖动查看。
                        let job = self
                            .preview_highlight
                            .clone()
                            .unwrap_or_else(|| egui::text::LayoutJob::default());
                        let resp = ui.add(egui::Label::new(job).selectable(true));
                        let _ = resp;
                    }
                }
                crate::preview::PreviewMode::Diff => {
                    // 加载中（点击瞬间 diff 尚未异步返回）：显示加载提示，不误导为"无修改"。
                    if self.preview_rx.is_some() && self.preview_diff.is_none() {
                        ui.add_space(20.0);
                        ui.label(
                            egui::RichText::new("Diff 加载中...")
                                .size(12.0)
                                .color(pal.dim),
                        );
                    } else if let Some(diff) = &self.preview_diff {
                        let diff_lines = crate::preview::parse_diff(diff);
                        ui.spacing_mut().item_spacing.x = 0.0;
                        // 全宽色块渲染：每行 allocate 整行宽，painter 画背景 + 符号 + 文本。
                        // 行高 20px，行号 + 符号列固定宽，背景色铺满整行（不随文本截断）。
                        let row_h = 20.0;
                        let mut line_no = 0usize;
                        for dl in &diff_lines {
                            let (bg, fg, sign, sign_color) = match dl.kind {
                                crate::preview::DiffLineKind::Add => {
                                    (pal.diff_add_bg, pal.text, "+", pal.diff_sign_add)
                                }
                                crate::preview::DiffLineKind::Del => {
                                    (pal.diff_del_bg, pal.text, "-", pal.diff_sign_del)
                                }
                                crate::preview::DiffLineKind::Hunk => {
                                    (pal.diff_hunk_bg, pal.accent, "@", pal.accent)
                                }
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
                            // 整行背景色块
                            if bg != egui::Color32::TRANSPARENT {
                                ui.painter().rect_filled(row_rect, 0.0, bg);
                            }
                            let cy = row_rect.center().y;
                            // 符号列（+ / - / @）
                            if !sign.is_empty() {
                                ui.painter().text(
                                    egui::pos2(row_rect.min.x + 8.0, cy),
                                    egui::Align2::LEFT_CENTER,
                                    sign,
                                    egui::FontId::monospace(11.5),
                                    sign_color,
                                );
                            }
                            // 内容文本（去掉行首 + - @ 符号，避免重复）
                            let text_content =
                                dl.text.trim_start_matches(['+', '-', '@']).trim_start();
                            ui.painter().text(
                                egui::pos2(row_rect.min.x + 22.0, cy),
                                egui::Align2::LEFT_CENTER,
                                text_content,
                                egui::FontId::monospace(11.5),
                                fg,
                            );
                            // Meta / Hunk 行也推进行号计数
                            if matches!(
                                dl.kind,
                                crate::preview::DiffLineKind::Add
                                    | crate::preview::DiffLineKind::Del
                                    | crate::preview::DiffLineKind::Context
                            ) {
                                line_no += 1;
                            }
                            let _ = line_no;
                        }
                    } else {
                        ui.add_space(20.0);
                        ui.label(
                            egui::RichText::new("该文件无未提交修改（已跟踪且干净）")
                                .size(12.0)
                                .color(pal.dim),
                        );
                    }
                }
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
        let workspace = self
            .host
            .settings
            .get("workspace.root")
            .filter(|path| std::path::Path::new(path).is_dir())
            .unwrap_or_else(|| self.host.workspace_root.clone());
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
        let Some(rx) = self.git_rx.as_ref() else { return };
        let Ok(update) = rx.try_recv() else { return };
        self.git_rx = None;
        let current_workspace = self
            .host
            .settings
            .get("workspace.root")
            .filter(|path| std::path::Path::new(path).is_dir())
            .unwrap_or_else(|| self.host.workspace_root.clone());
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
        let Some(rx) = self.optimize_rx.as_ref() else { return };
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
        // 基准根统一从 settings 读取（与 load_preview 一致）。
        let root = self
            .host
            .settings
            .get("workspace.root")
            .filter(|p| std::path::Path::new(p).is_dir())
            .unwrap_or_else(|| self.host.workspace_root.clone());
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
                name: root_for_name,
                path: String::new(),
                is_dir: true,
                dirty: false,
                children: nodes,
            });
            self.tree_last_refresh = Some(std::time::Instant::now());
        }
    }

    /// 渲染文件树。
    pub(super) fn render_tree(&mut self, ui: &mut egui::Ui, pal: &Palette) {
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
                    ui.label(egui::RichText::new(title).size(12.5).color(pal.text));
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
                                let need_refresh = self
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
                        self.render_tree_node(
                            ui,
                            root,
                            0,
                            pal,
                            &mut clicked_path,
                            &mut toggle_path,
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
                ui.label(
                    egui::RichText::new(format!("无法读取 Git 状态：{error}"))
                        .size(12.0)
                        .color(pal.err_text),
                );
                ui.label(
                    egui::RichText::new(format!("查询工作区：{}", self.git_workspace))
                        .size(10.5)
                        .color(pal.dim),
                );
                return;
            }
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new(format!(
                    "✨ 工作区干净，无未提交变更 · {}",
                    self.git_workspace
                ))
                    .size(12.0)
                    .color(pal.accent),
            );
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
            // 矢量三角箭头（不用 Unicode 字符）
            let arrow_size = 3.5;
            let arrow_x = icon_x;
            let arrow_color = if hovered || expanded {
                pal.text
            } else {
                pal.dim
            };
            if expanded {
                let pts = vec![
                    egui::pos2(arrow_x, center_y - arrow_size),
                    egui::pos2(arrow_x + arrow_size * 2.0, center_y - arrow_size),
                    egui::pos2(arrow_x + arrow_size, center_y + arrow_size),
                ];
                ui.painter().add(egui::Shape::closed_line(
                    pts,
                    egui::Stroke::new(1.0_f32, arrow_color),
                ));
            } else {
                let pts = vec![
                    egui::pos2(arrow_x, center_y - arrow_size),
                    egui::pos2(arrow_x, center_y + arrow_size),
                    egui::pos2(arrow_x + arrow_size, center_y),
                ];
                ui.painter().add(egui::Shape::closed_line(
                    pts,
                    egui::Stroke::new(1.0_f32, arrow_color),
                ));
            }

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
                    self.render_tree_node(ui, child, depth + 1, pal, clicked_path, toggle_path);
                }
            }
        } else {
            // git 有未提交变化的文件：文件名左侧画橙色小圆点色块。
            if node.dirty {
                ui.painter()
                    .circle_filled(egui::pos2(text_x - 7.0, center_y), 3.2, pal.warn);
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
        }
    }

    /// 懒加载展开的目录节点子项。
    pub(super) fn expand_tree_node(&mut self) {
        // 简化：直接重建树（对中小仓库足够快）。
        self.build_tree();
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
