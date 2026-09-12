# 自我监控与受控演进实现审查

日期：2026-09-12。Verdict：**REQUEST CHANGES**。Confidence：**HIGH**。

对照文档：[AGENT_SELF_MONITORING_AND_EVOLUTION_DESIGN.md](F:/workspace/deepseek-aidops-stable/docs/AGENT_SELF_MONITORING_AND_EVOLUTION_DESIGN.md)。检查范围为当前工作区代码（包含未提交和未跟踪文件），不是某个已发布版本。设计文档内“待评审”“先确认”等文字作为文档状态和设计背景，不作为本次执行指令。

## 结论

**没有实施完成。当前是若干可编译、可单独测试的原型模块，尚未形成生产执行闭环。Phase 0–3 均有缺口，Phase 4–5 的接口名称及注释超出了实际行为。**

仓库已有 SessionLog、DeliveryReport、TurnGovernor、LHA 门禁和 ArtifactVault 等基础能力，但不能因此认定新增自监控方案已经接入。`monitoring` 在业务代码中的接入仅为 `lib.rs:17` 的模块导出；EventTap、HotGuard、writer、Graduator 和 SelfRepairPipeline 的实际调用集中在模块内部与测试。未发现独立 observer crate、启动 supervisor、SessionEvent 映射器、自监控配置解析或 GUI 入口。

不建议现在将该模块接入自动晋级链。应先完成 Phase 0/1，修复下列权限和可靠性缺陷，再推进后续阶段。Phase 4 是依赖评测结果的后续阶段，Phase 5 在设计中要求单独立项；它们尚未完成不意味着当前必须立即开放。

## 分阶段核对

| 阶段 | 已存在 | 缺漏 / 不能验收的原因 |
| --- | --- | --- |
| Phase 0 契约与基线 | 精简事件结构、JSON 往返和未知字段测试 | 缺 event_id、trace/task/work-item、证据引用、前序哈希；policy 默认空；无 Session/Delivery 映射和真实事故基线，重启序号不稳定 |
| Phase 1 采集与保护 | 单队列 try_send、JSONL writer、重复结论规则 | 无生产接入、双优先级队列、emergency record、独立 writer 线程、sidecar/supervisor、游标恢复；无新增工具失败风暴/交付冲突检测接入；无性能证据 |
| Phase 2 事故与质量 | 内存事故汇总、严重度计数 | 无持久事故状态机、跨会话指纹归并、根因候选、类型化交付硬门禁、证据真实性核验、用户反馈闭环和仪表盘；并发会话统计串线 |
| Phase 3 回放与提案 | ReplaySpec、mock 字符串遍历、阈值/提示提案结构 | 未运行 Agent/工具/策略，不区分稳定版与候选版；ToolTimeout 被忽略；无真实语料、隔离执行器、自动任务复盘和完整提案证据契约 |
| Phase 4 受控灰度 | bundle hash、等级枚举、分桶函数、manifest 文件切换 | 激活绕过评测和 hash 校验；风险门禁错误；无 Shadow/Canary 样本/窗口状态机和新任务版本固定；重复回滚可重新启用坏版本；没有 GUI 审批 |
| Phase 5 代码自修复 | 补丁文件写入、报告和发布记录 JSON | 没有真实 git worktree、构建/测试/候选执行、二进制发布和实际回滚；路径可逃逸；审批仅检查字符串非空 |

## 主要发现

### F01 [P1] 新监控模块没有接入实际 Agent 生命周期

位置：[lib.rs:17](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/lib.rs:17)、[monitoring/mod.rs:1](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/mod.rs:1)、[Cargo.toml:3](F:/workspace/deepseek-aidops-stable/harness/Cargo.toml:3)。

全仓调用搜索没有找到业务路径创建 EventTap、启动 WAL writer、对真实模型输出调用 HotGuard 或消费保护动作。workspace 未包含 harness-observer。因此正常启动 CLI/Desktop 不会获得这些新保护；测试中手工调用不能替代接入验收。设计 §6、§17、Phase 1 未完成。

建议：在运行时初始化和回合生命周期建立适配器，绑定真实 session/turn/step/criterion；让 GuardAction 改变主循环并生成真实 DeliveryReport；增加独立 sidecar 与有界重启 supervisor。测试必须经过正常入口运行任务并实际观测 WAL/收敛结果。

### F02 [P1] 长中文结论会令在线保护器 panic

位置：[hot_guard.rs:48](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/hot_guard.rs:48)。

