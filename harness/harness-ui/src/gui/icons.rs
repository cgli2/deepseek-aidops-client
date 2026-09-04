//! Vector icons and the AIOPS brand mark.

use super::theme::Palette;

/// 侧栏功能图标：矢量线条绘制（不依赖字体字形，CJK 字体缺字也不会变豆腐块）。
// Chip/Menu/Update 为预留字形，当前侧栏未使用，保留绘制实现供后续启用。
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub(super) enum Icon {
    Chat,
    Folder,
    GitBranch,
    Layers,
    Chip,
    Gear,
    Menu,
    Update,
}

pub(super) fn draw_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    icon: Icon,
    color: egui::Color32,
) {
    let r = egui::Rect::from_center_size(center, egui::vec2(16.0, 16.0));
    let stroke = egui::Stroke::new(1.5_f32, color);
    let thin = egui::Stroke::new(1.1_f32, color);
    match icon {
        // 对话气泡 + 内部文本线
        Icon::Chat => {
            let body = egui::Rect::from_min_size(
                r.min + egui::vec2(0.5, 0.5),
                egui::vec2(r.width() - 1.0, r.height() * 0.72),
            );
            painter.rect(
                body,
                egui::Rounding::same(4.0),
                egui::Color32::TRANSPARENT,
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(body.min.x + 4.0, body.max.y),
                    egui::pos2(body.min.x + 2.5, body.max.y + 3.0),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(body.min.x + 2.5, body.max.y + 3.0),
                    egui::pos2(body.min.x + 7.5, body.max.y),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    egui::pos2(body.min.x + 3.5, body.center().y - 1.5),
                    egui::pos2(body.max.x - 3.5, body.center().y - 1.5),
                ],
                thin,
            );
            painter.line_segment(
                [
                    egui::pos2(body.min.x + 3.5, body.center().y + 1.5),
                    egui::pos2(body.max.x - 5.5, body.center().y + 1.5),
                ],
                thin,
            );
        }
        // 文件夹（带左上标签页）
        Icon::Folder => {
            let tab = r.height() * 0.26;
            let pts = vec![
                egui::pos2(r.min.x, r.max.y - 1.0),
                egui::pos2(r.min.x, r.min.y + 1.0),
                egui::pos2(r.min.x + r.width() * 0.42, r.min.y + 1.0),
                egui::pos2(r.min.x + r.width() * 0.52, r.min.y + tab),
                egui::pos2(r.max.x, r.min.y + tab),
                egui::pos2(r.max.x, r.max.y - 1.0),
            ];
            painter.add(egui::Shape::closed_line(pts, stroke));
        }
        // 插件：两个错位叠放的卡片
        Icon::Layers => {
            let back = egui::Rect::from_min_size(
                r.min + egui::vec2(3.0, 0.0),
                egui::vec2(r.width() - 3.0, r.height() - 3.0),
            );
            let front = egui::Rect::from_min_size(
                r.min + egui::vec2(0.0, 3.0),
                egui::vec2(r.width() - 3.0, r.height() - 3.0),
            );
            painter.rect(
                back,
                egui::Rounding::same(2.5),
                egui::Color32::TRANSPARENT,
                thin,
            );
            painter.rect(
                front,
                egui::Rounding::same(2.5),
                egui::Color32::TRANSPARENT,
                stroke,
            );
        }
        // 模型：芯片（内外框 + 四边引脚）
        Icon::Chip => {
            let c = r.center();
            let outer = egui::Rect::from_center_size(c, egui::vec2(10.5, 10.5));
            let inner = egui::Rect::from_center_size(c, egui::vec2(4.5, 4.5));
            painter.rect(
                outer,
                egui::Rounding::same(2.0),
                egui::Color32::TRANSPARENT,
                stroke,
            );
            painter.rect(inner, egui::Rounding::same(1.0), color, egui::Stroke::NONE);
            for i in 0..3 {
                let off = -3.25 + i as f32 * 3.25;
                painter.line_segment(
                    [
                        egui::pos2(c.x + off, outer.min.y - 2.5),
                        egui::pos2(c.x + off, outer.min.y),
                    ],
                    thin,
                );
                painter.line_segment(
                    [
                        egui::pos2(c.x + off, outer.max.y),
                        egui::pos2(c.x + off, outer.max.y + 2.5),
                    ],
                    thin,
                );
                painter.line_segment(
                    [
                        egui::pos2(outer.min.x - 2.5, c.y + off),
                        egui::pos2(outer.min.x, c.y + off),
                    ],
                    thin,
                );
                painter.line_segment(
                    [
                        egui::pos2(outer.max.x, c.y + off),
                        egui::pos2(outer.max.x + 2.5, c.y + off),
                    ],
                    thin,
                );
            }
        }
        // 设置：齿轮（圆环 + 8 根辐条）
        Icon::Gear => {
            let c = r.center();
            painter.circle(c, 3.2, egui::Color32::TRANSPARENT, stroke);
            painter.circle(c, 1.1, color, egui::Stroke::NONE);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::TAU / 8.0;
                let (sx, sy) = (a.sin(), a.cos());
                painter.line_segment(
                    [
                        egui::pos2(c.x + sx * 4.8, c.y + sy * 4.8),
                        egui::pos2(c.x + sx * 6.8, c.y + sy * 6.8),
                    ],
                    stroke,
                );
            }
        }
        // 汉堡菜单（收起/展开侧栏）
        Icon::Menu => {
            for i in 0..3 {
                let y = r.min.y + 3.0 + i as f32 * 4.5;
                painter.line_segment(
                    [egui::pos2(r.min.x + 1.0, y), egui::pos2(r.max.x - 1.0, y)],
                    stroke,
                );
            }
        }
        // 更新：环形箭头（刷新语义）
        Icon::Update => {
            let c = r.center();
            let rad = 5.2;
            // 四段弧线围成近圆环
            for q in 0..4 {
                let a0 = q as f32 * std::f32::consts::FRAC_PI_2 + 0.4;
                let a1 = a0 + std::f32::consts::FRAC_PI_2 - 0.8;
                let steps = 8;
                for s in 0..steps {
                    let t0 = a0 + (a1 - a0) * s as f32 / steps as f32;
                    let t1 = a0 + (a1 - a0) * (s + 1) as f32 / steps as f32;
                    painter.line_segment(
                        [
                            egui::pos2(c.x + t0.sin() * rad, c.y - t0.cos() * rad),
                            egui::pos2(c.x + t1.sin() * rad, c.y - t1.cos() * rad),
                        ],
                        stroke,
                    );
                }
            }
            // 箭头头部（指向右上）
            let head = egui::pos2(c.x + 0.4_f32.sin() * rad, c.y - 0.4_f32.cos() * rad);
            painter.line_segment([egui::pos2(head.x - 2.4, head.y - 1.6), head], stroke);
            painter.line_segment([egui::pos2(head.x - 0.4, head.y - 2.6), head], stroke);
        }
        Icon::GitBranch => {
            // Git 分支：圆点 + 两条分叉线
            let c = r.center();
            // 圆点（分叉起点）
            painter.circle_filled(egui::pos2(c.x - 4.0, c.y + 4.0), 2.0, color);
            // 主干线（圆点向右上）
            painter.line_segment(
                [
                    egui::pos2(c.x - 2.5, c.y + 3.5),
                    egui::pos2(c.x + 3.0, c.y - 3.0),
                ],
                stroke,
            );
            // 上分支端点圆点
            painter.circle_filled(egui::pos2(c.x + 3.0, c.y - 3.0), 1.8, color);
            // 上分支延伸线
            painter.line_segment(
                [
                    egui::pos2(c.x + 3.0, c.y - 3.0),
                    egui::pos2(c.x + 5.5, c.y - 5.5),
                ],
                stroke,
            );
            // 下分支线
            painter.line_segment(
                [
                    egui::pos2(c.x - 4.0, c.y + 4.0),
                    egui::pos2(c.x - 5.5, c.y + 5.5),
                ],
                stroke,
            );
        }
    }
}

