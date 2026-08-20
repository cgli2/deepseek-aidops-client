//! Memory indexing, loading, and panel data refresh behavior.

use harness_capability::assets::FactKind;

use super::model::MemItem;
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
        let (tx, rx) = std::sync::mpsc::channel::<Vec<MemItem>>();
        std::thread::spawn(move || {
            let items = handle.block_on(async move {
                let mut out: Vec<MemItem> = Vec::new();
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
                        let skills = if query.trim().is_empty() {
                            skill.list_skills().await.unwrap_or_default()
                        } else {
                            skill.match_skills(&query).await.unwrap_or_default()
                        };
                        for s in skills {
                            out.push(MemItem {
                                title: format!("{} ({})", s.name, s.version),
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
                        let syms = if query.trim().is_empty() {
                            code.list_symbols().await.unwrap_or_default()
                        } else {
                            code.query_symbols(&query).await.unwrap_or_default()
                        };
                        for x in syms {
                            out.push(MemItem {
                                title: format!("{} @ {}", x.name, x.file),
                                meta: x.kind,
                                body: format!("{} ｜ 调用: {}", x.summary, x.calls.join(", ")),
                            });
                        }
                    }
                    _ => {}
                }
                out
            });
            let _ = tx.send(items);
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
                Ok(items) => {
                    self.mem_refresh_rx = None;
                    self.mem_items = items;
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
}
