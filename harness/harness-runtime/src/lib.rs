//! harness-runtime：tokio 编排 + Agent 循环 + 工具管线 + 多任务调度（原 §5.6 / §7）。

pub mod agent_loop;
pub mod controller;
pub mod events;
pub mod scheduler;
pub mod subagent;
pub mod task;

pub use agent_loop::AgentLoop;
pub use controller::SessionController;
pub use events::{PreStep, TurnStopping};
pub use scheduler::Scheduler;
pub use subagent::InProcessSubagent;
pub use task::{SessionId, Task};
