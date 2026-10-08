//! Stateless reusable GUI controls with industrial-grade micro-animations.

use super::fonts::{FONT_BODY, FONT_CAPTION, FONT_MICRO, FONT_SECONDARY, FONT_UI};
use super::icons::{Icon, draw_icon};
use super::model::{PluginKind, PluginUiRow};
use super::theme::Palette;

/// 桌面标准控件尺寸常量（macOS 4px 模块化排版）
pub const BTN_HEIGHT_REGULAR: f32 = 28.0;
pub const BTN_HEIGHT_COMPACT: f32 = 24.0;
pub const BTN_ROUNDING: f32 = 5.0;

/// 线性插值颜色（含 Alpha 通道，平滑过渡）。
pub(crate) fn lerp_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    egui::Color32::from_rgba_premultiplied(
        (a.r() as f32 * (1.0 - t) + b.r() as f32 * t).round() as u8,
        (a.g() as f32 * (1.0 - t) + b.g() as f32 * t).round() as u8,
        (a.b() as f32 * (1.0 - t) + b.b() as f32 * t).round() as u8,
        (a.a() as f32 * (1.0 - t) + b.a() as f32 * t).round() as u8,
    )
}

/// 统一交互动效管道：返回 (hover_t 0.0..1.0 约120ms阻尼, active_t 0.0..1.0 约70ms按下阻尼)。
pub(crate) fn animate_interaction(
    ui: &mut egui::Ui,
    id: egui::Id,
    response: &egui::Response,
) -> (f32, f32) {
    let hover_t = ui
        .ctx()
        .animate_bool_responsive(id.with("hov"), response.hovered());
    let active_t = ui
        .ctx()
        .animate_bool_responsive(id.with("act"), response.is_pointer_button_down_on());
    (hover_t, active_t)
}

/// 绘制 Apple 风格顶边 1.0px Specular Highlight 天光内发丝高光（增强视窗与卡片的立体物理景深）
pub(crate) fn draw_specular_highlight(
    painter: &egui::Painter,
    rect: egui::Rect,
    rounding: egui::Rounding,
    highlight_color: egui::Color32,
) {
    if highlight_color == egui::Color32::TRANSPARENT || !rect.is_positive() {
        return;
    }
    let inset_x = (rounding.nw.max(rounding.ne) * 0.75).clamp(1.0, 8.0);
    let y = rect.min.y + 0.5;
    let p1 = egui::pos2(rect.min.x + inset_x, y);
    let p2 = egui::pos2(rect.max.x - inset_x, y);
    if p2.x > p1.x {
        painter.line_segment([p1, p2], egui::Stroke::new(1.0, highlight_color));
    }
}

/// 绘制 macOS Aqua 外发光双层焦点环（2.0px 外扩柔光光晕，清晰提示键盘与交互聚焦）
pub(crate) fn draw_focus_ring(
    painter: &egui::Painter,
    rect: egui::Rect,
    rounding: egui::Rounding,
    ring_color: egui::Color32,
    halo_color: egui::Color32,
) {
    if ring_color == egui::Color32::TRANSPARENT || !rect.is_positive() {
        return;
    }
    // 外层柔光光晕 (halo)
    let halo_rect = rect.expand(2.5);
    let halo_rounding = egui::Rounding {
        nw: rounding.nw + 2.5,
        ne: rounding.ne + 2.5,
        sw: rounding.sw + 2.5,
        se: rounding.se + 2.5,
    };
    painter.rect(
        halo_rect,
        halo_rounding,
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(1.5, halo_color),
    );
    // 内层主焦点环 (ring)
    let ring_rect = rect.expand(1.2);
    let ring_rounding = egui::Rounding {
        nw: rounding.nw + 1.2,
        ne: rounding.ne + 1.2,
        sw: rounding.sw + 1.2,
        se: rounding.se + 1.2,
    };
    painter.rect(
        ring_rect,
        ring_rounding,
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(1.2, ring_color),
    );
}

