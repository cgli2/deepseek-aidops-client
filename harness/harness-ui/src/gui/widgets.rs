//! Stateless reusable GUI controls.

use super::icons::{Icon, draw_icon};
use super::model::{PluginKind, PluginUiRow};
use super::theme::Palette;

/// 侧栏扁平导航项：透明底、悬停微亮、矢量图标。返回是否点击。
pub(super) fn nav_item(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: Icon,
    label: &str,
    expanded: bool,
    enabled: bool,
    accent: bool,
) -> bool {
    #[cfg(target_os = "macos")]
    let height = 38.0;
    #[cfg(not(target_os = "macos"))]
    let height = 36.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let hovered = enabled && response.hovered();
    if hovered {
        ui.painter()
            .rect_filled(rect.shrink(2.0), egui::Rounding::same(8.0), pal.hover);
    }
    let icon_color = if accent { pal.accent } else { pal.dim };
    let text_color = if !enabled { pal.dim } else { pal.text };
    let icon_center = egui::pos2(
        rect.min.x + if expanded { 20.0 } else { rect.width() / 2.0 },
        rect.center().y,
    );
    draw_icon(ui.painter(), icon_center, icon, icon_color);
    if expanded {
        ui.painter().text(
            egui::pos2(rect.min.x + 40.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(if cfg!(target_os = "macos") {
                13.5
            } else {
                13.0
            }),
            text_color,
        );
    }
    response.clicked() && enabled
}

/// 模态面板右上角关闭按钮（矢量 ✕，悬停微亮）。
pub(super) fn close_button(ui: &mut egui::Ui, pal: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(6.0), pal.hover);
    }
    let c = rect.center();
    let d = 4.5;
    let stroke = egui::Stroke::new(1.6_f32, if resp.hovered() { pal.text } else { pal.dim });
    ui.painter().line_segment(
        [egui::pos2(c.x - d, c.y - d), egui::pos2(c.x + d, c.y + d)],
        stroke,
    );
    ui.painter().line_segment(
        [egui::pos2(c.x - d, c.y + d), egui::pos2(c.x + d, c.y - d)],
        stroke,
    );
    resp.clicked()
}

/// 主操作按钮（柔和青底、内容自适应宽度，不再占满整行）。
pub(super) fn accent_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> bool {
    // 宽度按文字估算：CJK 约 13.5px、ASCII 约 7.5px，再加左右内边距。
    let text_w: f32 = label
        .chars()
        .map(|c| if c.is_ascii() { 7.5 } else { 13.5 })
        .sum();
    let w = (text_w + 44.0).max(130.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 34.0), egui::Sense::click());
    let fill = if resp.hovered() {
        pal.btn_hover
    } else {
        pal.btn_fill
    };
    ui.painter()
        .rect_filled(rect, egui::Rounding::same(8.0), fill);
    ui.painter().rect(
        rect,
        egui::Rounding::same(8.0),
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(1.0_f32, pal.btn_border),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(13.0),
        pal.btn_text,
    );
    resp.clicked()
}

