//! 单一闭环控制器（spec §4.1）：observe → measure → decide。
//!
//! 决策只有四分支：continue / switch_strategy / degrade / terminate。旧守卫在本模块
//! 里降级为传感器——只产信号，不终止回合；终止权收归 TurnGovernor（Task 4 落地）。

pub mod sensors;
pub mod strategy;

pub use sensors::{artifact_text, delta_between, WindowDelta};