/// 侧栏扁平导航项：半透明微光悬停、矢量图标渐亮、按下微形变。返回是否点击。
pub(super) fn nav_item(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: Icon,
    label: &str,
    expanded: bool,
    enabled: bool,
    accent: bool,
) -> bool {
    let height = 30.0;
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    let (hover_t, active_t) = if enabled {
        animate_interaction(ui, response.id, &response)
    } else {
        (0.0, 0.0)
    };

    // 绘制半透明悬停层
    let bg_color = if active_t > 0.05 {
        pal.translucent_active()
    } else {
        pal.translucent_hover(hover_t)
    };
    if bg_color != egui::Color32::TRANSPARENT {
        let draw_rect = rect.shrink(1.5 + 0.3 * active_t);
        ui.painter()
            .rect_filled(draw_rect, egui::Rounding::same(BTN_ROUNDING), bg_color);
    }

    // 图标与文字随悬停微提亮
    let base_icon_color = if accent { pal.accent } else { pal.dim };
    let target_icon_color = if accent { pal.accent } else { pal.text };
    let icon_color = if !enabled {
        pal.dim
    } else {
        lerp_color(base_icon_color, target_icon_color, hover_t)
    };

    let text_color = if !enabled {
        pal.dim
    } else {
        lerp_color(pal.dim, pal.text, hover_t * 0.85 + 0.15)
    };

    let icon_center = egui::pos2(
        rect.min.x + if expanded { 18.0 } else { rect.width() / 2.0 },
        rect.center().y + 0.2 * active_t,
    );
    draw_icon(ui.painter(), icon_center, icon, icon_color);

    if expanded {
        ui.painter().text(
            egui::pos2(rect.min.x + 36.0, rect.center().y + 0.2 * active_t),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(FONT_BODY),
            text_color,
        );
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label)
    });
    response.clicked() && enabled
}

/// 模态面板右上角关闭按钮（矢量关闭图标，平滑半透明光晕与微阻尼）。
pub(super) fn close_button(ui: &mut egui::Ui, pal: &Palette) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
    let (hover_t, active_t) = animate_interaction(ui, resp.id, &resp);

    let bg_color = if active_t > 0.05 {
        pal.translucent_active()
    } else {
        pal.translucent_hover(hover_t)
    };
    if bg_color != egui::Color32::TRANSPARENT {
        let draw_rect = rect.shrink(0.4 * active_t);
        ui.painter()
            .rect_filled(draw_rect, egui::Rounding::same(4.5), bg_color);
    }

    let c = rect.center() + egui::vec2(0.0, 0.3 * active_t);
    let cross_color = lerp_color(pal.dim, pal.text, hover_t);
    draw_icon(ui.painter(), c, Icon::X, cross_color);
    resp.clicked()
}

/// 通用紧凑图标按钮（24x24，对标 macOS 工具栏图标按钮，微阻尼悬停与按下物理微缩）。
pub(super) fn icon_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: Icon,
    tooltip: &str,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
    let (hover_t, active_t) = animate_interaction(ui, resp.id, &resp);

    let bg_color = if active_t > 0.05 {
        pal.translucent_active()
    } else {
        pal.translucent_hover(hover_t)
    };
    let draw_rect = rect.shrink(0.35 * active_t);
    if bg_color != egui::Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(draw_rect, egui::Rounding::same(4.5), bg_color);
    }
    let icon_color = lerp_color(pal.dim, pal.text, hover_t);
    draw_icon(ui.painter(), draw_rect.center(), icon, icon_color);
    if !tooltip.is_empty() {
        resp.clone().on_hover_text(tooltip);
    }
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tooltip)
    });
    resp.clicked()
}

/// 主操作按钮（自适应宽度、微阻尼悬停、按下物理微下沉）。
pub(super) fn accent_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> bool {
    accent_button_ex(ui, pal, label, true)
}