/// 插件列表单行：返回（是否移除、启用状态是否变化）。
pub(super) fn plugin_row_ui(
    ui: &mut egui::Ui,
    pal: &Palette,
    row: &mut PluginUiRow,
) -> (bool, bool) {
    let mut removed = false;
    let was_enabled = row.enabled;
    // 统一行宽：内容区撑满外层可用宽度（扣除左右内边距），
    // 卡片边框左右对齐且不超出面板。
    let margin = egui::Margin::symmetric(12.0, 9.0);
    let row_w = (ui.available_width() - margin.sum().x).max(200.0);
    egui::Frame::default()
        .fill(pal.field)
        .rounding(egui::Rounding::same(9.0))
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_min_width(row_w);
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                if row.kind == PluginKind::Core {
                    // 核心插件恒启用：禁用态控件直观传达「不可取消勾选」。
                    let mut on = true;
                    ui.add_enabled(false, egui::Checkbox::new(&mut on, ""));
                } else {
                    ui.checkbox(&mut row.enabled, "");
                }
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&row.name).size(13.0).color(pal.text));
                        ui.label(
                            egui::RichText::new(match row.kind {
                                PluginKind::Core => "核心",
                                PluginKind::Wasm => "WASM",
                                PluginKind::Trellis => "Trellis",
                            })
                            .size(10.0)
                            .color(pal.accent),
                        );
                        if row.kind != PluginKind::Core {
                            ui.label(
                                egui::RichText::new(if row.active {
                                    "运行中"
                                } else if row.enabled {
                                    "待加载"
                                } else {
                                    "已禁用"
                                })
                                .size(10.0)
                                .color(pal.dim),
                            );
                        }
                    });
                    ui.add(
                        egui::Label::new(egui::RichText::new(&row.desc).size(11.0).color(pal.dim))
                            // 描述单行展示不换行，超长截断省略。
                            .wrap_mode(egui::TextWrapMode::Truncate),
                    );
                });
                if row.kind == PluginKind::Wasm {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ghost_button(ui, pal, "移除") {
                            removed = true;
                        }
                    });
                }
            });
        });
    ui.add_space(6.0);
    (removed, was_enabled != row.enabled)
}

/// 次级按钮（描边幽灵风格）。
pub(super) fn ghost_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> bool {
    // 高度与主操作按钮 accent_button 保持一致（34px）。
    let size = egui::vec2(label.chars().count() as f32 * 13.0 + 20.0, 34.0);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.hovered() {
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(6.0), pal.hover);
    }
    ui.painter().rect(
        rect,
        egui::Rounding::same(6.0),
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(1.0_f32, pal.border),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(12.0),
        if resp.hovered() { pal.text } else { pal.dim },
    );
    resp.clicked()
}

#[derive(Clone, Copy)]
pub(super) enum SidebarActionIcon {
    Add,
    Archive,
}

pub(super) fn sidebar_control_height() -> f32 {
    if cfg!(target_os = "macos") {
        26.0
    } else {
        24.0
    }
}

/// 侧栏紧凑图标按钮：固定尺寸和矢量图形，避免平台字体造成基线偏移。
pub(super) fn sidebar_icon_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: SidebarActionIcon,
    tooltip: &str,
) -> bool {
    let height = sidebar_control_height();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(height, height), egui::Sense::click());
    let fill = if response.hovered() {
        pal.hover
    } else {
        pal.field
    };
    ui.painter().rect(
        rect,
        egui::Rounding::same(6.0),
        fill,
        egui::Stroke::new(1.0_f32, pal.border),
    );
    let color = if response.hovered() {
        pal.text
    } else {
        pal.dim
    };
    let stroke = egui::Stroke::new(1.4_f32, color);
    let c = rect.center();
    match icon {
        SidebarActionIcon::Add => {
            ui.painter().line_segment(
                [c + egui::vec2(-4.0, 0.0), c + egui::vec2(4.0, 0.0)],
                stroke,
            );
            ui.painter().line_segment(
                [c + egui::vec2(0.0, -4.0), c + egui::vec2(0.0, 4.0)],
                stroke,
            );
        }
        SidebarActionIcon::Archive => {
            let body =
                egui::Rect::from_center_size(c + egui::vec2(0.0, 1.5), egui::vec2(10.0, 7.0));
            ui.painter().rect(
                body,
                egui::Rounding::same(1.5),
                egui::Color32::TRANSPARENT,
                stroke,
            );
            ui.painter().line_segment(
                [c + egui::vec2(-5.5, -3.0), c + egui::vec2(5.5, -3.0)],
                stroke,
            );
            ui.painter().line_segment(
                [c + egui::vec2(0.0, -6.0), c + egui::vec2(0.0, -1.0)],
                stroke,
            );
            ui.painter().line_segment(
                [c + egui::vec2(-2.0, -3.0), c + egui::vec2(0.0, -1.0)],
                stroke,
            );
            ui.painter().line_segment(
                [c + egui::vec2(2.0, -3.0), c + egui::vec2(0.0, -1.0)],
                stroke,
            );
        }
    }
    response.on_hover_text(tooltip).clicked()
}

