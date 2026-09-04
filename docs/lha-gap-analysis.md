# lha 改造完整性差距分析（只分析，不改代码）

> ⚠️ 本文档的结论已全部并入最终版 **`docs/LHA改造完整性分析.md`（最终合并版）**，请以该文档为准。本文保留仅作历史证据基线，不再更新。

> 结论先行：lha 模块的实现、接线、测试三层均已存在，且编译门禁已验证通过；仍欠缺——
> ① 默认会话路径是否接入 lha 的集成决策；② 若干专项测试与文档收尾。
> 本文档不修改任何代码，结论均来自定向读取、定向 search 与实际执行的编译验证。

## 一、证据基线（本会话直接核实）

| 层 | 证据 | 状态 |
| --- | --- | --- |
| 模块声明 | `harness-runtime/src/lib.rs:16` — `pub mod lha;` | 已存在 |
| 公共 re-export | `lib.rs:48-65` — 约 90 个类型完整 re-export（FactMatrix、SandboxTx、EnergyLedger、DurableDag、WalRecord、LongHorizonRuntime 等） | 已存在 |
| 模块内部结构 | `lha/mod.rs`（orchestrator / quality / watchdog 等多文件组织） | 已存在 |
| 消费者接线 | `long_horizon.rs:21-25` — `LongHorizonManager` 构建并持有 `LongHorizonRuntime` | 已存在 |
| 配置面 | `long_horizon.rs:49-67` — `HARNESS_LHA_*` 环境变量（总 token 预算、租约 TTL、RPM/TPM 限流、重试次数），默认值齐全 | 已存在 |
| 测试覆盖 | `harness-runtime/tests/lha_p2.rs` — 已实际执行：**4 passed / 0 failed（0.05s）**，覆盖 HITL 不可逆效应绑定、契约锁、全局预算耗尽的终态持久化、MVCC 工件投递与恢复 | ✅ 已验证通过 |
| capability 层集成 | `harness-capability/src/index.rs` — 资产索引器只覆盖 Skill / Wiki / CodeGraph，不含 lha | 无集成（是否缺口取决于设计边界） |
| 主路径引用 | `agent_loop.rs` / `execution.rs` / `goal_execution.rs` / `intent.rs` import 区均无 `crate::lha` 引用 | 无默认路径接线 |

## 二、欠缺的步骤与功能

### P0（阻塞项）

1. ~~编译门禁无成功记录~~ **已于本轮验证通过**
   - `cargo +stable-x86_64-pc-windows-msvc check --manifest-path harness/Cargo.toml -p harness-runtime`
   - 实际执行结果：`Finished dev profile [unoptimized + debuginfo] target(s) in 5.04s`，零错误零警告输出。
   - 该项不再缺失，剩余阻塞项仅剩下述集成决策。

2. **默认会话路径未接线 lha（待决策）**
   - lha 的唯一消费者是 `long_horizon.rs` 的 `LongHorizonManager`（独立 durable 控制面入口）。
   - 需要决策：这是设计上的 opt-in 路径，还是遗漏的默认集成。该决策会改变产品行为。

### P1（功能完整性）

3. 专项测试覆盖范围有限：`lha_p2.rs` 已验证通过（4/4），但其覆盖点为 HITL、契约锁、预算终态、MVCC 工件恢复；sandbox 快照回滚原子性、fact_matrix JSON 幂等覆盖的专项测试未见证据。
4. WAL 崩溃重放路径的测试覆盖未确认。

### P2（交付质量）

5. CHANGELOG / README 未反映 lha 改造。
6. capability / 资产层与 lha 无集成（若需在面板展示 lha 状态则为缺口）。

## 三、建议补全顺序

1. ~~跑一次 MSVC `cargo check -p harness-runtime`~~ 已完成，零错误通过（5.04s）。
2. 决策：lha 是否接入默认会话路径（当前仅 `LongHorizonManager` 独立入口）。
3. 补 sandbox 原子性、fact_matrix 幂等、WAL 重放的专项单测。
4. 更新 CHANGELOG / README。

## 四、交付边界

- 本文档仅为差距分析，未修改任何代码。
- 所有结论仅引用上文列出的直接读取 / search 证据；早前无证据的历史结论已全部撤销。