/// 主操作按钮（支持禁用态，尺寸位置严格不变，对标 macOS 桌面原生按钮）。
pub(super) fn accent_button_ex(
    ui: &mut egui::Ui,
    pal: &Palette,
    label: &str,
    enabled: bool,
) -> bool {
    let text_w: f32 = label
        .chars()
        .map(|c| if c.is_ascii() { 7.0 } else { 12.0 })
        .sum();
    let w = (text_w + 24.0).max(68.0);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(w, BTN_HEIGHT_REGULAR), egui::Sense::click());
    let (hover_t, active_t) = if enabled {
        animate_interaction(ui, resp.id, &resp)
    } else {
        (0.0, 0.0)
    };

    let fill = if !enabled {
        pal.field
    } else {
        lerp_color(pal.btn_fill, pal.btn_hover, hover_t)
    };

    let border_color = if !enabled {
        pal.border
    } else {
        lerp_color(pal.btn_border, pal.accent, hover_t * 0.7)
    };

    // 按下时发生微量形变，呈现机械按压反馈
    let draw_rect = rect.shrink(0.4 * active_t);
    ui.painter()
        .rect_filled(draw_rect, egui::Rounding::same(BTN_ROUNDING), fill);
    ui.painter().rect(
        draw_rect,
        egui::Rounding::same(BTN_ROUNDING),
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(1.0_f32, border_color),
    );

    let text_color = if !enabled {
        pal.dim
    } else {
        lerp_color(pal.btn_text, pal.text, hover_t * 0.5)
    };

    ui.painter().text(
        draw_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(FONT_UI),
        text_color,
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label)
    });
    enabled && resp.clicked()
}

/// 插件列表单行：返回（是否移除、启用状态是否变化）。
pub(super) fn plugin_row_ui(
    ui: &mut egui::Ui,
    pal: &Palette,
    row: &mut PluginUiRow,
) -> (bool, bool) {
    let mut removed = false;
    let was_enabled = row.enabled;
    let margin = egui::Margin::symmetric(12.0, 8.0);
    let row_w = (ui.available_width() - margin.sum().x).max(200.0);
    egui::Frame::default()
        .fill(pal.field)
        .rounding(egui::Rounding::same(6.0))
        .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
        .inner_margin(margin)
        .show(ui, |ui| {
            ui.set_min_width(row_w);
            ui.horizontal(|ui| {
                ui.add_space(2.0);
                if row.kind == PluginKind::Core {
                    let mut on = true;
                    ui.add_enabled(false, egui::Checkbox::new(&mut on, ""));
                } else {
                    ui.checkbox(&mut row.enabled, "");
                }
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&row.name).size(FONT_UI).color(pal.text));
                        ui.label(
                            egui::RichText::new(match row.kind {
                                PluginKind::Core => "核心",
                                PluginKind::Wasm => "WASM",
                                PluginKind::Trellis => "Trellis",
                            })
                            .size(FONT_MICRO)
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
                                .size(FONT_MICRO)
                                .color(pal.dim),
                            );
                        }
                    });
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&row.desc)
                                .size(FONT_CAPTION)
                                .color(pal.dim),
                        )
                        .wrap_mode(egui::TextWrapMode::Truncate),
                    );
                });
                if row.kind == PluginKind::Wasm {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if compact_button(ui, pal, "移除") {
                            removed = true;
                        }
                    });
                }
            });
        });
    ui.add_space(4.0);
    (removed, was_enabled != row.enabled)
}

/// 次级幽灵按钮（细描边、平滑半透明底色过渡）。
pub(super) fn ghost_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> bool {
    let text_w: f32 = label
        .chars()
        .map(|c| if c.is_ascii() { 7.0 } else { 12.0 })
        .sum();
    let size = egui::vec2((text_w + 20.0).max(56.0), BTN_HEIGHT_REGULAR);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let (hover_t, active_t) = animate_interaction(ui, resp.id, &resp);

    let bg_color = if active_t > 0.05 {
        pal.translucent_active()
    } else {
        pal.translucent_hover(hover_t)
    };
    let draw_rect = rect.shrink(0.4 * active_t);
    if bg_color != egui::Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(draw_rect, egui::Rounding::same(BTN_ROUNDING), bg_color);
    }
    let border_color = lerp_color(pal.border, pal.accent.gamma_multiply(0.6), hover_t);
    ui.painter().rect(
        draw_rect,
        egui::Rounding::same(BTN_ROUNDING),
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(1.0_f32, border_color),
    );

    let text_color = lerp_color(pal.dim, pal.text, hover_t);
    ui.painter().text(
        draw_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(FONT_UI),
        text_color,
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label)
    });
    resp.clicked()
}

