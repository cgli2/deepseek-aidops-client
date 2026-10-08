//! Read-only monitoring view. Refresh runs outside the GUI thread.
use std::sync::{Arc, Mutex};
use super::fonts::{FONT_CAPTION, FONT_TITLE, FONT_UI};
use super::icons::{Icon, draw_icon};
use super::theme::Palette;
use super::widgets::compact_button;

pub(super) fn show(ui: &mut egui::Ui, project: &str, pal: &Palette) {
    let card_frame = egui::Frame::default()
        .fill(pal.card_bg)
        .rounding(egui::Rounding::same(8.0))
        .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
        .inner_margin(egui::Margin::same(12.0));

    card_frame.show(ui, |ui| {
        let key = egui::Id::new(("self-monitor-status", project));
        let data = ui.data_mut(|d| {
            if let Some(value) = d.get_temp::<Arc<Mutex<String>>>(key) {
                value
            } else {
                let value = Arc::new(Mutex::new("点击刷新查看最近任务的监控状态。".to_string()));
                d.insert_temp(key, value.clone());
                value
            }
        });

        ui.horizontal(|ui| {
            let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            draw_icon(ui.painter(), icon_rect.center(), Icon::Activity, pal.accent);
            ui.label(
                egui::RichText::new("运行健康与事故监控")
                    .size(FONT_TITLE)
                    .strong()
                    .color(pal.text),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if compact_button(ui, pal, "刷新状态") {
                    let data = data.clone();
                    let context = ui.ctx().clone();
                    let root = std::path::Path::new(project).join(".harness/self-monitor/spool");
                    std::thread::spawn(move || {
                        let mut entries: Vec<_> = std::fs::read_dir(&root)
                            .into_iter()
                            .flatten()
                            .filter_map(Result::ok)
                            .take(1000)
                            .map(|e| e.path())
                            .filter(|p| p.is_dir())
                            .collect();
                        entries.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
                        let rows = entries
                            .iter()
                            .rev()
                            .take(10)
                            .map(|p| {
                                let id = p.file_name().unwrap_or_default().to_string_lossy();
                                let health = std::fs::read_to_string(p.join("observer/health.json"))
                                    .ok()
                                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
                                let state = health
                                    .as_ref()
                                    .and_then(|v| v["state"].as_str())
                                    .unwrap_or("尚未完成观察");
                                let label = match state {
                                    "healthy" => "观察完成",
                                    "ObserverDegraded" => "观察器不可用",
                                    _ => state,
                                };
                                format!("{} · {}", id, label)
                            })
                            .collect::<Vec<_>>();
                        if let Ok(mut text) = data.lock() {
                            *text = if rows.is_empty() {
                                "暂无监控记录。".into()
                            } else {
                                rows.join("\n")
                            };
                        }
                        context.request_repaint();
                    });
                }
            });
        });

        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("自动发布关闭；监控故障不会阻断任务。")
                .size(FONT_CAPTION)
                .color(pal.dim),
        );
        ui.add_space(6.0);

        if let Ok(text) = data.try_lock() {
            egui::Frame::default()
                .fill(pal.field)
                .rounding(egui::Rounding::same(6.0))
                .stroke(egui::Stroke::new(1.0_f32, pal.card_border))
                .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(text.as_str())
                            .size(FONT_UI)
                            .color(pal.text),
                    );
                });
        }
    });
}
