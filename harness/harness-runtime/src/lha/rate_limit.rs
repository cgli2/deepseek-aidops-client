//! P1 global request/token budget controller with deterministic backpressure.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderLimit {
    pub requests_per_minute: u64,
    pub tokens_per_minute: u64,
    pub request_burst: u64,
    pub token_burst: u64,
}

impl ProviderLimit {
    fn validate(self) -> Result<Self, RateLimitError> {
        if self.requests_per_minute == 0
            || self.tokens_per_minute == 0
            || self.request_burst == 0
            || self.token_burst == 0
        {
            return Err(RateLimitError::InvalidLimit);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    Granted,
    Backpressure { retry_after_ms: u64 },
    RequestTooLarge { requested: u64, burst: u64 },
    GracefulExhaustion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimitError {
    InvalidLimit,
    UnknownProvider(String),
    DuplicateProvider(String),
    Persistence,
}

impl std::fmt::Display for RateLimitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLimit => write!(f, "rate limits must be greater than zero"),
            Self::UnknownProvider(provider) => write!(f, "unknown provider: {provider}"),
            Self::DuplicateProvider(provider) => {
                write!(f, "provider already registered: {provider}")
            }
            Self::Persistence => write!(f, "failed to persist global budget state"),
        }
    }
}

impl std::error::Error for RateLimitError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Bucket {
    limit: ProviderLimit,
    request_units: u128,
    token_units: u128,
    last_refill_ms: u64,
    blocked_until_ms: u64,
    consecutive_429: u32,
}

impl Bucket {
    const SCALE: u128 = 60_000;

    fn new(limit: ProviderLimit, now_ms: u64) -> Self {
        Self {
            request_units: u128::from(limit.request_burst) * Self::SCALE,
            token_units: u128::from(limit.token_burst) * Self::SCALE,
            limit,
            last_refill_ms: now_ms,
            blocked_until_ms: 0,
            consecutive_429: 0,
        }
    }

    fn refill(&mut self, now_ms: u64) {
        let elapsed = now_ms.saturating_sub(self.last_refill_ms);
        self.last_refill_ms = now_ms;
        self.request_units = self
            .request_units
            .saturating_add(u128::from(elapsed) * u128::from(self.limit.requests_per_minute))
            .min(u128::from(self.limit.request_burst) * Self::SCALE);
        self.token_units = self
            .token_units
            .saturating_add(u128::from(elapsed) * u128::from(self.limit.tokens_per_minute))
            .min(u128::from(self.limit.token_burst) * Self::SCALE);
    }

    fn retry_after_ms(&self, estimated_tokens: u64) -> u64 {
        let request_deficit = Self::SCALE.saturating_sub(self.request_units);
        let token_cost = u128::from(estimated_tokens) * Self::SCALE;
        let token_deficit = token_cost.saturating_sub(self.token_units);
        let request_wait = div_ceil(request_deficit, u128::from(self.limit.requests_per_minute));
        let token_wait = div_ceil(token_deficit, u128::from(self.limit.tokens_per_minute));
        request_wait.max(token_wait).min(u128::from(u64::MAX)) as u64
    }
}

fn div_ceil(value: u128, denominator: u128) -> u128 {
    value
        .saturating_add(denominator.saturating_sub(1))
        .checked_div(denominator)
        .unwrap_or(u128::MAX)
}

/// One control-plane budget shared by all workers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalBudgetController {
    providers: BTreeMap<String, Bucket>,
    total_tokens_remaining: u64,
}

impl GlobalBudgetController {
    pub fn new(total_tokens: u64) -> Self {
        Self {
            providers: BTreeMap::new(),
            total_tokens_remaining: total_tokens,
        }
    }

    pub fn register_provider(
        &mut self,
        provider: impl Into<String>,
        limit: ProviderLimit,
        now_ms: u64,
    ) -> Result<(), RateLimitError> {
        let provider = provider.into();
        if self.providers.contains_key(&provider) {
            return Err(RateLimitError::DuplicateProvider(provider));
        }
        self.providers
            .insert(provider, Bucket::new(limit.validate()?, now_ms));
        Ok(())
    }

