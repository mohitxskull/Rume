//! Resilience middleware: circuit breakers, rate limiting, concurrency shedding, and adaptive timeouts.

pub mod circuit_breaker;
pub mod concurrency;
pub mod error;
pub mod rate_limiter;
pub mod timeout;

pub use circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerLayer, CircuitState,
};
pub use concurrency::{ConcurrencyLimitLayer, ConcurrencyLimiter, ConcurrencyPermit};
pub use error::ResilienceError;
pub use rate_limiter::{RateLimitConfig, RateLimitLayer, RateLimitStatus, RateLimiter};
pub use timeout::TimeoutLayer;