#[derive(Clone, Copy)]
pub(super) enum SidebarActionIcon {
    Add,
    Archive,
}

pub(super) fn sidebar_control_height() -> f32 {
    24.0
}

/// 侧栏紧凑图标按钮：常态透明、悬停平滑半透明浮出、点击微下沉。
pub(super) fn sidebar_icon_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: SidebarActionIcon,
    tooltip: &str,
) -> bool {
    let height = sidebar_control_height();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(height, height), egui::Sense::click());
    let (hover_t, active_t) = animate_interaction(ui, response.id, &response);

    let bg = if active_t > 0.05 {
        pal.translucent_active()
    } else {
        pal.translucent_hover(hover_t)
    };

    let draw_rect = rect.shrink(0.3 * active_t);
    if bg != egui::Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(draw_rect, egui::Rounding::same(4.5), bg);
        let border = lerp_color(egui::Color32::TRANSPARENT, pal.border, hover_t);
        ui.painter().rect(
            draw_rect,
            egui::Rounding::same(4.5),
            egui::Color32::TRANSPARENT,
            egui::Stroke::new(1.0_f32, border),
        );
    }

    let color = lerp_color(pal.dim, pal.text, hover_t);
    let semantic_icon = match icon {
        SidebarActionIcon::Add => Icon::Plus,
        SidebarActionIcon::Archive => Icon::ArchiveBox,
    };
    draw_icon(ui.painter(), draw_rect.center(), semantic_icon, color);
    response.on_hover_text(tooltip).clicked()
}

/// 带搜索图标和清除动作的侧栏搜索框。
pub(super) fn sidebar_search_field(ui: &mut egui::Ui, pal: &Palette, value: &mut String) {
    let id = egui::Id::new("history_search_input");
    let focused = ui.memory(|memory| memory.has_focus(id));
    let stroke_color = if focused { pal.accent } else { pal.border };
    let mut clear = false;
    let frame_resp = egui::Frame::default()
        .fill(pal.field)
        .rounding(egui::Rounding::same(BTN_ROUNDING))
        .stroke(egui::Stroke::new(1.0_f32, stroke_color))
        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let icon_c = ui.cursor().min + egui::vec2(6.0, 9.0);
                draw_icon(
                    ui.painter(),
                    icon_c,
                    Icon::Search,
                    if focused { pal.accent } else { pal.dim },
                );
                ui.add_space(14.0);

                let text_edit = egui::TextEdit::singleline(value)
                    .id(id)
                    .hint_text("搜索会话历史...")
                    .text_color(pal.text)
                    .margin(egui::Margin::symmetric(0.0, 0.0))
                    .frame(false)
                    .desired_width(ui.available_width() - 22.0);
                ui.add(text_edit);

                if !value.is_empty() {
                    let (clear_rect, response) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
                    let (hov, _) = animate_interaction(ui, response.id, &response);
                    if hov > 0.01 {
                        ui.painter().circle_filled(
                            clear_rect.center(),
                            7.0,
                            pal.translucent_hover(hov),
                        );
                    }
                    let cross_color = lerp_color(pal.dim, pal.text, hov);
                    draw_icon(ui.painter(), clear_rect.center(), Icon::X, cross_color);
                    clear = response.on_hover_text("清除搜索").clicked();
                }
            });
        });
    if focused {
        draw_focus_ring(
            ui.painter(),
            frame_resp.response.rect,
            egui::Rounding::same(BTN_ROUNDING),
            pal.focus_ring,
            pal.focus_ring_halo,
        );
    }
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