    pub fn acquire(
        &mut self,
        provider: &str,
        estimated_tokens: u64,
        now_ms: u64,
    ) -> Result<Admission, RateLimitError> {
        if estimated_tokens > self.total_tokens_remaining {
            return Ok(Admission::GracefulExhaustion);
        }
        let bucket = self
            .providers
            .get_mut(provider)
            .ok_or_else(|| RateLimitError::UnknownProvider(provider.into()))?;
        if estimated_tokens > bucket.limit.token_burst {
            return Ok(Admission::RequestTooLarge {
                requested: estimated_tokens,
                burst: bucket.limit.token_burst,
            });
        }
        bucket.refill(now_ms);
        if now_ms < bucket.blocked_until_ms {
            return Ok(Admission::Backpressure {
                retry_after_ms: bucket.blocked_until_ms - now_ms,
            });
        }
        let request_cost = Bucket::SCALE;
        let token_cost = u128::from(estimated_tokens) * Bucket::SCALE;
        if bucket.request_units < request_cost || bucket.token_units < token_cost {
            return Ok(Admission::Backpressure {
                retry_after_ms: bucket.retry_after_ms(estimated_tokens).max(1),
            });
        }
        bucket.request_units -= request_cost;
        bucket.token_units -= token_cost;
        bucket.consecutive_429 = 0;
        self.total_tokens_remaining -= estimated_tokens;
        Ok(Admission::Granted)
    }

    /// Provider 429s trigger capped exponential backoff with deterministic jitter. The
    /// deterministic component makes WAL replay produce the same schedule.
    pub fn record_429(&mut self, provider: &str, now_ms: u64) -> Result<u64, RateLimitError> {
        let bucket = self
            .providers
            .get_mut(provider)
            .ok_or_else(|| RateLimitError::UnknownProvider(provider.into()))?;
        bucket.consecutive_429 = bucket.consecutive_429.saturating_add(1).min(10);
        let exponent = bucket.consecutive_429.saturating_sub(1);
        let base = 1_000_u64.saturating_mul(1_u64 << exponent).min(60_000);
        let jitter = stable_jitter(provider, bucket.consecutive_429, base / 4 + 1);
        let delay = base.saturating_add(jitter).min(75_000);
        bucket.blocked_until_ms = now_ms.saturating_add(delay);
        Ok(delay)
    }

    pub fn total_tokens_remaining(&self) -> u64 {
        self.total_tokens_remaining
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), RateLimitError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|_| RateLimitError::Persistence)?;
        }
        super::storage::atomic_write(
            path,
            &serde_json::to_vec_pretty(self).map_err(|_| RateLimitError::Persistence)?,
        )
        .map_err(|_| RateLimitError::Persistence)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, RateLimitError> {
        super::storage::recover_atomic(path.as_ref()).map_err(|_| RateLimitError::Persistence)?;
        serde_json::from_slice(&fs::read(path).map_err(|_| RateLimitError::Persistence)?)
            .map_err(|_| RateLimitError::Persistence)
    }
}

fn stable_jitter(provider: &str, attempt: u32, modulus: u64) -> u64 {
    let mut hash = 14_695_981_039_346_656_037_u64;
    for byte in provider.bytes().chain(attempt.to_le_bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash % modulus
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller(total: u64) -> GlobalBudgetController {
        let mut value = GlobalBudgetController::new(total);
        value
            .register_provider(
                "primary",
                ProviderLimit {
                    requests_per_minute: 60,
                    tokens_per_minute: 600,
                    request_burst: 2,
                    token_burst: 100,
                },
                0,
            )
            .unwrap();
        value
    }

    #[test]
    fn token_and_request_buckets_apply_backpressure_and_refill() {
        let mut value = controller(1_000);
        assert_eq!(value.acquire("primary", 50, 0).unwrap(), Admission::Granted);
        assert_eq!(value.acquire("primary", 50, 0).unwrap(), Admission::Granted);
        assert!(matches!(
            value.acquire("primary", 1, 0).unwrap(),
            Admission::Backpressure { .. }
        ));
        assert_eq!(
            value.acquire("primary", 1, 1_000).unwrap(),
            Admission::Granted
        );
    }

    #[test]
    fn global_exhaustion_is_explicit_and_does_not_overdraw() {
        let mut value = controller(10);
        assert_eq!(
            value.acquire("primary", 11, 0).unwrap(),
            Admission::GracefulExhaustion
        );
        assert_eq!(value.total_tokens_remaining(), 10);
    }

    #[test]
    fn rate_limit_backoff_is_capped_and_replay_deterministic() {
        let mut first = controller(1_000);
        let mut second = controller(1_000);
        for attempt in 0..12 {
            let a = first.record_429("primary", attempt * 100_000).unwrap();
            let b = second.record_429("primary", attempt * 100_000).unwrap();
            assert_eq!(a, b);
            assert!(a <= 75_000);
        }
    }
}
