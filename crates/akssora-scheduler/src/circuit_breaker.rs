use std::fmt;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const FAILURE_THRESHOLD: u32 = 5;
const SUCCESS_THRESHOLD: u32 = 3;
const OPEN_DURATION_SECS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

impl fmt::Display for CircuitState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "closed"),
            CircuitState::Open => write!(f, "open"),
            CircuitState::HalfOpen => write!(f, "half_open"),
        }
    }
}

#[derive(Debug)]
pub struct CircuitBreaker {
    failures: AtomicU32,
    successes: AtomicU32,
    opened_at: AtomicU64,
    state: AtomicU32, // 0 = Closed, 1 = Open, 2 = HalfOpen
}

impl CircuitBreaker {
    pub fn new() -> Self {
        Self {
            failures: AtomicU32::new(0),
            successes: AtomicU32::new(0),
            opened_at: AtomicU64::new(0),
            state: AtomicU32::new(CircuitState::Closed as u32),
        }
    }

    pub fn state(&self) -> CircuitState {
        match self.state.load(Ordering::Acquire) {
            0 => CircuitState::Closed,
            1 => {
                let opened = self.opened_at.load(Ordering::Acquire);
                let now = now_secs();
                if now.saturating_sub(opened) >= OPEN_DURATION_SECS {
                    self.state
                        .store(CircuitState::HalfOpen as u32, Ordering::Release);
                    self.successes.store(0, Ordering::Relaxed);
                    tracing::info!("circuit breaker transitioning to half_open");
                    CircuitState::HalfOpen
                } else {
                    CircuitState::Open
                }
            }
            2 => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }

    pub fn record_success(&self) {
        match self.state.load(Ordering::Acquire) {
            0 => {
                self.failures.store(0, Ordering::Relaxed);
            }
            1 => {}
            2 => {
                let prev = self.successes.fetch_add(1, Ordering::AcqRel);
                if prev + 1 >= SUCCESS_THRESHOLD {
                    self.state
                        .store(CircuitState::Closed as u32, Ordering::Release);
                    self.failures.store(0, Ordering::Relaxed);
                    tracing::info!("circuit breaker closed after recovery");
                }
            }
            _ => {}
        }
    }

    pub fn record_failure(&self) {
        match self.state.load(Ordering::Acquire) {
            0 => {
                let prev = self.failures.fetch_add(1, Ordering::AcqRel);
                if prev + 1 >= FAILURE_THRESHOLD {
                    self.trip();
                }
            }
            1 => {
                self.opened_at.store(now_secs(), Ordering::Release);
            }
            2 => {
                self.trip();
            }
            _ => {}
        }
    }

    fn trip(&self) {
        self.state
            .store(CircuitState::Open as u32, Ordering::Release);
        self.opened_at.store(now_secs(), Ordering::Release);
        self.successes.store(0, Ordering::Relaxed);
        tracing::warn!("circuit breaker tripped open");
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_closed() {
        let cb = CircuitBreaker::new();
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn trips_after_threshold_failures() {
        let cb = CircuitBreaker::new();
        for _ in 0..FAILURE_THRESHOLD {
            cb.record_failure();
        }
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn resets_on_success_when_closed() {
        let cb = CircuitBreaker::new();
        for _ in 0..FAILURE_THRESHOLD - 1 {
            cb.record_failure();
        }
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert_eq!(cb.failures.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn half_open_allows_success_path() {
        let cb = CircuitBreaker::new();
        for _ in 0..FAILURE_THRESHOLD {
            cb.record_failure();
        }
        assert_eq!(cb.state(), CircuitState::Open);

        cb.opened_at.store(0, Ordering::Release);

        assert_eq!(cb.state(), CircuitState::HalfOpen);

        for _ in 0..SUCCESS_THRESHOLD {
            cb.record_success();
        }
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn half_open_failure_reopens() {
        let cb = CircuitBreaker::new();
        for _ in 0..FAILURE_THRESHOLD {
            cb.record_failure();
        }
        cb.opened_at.store(0, Ordering::Release);
        let _ = cb.state(); // → HalfOpen
        cb.record_failure(); // failure in half-open → back to Open
        assert_eq!(cb.state(), CircuitState::Open);
    }
}