/// 状态徽标胶囊（如 [Running]、[+12 -4]、[fs.write]）：圆角药丸造型、自适应文本。
pub(super) fn badge_pill(
    ui: &mut egui::Ui,
    text: &str,
    text_color: egui::Color32,
    bg_color: egui::Color32,
    border_color: egui::Color32,
) -> egui::Response {
    let font_id = egui::FontId::proportional(11.0);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font_id, text_color);
    let padding = egui::vec2(12.0, 5.0);
    let size = galley.size() + padding;
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::hover());
    let (hov, _) = animate_interaction(ui, resp.id, &resp);

    let actual_bg = if hov > 0.01 {
        lerp_color(bg_color, bg_color.gamma_multiply(1.3), hov)
    } else {
        bg_color
    };

    ui.painter()
        .rect_filled(rect, egui::Rounding::same(rect.height() / 2.0), actual_bg);
    if border_color != egui::Color32::TRANSPARENT {
        ui.painter().rect_stroke(
            rect,
            egui::Rounding::same(rect.height() / 2.0),
            egui::Stroke::new(1.0_f32, border_color),
        );
    }
    let text_pos = rect.center() - galley.size() / 2.0;
    ui.painter().galley(text_pos, galley, text_color);
    resp
}

/// 单个分段选项卡定义：可选矢量 Icon + 文本标签。
#[derive(Clone, Copy)]
pub(super) struct TabOption<'a> {
    pub(super) icon: Option<Icon>,
    pub(super) label: &'a str,
}

impl<'a> TabOption<'a> {
    #[allow(dead_code)]
    pub(crate) const fn new(icon: Option<Icon>, label: &'a str) -> Self {
        Self { icon, label }
    }
    #[allow(dead_code)]
    pub(super) const fn text_only(label: &'a str) -> Self {
        Self { icon: None, label }
    }
}

