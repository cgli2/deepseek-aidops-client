use uuid::Uuid;

use harness_core::types::UserInput;

pub type SessionId = Uuid;

/// 一个待执行的任务（领取输入 → 跑一个 turn）。
#[derive(Debug, Clone)]
pub struct Task {
    pub session: SessionId,
    pub input: UserInput,
}
