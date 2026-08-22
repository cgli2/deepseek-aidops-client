//! Memory indexing, loading, and panel data refresh behavior.

use harness_capability::assets::FactKind;

use super::model::{MemItem, MemRefresh};
use super::AppState;

impl AppState {
    /// 经 `host.rt` 独立 runtime 查询四类资产服务，把结果填充到 `mem_items`。
    /// 查询为空时列出全部（list_*），否则按关键词匹配（match/query），保证面板默认可见。
    pub(super) fn refresh_mem(&mut self) {
        let tab = self.mem_tab.clone();
        let query = self.mem_query.clone();
        // 会话 id 去掉 `.jsonl` 后缀：`recent_turns` 内部会自行拼接扩展名，
        // 带后缀会拼出 `xxx.jsonl.jsonl` 永远读不到文件，对话记忆轮次恒空。
        let session = self.current_session.trim_end_matches(".jsonl").to_string();
        let conv = self.host.conv.clone();
        let skill = self.host.skill.clone();
        let wiki = self.host.wiki.clone();
        let code = self.host.code.clone();
        // 关键修复：GUI 线程已处于 tokio 主 runtime 内，直接 block_on 会 panic 闪退。
        // 改在独立 OS 线程里 block_on（该线程无 runtime context，不重入），结果经 mpsc 回传。
        let handle = self.host.rt.handle();
        let (tx, rx) = std::sync::mpsc::channel::<MemRefresh>();
        std::thread::spawn(move || {
            let result = handle.block_on(async move {
                let mut out: Vec<MemItem> = Vec::new();
                let mut code_symbols: Vec<harness_capability::assets::CodeSymbol> = Vec::new();
                match tab.as_str() {
                    "chat" => {
                        if let Ok(facts) = conv.list_facts().await {
                            for f in facts {
                                let kind_label = match f.kind {
                                    FactKind::Preference => "偏好",
                                    FactKind::Decision => "决策",
                                    _ => "事实",
                                };
                                out.push(MemItem {
                                    title: format!("[{}] {}", f.layer.as_str(), kind_label),
                                    meta: f.id,
                                    body: f.content,
                                });
                            }
                        }
                        if let Ok(turns) = conv.recent_turns(&session, 50).await {
                            for t in turns {
                                out.push(MemItem {
                                    title: format!("{} / {}", t.role, t.session_id),
                                    meta: t.ts,
                                    body: t.content,
                                });
                            }
                        }
                    }
                    "skill" => {
                        // 管理界面用全量（含禁用）；列表展示用匹配结果。
                        let all = skill.list_skills().await.unwrap_or_default();
                        let skills = if query.trim().is_empty() {
                            all.clone()
                        } else {
                            skill.match_skills(&query).await.unwrap_or_default()
                        };
                        for s in skills {
                            out.push(MemItem {
                                title: format!(
                                    "{} ({}){}",
                                    s.name,
                                    s.version,
                                    if s.enabled { "" } else { " [已禁用]" }
                                ),
                                meta: s.id,
                                body: format!(
                                    "触发边界: {}\n步骤: {}",
                                    s.trigger_boundary,
                                    s.steps.join("；")
                                ),
                            });
                        }
                    }
                    "wiki" => {
                        let pages = if query.trim().is_empty() {
                            wiki.list_pages().await.unwrap_or_default()
                        } else {
                            wiki.query_pages(&query).await.unwrap_or_default()
                        };
                        for p in pages {
                            let body: String = p.blocks.join("\n");
                            let body = if body.chars().count() > 400 {
                                format!("{}…", body.chars().take(400).collect::<String>())
                            } else {
                                body
                            };
                            out.push(MemItem {
                                title: p.title,
                                meta: format!("{} 个链接", p.links.len()),
                                body,
                            });
                        }
                    }
                    "code" => {
                        // 代码图谱：结构化视图需要原始符号（含文件/类型/调用关系），
                        // 不再压成 MemItem 文本平铺，直接回传原始数据由 code_graph 渲染。
                        let syms = if query.trim().is_empty() {
                            code.list_symbols().await.unwrap_or_default()
                        } else {
                            code.query_symbols(&query).await.unwrap_or_default()
                        };
                        code_symbols = syms;
                    }
                    _ => {}
                }
                MemRefresh {
                    items: out,
                    code_symbols,
                }
            });
            let _ = tx.send(result);
        });
        // 非阻塞：只存接收端，下一帧 poll_mem 轮询填充 mem_items，不卡 UI 线程。
        self.mem_refresh_rx = Some(rx);
    }