/// 分段式选项卡切换器（Segmented Tabs，如 [文件预览 | 代码变更 | 运行时遥测]）：
/// 具备 macOS Sonoma 级别的弹性滑动胶囊（Sliding Pill Animation）与矢量图标居中对齐。
pub(super) fn segmented_icon_tabs(
    ui: &mut egui::Ui,
    pal: &Palette,
    options: &[TabOption<'_>],
    selected: usize,
) -> Option<usize> {
    let mut clicked = None;
    let tabs_id = ui.make_persistent_id("segmented_icon_tabs_bar");
    let frame_margin = egui::Margin::same(2.0);

    egui::Frame::default()
        .fill(pal.field)
        .rounding(egui::Rounding::same(7.0))
        .stroke(egui::Stroke::new(1.0_f32, pal.border))
        .inner_margin(frame_margin)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;

                // 预先计算各 Tab 矩形
                let mut tab_rects = Vec::with_capacity(options.len());
                let mut responses = Vec::with_capacity(options.len());

                for (idx, opt) in options.iter().enumerate() {
                    let is_sel = idx == selected;
                    let text_w: f32 = opt
                        .label
                        .chars()
                        .map(|c| if c.is_ascii() { 7.0 } else { 12.0 })
                        .sum();
                    let icon_w = if opt.icon.is_some() { 18.0 } else { 0.0 };
                    let total_w = text_w + icon_w + 16.0;

                    let (rect, resp) =
                        ui.allocate_exact_size(egui::vec2(total_w, 24.0), egui::Sense::click());

                    if resp.clicked() && !is_sel {
                        clicked = Some(idx);
                    }
                    tab_rects.push(rect);
                    responses.push(resp);
                }

                // 弹性滑动胶囊动画（Spring / Ease 插值滑动）
                let target_rect = tab_rects
                    .get(selected)
                    .copied()
                    .unwrap_or(egui::Rect::NOTHING);
                let pill_min_x = ui.ctx().animate_value_with_time(
                    tabs_id.with("pill_x"),
                    target_rect.min.x,
                    0.13,
                );
                let pill_w = ui.ctx().animate_value_with_time(
                    tabs_id.with("pill_w"),
                    target_rect.width(),
                    0.13,
                );
                let animated_pill = egui::Rect::from_min_size(
                    egui::pos2(pill_min_x, target_rect.min.y),
                    egui::vec2(pill_w, target_rect.height()),
                );

                // 绘制活动滑块胶囊卡片（带立体柔光投影、高亮底色与天光高光发丝）
                if animated_pill.is_positive() {
                    let shadow_rect = animated_pill.translate(egui::vec2(0.0, 1.0));
                    ui.painter().rect_filled(
                        shadow_rect,
                        egui::Rounding::same(5.0),
                        egui::Color32::from_black_alpha(if pal.is_dark { 50 } else { 20 }),
                    );
                    ui.painter()
                        .rect_filled(animated_pill, egui::Rounding::same(5.0), pal.card_bg);
                    ui.painter().rect(
                        animated_pill,
                        egui::Rounding::same(5.0),
                        egui::Color32::TRANSPARENT,
                        egui::Stroke::new(1.0_f32, pal.card_border),
                    );
                    draw_specular_highlight(
                        ui.painter(),
                        animated_pill,
                        egui::Rounding::same(5.0),
                        pal.specular_highlight,
                    );
                }

                // 绘制各 Tab 图标与文字内容
                for (idx, (opt, (rect, resp))) in options
                    .iter()
                    .zip(tab_rects.iter().zip(responses.iter()))
                    .enumerate()
                {
                    let is_sel = idx == selected;
                    let (hov, _) = animate_interaction(ui, resp.id, resp);

                    // 未选中项悬停时微底色
                    if !is_sel && hov > 0.01 {
                        ui.painter().rect_filled(
                            *rect,
                            egui::Rounding::same(5.0),
                            pal.translucent_hover(hov),
                        );
                    }

                    let text_color = if is_sel {
                        pal.text
                    } else {
                        lerp_color(pal.dim, pal.text, hov)
                    };

                    let icon_color = if is_sel {
                        pal.accent
                    } else {
                        lerp_color(pal.dim, pal.text, hov)
                    };

                    let c = rect.center();
                    if let Some(icon) = opt.icon {
                        let text_w: f32 = opt
                            .label
                            .chars()
                            .map(|c| if c.is_ascii() { 7.0 } else { 12.0 })
                            .sum();
                        let icon_c = egui::pos2(c.x - text_w / 2.0 - 2.0, c.y);
                        draw_icon(ui.painter(), icon_c, icon, icon_color);

                        let text_pos = egui::pos2(icon_c.x + 10.0, c.y);
                        ui.painter().text(
                            text_pos,
                            egui::Align2::LEFT_CENTER,
                            opt.label,
                            egui::FontId::proportional(FONT_SECONDARY),
                            text_color,
                        );
                    } else {
                        ui.painter().text(
                            c,
                            egui::Align2::CENTER_CENTER,
                            opt.label,
                            egui::FontId::proportional(FONT_SECONDARY),
                            text_color,
                        );
                    }
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::RadioButton,
                            true,
                            is_sel,
                            opt.label,
                        )
                    });
                }
            });
        });
    clicked
}

/// 兼容传统纯字符串签名的分段选项卡。
#[allow(dead_code)]
pub(super) fn segmented_tabs(
    ui: &mut egui::Ui,
    pal: &Palette,
    options: &[&str],
    selected: usize,
) -> Option<usize> {
    let opts: Vec<TabOption<'_>> = options.iter().map(|&s| TabOption::text_only(s)).collect();
    segmented_icon_tabs(ui, pal, &opts, selected)
}

