//! 此文件已迁移到 Definition 层：`harness-capability/src/index.rs`。
//! 索引器只依赖 Definition trait（不耦合具体 Provider），因此放在 capability 中更合适，
//! 桌面 UI 等 Consumer 可直接调用而无需依赖本 Provider crate。
//! 本文件不再被声明为模块（lib.rs 已改为从 `harness_capability::index` 转发）。
