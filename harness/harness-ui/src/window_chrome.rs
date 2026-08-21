//! 跨平台窗口顶部工作台。macOS 融入原生标题栏；Windows 使用自绘窗口控制区。

use egui::{Color32, Context, Response, Sense, Stroke, Ui, ViewportCommand};

#[cfg(target_os = "windows")]
const TITLEBAR_HEIGHT: f32 = 38.0;
#[cfg(not(target_os = "windows"))]
const TITLEBAR_HEIGHT: f32 = 36.0;

#[derive(Clone, Copy)]
pub struct ChromeColors {
    pub fill: Color32,
    pub border: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub accent: Color32,
    #[cfg(target_os = "windows")]
    pub hover: Color32,
}

fn enabled_value(value: &str) -> bool {
    value != "0" && !value.eq_ignore_ascii_case("false") && !value.eq_ignore_ascii_case("off")
}

/// 环境变量用于故障排查时强制覆盖，持久化设置用于日常配置。
pub fn integrated_titlebar_enabled(configured: Option<&str>) -> bool {
    std::env::var("AIOPS_NATIVE_TITLEBAR")
        .ok()
        .as_deref()
        .map(enabled_value)
        .or_else(|| configured.map(enabled_value))
        .unwrap_or(cfg!(any(target_os = "macos", target_os = "windows")))
}

pub fn titlebar_height() -> f32 {
    TITLEBAR_HEIGHT
}

fn theme_button(ui: &mut Ui, colors: ChromeColors, dark: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(60.0, 24.0), Sense::click());
    let border = if response.hovered() {
        Stroke::new(1.0, colors.border)
    } else {
        Stroke::NONE
    };
    ui.painter().rect(rect, 6.0, Color32::TRANSPARENT, border);
    let c = egui::pos2(rect.left() + 13.0, rect.center().y);
    let stroke = Stroke::new(
        1.25,
        if response.hovered() {
            colors.text
        } else {
            colors.dim
        },
    );
    if dark {
        ui.painter().circle(c, 3.5, Color32::TRANSPARENT, stroke);
        for i in 0..8 {
            let angle = i as f32 * std::f32::consts::TAU / 8.0;
            let direction = egui::vec2(angle.cos(), angle.sin());
            ui.painter()
                .line_segment([c + direction * 5.5, c + direction * 7.0], stroke);
        }
    } else {
        ui.painter().circle_filled(c, 5.5, stroke.color);
        ui.painter()
            .circle_filled(c + egui::vec2(2.5, -2.0), 5.0, colors.fill);
    }
    ui.painter().text(
        egui::pos2(rect.left() + 25.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        if dark { "浅色" } else { "深色" },
        egui::FontId::proportional(12.0),
        colors.text,
    );
    response
}

#[cfg(target_os = "windows")]
fn window_button(ui: &mut Ui, colors: ChromeColors, kind: u8, maximized: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(46.0, titlebar_height()), Sense::click());
    let fill = if response.hovered() {
        if kind == 2 {
            Color32::from_rgb(0xc4, 0x2b, 0x1c)
        } else {
            colors.hover
        }
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    let color = if response.hovered() && kind == 2 {
        Color32::WHITE
    } else {
        colors.text
    };
    let stroke = Stroke::new(1.15, color);
    let c = rect.center();
    match kind {
        0 => {
            ui.painter().line_segment(
                [c + egui::vec2(-5.0, 3.0), c + egui::vec2(5.0, 3.0)],
                stroke,
            );
        }
        1 if maximized => {
            let back =
                egui::Rect::from_center_size(c + egui::vec2(2.0, -2.0), egui::vec2(8.0, 7.0));
            let front =
                egui::Rect::from_center_size(c + egui::vec2(-1.0, 1.0), egui::vec2(8.0, 7.0));
            ui.painter().rect_stroke(back, 0.0, stroke);
            ui.painter().rect_filled(front.expand(1.0), 0.0, fill);
            ui.painter().rect_stroke(front, 0.0, stroke);
        }
        1 => {
            let square = egui::Rect::from_center_size(c, egui::vec2(9.0, 8.0));
            ui.painter().rect_stroke(square, 0.0, stroke);
        }
        _ => {
            ui.painter().line_segment(
                [c + egui::vec2(-4.5, -4.5), c + egui::vec2(4.5, 4.5)],
                stroke,
            );
            ui.painter().line_segment(
                [c + egui::vec2(-4.5, 4.5), c + egui::vec2(4.5, -4.5)],
                stroke,
            );
        }
    }
    response
}

/// 标题栏可触发的动作：主题切换 / 文件树面板开关。
#[derive(Default)]
pub struct ChromeActions {
    pub toggle_theme: bool,
    pub toggle_tree: bool,
    pub toggle_sidebar: bool,
}

/// 主导航最左侧的侧栏开关，仅绘制图标，文字通过悬停提示呈现。
fn sidebar_button(ui: &mut Ui, colors: ChromeColors, expanded: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 26.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, colors.border.gamma_multiply(0.35));
    }
    let stroke = Stroke::new(1.35, if response.hovered() { colors.text } else { colors.dim });
    let icon = egui::Rect::from_center_size(rect.center(), egui::vec2(15.0, 13.0));
    ui.painter().rect_stroke(icon, 2.0, stroke);
    ui.painter().vline(icon.left() + 4.5, icon.y_range(), stroke);
    response.on_hover_text(if expanded { "收起" } else { "展开" })
}

