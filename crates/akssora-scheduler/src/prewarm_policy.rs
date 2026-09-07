use crate::demand_tracker::{DemandTracker, TemplateDemand};

pub trait PrewarmPolicy: Send + Sync {
    fn desired_pool_size(
        &self,
        demand: &TemplateDemand,
        current_pool_size: usize,
        host_capacity: usize,
    ) -> usize;
}

pub struct EmaPrewarmPolicy {
    pub alpha: f64,
    pub buffer_factor: f64,
    pub min_pool: usize,
    pub max_pool: usize,
}

impl EmaPrewarmPolicy {
    pub fn new() -> Self {
        Self {
            alpha: 0.3,
            buffer_factor: 1.5,
            min_pool: 1,
            max_pool: 20,
        }
    }

    pub fn with_alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha.clamp(0.01, 1.0);
        self
    }

    pub fn with_buffer_factor(mut self, factor: f64) -> Self {
        self.buffer_factor = factor.max(0.1);
        self
    }

    pub fn with_min_pool(mut self, min: usize) -> Self {
        self.min_pool = min;
        self
    }

    pub fn with_max_pool(mut self, max: usize) -> Self {
        self.max_pool = max;
        self
    }
}

impl Default for EmaPrewarmPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl PrewarmPolicy for EmaPrewarmPolicy {
    fn desired_pool_size(
        &self,
        demand: &TemplateDemand,
        _current_pool_size: usize,
        host_capacity: usize,
    ) -> usize {
        let current_rate = demand.requests_per_minute;

        let raw_desired = current_rate * self.buffer_factor;
        let desired = raw_desired.ceil() as usize;

        let clamped = desired.clamp(self.min_pool, self.max_pool);

        clamped.min(host_capacity)
    }
}

pub struct FixedPrewarmPolicy {
    pub size: usize,
}

impl FixedPrewarmPolicy {
    pub fn new(size: usize) -> Self {
        Self { size }
    }
}

impl PrewarmPolicy for FixedPrewarmPolicy {
    fn desired_pool_size(
        &self,
        _demand: &TemplateDemand,
        _current_pool_size: usize,
        host_capacity: usize,
    ) -> usize {
        self.size.min(host_capacity)
    }
}

pub struct AdaptivePrewarmPolicy {
    alpha: f64,
    buffer_factor: f64,
    min_pool: usize,
    max_pool: usize,
    smoothed_rates: dashmap::DashMap<String, f64>,
}

impl AdaptivePrewarmPolicy {
    pub fn new() -> Self {
        Self {
            alpha: 0.3,
            buffer_factor: 1.5,
            min_pool: 1,
            max_pool: 20,
            smoothed_rates: dashmap::DashMap::new(),
        }
    }

    pub fn with_alpha(mut self, alpha: f64) -> Self {
        self.alpha = alpha.clamp(0.01, 1.0);
        self
    }

    pub fn with_buffer_factor(mut self, factor: f64) -> Self {
        self.buffer_factor = factor.max(0.1);
        self
    }

    pub fn with_min_pool(mut self, min: usize) -> Self {
        self.min_pool = min;
        self
    }

    pub fn with_max_pool(mut self, max: usize) -> Self {
        self.max_pool = max;
        self
    }

    pub fn smoothed_rate(&self, template_id: &str) -> f64 {
        self.smoothed_rates
            .get(template_id)
            .map(|r| *r)
            .unwrap_or(0.0)
    }
}

impl Default for AdaptivePrewarmPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl PrewarmPolicy for AdaptivePrewarmPolicy {
    fn desired_pool_size(
        &self,
        demand: &TemplateDemand,
        _current_pool_size: usize,
        host_capacity: usize,
    ) -> usize {
        let current_rate = demand.requests_per_minute;

        let previous = self
            .smoothed_rates
            .get(&demand.template_id)
            .map(|r| *r)
            .unwrap_or(current_rate);

        let smoothed = self.alpha * current_rate + (1.0 - self.alpha) * previous;

        self.smoothed_rates
            .insert(demand.template_id.clone(), smoothed);

        let raw_desired = smoothed * self.buffer_factor;
        let desired = raw_desired.ceil() as usize;

        desired
            .clamp(self.min_pool, self.max_pool)
            .min(host_capacity)
    }
}

