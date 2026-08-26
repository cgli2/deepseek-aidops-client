//! 代码图谱面板：把平铺的符号列表变成「可读、可导航」的结构化视图。
//!
//! 设计：
//! - 顶部统计卡：符号 / 文件 / 函数 / 类型 四个计数，一眼看清规模；
//! - 符号按文件分组，组头可折叠（组头显示文件相对路径 + 计数徽标）；
//! - 每个符号一行：彩色类型徽标 + 名称 + 签名 + 「调用 N / 被调 M」计数；
//! - 点击符号或关系 chip 可在图谱内跳转（选中详情面板展示 调用方/被调方 导航）；
//! - 渲染函数是纯 UI：不持有数据服务，只消费传入的 `&[CodeSymbol]` 与选中项。

use super::theme::Palette;
use super::widgets::close_button;
use harness_capability::assets::CodeSymbol;

/// 类型 → (标签, 主题色)。未知类型归为「符号」，保证每个符号都有颜色可读的徽标。
fn kind_style(kind: &str) -> (String, egui::Color32) {
    let k = kind.to_ascii_lowercase();
    let (label, color) = match k.as_str() {
        "fn" | "function" => ("fn", egui::Color32::from_rgb(0x4f, 0xb6, 0xd1)),
        "method" => ("method", egui::Color32::from_rgb(0x59, 0xc9, 0x9a)),
        "struct" => ("struct", egui::Color32::from_rgb(0xbb, 0x86, 0xe6)),
        "class" => ("class", egui::Color32::from_rgb(0xe0, 0xa2, 0x5a)),
        "enum" => ("enum", egui::Color32::from_rgb(0xe5, 0x8f, 0x9e)),
        "trait" => ("trait", egui::Color32::from_rgb(0x6f, 0xa8, 0xef)),
        "interface" => ("interface", egui::Color32::from_rgb(0x6f, 0xa8, 0xef)),
        "const" | "constant" => ("const", egui::Color32::from_rgb(0xd4, 0xb8, 0x6b)),
        "module" => ("module", egui::Color32::from_rgb(0x9a, 0xa6, 0xb8)),
        "type" => ("type", egui::Color32::from_rgb(0x9a, 0xa6, 0xb8)),
        "macro" => ("macro", egui::Color32::from_rgb(0xc0, 0x7f, 0x6a)),
        _ => ("符号", egui::Color32::from_rgb(0x8f, 0xa1, 0xb5)),
    };
    (label.to_string(), color)
}

/// 取文件相对路径的「短名」（最后一个 `/` 之后），分组显示时更紧凑。
fn file_short_name(file: &str) -> &str {
    file.rsplit(['/', '\\']).next().unwrap_or(file)
}

/// 把符号按文件分组，组序按文件名稳定排序。
fn group_by_file(symbols: &[CodeSymbol]) -> Vec<(String, Vec<usize>)> {
    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, s) in symbols.iter().enumerate() {
        if let Some(g) = groups.iter_mut().find(|(f, _)| *f == s.file) {
            g.1.push(i);
        } else {
            groups.push((s.file.clone(), vec![i]));
        }
    }
    groups.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    groups
}

