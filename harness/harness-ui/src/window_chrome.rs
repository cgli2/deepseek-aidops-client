//! 跨平台窗口顶部工作台。macOS 融入原生标题栏；Windows 使用自绘窗口控制区。

use egui::{Color32, Context, Response, Sense, Stroke, Ui, ViewportCommand};

use crate::gui::icons::{Icon, draw_icon_sized};

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
    pub card: Color32,
    pub success: Color32,
    pub warn: Color32,
    #[cfg(target_os = "windows")]
    pub hover: Color32,
    pub is_dark: bool,
}

impl ChromeColors {
    /// 工业级半透明悬停叠加色（微阻尼柔光，根据 hover_t 渐进呈现）
    pub fn translucent_hover(&self, hover_t: f32) -> Color32 {
        if hover_t <= 0.001 {
            return Color32::TRANSPARENT;
        }
        if self.is_dark {
            Color32::from_white_alpha((24.0 * hover_t).min(255.0) as u8)
        } else {
            Color32::from_black_alpha((18.0 * hover_t).min(255.0) as u8)
        }
    }
}

/// 顶部工作台上下文信息（对标 Codex 面包屑与状态指示）
pub struct WorkbenchContext<'a> {
    pub project_name: &'a str,
    pub session_title: &'a str,
    pub model_name: &'a str,
    pub status: &'a str,
    pub busy: bool,
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
    let (hover_t, active_t) = crate::gui::widgets::animate_interaction(ui, response.id, &response);
    let draw_rect = rect.shrink(0.5 * active_t);
    let fill = colors.translucent_hover(hover_t);
    let border = if hover_t > 0.01 {
        Stroke::new(
            1.0_f32,
            crate::gui::widgets::lerp_color(Color32::TRANSPARENT, colors.border, hover_t),
        )
    } else {
        Stroke::NONE
    };
    ui.painter().rect(draw_rect, 6.0, fill, border);

    let c = egui::pos2(draw_rect.left() + 13.0, draw_rect.center().y);
    let icon_color = crate::gui::widgets::lerp_color(colors.dim, colors.text, hover_t);
    draw_icon_sized(
        ui.painter(),
        c,
        if dark { Icon::Sun } else { Icon::Moon },
        icon_color,
        15.0,
    );
    ui.painter().text(
        egui::pos2(draw_rect.left() + 25.0, draw_rect.center().y),
        egui::Align2::LEFT_CENTER,
        if dark { "浅色" } else { "深色" },
        egui::FontId::proportional(12.0),
        crate::gui::widgets::lerp_color(colors.dim, colors.text, hover_t * 0.7 + 0.3),
    );
    response.on_hover_text(if dark {
        "切换至浅色主题"
    } else {
        "切换至深色主题"
    })
}

#[cfg(target_os = "windows")]
fn window_button(ui: &mut Ui, colors: ChromeColors, kind: u8, maximized: bool) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(46.0, titlebar_height()), Sense::click());
    let (hover_t, active_t) = crate::gui::widgets::animate_interaction(ui, response.id, &response);
    let fill = if kind == 2 {
        crate::gui::widgets::lerp_color(
            Color32::TRANSPARENT,
            Color32::from_rgb(0xc4, 0x2b, 0x1c),
            hover_t,
        )
    } else {
        colors.translucent_hover(hover_t)
    };
    ui.painter().rect_filled(rect, 0.0, fill);
    let color = if kind == 2 {
        crate::gui::widgets::lerp_color(colors.text, Color32::WHITE, hover_t)
    } else {
        crate::gui::widgets::lerp_color(colors.dim, colors.text, hover_t)
    };
    let c = rect.center() + egui::vec2(0.0, 0.5 * active_t);
    let icon = match kind {
        0 => Icon::WindowMinimize,
        1 if maximized => Icon::WindowRestore,
        1 => Icon::WindowMaximize,
        _ => Icon::X,
    };
    draw_icon_sized(ui.painter(), c, icon, color, 13.0);
    response
}

/// 标题栏可触发的动作：主题切换 / 文件树面板开关 / 协同检查器开关。
#[derive(Default)]
pub struct ChromeActions {
    pub toggle_theme: bool,
    pub toggle_tree: bool,
    pub toggle_sidebar: bool,
    pub toggle_inspector: bool,
}