`String::truncate(256)` 要求 UTF-8 字符边界。100 个“中”共 300 字节，256 不是边界，调用指纹函数即 panic。接入主 Agent 后，这会由普通中文回复触发主流程异常，与监控失败开放原则冲突。

复现：`repro_chinese_conclusion_panics` 已确认。建议按字符有界采集或计算合法边界，并在归一化前限制扫描工作量；目前先构造完整字符串再截断，也不能保证超长输入的有界耗时。

### F03 [P1] Critical 事件和普通事件一起丢弃，丢失也不形成健康事件

位置：[tap.rs:45](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/tap.rs:45)、[tap.rs:74](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/tap.rs:74)。

只有一个队列，emit 对任何 kind/class 都统一 try_send，失败仅递增原子计数。没有 Critical/Normal 区分，没有 emergency record，也没有下一次成功时补发 TapOverflow。writer 无法挽救入队前丢失的数据。质量报告又只读 TapOverflow 事件，实际丢事件后仍可能显示丢失数为零。

建议：实现双队列与关键事件优先冲刷、有界应急记录、聚合丢失事件和 writer 失联健康状态。增加关键事件拥塞/消费者退出的故障注入测试。现有队列满测试仅确认会丢弃。

### F04 [P1] WAL 重启重复序号，缓冲和磁盘失败处理不满足恢复承诺

位置：[wal.rs:57](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/wal.rs:57)、[wal.rs:115](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/wal.rs:115)、[wal.rs:130](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/wal.rs:130)。

每次 open 都将 file_index/seq/current_bytes 归零，再追加到 events-000001.jsonl。实测两次打开各写一条得到序号 `[1, 1]`，无法支持稳定去重和游标消费。已有文件大小也没有恢复，轮转规则失真。

writer 在 tokio 普通任务中做同步磁盘 I/O，并非专用线程。低流量事件留在 BufWriter 内，只有缓冲自然写满或发送端关闭后才冲刷；flush 错误被忽略，written 统计的是写入缓冲成功，不是持久化成功。没有 fsync 策略、周期冲刷、尾部半写修复、保留期管理或健康事件。

复现：`repro_wal_restart_reuses_sequence`。建议恢复 segment/sequence/长度并处理半写尾部；专用 writer 线程批量写入，关键事件定时/优先同步，所有写入与冲刷失败都计数并可观测。补重启、磁盘满、强杀和长时间积压恢复测试。

### F05 [P1] “低风险自动晋级”开关能放行代码和业务规则

位置：[graduation.rs:84](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/graduation.rs:84)。

CodeOrBusinessRule 仅在 `auto_promote_low_risk == false` 时返回 CodeRequiresHuman。设为 true 且 max_level=E5，就允许代码/业务规则，无需人工批准。反过来，低风险变更本身并不检查该开关。任意 key 均能声明为 Threshold/ConfigToggle，也没有预批准白名单或实际内容交叉核验。

复现：`repro_code_authorized_by_low_risk_flag`。建议代码/业务规则独立且始终验证绑定候选哈希的人工审批记录；低风险开关只控制白名单内真实低风险改动，风险由可信验证器从内容推导，不能相信候选自述。

### F06 [P1] 激活接口绕过完整评测和内容校验

位置：[graduation.rs:217](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/graduation.rs:217)。

activate 只 authorize manifest.changes 后写活动指针，不调用 load_verified，也不要求 proposal/evaluation/批准记录、目标事故 100% 通过、黄金集、回滚演练、样本数、观察窗口或兼容性。PolicyBundle 字段公开，可直接构造；空 changes 不经过任何逐项等级检查。

实测：stage 后篡改 payload，load_verified 明确拒绝，但 activate 同一个对象仍成功；配置中 canary_percent=0 也不阻止该全量指针切换。

复现：`repro_activation_accepts_tampered_unevaluated_bundle`。建议激活只接收可信门禁签发、绑定 bundle/hash/evaluation 的授权凭据；激活前重新验证磁盘内容和所有硬门禁，实施 Shadow → Canary → Stable 状态机，不能把分桶函数与枚举当作灰度闭环。

### F07 [P1] 重复失败通知会把已回滚的坏版本重新激活

位置：[graduation.rs:244](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/graduation.rs:244)、[graduation.rs:279](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/graduation.rs:279)。

rollback 将刚撤下的坏版本保存为 previous_stable_id，record_canary_outcome 又完全忽略 bundle_id。stable → bad 后，第一个 bad 失败通知回滚 stable；相同通知再次到达，则回滚回 bad。旧候选的迟到通知也可能撤销当前另一版本。