/// 历史条目「删除」小图标：垃圾桶（矢量，语义明确不会被误认成关闭）。
pub(super) fn draw_trash_icon(painter: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let s = egui::Stroke::new(1.2_f32, color);
    // 盖沿 + 提手
    painter.line_segment(
        [
            egui::pos2(c.x - 4.5, c.y - 2.8),
            egui::pos2(c.x + 4.5, c.y - 2.8),
        ],
        s,
    );
    painter.line_segment(
        [
            egui::pos2(c.x - 1.8, c.y - 4.6),
            egui::pos2(c.x + 1.8, c.y - 4.6),
        ],
        s,
    );
    // 桶身（略收底）
    painter.add(egui::Shape::closed_line(
        vec![
            egui::pos2(c.x - 3.4, c.y - 2.0),
            egui::pos2(c.x + 3.4, c.y - 2.0),
            egui::pos2(c.x + 2.7, c.y + 4.6),
            egui::pos2(c.x - 2.7, c.y + 4.6),
        ],
        s,
    ));
    // 桶身竖纹
    painter.line_segment([egui::pos2(c.x, c.y - 0.6), egui::pos2(c.x, c.y + 3.2)], s);
}

/// 历史条目「重命名」小图标：铅笔（矢量）。
pub(super) fn draw_pencil_icon(painter: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let s = egui::Stroke::new(1.2_f32, color);
    // 笔身（左下笔尖 → 右上笔尾）
    painter.line_segment(
        [
            egui::pos2(c.x - 3.8, c.y + 3.8),
            egui::pos2(c.x + 3.4, c.y - 3.4),
        ],
        s,
    );
    // 笔尾加粗端
    painter.line_segment(
        [
            egui::pos2(c.x + 2.2, c.y - 4.6),
            egui::pos2(c.x + 4.6, c.y - 2.2),
        ],
        s,
    );
    // 笔尖三角
    painter.add(egui::Shape::closed_line(
        vec![
            egui::pos2(c.x - 3.8, c.y + 3.8),
            egui::pos2(c.x - 1.6, c.y + 3.2),
            egui::pos2(c.x - 3.2, c.y + 1.6),
        ],
        s,
    ));
}