/// 渲染主入口：统计 + 分组列表 + 选中详情。
///
/// 内容直接平铺在调用方提供的 `ui` 上（外层 settings 弹层的滚动区驱动滚动）。
/// `scroll_hint` 由外层传入：图谱内发生跳转（点击符号/关系 chip）时置 true，
/// 外层捕获后把主滚动区滚回顶部，给用户视觉锚点。返回渲染的「行数」（供计数）。
pub(super) fn render(
    ui: &mut egui::Ui,
    pal: &Palette,
    symbols: &[CodeSymbol],
    expanded: &mut std::collections::HashSet<String>,
    sel: &mut Option<String>,
    scroll_hint: &mut bool,
) -> usize {
    render_stats(ui, pal, symbols);
    ui.add_space(8.0);
    if symbols.is_empty() {
        empty_hint(ui, pal);
        return 0;
    }

    let groups = group_by_file(symbols);
    let mut rows = 0usize;
    let mut scroll_now = *scroll_hint;
    *scroll_hint = false;

    // 惰性初始化：默认全部组展开（保持可读性，不默认折叠隐藏内容）。
    if expanded.is_empty() {
        expanded.extend(groups.iter().map(|(f, _)| f.clone()));
    }
    for (file, idxs) in &groups {
        rows += 1;
        let is_open = expanded.contains(file);
        let mut toggle = false;
        let header = ui.horizontal(|ui| {
            // 折叠三角（矢量，不依赖字体字形）。
            let (tri, tri_resp) =
                ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
            let c = tri.center();
            let s = egui::Stroke::new(
                1.4_f32,
                if tri_resp.hovered() {
                    pal.text
                } else {
                    pal.dim
                },
            );
            let pts: [egui::Pos2; 3] = if is_open {
                [
                    egui::pos2(c.x - 3.2, c.y - 2.0),
                    egui::pos2(c.x + 3.2, c.y - 2.0),
                    egui::pos2(c.x, c.y + 2.6),
                ]
            } else {
                [
                    egui::pos2(c.x - 1.8, c.y - 3.2),
                    egui::pos2(c.x - 1.8, c.y + 3.2),
                    egui::pos2(c.x + 2.8, c.y),
                ]
            };
            ui.painter().add(egui::Shape::closed_line(pts.to_vec(), s));
            if tri_resp.clicked() {
                toggle = true;
            }
            ui.label(
                egui::RichText::new(file_short_name(file))
                    .size(12.5)
                    .strong()
                    .color(pal.text),
            );
            // 文件路径完整显示（dim 小字）。
            ui.label(egui::RichText::new(file).size(10.5).color(pal.dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                count_badge(ui, pal, idxs.len());
            });
            if tri_resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        });
        let header_rect = header.response.rect;
        let header_resp = ui.interact(
            header_rect,
            ui.make_persistent_id(("code_group", file)),
            egui::Sense::click(),
        );
        if header_resp.double_clicked() || toggle {
            if !expanded.insert(file.clone()) {
                expanded.remove(file);
            }
        }
        ui.add_space(3.0);
        if !is_open {
            ui.add_space(4.0);
            continue;
        }
        for &i in idxs {
            rows += 1;
            if symbol_row(ui, pal, &symbols[i], symbols, sel) {
                scroll_now = true;
            }
        }
        ui.add_space(6.0);
    }

    // 选中详情：点击符号后在此渲染（列表下方，方便边看边导航）。
    if let Some(id) = sel.clone() {
        if let Some(sym) = symbols.iter().find(|s| s.id == id) {
            ui.add_space(6.0);
            let drect = detail_panel(ui, pal, sym, symbols, sel, &mut scroll_now);
            // 本帧发生跳转（点符号 / 点关系 chip）：把详情面板滚进视野。
            if scroll_now {
                ui.scroll_to_rect(drect, None);
            }
        }
    }
    *scroll_hint = scroll_now;
    rows
}

/// 顶部统计卡：符号 / 文件 / 函数 / 类型。
fn render_stats(ui: &mut egui::Ui, pal: &Palette, symbols: &[CodeSymbol]) {
    let mut files: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut funcs = 0usize;
    let mut kinds: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for s in symbols {
        files.insert(&s.file);
        let k = s.kind.to_ascii_lowercase();
        if k == "fn" || k == "function" || k == "method" {
            funcs += 1;
        }
        kinds.insert(s.kind.as_str());
    }
    let stats = [
        ("符号", symbols.len()),
        ("文件", files.len()),
        ("函数", funcs),
        ("类型", kinds.len()),
    ];
    ui.horizontal_wrapped(|ui| {
        for (label, n) in stats {
            let w = 92.0;
            let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 46.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(rect, egui::Rounding::same(9.0), pal.field);
            ui.painter().rect(
                rect,
                egui::Rounding::same(9.0),
                egui::Color32::TRANSPARENT,
                egui::Stroke::new(1.0_f32, pal.border),
            );
            ui.painter().text(
                egui::pos2(rect.min.x + 12.0, rect.center().y - 6.0),
                egui::Align2::LEFT_CENTER,
                format!("{n}"),
                egui::FontId::proportional(18.0),
                pal.text,
            );
            ui.painter().text(
                egui::pos2(rect.min.x + 12.0, rect.center().y + 11.0),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(10.5),
                pal.dim,
            );
            ui.add_space(6.0);
        }
    });
}