pub fn compute_prewarm_targets(
    tracker: &DemandTracker,
    policy: &dyn PrewarmPolicy,
    host_capacity: usize,
) -> std::collections::HashMap<String, usize> {
    let mut targets = std::collections::HashMap::new();

    for demand in tracker.all_demands() {
        let desired = policy.desired_pool_size(&demand, 0, host_capacity);
        if desired > 0 {
            targets.insert(demand.template_id, desired);
        }
    }

    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demand_tracker::DemandTracker;
    use std::time::Duration;

    fn demand_with_rate(rate_per_minute: f64) -> TemplateDemand {
        TemplateDemand {
            template_id: "test".to_string(),
            request_count: (rate_per_minute * 10.0) as usize, // 10-minute window
            requests_per_minute: rate_per_minute,
            avg_interval_secs: if rate_per_minute > 0.0 {
                60.0 / rate_per_minute
            } else {
                600.0
            },
            last_request_at: Some(chrono::Utc::now()),
            window_start: Some(chrono::Utc::now() - chrono::Duration::minutes(10)),
        }
    }

    #[test]
    fn fixed_policy() {
        let policy = FixedPrewarmPolicy::new(5);
        let demand = demand_with_rate(10.0);
        assert_eq!(policy.desired_pool_size(&demand, 0, 100), 5);
        assert_eq!(policy.desired_pool_size(&demand, 0, 3), 3); // capped by capacity
    }

    #[test]
    fn ema_policy_basic() {
        let policy = EmaPrewarmPolicy::new();
        let demand = demand_with_rate(4.0); // 4 req/min

        let size = policy.desired_pool_size(&demand, 0, 100);
        assert_eq!(size, 6);
    }

    #[test]
    fn ema_policy_respects_min_pool() {
        let policy = EmaPrewarmPolicy::new().with_min_pool(3);
        let demand = demand_with_rate(0.0); // no demand

        let size = policy.desired_pool_size(&demand, 0, 100);
        assert_eq!(size, 3);
    }

    #[test]
    fn ema_policy_respects_max_pool() {
        let policy = EmaPrewarmPolicy::new().with_max_pool(5);
        let demand = demand_with_rate(100.0); // huge demand

        let size = policy.desired_pool_size(&demand, 0, 100);
        assert_eq!(size, 5);
    }

    #[test]
    fn ema_policy_respects_host_capacity() {
        let policy = EmaPrewarmPolicy::new();
        let demand = demand_with_rate(50.0);

        let size = policy.desired_pool_size(&demand, 0, 10);
        assert_eq!(size, 10);
    }

    #[test]
    fn adaptive_policy_smoothing() {
        let policy = AdaptivePrewarmPolicy::new().with_alpha(0.5);

        let d1 = demand_with_rate(10.0);
        let s1 = policy.desired_pool_size(&d1, 0, 100);
        assert_eq!(s1, 15);

        let d2 = demand_with_rate(2.0);
        let s2 = policy.desired_pool_size(&d2, 0, 100);
        assert_eq!(s2, 9);
    }

    #[test]
    fn test_compute_prewarm_targets() {
        let tracker = DemandTracker::new(Duration::from_secs(600));
        tracker.record("alpine");
        tracker.record("alpine");
        tracker.record("alpine");
        tracker.record("python");

        let policy = FixedPrewarmPolicy::new(2);
        let targets = super::compute_prewarm_targets(&tracker, &policy, 100);

        assert_eq!(targets.get("alpine"), Some(&2));
        assert_eq!(targets.get("python"), Some(&2));
    }
}
