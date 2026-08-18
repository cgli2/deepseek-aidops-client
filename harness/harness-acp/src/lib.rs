//! harness-acp：可选 ACP（Agent Client Protocol）stdio JSON-RPC 服务器。
//!
//! 这是 dsh 插件机制在"进程外边界"的体现：外部宿主（编辑器 / CLI 包装器）经 ACP 与
//! harness 对话，harness 这一侧仍只是事件总线的消费者——把外部请求转译为内部事件，
//! 不反向修改循环（完成文档 §11 / §14）。当前实现覆盖初始化、状态、重放与 prompt；
//! 更高版本 ACP 的增量通知可在不修改 AgentLoop 的前提下继续扩展。

pub mod server;

pub use server::{AcpRequest, AcpResponse, AcpServer};