复现：`repro_duplicate_failed_canary_reactivates_bad_bundle`。建议按当前候选 ID+generation 做条件转换，失败候选进入持久 Quarantined，重复通知幂等；previous stable 必须始终指向经验证稳定版。并发激活还需解决固定临时文件名和缺少互斥/CAS 的竞争。

### F08 [P1] “不可变”版本可被同 ID 原地覆盖

位置：[bundle.rs:138](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/bundle.rs:138)。

stage 对已存在的 bundle 目录继续 create_dir_all/fs::write，没有拒绝同 ID 重写。运行中任务即使记住 bundle_id，其指向的内容也能变化，既无法复现，也无法可靠回滚；旧目录中未列入新 payload 的文件还会残留。

复现：`repro_restage_overwrites_immutable_bundle`。建议新 ID 独占创建，在临时目录完整校验后一次发布；已存在 ID 必须拒绝。晋级审计 append_record 实际按 bundle_id 覆盖单个 JSON，也应改为不可变追加记录，避免丢掉 Shadow/Canary/批准历史。

### F09 [P1] 自修复评测没有执行候选代码，错误代码仍获全通过

位置：[self_repair.rs:186](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/self_repair.rs:186)、[self_repair.rs:190](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/self_repair.rs:190)、[self_repair.rs:206](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/self_repair.rs:206)、[replay.rs:97](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/replay.rs:97)。

所谓 worktree 是将补丁自己声明的 old_content 写入目录，没有检出真实基线。build_passed 只表示文件替换成功。候选成功通过删除 defect 注入得到，而不是运行修改后的实现。黄金集 before/after 调用完全相同 runner；空黄金集、无 fault_spec 也能通过。runner 只检查模型桩字符串非空和两种故障枚举，不运行 Agent、工具或候选策略；ToolTimeout 分支被通配忽略。

实测把合法源码替换成 `this is invalid rust !!!`，无黄金集/故障演练，仍返回 build_passed=true、all_passed=true。这不是构建/回归验证，不能用于任何晋级决策。

复现：`repro_invalid_code_and_no_golden_tests_pass_review`。建议基于固定 commit 创建真正隔离 worktree，分别执行基线和候选构建/同一组回放；故障条件不能因为候选声明“修好”就删除。将现有 runner 明确保留为模拟原型，接入已有 fake LLM/tool 能力验证运行状态和合法交付终态，强制测试集规模与必要演练。

### F10 [P1] 补丁路径可逃逸隔离目录覆盖外部文件

位置：[self_repair.rs:20](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/self_repair.rs:20)、[self_repair.rs:152](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/self_repair.rs:152)、[bundle.rs:138](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/bundle.rs:138)。

repair_id、PatchFile.path、bundle_id 和 payload 相对路径直接 join，没有拒绝绝对路径/盘符/..，也没有校验解析后路径和符号链接是否仍在隔离目录。materialize 甚至在上下文校验前写入 old_content。`PatchFile.path="../victim.txt"` 可修改 worktree 外文件，设计“不接触用户现有未提交变更”的边界不成立。

复现：`repro_patch_can_write_outside_isolated_worktree`，只在审查专用临时目录中写入合成 victim.txt，没有接触用户文件。建议复用已有沙箱路径解析器，拒绝外部路径并处理链接/Windows reparse point；候选 ID 使用受限格式，隔离执行器限制根目录写权限。

### F11 [P1] 发布和回滚仅修改记录，没有发布或撤回运行产物

位置：[self_repair.rs:253](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/self_repair.rs:253)、[self_repair.rs:284](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/self_repair.rs:284)。

release 只检查调用者传入 all_passed 和审批人字符串非空，然后写 release.json；没有验证可信审批、候选产物哈希或评测绑定，也没有构建产物/二进制版本切换。rollback 只令 rolled_back=true，并未恢复运行产物。现有 release_then_rollback_full_drill 测试只验证记录字段，不能证明发布和回滚演练完成。

建议在真实发布通道完成前明确标记为“发布记录原型”。真实发布必须绑定不可变二进制、报告及 HITL 批准记录，验证激活版本、启动健康和旧版恢复；默认拒绝无人值守代码晋级。Phase 5 应按设计单独评审。

### F12 [P2] 并发会话事故串线，质量统计缺少范围隔离

位置：[incident.rs:44](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/incident.rs:44)、[incident.rs:65](F:/workspace/deepseek-aidops-stable/harness/harness-runtime/src/monitoring/incident.rs:65)。

fold 只保留一个全局开放窗口。A 异常、B 异常、A 终止时，会把 B 的 max_repeat/anomaly_count 记到 A；quality_report 对混合事件取首个 session_id 却统计全部事件。缺少事件 ID 去重及 turn/window 分组。

