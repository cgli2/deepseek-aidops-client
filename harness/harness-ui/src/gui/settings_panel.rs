//! Settings, update, profile, and plugin management behavior.

use std::sync::Arc;

use harness_core::update::UpdateStatus;

use super::AppState;
use super::model::{BUILTIN_PLUGINS, PluginKind, PluginUiRow};
use super::theme::Palette;
use super::widgets::{accent_button, field_label, ghost_button};

impl AppState {
    // ── 版本更新：顶部横幅 ─────────────────────────────────────
    /// 中央面板顶部横幅：展示检查中 / 新版本提示 / 下载进度 / 待重启 / 错误。
    pub(super) fn draw_update_banner(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let status = self
            .update_status
            .lock()
            .map(|g| g.clone())
            .unwrap_or(UpdateStatus::Idle);
        // 读取升级策略：自动安装开启时，「立即升级」走下载+重启；否则打开下载页。
        let auto_install = cfg!(windows)
            && self
                .host
                .settings
                .get("update.auto_install")
                .map(|v| v == "true")
                .unwrap_or(false);
        match status {
            UpdateStatus::Idle | UpdateStatus::UpToDate => {}
            UpdateStatus::Checking => {
                ui.label(
                    egui::RichText::new("正在检查更新…")
                        .size(12.0)
                        .color(pal.dim),
                );
                ui.add_space(8.0);
            }
            UpdateStatus::Error(e) => {
                ui.label(
                    egui::RichText::new(format!("更新检查失败：{e}"))
                        .size(12.0)
                        .color(pal.warn),
                );
                ui.add_space(8.0);
            }
            UpdateStatus::Downloading => {
                ui.label(
                    egui::RichText::new("正在下载新版本…")
                        .size(12.0)
                        .color(pal.accent),
                );
                ui.add_space(8.0);
            }
            UpdateStatus::ReadyToRestart { version, .. } => {
                egui::Frame::default()
                    .fill(pal.banner_ok)
                    .rounding(egui::Rounding::same(12.0))
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("已下载 v{version}，重启后生效"))
                                    .size(12.5)
                                    .strong()
                                    .color(pal.text),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if accent_button(ui, &pal, "重启") {
                                        self.restart_to_apply_update();
                                    }
                                },
                            );
                        });
                    });
                ui.add_space(8.0);
            }
            UpdateStatus::Available(rel) => {
                let mandatory = rel.mandatory.unwrap_or(false);
                egui::Frame::default()
                    .fill(pal.banner_warn)
                    .rounding(egui::Rounding::same(12.0))
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "发现新版本 v{}（当前 v{}）",
                                        rel.version,
                                        harness_core::update::CURRENT_VERSION
                                    ))
                                    .size(13.0)
                                    .strong()
                                    .color(pal.text),
                                );
                                if mandatory {
                                    ui.label(
                                        egui::RichText::new("· 必须更新")
                                            .size(11.0)
                                            .color(pal.warn),
                                    );
                                }
                            });
                            if let Some(notes) = &rel.notes {
                                ui.label(egui::RichText::new(notes).size(11.5).color(pal.dim));
                            }
                            ui.horizontal(|ui| {
                                if auto_install {
                                    if accent_button(ui, &pal, "立即升级") {
                                        harness_core::update::spawn_download(
                                            self.update_status.clone(),
                                            rel.clone(),
                                        );
                                    }
                                } else if accent_button(ui, &pal, "立即升级") {
                                    harness_core::update::open_url(&rel.url);
                                }
                                if ghost_button(ui, &pal, "查看下载页") {
                                    harness_core::update::open_url(&rel.url);
                                }
                                if !mandatory {
                                    if ghost_button(ui, &pal, "稍后") {
                                        if let Ok(mut g) = self.update_status.lock() {
                                            *g = UpdateStatus::UpToDate;
                                        }
                                    }
                                    if ghost_button(ui, &pal, "忽略此版本") {
                                        let _ = self
                                            .host
                                            .settings
                                            .set("update.skipped_version", &rel.version);
                                        if let Ok(mut g) = self.update_status.lock() {
                                            *g = UpdateStatus::UpToDate;
                                        }
                                    }
                                }
                            });
                        });
                    });
                ui.add_space(8.0);
            }
        }
    }

    /// 「更新」设置页：清单 URL / 通道 / 自动开关 / 立即检查 / 当前版本。
    pub(super) fn draw_update_settings(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        field_label(
            ui,
            &pal,
            &format!("当前版本：v{}", harness_core::update::CURRENT_VERSION),
        );
        ui.add_space(6.0);

        field_label(ui, &pal, "清单 URL（manifest.json）");
        let _ = ui.text_edit_singleline(&mut self.f_update_url);
        ui.label(
            egui::RichText::new("远端返回的 JSON 含 version / url / 可选 notes·sha256·mandatory。可放任意静态托管（COS / 对象存储 / nginx / 内网文件服务）。支持简写 github:owner/repo（自动解析为 raw.githubusercontent.com 直链，无需 GitHub API 令牌）。")
                .size(11.0)
                .color(pal.dim),
        );
        ui.add_space(8.0);

        field_label(ui, &pal, "更新通道");
        egui::ComboBox::from_id_salt("update-channel")
            .width(200.0)
            .selected_text(&self.f_update_channel)
            .show_ui(ui, |ui| {
                for ch in ["stable", "beta"] {
                    ui.selectable_value(&mut self.f_update_channel, ch.to_string(), ch);
                }
            });
        ui.add_space(8.0);

        let _ = ui.checkbox(&mut self.f_auto_check, "自动检查更新（启动后节流 24 小时）");
        ui.add_enabled_ui(cfg!(windows), |ui| {
            let _ = ui.checkbox(
                &mut self.f_auto_install,
                "自动下载并安装（当前仅 Windows 支持）",
            );
        });
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            if accent_button(ui, &pal, "保存更新设置") {
                let s = &self.host.settings;
                let _ = s.set("update.manifest_url", &self.f_update_url.trim());
                let _ = s.set("update.channel", &self.f_update_channel);
                let _ = s.set("update.auto_check", &self.f_auto_check.to_string());
                let _ = s.set("update.auto_install", &self.f_auto_install.to_string());
                self.note = "更新设置已保存".into();
            }
            if ghost_button(ui, &pal, "立即检查") {
                let skip = self
                    .host
                    .settings
                    .get("update.skipped_version")
                    .unwrap_or_default();
                harness_core::update::spawn_check(
                    self.update_status.clone(),
                    &self.f_update_url.trim(),
                    &self.f_update_channel,
                    &skip,
                    true,
                );
            }
        });
        ui.add_space(10.0);

        // 设置页内也展示当前更新状态与操作。
        self.draw_update_banner(ui, pal);
    }

    /// 触发待升级替换并重启（下载完成后点「重启」调用）。
    pub(super) fn restart_to_apply_update(&self) {
        if let Some(exe) = std::env::current_exe().ok() {
            if let Some(dir) = exe.parent() {
                harness_core::update::try_apply_and_relaunch(dir);
            }
        }
    }

    pub(super) fn load_profile(&mut self, name: &str) {
        let Some(profile) = self
            .host
            .settings
            .model_profiles()
            .into_iter()
            .find(|p| p.name == name)
        else {
            return;
        };
        self.f_provider = profile.provider.clone();
        self.f_base = profile.base_url.clone();
        self.f_model = profile.model.clone();
        match self.host.llm_control.configure_provider(
            profile.provider,
            profile.base_url,
            profile.model,
            profile.api_key,
            self.effort(),
        ) {
            Ok(()) => self.note = self.host.llm_control.status(),
            Err(error) => self.note = format!("配置错误: {error}"),
        }
    }

    pub(super) fn save_preferences(&mut self) {
        let settings = &self.host.settings;
        let _ = settings.set("permission.mode", &self.permission);
        for row in &self.plugin_rows {
            if row.kind == PluginKind::Wasm {
                let _ = settings.set_plugin_enabled(&row.id, &row.name, row.enabled);
            }
        }
        // Trellis 插件：启停与文件路径在勾选/编辑时已热生效（共享控制句柄），
        // 这里无损写回 .harness.toml（未知字段保留 + 原子写），保证重启后保持。
        if let Some(t) = self
            .plugin_rows
            .iter()
            .find(|r| r.kind == PluginKind::Trellis)
        {
            if let Ok((mut cfg, raw, Some(path))) = harness_core::Config::load_with_raw() {
                cfg.trellis.enabled = t.enabled;
                cfg.trellis.spec_file = t.spec_file.clone();
                cfg.trellis.tasks_file = t.tasks_file.clone();
                let _ = cfg.save_preserving(&path, &raw);
            }
        }
        // 运行时调参：持久化到 settings.db，并写入进程级开关（立即生效，无需重启）。
        // 空输入解析失败 → None → 回退环境变量 / 默认值。
        let _ = settings.set("runtime.context_budget", self.f_context_budget.trim());
        let _ = settings.set("runtime.max_steps", self.f_max_steps.trim());
        let _ = settings.set("runtime.max_tokens", self.f_max_tokens.trim());
        harness_core::tuning::set_context_budget_chars(self.f_context_budget.trim().parse().ok());
        harness_core::tuning::set_max_steps(self.f_max_steps.trim().parse().ok());
        harness_core::tuning::set_max_output_tokens(self.f_max_tokens.trim().parse().ok());
        self.host.sink.set_permission(self.permission.clone());
        self.note = "偏好与运行时参数已保存并生效".into();
        // 不自动关闭：note 在弹窗内可见，给用户明确的保存反馈。
    }

    /// 构建插件列表：核心内置恒启用（忽略历史禁用记录）；WASM 插件读持久化状态；
    /// Trellis（spec 驱动开发插件）读共享控制句柄——与 bin 装配注入 UI 的是同一实例，
    /// 勾选启停即时生效（无需重启）。
    pub(super) fn load_plugin_rows(
        settings: &crate::SettingsDb,
        runtime: &harness_provider_wasm::WasmPluginRuntime,
        trellis: &Arc<harness_provider_trellis::TrellisControl>,
    ) -> Vec<PluginUiRow> {
        let mut rows: Vec<PluginUiRow> = BUILTIN_PLUGINS
            .iter()
            .map(|(id, name, desc)| PluginUiRow {
                id: (*id).into(),
                name: (*name).into(),
                desc: (*desc).into(),
                kind: PluginKind::Core,
                enabled: true,
                active: true,
                spec_file: String::new(),
                tasks_file: String::new(),
            })
            .collect();
        for p in settings.plugins() {
            if let Some(path) = p.path {
                let active = runtime.is_active(&p.id);
                rows.push(PluginUiRow {
                    id: p.id,
                    name: p.name,
                    desc: path,
                    kind: PluginKind::Wasm,
                    enabled: p.enabled,
                    active,
                    spec_file: String::new(),
                    tasks_file: String::new(),
                });
            }
        }
        rows.push(PluginUiRow {
            id: "trellis".into(),
            name: "Trellis".into(),
            desc: "spec 驱动开发：PreStep 注入项目规格并维护任务状态机".into(),
            kind: PluginKind::Trellis,
            enabled: trellis.enabled(),
            active: trellis.enabled(),
            spec_file: trellis.spec_file(),
            tasks_file: trellis.tasks_file(),
        });
        rows
    }

    /// 导入 WASM 插件入口：先经 `harness-provider-wasm` 的 wasmtime 沙箱校验再登记。
    pub(super) fn import_wasm_plugin(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("选择 WASM 插件")
            .add_filter("WASM 插件", &["wasm", "wat"])
            .pick_file()
        else {
            return;
        };
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("plugin")
            .to_string();
        let id = format!("wasm:{stem}");
        let path_s = path.display().to_string();
        // 真正启用：在零直接能力的 Wasmtime 容器中实例化并调用可选 on_load。
        if let Err(e) = self.host.wasm_plugins.activate(&id, &path) {
            self.note = format!("插件校验或启用失败: {e}");
            return;
        }
        if let Err(e) = self.host.settings.add_wasm_plugin(&id, &stem, &path_s) {
            let _ = self.host.wasm_plugins.deactivate(&id);
            self.note = format!("登记插件失败: {e}");
            return;
        }
        if let Some(row) = self.plugin_rows.iter_mut().find(|row| row.id == id) {
            row.name = stem.clone();
            row.desc = path_s;
            row.enabled = true;
            row.active = true;
        } else {
            self.plugin_rows.push(PluginUiRow {
                id,
                name: stem.clone(),
                desc: path_s,
                kind: PluginKind::Wasm,
                enabled: true,
                active: true,
                spec_file: String::new(),
                tasks_file: String::new(),
            });
        }
        self.note = format!("插件「{stem}」已通过沙箱校验并登记，默认启用");
    }
}