/// 主导航最左侧的侧栏开关，仅绘制图标，文字通过悬停提示呈现。
fn sidebar_button(ui: &mut Ui, colors: ChromeColors, expanded: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 26.0), Sense::click());
    let (hover_t, active_t) = crate::gui::widgets::animate_interaction(ui, response.id, &response);
    let draw_rect = rect.shrink(0.5 * active_t);
    let fill = colors.translucent_hover(hover_t);
    let border = if hover_t > 0.01 {
        Stroke::new(
            1.0_f32,
            crate::gui::widgets::lerp_color(Color32::TRANSPARENT, colors.border, hover_t),
        )
    } else {
        Stroke::NONE
    };
    ui.painter().rect(draw_rect, 6.0, fill, border);

    let base_color = if expanded { colors.accent } else { colors.dim };
    let target_color = if expanded { colors.accent } else { colors.text };
    let icon_color = crate::gui::widgets::lerp_color(base_color, target_color, hover_t);
    draw_icon_sized(
        ui.painter(),
        draw_rect.center(),
        Icon::Sidebar,
        icon_color,
        15.0,
    );
    response.on_hover_text(if expanded {
        "收起侧栏"
    } else {
        "展开侧栏"
    })
}

/// 文件树开关按钮（矢量树形图标，激活态用 accent 色）。
fn tree_button(ui: &mut Ui, colors: ChromeColors, open: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 26.0), Sense::click());
    let (hover_t, active_t) = crate::gui::widgets::animate_interaction(ui, response.id, &response);
    let draw_rect = rect.shrink(0.5 * active_t);

    let fill = if open {
        crate::gui::widgets::lerp_color(
            colors.translucent_hover(0.7),
            colors.translucent_hover(1.0),
            hover_t,
        )
    } else {
        colors.translucent_hover(hover_t)
    };
    let border = if open || hover_t > 0.01 {
        let alpha_t = if open {
            (0.6 + 0.4 * hover_t).min(1.0)
        } else {
            hover_t
        };
        Stroke::new(
            1.0_f32,
            crate::gui::widgets::lerp_color(Color32::TRANSPARENT, colors.border, alpha_t),
        )
    } else {
        Stroke::NONE
    };
    ui.painter().rect(draw_rect, 6.0, fill, border);

    let base_color = if open { colors.accent } else { colors.dim };
    let target_color = if open { colors.accent } else { colors.text };
    let icon_color = crate::gui::widgets::lerp_color(base_color, target_color, hover_t);

    crate::gui::icons::draw_icon(
        ui.painter(),
        draw_rect.center(),
        crate::gui::icons::Icon::ListTree,
        icon_color,
    );
    response.on_hover_text(if open {
        "收起项目文件树"
    } else {
        "打开项目文件树"
    })
}

/// 协同检查器开关按钮（矢量图标，左右分栏样式）。
fn inspector_button(ui: &mut Ui, colors: ChromeColors, open: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 26.0), Sense::click());
    let (hover_t, active_t) = crate::gui::widgets::animate_interaction(ui, response.id, &response);
    let draw_rect = rect.shrink(0.5 * active_t);

    let fill = if open {
        crate::gui::widgets::lerp_color(
            colors.translucent_hover(0.7),
            colors.translucent_hover(1.0),
            hover_t,
        )
    } else {
        colors.translucent_hover(hover_t)
    };
    let border = if open || hover_t > 0.01 {
        let alpha_t = if open {
            (0.6 + 0.4 * hover_t).min(1.0)
        } else {
            hover_t
        };
        Stroke::new(
            1.0_f32,
            crate::gui::widgets::lerp_color(Color32::TRANSPARENT, colors.border, alpha_t),
        )
    } else {
        Stroke::NONE
    };
    ui.painter().rect(draw_rect, 6.0, fill, border);

    let base_color = if open { colors.accent } else { colors.dim };
    let target_color = if open { colors.accent } else { colors.text };
    let icon_color = crate::gui::widgets::lerp_color(base_color, target_color, hover_t);
    draw_icon_sized(
        ui.painter(),
        draw_rect.center(),
        Icon::Inspector,
        icon_color,
        15.0,
    );
    response.on_hover_text(if open {
        "收起协同检查器"
    } else {
        "打开协同检查器 (预览/Diff/遥测)"
    })
}