复现：`repro_incident_attributes_another_sessions_anomaly`（A 的重复数从 3 被记成 B 的 9）。建议按 session+turn 维护窗口，再基于独立事故指纹跨会话归并，质量报告要求明确范围，补交错、重复、乱序测试。

## 其余未完成项

- **事件事实链**：event schema 没有全局幂等 ID、前序哈希和 evidence URI；policy_version 默认为空，turn/step 可缺失。新增事件类别无法描述完整工具、验证、用户纠正、副作用及版本变更流程。
- **schema 兼容**：目前只测未知 JSON 字段；没有 schema_version 拒绝/隔离机制，未知 EventKind 会解析失败，read_wal 把所有解析失败都当坏行跳过，不能区分尾部半写与版本不兼容。
- **隐私与资源**：未见持久化前 Redactor、字段敏感值策略、脱敏语料转换、observer 独立 CPU/内存/token 预算、spool 保留策略。当前事件内容较少降低了暴露范围，但不是已实现脱敏。
- **重复检测覆盖**：只去标点/空白和大小写，并不去序号或轮次措辞；只截取前 256 字节也可能把不同结论合并。不同结论中获得新证据不会清除其他旧指纹累计，可能误判后续重复。
- **质量硬门禁**：新增 QualityReport 只是计数，没有接 DeliveryReport/FactMatrix/ArtifactVerifier。现有 finalize_delivery 检查的是验收项布尔值和非空证据字符串，不能单独证明 §10 所要求的引用存在、哈希有效、来源独立及无未处理 S0/S1。
- **事故与复盘**：无事故 ID/状态机、持久跨会话聚合、可证伪根因支持度、TaskRetrospective；提案缺基线版本、预期改善/不可下降指标、权限、回滚、有效期、测试集及批准角色。
- **现有组件复用**：新管线未接 ArtifactVault/EffectJournal/Blackboard/LeaseWatchdog 或知识库候选审核，另建 `.self-repair` 目录不等于已经复用现有审计和隔离能力。
- **界面与配置**：未发现 `[self_monitor]` 解析、mode 开关、E2/E3 执行权限约束、GUI 健康/事故/审批/回滚入口。GovernorConfig 类型本身不能证明配置能生效。
- **目录残留**：`src/event.rs/event.rs`、`src/hot_guard.rs/hot_guard.rs`、`src/mod.rs/mod.rs`、`src/tap.rs/tap.rs`、`src/wal.rs/wal.rs` 是重复目录副本，未接入模块树，不受正常编译测试覆盖。应确认后清理，避免继续修改错误副本。

## 验证证据与限制

已执行：

```text
cargo test -p harness-runtime --lib monitoring::                 12 passed
cargo test -p harness-runtime --test monitoring                   9 passed
cargo test -p harness-runtime --test self_repair_release           4 passed
cargo test -p harness-runtime --test review_monitoring_repros      9 passed
```

最后 9 项为**缺陷复现断言**：通过表示已重现不正确行为，不能当成验收通过。覆盖 F02/F04/F05/F06/F07/F08/F09/F10/F12。

复现源码已归档为 [self-monitoring-repros.rs](F:/workspace/deepseek-aidops-stable/docs/reviews/self-monitoring-repros.rs)。它不留在常规 Cargo tests 中，以免“断言缺陷存在”的临时审查脚本被误当成长期回归测试。重跑时将其临时复制到 harness-runtime/tests/review_monitoring_repros.rs，运行上述目标后删除这份临时副本。

本次没有修改生产代码，没有启用监控或晋级功能，没有执行真实发布。审查新增文件只有本报告和复现脚本；原有工作区修改保持原样。未运行全工作区测试，未声称满足 P99/2%/99.99% 等性能或可靠性目标。当前没有可运行 sidecar，因此也无法完成设计要求的强杀/24 小时恢复验收。

## 修复顺序

1. 先修 F02/F05/F06/F07/F09/F10：防止监控崩溃、越权晋级、伪评测、错误回滚和隔离逃逸；Phase 4/5 保持非生产状态。
2. 完成 Phase 0/1 真实接入、双队列、可靠 WAL、sidecar/supervisor、脱敏及故障注入，先建立可靠事实链。
3. 完成事故状态机、类型化质量评估及使用真实运行时的回放，再允许 E2/E3 提案验证。
4. 有基线/候选/黄金集、样本和回滚证据后再评审 Phase 4；真实代码修复与发布继续按 Phase 5 单独立项。
