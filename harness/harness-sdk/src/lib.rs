//! harness-sdk：可选的进程外 JSON-RPC 客户端（dsh 插件机制 §11 的宿主侧边界）。
//!
//! 供宿主应用（编辑器插件 / CLI 包装器）连接 ACP 服务器。与 `harness-acp` 成对：
//! 一端是服务器（harness 内、事件总线消费者），一端是客户端（宿主进程内）。

pub mod client;

pub use client::{RpcRequest, SdkClient};