/// 绘制全宽标题栏，返回标题栏触发的动作。
pub fn show(
    ctx: &Context,
    colors: ChromeColors,
    dark: bool,
    wb: &WorkbenchContext,
    integrated: bool,
    _workspace_left: f32,
    tree_open: bool,
    preview_open: bool,
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

            // 高度只由 TopBottomPanel::exact_height 决定。
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

                // ── Codex 式面包屑与当前上下文 ──
                ui.add_space(8.0);
                // 项目名（矢量文件夹图标 + 项目名）
                let proj_name = if wb.project_name.is_empty() {
                    "默认工作区"
                } else {
                    wb.project_name
                };
                let (f_rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), Sense::hover());
                crate::gui::icons::draw_icon(
                    ui.painter(),
                    f_rect.center(),
                    crate::gui::icons::Icon::Folder,
                    colors.dim,
                );
                ui.add_space(2.0);
                ui.label(egui::RichText::new(proj_name).size(12.0).color(colors.dim));
                ui.add_space(3.0);
                ui.label(egui::RichText::new("/").size(11.0).color(colors.border));
                ui.add_space(3.0);
                // 会话名（截断）
                let session_display: String = if wb.session_title.is_empty() {
                    "新会话".to_string()
                } else {
                    let mut s: String = wb.session_title.chars().take(20).collect();
                    if wb.session_title.chars().count() > 20 {
                        s.push('…');
                    }
                    s
                };
                ui.label(
                    egui::RichText::new(session_display)
                        .size(12.5)
                        .strong()
                        .color(colors.text),
                );

                // ── 右侧控制栏与状态指示 ──
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
                    if inspector_button(ui, colors, preview_open).clicked() {
                        actions.toggle_inspector = true;
                    }
                    ui.add_space(6.0);

                    // 模型胶囊（矢量机器人图标 + 模型名）
                    if !wb.model_name.is_empty() {
                        let pill_w = wb.model_name.len() as f32 * 6.5 + 30.0;
                        let (m_rect, _) =
                            ui.allocate_exact_size(egui::vec2(pill_w, 22.0), egui::Sense::hover());
                        ui.painter().rect(
                            m_rect,
                            egui::Rounding::same(11.0),
                            colors.card,
                            egui::Stroke::new(1.0_f32, colors.border),
                        );
                        let bot_c = egui::pos2(m_rect.left() + 11.0, m_rect.center().y);
                        crate::gui::icons::draw_icon(
                            ui.painter(),
                            bot_c,
                            crate::gui::icons::Icon::Bot,
                            colors.accent,
                        );
                        ui.painter().text(
                            egui::pos2(m_rect.left() + 22.0, m_rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            wb.model_name,
                            egui::FontId::proportional(11.0),
                            colors.dim,
                        );
                        ui.add_space(6.0);
                    }

                    // Agent 运行状态胶囊
                    if wb.busy {
                        let time = ui.input(|i| i.time);
                        ui.ctx().request_repaint(); // 60FPS 顺滑旋转
                        let status_label = "Agent 执行中";
                        let pill_w = status_label.len() as f32 * 6.8 + 26.0;
                        let (pill_rect, _) =
                            ui.allocate_exact_size(egui::vec2(pill_w, 22.0), egui::Sense::hover());
                        ui.painter().rect(
                            pill_rect,
                            egui::Rounding::same(11.0),
                            colors.card,
                            egui::Stroke::new(1.0_f32, colors.border),
                        );
                        let sp_c = egui::pos2(pill_rect.left() + 12.0, pill_rect.center().y);
                        crate::gui::icons::draw_smooth_spinner(
                            ui.painter(),
                            sp_c,
                            5.0,
                            colors.accent,
                            time,
                        );
                        ui.painter().text(
                            egui::pos2(pill_rect.left() + 21.0, pill_rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            status_label,
                            egui::FontId::proportional(11.0),
                            colors.accent,
                        );
                    } else {
                        let is_err = wb.status.contains("错误") || wb.status.contains("失败");
                        let (dot_color, status_label) = if is_err {
                            (
                                colors.warn,
                                if wb.status.is_empty() {
                                    "异常"
                                } else {
                                    wb.status
                                },
                            )
                        } else {
                            (colors.success, "就绪")
                        };
                        let pill_w = status_label.len() as f32 * 6.8 + 24.0;
                        let (pill_rect, _) =
                            ui.allocate_exact_size(egui::vec2(pill_w, 22.0), egui::Sense::hover());
                        ui.painter().rect(
                            pill_rect,
                            egui::Rounding::same(11.0),
                            colors.card,
                            egui::Stroke::new(1.0_f32, colors.border),
                        );
                        let dot_c = egui::pos2(pill_rect.left() + 11.0, pill_rect.center().y);
                        draw_icon_sized(ui.painter(), dot_c, Icon::CircleDot, dot_color, 10.0);
                        ui.painter().text(
                            egui::pos2(pill_rect.left() + 19.0, pill_rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            status_label,
                            egui::FontId::proportional(11.0),
                            colors.text,
                        );
                    }
                });
            });
            ui.painter().hline(
                full_rect.x_range(),
                full_rect.bottom(),
                Stroke::new(1.0_f32, colors.border),
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