/// 文件树开关按钮（矢量树形图标，激活态用 accent 色）。
fn tree_button(ui: &mut Ui, colors: ChromeColors, open: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(26.0, 24.0), Sense::click());
    let border = if response.hovered() {
        Stroke::new(1.0, colors.border)
    } else {
        Stroke::NONE
    };
    ui.painter().rect(rect, 6.0, Color32::TRANSPARENT, border);
    let c = rect.center();
    let color = if open { colors.accent } else { colors.dim };
    let s = Stroke::new(1.25, color);
    // 树形：根节点 + 子节点 + 连接线。
    ui.painter().rect_stroke(
        egui::Rect::from_center_size(c + egui::vec2(-4.0, -4.0), egui::vec2(6.0, 4.5)),
        1.0,
        s,
    );
    ui.painter().rect_stroke(
        egui::Rect::from_center_size(c + egui::vec2(3.5, 3.5), egui::vec2(6.0, 4.5)),
        1.0,
        s,
    );
    ui.painter()
        .line_segment([c + egui::vec2(-4.0, -1.8), c + egui::vec2(-4.0, 3.5)], s);
    ui.painter()
        .line_segment([c + egui::vec2(-4.0, 3.5), c + egui::vec2(0.5, 3.5)], s);
    response.on_hover_text("项目文件树")
}

/// 绘制全宽标题栏，返回标题栏触发的动作。
pub fn show(
    ctx: &Context,
    colors: ChromeColors,
    dark: bool,
    status: &str,
    integrated: bool,
    workspace_left: f32,
    tree_open: bool,
    sidebar_expanded: bool,
) -> ChromeActions {
    let mut actions = ChromeActions::default();
    let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
    egui::TopBottomPanel::top("integrated_workbench_titlebar")
        .exact_height(titlebar_height())
        .frame(egui::Frame::default().fill(colors.fill))
        .show(ctx, |ui| {
            let full_rect = ui.max_rect();
            let drag = ui.interact(
                full_rect,
                ui.id().with("window_drag"),
                Sense::click_and_drag(),
            );
            if drag.double_clicked() {
                // 透明内容区不会自动获得 macOS 原生标题栏的双击行为，应用只发送
                // 一次最大化/恢复命令；标题栏自身高度始终由固定常量控制。
                ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
            } else if drag.drag_started() {
                // 双击与拖动必须互斥，否则系统拖动和应用最大化会同时改变窗口尺寸。
                ctx.send_viewport_cmd(ViewportCommand::StartDrag);
            }

            // 高度只由 TopBottomPanel::exact_height 决定。不要把重排中的可用高度
            // 回写为 min_height，否则最大化/恢复的中间帧会让标题栏先变高再缩回。
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                let system_safe_space = if cfg!(target_os = "macos") && integrated {
                    78.0
                } else {
                    14.0
                };
                ui.add_space(system_safe_space);
                if sidebar_button(ui, colors, sidebar_expanded).clicked() {
                    actions.toggle_sidebar = true;
                }
                // 标题仍与工作区起点大致对齐，侧栏开关固定在导航最左侧。
                ui.add_space((workspace_left - system_safe_space - 28.0).max(12.0));
                ui.label(
                    egui::RichText::new("对话工作台")
                        .size(14.0)
                        .strong()
                        .color(colors.text),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    #[cfg(target_os = "windows")]
                    if integrated {
                        if window_button(ui, colors, 2, maximized).clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                        }
                        if window_button(ui, colors, 1, maximized).clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                        }
                        if window_button(ui, colors, 0, maximized).clicked() {
                            ctx.send_viewport_cmd(ViewportCommand::Minimized(true));
                        }
                    }
                    ui.add_space(8.0);
                    if theme_button(ui, colors, dark).clicked() {
                        actions.toggle_theme = true;
                    }
                    ui.add_space(6.0);
                    if tree_button(ui, colors, tree_open).clicked() {
                        actions.toggle_tree = true;
                    }
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(status).size(11.0).color(colors.accent),
                        )
                        .truncate(),
                    );
                });
            });
            ui.painter().hline(
                full_rect.x_range(),
                full_rect.bottom(),
                Stroke::new(1.0, colors.border),
            );
        });
    actions
}

#[cfg(target_os = "windows")]
pub fn handle_resize(ctx: &Context, integrated: bool) {
    if !integrated || ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
        return;
    }
    let Some(position) = ctx.input(|i| {
        if i.pointer.primary_pressed() {
            i.pointer.interact_pos()
        } else {
            None
        }
    }) else {
        return;
    };
    let rect = ctx.screen_rect();
    let edge = 5.0;
    let left = position.x <= rect.left() + edge;
    let right = position.x >= rect.right() - edge;
    let top = position.y <= rect.top() + edge;
    let bottom = position.y >= rect.bottom() - edge;
    let direction = match (left, right, top, bottom) {
        (true, _, true, _) => Some(egui::ResizeDirection::NorthWest),
        (_, true, true, _) => Some(egui::ResizeDirection::NorthEast),
        (true, _, _, true) => Some(egui::ResizeDirection::SouthWest),
        (_, true, _, true) => Some(egui::ResizeDirection::SouthEast),
        (true, _, _, _) => Some(egui::ResizeDirection::West),
        (_, true, _, _) => Some(egui::ResizeDirection::East),
        (_, _, true, _) => Some(egui::ResizeDirection::North),
        (_, _, _, true) => Some(egui::ResizeDirection::South),
        _ => None,
    };
    if let Some(direction) = direction {
        ctx.send_viewport_cmd(ViewportCommand::BeginResize(direction));
    }
}

#[cfg(not(target_os = "windows"))]
pub fn handle_resize(_ctx: &Context, _integrated: bool) {}

#[cfg(test)]
mod tests {
    use super::enabled_value;

    #[test]
    fn parses_disabled_values() {
        for value in ["0", "false", "FALSE", "off", "OFF"] {
            assert!(!enabled_value(value));
        }
        for value in ["1", "true", "on"] {
            assert!(enabled_value(value));
        }
    }
}
