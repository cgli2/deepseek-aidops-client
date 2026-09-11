//! 模型响应恢复策略。
//!
//! `finish_reason=length` 不是任务终态，而是单次模型请求的输出预算耗尽。
//! 这里把恢复策略从 agent 主循环中抽离成一个小状态机：连续失败逐级增加
//! 输出预算，任何有效正文/工具调用都会关闭熔断并清零 streak。

const OUTPUT_STARVATION_RETRIES: usize = 3;
const PROTOCOL_EMPTY_RETRIES: usize = 1;
const FIRST_STARVATION_CAP: u64 = 8_192;
const MAX_RECOVERY_CAP: u64 = 32_768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmptyResponseClass {
    /// 输出额度全部被 reasoning 消耗，尚未产生正文或工具调用。
    OutputStarvation,
    /// 上游正常结束但没有任何可执行载荷。
    ProtocolEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecoveryPlan {
    pub attempt: usize,
    pub max_attempts: usize,
    pub max_output_tokens: u64,
    /// 输出饥饿恢复必须显式关闭思考，给工具调用/正文保留 token。
    pub disable_reasoning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecoveryDecision {
    Retry(RecoveryPlan),
    Exhausted {
        class: EmptyResponseClass,
        attempts: usize,
        last_output_cap: u64,
    },
}

#[derive(Debug, Default)]
pub(crate) struct ResponseRecovery {
    consecutive_empty: usize,
    last_output_cap: u64,
}

impl ResponseRecovery {
    pub fn reset_after_actionable_response(&mut self) {
        self.consecutive_empty = 0;
        self.last_output_cap = 0;
    }

    pub fn on_empty(&mut self, reason: &str, request_output_cap: u64) -> RecoveryDecision {
        let class = classify(reason);
        let max_attempts = match class {
            EmptyResponseClass::OutputStarvation => OUTPUT_STARVATION_RETRIES,
            EmptyResponseClass::ProtocolEmpty => PROTOCOL_EMPTY_RETRIES,
        };

        if self.consecutive_empty >= max_attempts {
            return RecoveryDecision::Exhausted {
                class,
                attempts: self.consecutive_empty,
                last_output_cap: self.last_output_cap.max(request_output_cap),
            };
        }

        self.consecutive_empty += 1;
        let max_output_tokens = match class {
            EmptyResponseClass::OutputStarvation => {
                // 第一次先回到可容纳“短思考 + 一个工具调用”的健康下限；若 Provider
                // 不支持关闭思考，后续再指数扩容。不能像旧策略那样从 3072 降到 1024。
                let previous = self.last_output_cap.max(request_output_cap);
                if self.consecutive_empty == 1 {
                    previous.max(FIRST_STARVATION_CAP)
                } else {
                    previous.saturating_mul(2).min(MAX_RECOVERY_CAP)
                }
            }
            EmptyResponseClass::ProtocolEmpty => request_output_cap.max(4_096),
        };
        self.last_output_cap = max_output_tokens;

        RecoveryDecision::Retry(RecoveryPlan {
            attempt: self.consecutive_empty,
            max_attempts,
            max_output_tokens,
            disable_reasoning: class == EmptyResponseClass::OutputStarvation,
        })
    }
}

fn classify(reason: &str) -> EmptyResponseClass {
    let normalized = reason
        .trim()
        .split_once(':')
        .map_or_else(|| reason.trim(), |(kind, _)| kind)
        .to_ascii_lowercase();
    match normalized.as_str() {
        "length" | "max_tokens" | "reasoning_only" | "incomplete_tool_arguments" => {
            EmptyResponseClass::OutputStarvation
        }
        _ => EmptyResponseClass::ProtocolEmpty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_starvation_escalates_instead_of_shrinking_the_cap() {
        let mut recovery = ResponseRecovery::default();
        let caps = [3_072, 8_192, 16_384];
        let expected = [8_192, 16_384, 32_768];
        for (request_cap, expected_cap) in caps.into_iter().zip(expected) {
            let RecoveryDecision::Retry(plan) = recovery.on_empty("length", request_cap) else {
                panic!("output starvation should still be recoverable");
            };
            assert_eq!(plan.max_output_tokens, expected_cap);
            assert!(plan.disable_reasoning);
        }
        assert!(matches!(
            recovery.on_empty("length", 32_768),
            RecoveryDecision::Exhausted {
                class: EmptyResponseClass::OutputStarvation,
                attempts: 3,
                last_output_cap: 32_768,
            }
        ));
    }

    #[test]
    fn actionable_response_resets_the_circuit_breaker() {
        let mut recovery = ResponseRecovery::default();
        let _ = recovery.on_empty("stop", 3_072);
        recovery.reset_after_actionable_response();
        let RecoveryDecision::Retry(plan) = recovery.on_empty("stop", 3_072) else {
            panic!("a later independent empty response gets its own recovery window");
        };
        assert_eq!(plan.attempt, 1);
    }

    #[test]
    fn incomplete_tool_arguments_use_bounded_output_starvation_recovery() {
        let mut recovery = ResponseRecovery::default();
        let RecoveryDecision::Retry(plan) = recovery.on_empty(
            "incomplete_tool_arguments: EOF while parsing a string at line 1 column 16108",
            4_096,
        ) else {
            panic!("truncated tool arguments should be recoverable");
        };
        assert_eq!(plan.max_attempts, 3);
        assert_eq!(plan.max_output_tokens, 8_192);
        assert!(plan.disable_reasoning);
    }
}