/// 附件图标：矢量回形针（嵌套 U，外圈右臂短于左臂形成"夹口"）。
/// 与 draw_pencil_icon 同一风格，不依赖字体字形。
pub(super) fn draw_paperclip_icon(painter: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    // 线宽加粗 + 几何放大：小尺寸 chip 内更醒目。
    let s = egui::Stroke::new(1.7_f32, color);
    // 外圈 U（左 → 下 → 右，右臂短）
    painter.line_segment(
        [
            egui::pos2(c.x - 4.2, c.y - 4.4),
            egui::pos2(c.x - 4.2, c.y + 4.8),
        ],
        s,
    );
    painter.line_segment(
        [
            egui::pos2(c.x - 4.2, c.y + 4.8),
            egui::pos2(c.x + 4.2, c.y + 4.8),
        ],
        s,
    );
    painter.line_segment(
        [
            egui::pos2(c.x + 4.2, c.y + 4.8),
            egui::pos2(c.x + 4.2, c.y - 1.6),
        ],
        s,
    );
    // 内圈 U（更短，开口向上）
    painter.line_segment(
        [
            egui::pos2(c.x - 1.9, c.y - 1.6),
            egui::pos2(c.x - 1.9, c.y + 2.3),
        ],
        s,
    );
    painter.line_segment(
        [
            egui::pos2(c.x - 1.9, c.y + 2.3),
            egui::pos2(c.x + 1.9, c.y + 2.3),
        ],
        s,
    );
    painter.line_segment(
        [
            egui::pos2(c.x + 1.9, c.y + 2.3),
            egui::pos2(c.x + 1.9, c.y - 0.7),
        ],
        s,
    );
}

/// 气泡头部「复制本条」小图标：两个错位叠放圆角矩形（矢量，不依赖字体字形）。
/// bg 为气泡填充色，用于前矩形遮挡后矩形线条。
pub(super) fn draw_copy_icon(
    painter: &egui::Painter,
    c: egui::Pos2,
    color: egui::Color32,
    bg: egui::Color32,
) {
    let s = egui::Stroke::new(0.9_f32, color);
    // 后矩形（右上偏移，尺寸更精致小巧）
    let back = egui::Rect::from_min_size(egui::pos2(c.x - 1.2, c.y - 3.8), egui::vec2(5.0, 5.5));
    painter.rect(
        back,
        egui::Rounding::same(1.0),
        egui::Color32::TRANSPARENT,
        s,
    );
    // 前矩形（左下偏移，bg 填充遮掉后矩形被盖住的边线）
    let front = egui::Rect::from_min_size(egui::pos2(c.x - 3.8, c.y - 1.7), egui::vec2(5.0, 5.5));
    painter.rect(front, egui::Rounding::same(1.0), bg, s);
}

/// 侧栏品牌标：几何与 `bin/assets/aidops-logo.svg` 保持一致。
/// 展开时为横向 Logo，收起时只保留脉冲结图形。
pub(super) fn draw_brand_logo(ui: &egui::Ui, rect: egui::Rect, expanded: bool, pal: &Palette) {
    let blue = egui::Color32::from_rgb(0x60, 0xa5, 0xfa);
    let mint = egui::Color32::from_rgb(0x5e, 0xea, 0xd4);
    // 展开态按“图形 + 双行字标”的整体视觉宽度居中，收起态单独居中图形。
    let logo_width = if expanded { 91.0 } else { 27.0 };
    let logo_height = 27.0;
    let origin = egui::pos2(
        rect.center().x - logo_width / 2.0,
        rect.center().y - logo_height / 2.0,
    );
    let point = |x: f32, y: f32| origin + egui::vec2(x, y);
    let stroke = 2.3_f32;
    let left = [
        point(0.0, 18.0),
        point(7.0, 4.0),
        point(14.0, 15.0),
        point(25.0, 0.0),
    ];
    let right = [
        point(1.0, 23.0),
        point(11.0, 10.0),
        point(18.0, 20.0),
        point(27.0, 11.0),
    ];
    ui.painter().add(egui::Shape::line(
        left.to_vec(),
        egui::Stroke::new(stroke, blue),
    ));
    ui.painter().add(egui::Shape::line(
        right.to_vec(),
        egui::Stroke::new(stroke, mint),
    ));
    for (pos, color) in [
        (left[0], blue),
        (left[3], blue),
        (right[0], mint),
        (right[3], mint),
    ] {
        ui.painter().circle_filled(pos, 2.1, color);
    }
    ui.painter()
        .circle_filled(right[1], 2.3, egui::Color32::from_rgb(0xf0, 0xf9, 0xff));

    if expanded {
        ui.painter().text(
            egui::pos2(origin.x + 38.0, origin.y + 10.0),
            egui::Align2::LEFT_CENTER,
            "AIOPS",
            egui::FontId::proportional(15.0),
            pal.text,
        );
        ui.painter().text(
            egui::pos2(origin.x + 39.0, origin.y + 23.0),
            egui::Align2::LEFT_CENTER,
            "DESKTOP",
            egui::FontId::proportional(8.5),
            pal.dim,
        );
    }
}