/// 极简线性设置导航项：2.5px 左侧高亮指示条、半透明柔和微底色、纯矢量图标、左对齐排版。
pub(super) fn settings_nav_item(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: Icon,
    label: &str,
    selected: bool,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 30.0), egui::Sense::click());
    let (hover_t, active_t) = animate_interaction(ui, resp.id, &resp);
    let draw_rect = rect.shrink(0.3 * active_t);

    if selected {
        let sel_bg = if pal.is_dark {
            egui::Color32::from_white_alpha(18)
        } else {
            egui::Color32::from_black_alpha(12)
        };
        ui.painter()
            .rect_filled(draw_rect, egui::Rounding::same(BTN_ROUNDING), sel_bg);
        let bar_h = (draw_rect.height() - 14.0).max(12.0);
        let bar = egui::Rect::from_min_size(
            egui::pos2(draw_rect.min.x + 2.0, draw_rect.center().y - bar_h / 2.0),
            egui::vec2(2.5, bar_h),
        );
        ui.painter()
            .rect_filled(bar, egui::Rounding::same(1.5), pal.accent);
    } else if hover_t > 0.001 {
        ui.painter().rect_filled(
            draw_rect,
            egui::Rounding::same(BTN_ROUNDING),
            pal.translucent_hover(hover_t),
        );
    }

    let icon_color = if selected {
        pal.accent
    } else {
        lerp_color(pal.dim, pal.text, hover_t)
    };
    draw_icon(
        ui.painter(),
        egui::pos2(draw_rect.min.x + 16.0, draw_rect.center().y),
        icon,
        icon_color,
    );

    let text_color = if selected {
        pal.text
    } else {
        lerp_color(pal.dim, pal.text, hover_t * 0.8 + 0.2)
    };
    ui.painter().text(
        egui::pos2(draw_rect.min.x + 32.0, draw_rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(FONT_UI),
        text_color,
    );
    resp.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            true,
            selected,
            label,
        )
    });
    resp.clicked()
}

/// 分组设置卡片容器：工业级微圆角、微底色、细边框与天光发丝高光。
pub(super) fn settings_card<R>(
    ui: &mut egui::Ui,
    pal: &Palette,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let frame_res = egui::Frame::default()
        .fill(pal.card_bg)
        .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
        .rounding(egui::Rounding::same(6.0))
        .inner_margin(egui::Margin::symmetric(14.0, 10.0))
        .show(ui, add_contents);
    draw_specular_highlight(
        ui.painter(),
        frame_res.response.rect,
        egui::Rounding::same(6.0),
        pal.specular_highlight,
    );
    frame_res.inner
}

/// 分组卡片内发丝级行分割线（1.0px 发丝线，macOS 层叠体系）。
pub(super) fn settings_hairline(ui: &mut egui::Ui, pal: &Palette) {
    ui.add_space(4.0);
    let sep = ui
        .allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover())
        .0;
    ui.painter().rect_filled(sep, 0.0, pal.line);
    ui.add_space(4.0);
}

/// 分组卡片双列设置行：左侧强标题 + 弱提示；右侧对齐交互控件。
pub(super) fn settings_row(
    ui: &mut egui::Ui,
    pal: &Palette,
    title: &str,
    hint: Option<&str>,
    add_control: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(title).size(FONT_UI).color(pal.text));
            if let Some(h) = hint {
                ui.add(
                    egui::Label::new(egui::RichText::new(h).size(FONT_CAPTION).color(pal.dim))
                        .wrap_mode(egui::TextWrapMode::Wrap),
                );
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            add_control(ui);
        });
    });
}

/// 22×22px 微型图标动作按钮：平时无边框、无底色，hover 浮出柔和半透明背景，点击微形变。
pub(super) fn micro_icon_button(
    ui: &mut egui::Ui,
    pal: &Palette,
    icon: Icon,
    tooltip: &str,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    let (hover_t, active_t) = animate_interaction(ui, resp.id, &resp);
    let draw_rect = rect.shrink(0.4 * active_t);

    if active_t > 0.05 {
        ui.painter().rect_filled(
            draw_rect,
            egui::Rounding::same(4.0),
            pal.translucent_active(),
        );
    } else if hover_t > 0.001 {
        ui.painter().rect_filled(
            draw_rect,
            egui::Rounding::same(4.0),
            pal.translucent_hover(hover_t),
        );
    }

    let color = lerp_color(pal.dim, pal.text, hover_t);
    draw_icon(ui.painter(), draw_rect.center(), icon, color);
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tooltip)
    });
    resp.on_hover_text(tooltip).clicked()
}