    /// 每帧轮询记忆索引/刷新的异步结果（非阻塞）。
    pub(super) fn poll_mem(&mut self) {
        // 索引结果
        if let Some(rx) = &self.mem_boot_rx {
            match rx.try_recv() {
                Ok(Ok((stats, facts))) => {
                    self.mem_boot_rx = None;
                    self.mem_index_msg = format!(
                        "已索引：{} 技能 / {} 文档 / {} 符号 / {} 事实",
                        stats.skills, stats.pages, stats.symbols, facts
                    );
                    self.mem_loaded = false; // 强制刷新
                }
                Ok(Err(e)) => {
                    self.mem_boot_rx = None;
                    self.mem_index_msg = format!("索引失败: {e}");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.mem_boot_rx = None;
                    self.mem_index_msg = "索引失败: 后台任务异常退出".into();
                }
            }
        }
        // 刷新结果
        if let Some(rx) = &self.mem_refresh_rx {
            match rx.try_recv() {
                Ok(flush) => {
                    self.mem_refresh_rx = None;
                    self.mem_items = flush.items;
                    // 代码图谱：更新原始符号，并把已失效的选中项清空。
                    self.mem_code_symbols = flush.code_symbols;
                    if let Some(sel) = &self.mem_code_sel {
                        if !self.mem_code_symbols.iter().any(|s| &s.id == sel) {
                            self.mem_code_sel = None;
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.mem_refresh_rx = None;
                }
            }
        }
    }

    /// 对当前工作区执行一次资产索引（扫描 SKILL.md / *.md / 源码 → Skill/Wiki/CodeGraph），
    /// 并把已有对话文件合并为事实（consolidate 入口）。
    /// 结果通过四类资产服务落盘（原生文件实现或 aidops 后端），并刷新面板。
    pub(super) fn bootstrap_mem(&mut self) {
        let conv = self.host.conv.clone();
        let skill = self.host.skill.clone();
        let wiki = self.host.wiki.clone();
        let code = self.host.code.clone();
        let ws = self.host.workspace_root.clone();
        let path = std::path::Path::new(&ws).to_path_buf();
        // 同 refresh_mem：在独立 OS 线程 block_on，避免 GUI 线程重入 runtime 导致闪退。
        // 非阻塞：只把接收端存起来，下一帧 poll_mem 轮询结果，不阻塞 UI 线程。
        let handle = self.host.rt.handle();
        let (tx, rx) = std::sync::mpsc::channel::<
            harness_core::error::Result<(harness_capability::index::IndexStats, usize)>,
        >();
        self.mem_boot_rx = Some(rx);
        std::thread::spawn(move || {
            let res = handle.block_on(async move {
                let stats =
                    harness_capability::index::bootstrap_assets(&skill, &wiki, &code, &path)
                        .await?;
                // 事实合并：全链路无人调用 `consolidate`（对话记忆面板恒空），
                // 在 bootstrap 时对全部已有对话文件补做一次合并（按 id 去重、幂等）。
                let mut facts = 0usize;
                let conv_dir = path.join(".harness-memory").join("conversations");
                if let Ok(entries) = std::fs::read_dir(&conv_dir) {
                    for e in entries.flatten() {
                        let p = e.path();
                        if p.extension().is_some_and(|x| x == "jsonl") {
                            if let Some(sid) = p.file_stem().and_then(|s| s.to_str()) {
                                if let Ok(f) = conv.consolidate(sid).await {
                                    facts += f.len();
                                }
                            }
                        }
                    }
                }
                Ok((stats, facts))
            });
            let _ = tx.send(res);
        });
        // 标记为已触发，避免重复索引；索引完成由 poll_mem 填充反馈。
        self.mem_bootstrapped = true;
    }

    /// 刷新技能管理列表（全量，含启用状态）。
    pub(super) fn refresh_skill_items(&mut self) {
        let skill = self.host.skill.clone();
        let handle = self.host.rt.handle();
        let (tx, rx) = std::sync::mpsc::channel::<Vec<harness_capability::assets::Skill>>();
        std::thread::spawn(move || {
            let items = handle.block_on(async move {
                skill.list_skills().await.unwrap_or_default()
            });
            let _ = tx.send(items);
        });
        if let Ok(items) = rx.recv() {
            self.skill_items = items;
        }
    }

    /// 切换技能启用状态（异步；完成后刷新列表）。
    pub(super) fn toggle_skill(&mut self, id: &str, enabled: bool) {
        let skill = self.host.skill.clone();
        let id = id.to_string();
        let handle = self.host.rt.handle();
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        std::thread::spawn(move || {
            let ok = handle.block_on(async move {
                skill.set_skill_enabled(&id, enabled).await.is_ok()
            });
            let _ = tx.send(ok);
        });
        let _ = rx.recv();
        self.refresh_skill_items();
        self.mem_loaded = false;
    }

    /// 删除技能（异步；完成后刷新列表）。
    pub(super) fn delete_skill_ui(&mut self, id: &str) {
        let skill = self.host.skill.clone();
        let id = id.to_string();
        let handle = self.host.rt.handle();
        let (tx, rx) = std::sync::mpsc::channel::<bool>();
        std::thread::spawn(move || {
            let ok = handle.block_on(async move {
                skill.delete_skill(&id).await.unwrap_or(false)
            });
            let _ = tx.send(ok);
        });
        let _ = rx.recv();
        self.refresh_skill_items();
        self.mem_loaded = false;
    }

    /// 技能库的物理存储目录（与 `NativeSkillLibrary` 落盘路径一致）。
    /// 设置界面展示该路径并提供「打开目录」，解决“导入后看不到文件”的问题。
    pub(super) fn skills_storage_dir(&self) -> std::path::PathBuf {
        std::path::Path::new(&self.host.workspace_root)
            .join(".harness-memory")
            .join("skills")
    }

    /// 在系统文件管理器中打开技能存储目录（不存在则先创建）。
    pub(super) fn open_skills_dir(&mut self) {
        let dir = self.skills_storage_dir();
        if let Err(error) = std::fs::create_dir_all(&dir) {
            self.note = format!("无法创建技能目录: {error}");
            return;
        }
        #[cfg(target_os = "windows")]
        let res = std::process::Command::new("explorer").arg(&dir).spawn();
        #[cfg(target_os = "macos")]
        let res = std::process::Command::new("open").arg(&dir).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let res = std::process::Command::new("xdg-open").arg(&dir).spawn();
        match res {
            Ok(_) => self.note = format!("已在文件管理器中打开：{}", dir.display()),
            Err(error) => self.note = format!("无法打开技能目录: {error}"),
        }
    }

    /// 导入用户选择的 `SKILL.md`（或普通 Markdown 技能文档）。
    ///
    /// 文件会被安置进约定目录 `<存储目录>/<文件名>/SKILL.md`，获得与其它技能包
    /// 一致的生命周期（启动自动加载、重复导入为更新、删除即回收）。
    /// 新注册的技能默认未启用，需勾选后才参与匹配。
    pub(super) fn import_skill_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("导入 SKILL.md")
            .add_filter("Markdown 技能", &["md", "markdown"])
            .pick_file()
        else {
            return;
        };
        let packs_root = self.skills_storage_dir();
        match harness_capability::index::install_skill_file_into(&path, &packs_root) {
            Ok(_) => self.finish_pack_import(),
            Err(error) => self.note = format!("无法安置技能文件: {error}"),
        }
    }

    /// 批量导入技能包：递归扫描所选文件夹（含子目录）下所有含 `SKILL.md`
    /// 的目录，每个目录作为一个独立技能单元（含资源文件）**复制进约定目录**，
    /// 之后由 `sync_skill_packs` 对账注册（与启动自动加载同一入口）。
    /// 幂等：同名包重复导入 = 整体替换更新，不产生副本。
    pub(super) fn import_skill_folder(&mut self) {
        let Some(dir) = rfd::FileDialog::new()
            .set_title("选择技能包文件夹（递归扫描 SKILL.md）")
            .pick_folder()
        else {
            return;
        };
        let packs_root = self.skills_storage_dir();
        match harness_capability::index::install_skill_packs_from(&dir, &packs_root) {
            Ok(installed) if installed.is_empty() => {
                self.note = "该文件夹下未找到任何含 SKILL.md 的技能包".into();
            }
            Ok(_) => self.finish_pack_import(),
            Err(error) => self.note = format!("导入技能包失败: {error}"),
        }
    }

    /// 文件安置完成后，对约定目录跑一次注册对账（与启动自动扫描同一入口），
    /// 并提示用户：新技能默认未启用，需勾选后才参与匹配。
    fn finish_pack_import(&mut self) {
        match self.sync_local_skill_packs() {
            Ok(rep) if rep.added + rep.updated > 0 => {
                let names = if rep.names.len() > 4 {
                    format!("{} 等 {} 个", rep.names[..4].join("、"), rep.names.len())
                } else {
                    rep.names.join("、")
                };
                self.note = format!(
                    "已导入技能包：新增 {} 个、更新 {} 个（{}）——新增默认未启用，勾选启用后才参与匹配",
                    rep.added, rep.updated, names
                );
            }
            Ok(_) => self.note = "技能包已安置，但未解析出有效的 SKILL.md".into(),
            Err(error) => self.note = format!("注册技能包失败: {error}"),
        }
        self.refresh_skill_items();
        self.mem_loaded = false;
    }

    /// 对约定存储目录执行一次「自动加载对账」（与 Agent 启动时的自动扫描
    /// 同一入口），同步等待结果。
    fn sync_local_skill_packs(
        &mut self,
    ) -> harness_core::error::Result<harness_capability::index::SkillImportReport> {
        let skill_lib = self.host.skill.clone();
        let packs_dir = self.skills_storage_dir();
        let handle = self.host.rt.handle();
        let (tx, rx) = std::sync::mpsc::channel::<
            harness_core::error::Result<harness_capability::index::SkillImportReport>,
        >();
        std::thread::spawn(move || {
            let res = handle.block_on(async move {
                harness_capability::index::sync_skill_packs(&*skill_lib, &packs_dir).await
            });
            let _ = tx.send(res);
        });
        rx.recv()
            .unwrap_or_else(|_| Err(harness_core::error::Error::Runtime("后台任务异常退出".into())))
    }
}