/// 侧栏文字按钮：macOS/Windows 各自使用合适高度，文字始终按按钮中心绘制。
pub(super) fn sidebar_text_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    label: &str,
    tooltip: &str,
) -> bool {
    let height = sidebar_control_height();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(44.0, height), egui::Sense::click());
    let fill = if response.hovered() {
        pal.hover
    } else {
        pal.field
    };
    ui.painter().rect(
        rect,
        egui::Rounding::same(6.0),
        fill,
        egui::Stroke::new(1.0_f32, pal.border),
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(if cfg!(target_os = "macos") {
            11.5
        } else {
            11.0
        }),
        if response.hovered() {
            pal.text
        } else {
            pal.dim
        },
    );
    response.on_hover_text(tooltip).clicked()
}

/// 带搜索图标和清除动作的侧栏搜索框。
pub(super) fn sidebar_search_field(ui: &mut egui::Ui, pal: &Palette, value: &mut String) {
    let id = ui.make_persistent_id("history_search_input");
    let focused = ui.memory(|memory| memory.has_focus(id));
    let stroke_color = if focused { pal.accent } else { pal.border };
    let mut clear = false;
    egui::Frame::default()
        .fill(pal.field)
        .rounding(egui::Rounding::same(7.0))
        .stroke(egui::Stroke::new(1.0_f32, stroke_color))
        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.set_min_height(sidebar_control_height() - 8.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (icon_rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                let center = icon_rect.center() + egui::vec2(-1.0, -1.0);
                let stroke = egui::Stroke::new(1.25_f32, pal.dim);
                ui.painter().circle_stroke(center, 4.0, stroke);
                ui.painter().line_segment(
                    [center + egui::vec2(3.0, 3.0), center + egui::vec2(6.0, 6.0)],
                    stroke,
                );
                ui.add(
                    egui::TextEdit::singleline(value)
                        .id_source(id)
                        .desired_width(f32::INFINITY)
                        .frame(false)
                        .margin(egui::Margin::same(0.0))
                        .hint_text(egui::RichText::new("搜索历史…").color(pal.dim)),
                );
                if !value.is_empty() {
                    let (clear_rect, response) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
                    if response.hovered() {
                        ui.painter()
                            .circle_filled(clear_rect.center(), 7.0, pal.hover);
                    }
                    let c = clear_rect.center();
                    let stroke = egui::Stroke::new(1.15_f32, pal.dim);
                    ui.painter().line_segment(
                        [c + egui::vec2(-2.5, -2.5), c + egui::vec2(2.5, 2.5)],
                        stroke,
                    );
                    ui.painter().line_segment(
                        [c + egui::vec2(-2.5, 2.5), c + egui::vec2(2.5, -2.5)],
                        stroke,
                    );
                    clear = response.on_hover_text("清除搜索").clicked();
                }
            });
        });
    if clear {
        value.clear();
    }
}

/// 表单字段标签（暗色小号，上方留白）。
pub(super) fn field_label(ui: &mut egui::Ui, pal: &Palette, label: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(label).size(12.0).color(pal.dim));
    ui.add_space(3.0);
}

// ── 记忆面板：浏览本地原生记忆资产（与 harness-provider-memory 落盘结构一致）──
// 注意：本面板读取 `<cwd>/.harness-memory` 下的本地文件，反映 dsh「不接入后端时的
// 原生记忆」。若已配置并连接 aidops 后端，后端的记忆以远端为准，此处仅展示本地副本。