/// 空态提示。
fn empty_hint(ui: &mut egui::Ui, pal: &Palette) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(
            "还没有代码符号。点击「重新索引资产」扫描工作区源码，自动建立代码图谱；之后即可按文件浏览符号、查看调用关系。",
        )
        .size(12.0)
        .color(pal.dim),
    );
}

/// 组头右侧的计数徽标。
fn count_badge(ui: &mut egui::Ui, pal: &Palette, n: usize) {
    let text = format!("{n}");
    let w = 14.0 + text.chars().count() as f32 * 8.5;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 18.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::Rounding::same(9.0), pal.hover);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        &text,
        egui::FontId::proportional(10.5),
        pal.dim,
    );
}

/// 单个符号行：类型徽标 + 名称 + 签名 + 关系计数。点击选中/跳转。
/// 返回是否发生了跳转（需要滚动提示）。
fn symbol_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    sym: &CodeSymbol,
    all: &[CodeSymbol],
    sel: &mut Option<String>,
) -> bool {
    let (kind_label, kind_color) = kind_style(&sym.kind);
    let is_sel = sel.as_ref().is_some_and(|id| id == &sym.id);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 34.0), egui::Sense::click());
    if is_sel || resp.hovered() {
        ui.painter()
            .rect_filled(rect.shrink(1.0), egui::Rounding::same(7.0), pal.hover);
    }
    if is_sel {
        // 左侧 accent 竖条标识选中。
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + 2.0, rect.min.y + 6.0),
            egui::vec2(2.5, rect.height() - 12.0),
        );
        ui.painter()
            .rect_filled(bar, egui::Rounding::same(2.0), pal.accent);
    }
    let mut x = rect.min.x + 12.0;
    // 类型徽标
    let badge = kind_badge(pal, &kind_label, kind_color);
    ui.painter().rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(x, rect.center().y - 9.0),
            egui::vec2(badge.w, 18.0),
        ),
        egui::Rounding::same(4.0),
        badge.bg,
    );
    ui.painter().text(
        egui::pos2(x + badge.w / 2.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        &kind_label,
        egui::FontId::proportional(10.0),
        badge.fg,
    );
    x += badge.w + 8.0;
    // 符号名
    ui.painter().text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &sym.name,
        egui::FontId::proportional(13.0),
        pal.text,
    );
    x += sym.name.chars().count() as f32 * 13.0 + 8.0;
    // 签名（dim，截断防溢出）
    if !sym.signature.is_empty() {
        let sig: String = sym.signature.chars().take(46).collect();
        let w = (sig.chars().count() as f32 * 6.8).min((ui.available_width() - x - 160.0).max(0.0));
        if w > 20.0 {
            ui.painter().text(
                egui::pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                &sig,
                egui::FontId::monospace(11.0),
                pal.dim,
            );
        }
    }
    // 右侧关系计数（调用 N / 被调 M）
    let callers = count_callers(all, &sym.id);
    let callees = sym.calls.len();
    let rel = format!("调用 {callees} · 被调 {callers}");
    ui.painter().text(
        egui::pos2(rect.max.x - 12.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        &rel,
        egui::FontId::proportional(10.5),
        pal.dim,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.clicked() {
        if sel.as_ref() == Some(&sym.id) {
            *sel = None;
        } else {
            *sel = Some(sym.id.clone());
            return true;
        }
    }
    false
}

struct KindBadge {
    w: f32,
    bg: egui::Color32,
    fg: egui::Color32,
}

fn kind_badge(pal: &Palette, label: &str, color: egui::Color32) -> KindBadge {
    let _ = pal;
    // 徽标底色 = 主题色 22% 透明度叠加；文字色 = 主题色本体。
    let w = 12.0 + label.chars().count() as f32 * 7.0;
    KindBadge {
        w,
        bg: egui::Color32::from_rgba_premultiplied(
            color.r(),
            color.g(),
            color.b(),
            (255.0 * 0.22) as u8,
        ),
        fg: color,
    }
}

/// 统计某符号被谁直接调用（在图内）。
fn count_callers(all: &[CodeSymbol], id: &str) -> usize {
    all.iter()
        .filter(|s| s.calls.iter().any(|c| c == id))
        .count()
}

/// 选中符号详情面板：名称、文件、签名、摘要 + 调用方/被调方可点击导航。
/// 返回面板的绘制矩形（供跳转滚动定位）。
fn detail_panel(
    ui: &mut egui::Ui,
    pal: &Palette,
    sym: &CodeSymbol,
    all: &[CodeSymbol],
    sel: &mut Option<String>,
    scroll_hint: &mut bool,
) -> egui::Rect {
    let (kind_label, kind_color) = kind_style(&sym.kind);
    let mut out_rect = ui.min_rect();
    egui::Frame::default()
        .fill(pal.field)
        .rounding(egui::Rounding::same(10.0))
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .show(ui, |ui| {
            out_rect = ui.min_rect();
            ui.set_min_width(ui.available_width());
            // 标题行：类型徽标 + 名称 + 关闭按钮
            ui.horizontal(|ui| {
                let badge = kind_badge(pal, &kind_label, kind_color);
                let (b, _) =
                    ui.allocate_exact_size(egui::vec2(badge.w, 18.0), egui::Sense::hover());
                ui.painter()
                    .rect_filled(b, egui::Rounding::same(4.0), badge.bg);
                ui.painter().text(
                    b.center(),
                    egui::Align2::CENTER_CENTER,
                    &kind_label,
                    egui::FontId::proportional(10.0),
                    badge.fg,
                );
                ui.label(
                    egui::RichText::new(&sym.name)
                        .size(14.0)
                        .strong()
                        .color(pal.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if close_button(ui, pal) {
                        *sel = None;
                    }
                });
            });
            ui.add_space(4.0);
            if !sym.signature.is_empty() {
                ui.label(
                    egui::RichText::new(&sym.signature)
                        .size(12.0)
                        .color(pal.dim),
                );
            }
            ui.label(
                egui::RichText::new(format!("文件: {}", sym.file))
                    .size(11.5)
                    .color(pal.dim),
            );
            if !sym.summary.is_empty() {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(&sym.summary).size(12.0).color(pal.text));
            }
            ui.add_space(8.0);
            // 被调方
            if !sym.calls.is_empty() {
                ui.label(
                    egui::RichText::new(format!("调用 ({}):", sym.calls.len()))
                        .size(11.5)
                        .color(pal.dim),
                );
                ui.horizontal_wrapped(|ui| {
                    for c in &sym.calls {
                        if let Some(target) = all.iter().find(|s| &s.id == c) {
                            if chip_button(ui, pal, &target.name) {
                                *sel = Some(target.id.clone());
                                *scroll_hint = true;
                            }
                        } else {
                            chip_text(ui, pal, c);
                        }
                    }
                });
                ui.add_space(6.0);
            }
            // 调用方
            let callers: Vec<&CodeSymbol> = all
                .iter()
                .filter(|s| s.calls.iter().any(|c| c == &sym.id))
                .collect();
            if !callers.is_empty() {
                ui.label(
                    egui::RichText::new(format!("被调用 ({}):", callers.len()))
                        .size(11.5)
                        .color(pal.dim),
                );
                ui.horizontal_wrapped(|ui| {
                    for c in callers {
                        if chip_button(ui, pal, &c.name) {
                            *sel = Some(c.id.clone());
                            *scroll_hint = true;
                        }
                    }
                });
            }
        });
    out_rect
}

/// 可点击关系 chip（悬停手型 + 主题描边）。
fn chip_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> bool {
    let text_w = label.chars().count() as f32 * 7.2 + 16.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(text_w, 20.0), egui::Sense::click());
    let fill = if resp.hovered() { pal.hover } else { pal.field };
    let stroke_color = if resp.hovered() {
        pal.accent
    } else {
        pal.border
    };
    ui.painter().rect(
        rect,
        egui::Rounding::same(10.0),
        fill,
        egui::Stroke::new(1.0_f32, stroke_color),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(10.5),
        if resp.hovered() { pal.accent } else { pal.text },
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.on_hover_text("跳转到该符号").clicked()
}

/// 不可点击的关系 chip（目标不在图内，灰色哑光态）。
fn chip_text(ui: &mut egui::Ui, pal: &Palette, label: &str) {
    let text_w = label.chars().count() as f32 * 7.2 + 16.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(text_w, 20.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::Rounding::same(10.0), pal.field);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(10.5),
        pal.dim,
    );
}
