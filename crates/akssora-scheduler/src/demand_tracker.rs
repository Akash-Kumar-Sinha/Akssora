use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemandEvent {
    pub template_id: String,
    pub timestamp: DateTime<Utc>,
    pub host_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateDemand {
    pub template_id: String,
    pub request_count: usize,
    pub requests_per_minute: f64,
    pub avg_interval_secs: f64,
    pub last_request_at: Option<DateTime<Utc>>,
    pub window_start: Option<DateTime<Utc>>,
}

pub struct DemandTracker {
    events: Arc<DashMap<String, Vec<DemandEvent>>>,
    window: Duration,
    max_events_per_template: usize,
}

impl DemandTracker {
    pub fn new(window: Duration) -> Self {
        Self {
            events: Arc::new(DashMap::new()),
            window,
            max_events_per_template: 10_000,
        }
    }

    pub fn with_max_events(mut self, max: usize) -> Self {
        self.max_events_per_template = max;
        self
    }

    pub fn record(&self, template_id: &str) {
        self.record_with_host(template_id, None);
    }

    pub fn record_with_host(&self, template_id: &str, host_id: Option<uuid::Uuid>) {
        let event = DemandEvent {
            template_id: template_id.to_string(),
            timestamp: Utc::now(),
            host_id,
        };

        self.events
            .entry(template_id.to_string())
            .or_default()
            .push(event);

        self.evict_if_needed(template_id);
    }

    pub fn rate_per_minute(&self, template_id: &str) -> f64 {
        let events = self.events_in_window(template_id);
        if events.is_empty() {
            return 0.0;
        }

        let window_secs = self.window.as_secs_f64();
        events.len() as f64 / (window_secs / 60.0)
    }

    pub fn demand_for(&self, template_id: &str) -> TemplateDemand {
        let events = self.events_in_window(template_id);

        let request_count = events.len();
        let window_secs = self.window.as_secs_f64();
        let requests_per_minute = if request_count > 0 {
            request_count as f64 / (window_secs / 60.0)
        } else {
            0.0
        };

        let avg_interval_secs = if request_count > 1 {
            let first = events.first().unwrap().timestamp;
            let last = events.last().unwrap().timestamp;
            let span = last.signed_duration_since(first);
            span.num_seconds() as f64 / (request_count - 1) as f64
        } else {
            window_secs
        };

        let last_request_at = events.last().map(|e| e.timestamp);
        let window_start = events.first().map(|e| e.timestamp);

        TemplateDemand {
            template_id: template_id.to_string(),
            request_count,
            requests_per_minute,
            avg_interval_secs,
            last_request_at,
            window_start,
        }
    }

    pub fn all_demands(&self) -> Vec<TemplateDemand> {
        self.events
            .iter()
            .map(|entry| self.demand_for(entry.key()))
            .collect()
    }

    pub fn top_templates(&self, n: usize) -> Vec<TemplateDemand> {
        let mut demands: Vec<TemplateDemand> = self
            .events
            .iter()
            .map(|entry| self.demand_for(entry.key()))
            .collect();

        demands.sort_by_key(|b| std::cmp::Reverse(b.request_count));
        demands.truncate(n);
        demands
    }

    pub fn event_count(&self, template_id: &str) -> usize {
        self.events_in_window(template_id).len()
    }

    pub fn total_events(&self) -> usize {
        self.events
            .iter()
            .map(|entry| self.events_in_window(entry.key()).len())
            .sum()
    }

    pub fn clear(&self) {
        self.events.clear();
    }

    fn events_in_window(&self, template_id: &str) -> Vec<DemandEvent> {
        let now = Utc::now();
        let window_start = now - self.window;

        self.events
            .get(template_id)
            .map(|events| {
                events
                    .iter()
                    .filter(|e| e.timestamp >= window_start)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn evict_if_needed(&self, template_id: &str) {
        if let Some(mut events) = self.events.get_mut(template_id)
            && events.len() > self.max_events_per_template
        {
            let drain_count = events.len() - self.max_events_per_template;
            events.drain(..drain_count);
        }
    }
}

impl Default for DemandTracker {
    fn default() -> Self {
        Self::new(Duration::from_secs(600))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_rate() {
        let tracker = DemandTracker::new(Duration::from_secs(60));

        tracker.record("alpine");
        tracker.record("alpine");
        tracker.record("alpine");

        let rate = tracker.rate_per_minute("alpine");
        assert!((rate - 3.0).abs() < 0.1, "rate was {rate}");
    }

    #[test]
    fn rate_zero_for_unknown_template() {
        let tracker = DemandTracker::default();
        assert_eq!(tracker.rate_per_minute("nonexistent"), 0.0);
    }

    #[test]
    fn top_templates_sorted() {
        let tracker = DemandTracker::new(Duration::from_secs(600));

        for _ in 0..10 {
            tracker.record("popular");
        }
        for _ in 0..3 {
            tracker.record("less-popular");
        }
        tracker.record("rare");

        let top = tracker.top_templates(2);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].template_id, "popular");
        assert_eq!(top[0].request_count, 10);
        assert_eq!(top[1].template_id, "less-popular");
        assert_eq!(top[1].request_count, 3);
    }

    #[test]
    fn demand_for_template() {
        let tracker = DemandTracker::new(Duration::from_secs(300));

        tracker.record("python");
        tracker.record("python");

        let demand = tracker.demand_for("python");
        assert_eq!(demand.template_id, "python");
        assert_eq!(demand.request_count, 2);
        assert!(demand.requests_per_minute > 0.0);
        assert!(demand.last_request_at.is_some());
    }

    #[test]
    fn multiple_templates_independent() {
        let tracker = DemandTracker::new(Duration::from_secs(60));

        tracker.record("a");
        tracker.record("a");
        tracker.record("b");

        assert_eq!(tracker.event_count("a"), 2);
        assert_eq!(tracker.event_count("b"), 1);
        assert_eq!(tracker.event_count("c"), 0);
    }

    #[test]
    fn clear_resets_everything() {
        let tracker = DemandTracker::new(Duration::from_secs(60));
        tracker.record("x");
        tracker.record("y");

        tracker.clear();
        assert_eq!(tracker.total_events(), 0);
    }

    #[test]
    fn record_with_host() {
        let tracker = DemandTracker::new(Duration::from_secs(60));
        let host = uuid::Uuid::new_v4();
        tracker.record_with_host("test", Some(host));

        let demand = tracker.demand_for("test");
        assert_eq!(demand.request_count, 1);
    }

    #[test]
    fn max_events_cap() {
        let tracker = DemandTracker::new(Duration::from_secs(600)).with_max_events(100);

        for _ in 0..150 {
            tracker.record("template");
        }

        let events = tracker.events.get("template").unwrap();
        assert!(events.len() <= 100);
    }
}
