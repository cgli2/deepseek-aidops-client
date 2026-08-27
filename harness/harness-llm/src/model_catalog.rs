//! 离线模型目录（对齐 cc-switch 的 `piModelCatalog.ts`）。
//!
//! 设计哲学（见 `docs/pi-thinking-level-map-requirements-zh.md`）：
//! - 预设**完整可靠**，随包发布、离线可用，绝不联网下载或猜测能力；
//! - 自定义模型（用户手填 ID）**不自动套用**预设能力，也不自动推断 `thinkingLevelMap`；
//! - 思考档位仅作「发送给上游的字符串值」载体，语义由上游决定。

use crate::Usage;

/// 思考档位（对齐 cc-switch `PiThinkingLevel`）。
///
/// 语义：`Off`/`Minimal`/`Low`/`Medium`/`High`/`XHigh`/`Max` 为各模型自有档位名；
/// `Auto` 表示不显式设置、交给模型默认。具体字符串如何映射到上游由对应 Provider 决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
    Auto,
}

impl ThinkingLevel {
    /// 转成发送给上游 `reasoning_effort` 的字符串；`Auto` 返回 `None`（即不发送该字段）。
    pub fn as_upstream(self) -> Option<&'static str> {
        match self {
            // DeepSeek/OpenAI 兼容端的枚举值是 `none`；`Off` 只是本地 UI 语义。
            ThinkingLevel::Off => Some("none"),
            ThinkingLevel::Minimal => Some("minimal"),
            ThinkingLevel::Low => Some("low"),
            ThinkingLevel::Medium => Some("medium"),
            ThinkingLevel::High => Some("high"),
            ThinkingLevel::XHigh => Some("xhigh"),
            ThinkingLevel::Max => Some("max"),
            ThinkingLevel::Auto => None,
        }
    }
}

/// 单模型的离线预设能力 + 定价（定价单位：USD / 1M tokens，离线快照，随价目调整）。
#[derive(Debug, Clone, Copy)]
pub struct ModelInfo {
    pub id: &'static str,
    /// 是否支持扩展思考（reasoning）。
    pub reasoning: bool,
    /// 上下文窗口（tokens）。
    pub context_window: u64,
    /// 单次最大输出（tokens）。
    pub max_tokens: u64,
    /// 支持的思考档位（不支持则为 `None`）。
    pub thinking_levels: Option<&'static [ThinkingLevel]>,
    /// 输入单价（USD / 1M tokens）。
    pub price_per_1m_prompt: f64,
    /// 输出单价（USD / 1M tokens）。
    pub price_per_1m_completion: f64,
}

/// 离线目录（仅覆盖本项目已知模型；未知模型由 `lookup` 返回 `None`，不猜测）。
static CATALOG: &[ModelInfo] = &[
    ModelInfo {
        id: "deepseek-chat",
        reasoning: false,
        context_window: 128_000,
        max_tokens: 8_192,
        thinking_levels: None,
        price_per_1m_prompt: 0.27,
        price_per_1m_completion: 1.10,
    },
    ModelInfo {
        id: "deepseek-reasoner",
        reasoning: true,
        context_window: 64_000,
        max_tokens: 8_192,
        thinking_levels: Some(&[
            ThinkingLevel::Off,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
        ]),
        price_per_1m_prompt: 0.55,
        price_per_1m_completion: 2.19,
    },
    ModelInfo {
        id: "deepseek-v4-flash",
        reasoning: true,
        context_window: 128_000,
        max_tokens: 16_384,
        thinking_levels: Some(&[
            ThinkingLevel::Off,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::XHigh,
            ThinkingLevel::Max,
        ]),
        price_per_1m_prompt: 0.27,
        price_per_1m_completion: 1.10,
    },
    ModelInfo {
        id: "deepseek-v4",
        reasoning: true,
        context_window: 128_000,
        max_tokens: 32_768,
        thinking_levels: Some(&[
            ThinkingLevel::Off,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::XHigh,
            ThinkingLevel::Max,
        ]),
        price_per_1m_prompt: 0.55,
        price_per_1m_completion: 2.19,
    },
];

/// 按模型 ID 查离线预设（精确匹配）。不存在返回 `None`（自定义模型不猜测）。
pub fn lookup(model: &str) -> Option<&'static ModelInfo> {
    CATALOG.iter().find(|m| m.id == model)
}

/// 估算一次请求成本（USD）。未知模型按 0 计价，不报错。
pub fn estimate_cost(model: &str, usage: &Usage) -> f64 {
    let Some(info) = lookup(model) else {
        return 0.0;
    };
    let prompt = usage.prompt_tokens as f64 / 1_000_000.0 * info.price_per_1m_prompt;
    let completion = usage.completion_tokens as f64 / 1_000_000.0 * info.price_per_1m_completion;
    prompt + completion
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entries_are_complete_and_disjoint() {
        for m in CATALOG {
            assert!(m.context_window > 0);
            assert!(m.max_tokens > 0);
            // 支持思考的模型必须显式给出档位集合。
            if m.reasoning {
                assert!(m.thinking_levels.is_some());
            }
        }
        // ID 不重复。
        for i in 0..CATALOG.len() {
            for j in (i + 1)..CATALOG.len() {
                assert_ne!(CATALOG[i].id, CATALOG[j].id);
            }
        }
    }

    #[test]
    fn unknown_model_is_not_guessed() {
        assert!(lookup("my-custom-model-xyz").is_none());
        assert_eq!(estimate_cost("my-custom-model-xyz", &Usage::default()), 0.0);
    }

    #[test]
    fn estimate_cost_uses_preset_prices() {
        let u = Usage {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            total_tokens: 2_000_000,
        };
        // deepseek-chat: 0.27 + 1.10 = 1.37 USD。
        assert!((estimate_cost("deepseek-chat", &u) - 1.37).abs() < 1e-9);
    }

    #[test]
    fn thinking_level_auto_maps_to_none() {
        assert_eq!(ThinkingLevel::Auto.as_upstream(), None);
        assert_eq!(ThinkingLevel::Off.as_upstream(), Some("none"));
        assert_eq!(ThinkingLevel::High.as_upstream(), Some("high"));
    }
}