/// 弱化文字交互动作（非按钮必要，不要时按钮风格）：常态无边框、无底色，仅文字淡亮过渡。
pub(super) fn subtle_text_action(
    ui: &mut egui::Ui,
    pal: &Palette,
    label: &str,
    tooltip: &str,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(label.chars().count() as f32 * 11.5 + 8.0, 22.0),
        egui::Sense::click(),
    );
    let (hover_t, active_t) = animate_interaction(ui, resp.id, &resp);
    let draw_rect = rect.shrink(0.3 * active_t);

    if hover_t > 0.001 {
        ui.painter().rect_filled(
            draw_rect,
            egui::Rounding::same(4.0),
            pal.translucent_hover(hover_t * 0.7),
        );
    }

    let text_color = lerp_color(pal.dim, pal.accent, hover_t);
    ui.painter().text(
        draw_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(FONT_CAPTION),
        text_color,
    );

    resp.on_hover_text(tooltip).clicked()
}

/// 紧凑按钮（高度 24px，对标 macOS 桌面次要控件，解除 130px 臃肿限制）。
pub(super) fn compact_button(ui: &mut egui::Ui, pal: &Palette, label: &str) -> bool {
    let text_w: f32 = label
        .chars()
        .map(|c| if c.is_ascii() { 6.5 } else { 11.5 })
        .sum();
    let size = egui::vec2((text_w + 16.0).max(48.0), BTN_HEIGHT_COMPACT);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let (hover_t, active_t) = animate_interaction(ui, resp.id, &resp);

    let bg_color = if active_t > 0.05 {
        pal.translucent_active()
    } else {
        pal.translucent_hover(hover_t)
    };
    let draw_rect = rect.shrink(0.3 * active_t);
    if bg_color != egui::Color32::TRANSPARENT {
        ui.painter()
            .rect_filled(draw_rect, egui::Rounding::same(4.5), bg_color);
    }
    let border_color = lerp_color(pal.border, pal.accent.gamma_multiply(0.5), hover_t);
    ui.painter().rect(
        draw_rect,
        egui::Rounding::same(4.5),
        egui::Color32::TRANSPARENT,
        egui::Stroke::new(1.0_f32, border_color),
    );

    let text_color = lerp_color(pal.dim, pal.text, hover_t);
    ui.painter().text(
        draw_rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(FONT_SECONDARY),
        text_color,
    );
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label)
    });
    resp.clicked()
}

/// 下拉菜单/弹层无边框选择行（带左侧勾选对齐，支持柔和渐变 hover，无粗暴灰块）
pub(super) fn menu_check_item(
    ui: &mut egui::Ui,
    pal: &Palette,
    label: &str,
    selected: bool,
) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 24.0), egui::Sense::click());
    let (hover_t, active_t) = animate_interaction(ui, resp.id, &resp);
    let draw_rect = rect.shrink(0.3 * active_t);

    if selected {
        let sel_bg = if pal.is_dark {
            egui::Color32::from_white_alpha(15)
        } else {
            egui::Color32::from_black_alpha(10)
        };
        ui.painter()
            .rect_filled(draw_rect, egui::Rounding::same(4.5), sel_bg);
    } else if hover_t > 0.001 {
        ui.painter().rect_filled(
            draw_rect,
            egui::Rounding::same(4.5),
            pal.translucent_hover(hover_t),
        );
    }

    if selected {
        draw_icon(
            ui.painter(),
            egui::pos2(draw_rect.left() + 10.0, draw_rect.center().y),
            Icon::Check,
            pal.accent,
        );
    }

    let text_x = draw_rect.left() + 22.0;
    let text_color = if selected {
        pal.text
    } else {
        lerp_color(pal.dim, pal.text, hover_t)
    };
    ui.painter().text(
        egui::pos2(text_x, draw_rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(FONT_UI),
        text_color,
    );

    resp.clicked()
}
