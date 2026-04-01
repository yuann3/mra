//! Windowed restart counters.
//!
//! [`RestartTracker`] tracks per-child restarts with exponential backoff.
//! [`IntensityTracker`] tracks supervisor-wide restart frequency.
//! Both are internal building blocks for [`RestartManager`](super::restart_manager::RestartManager).

use std::collections::VecDeque;
use std::time::Duration;

use tokio::time::Instant;

use super::config::RestartIntensity;
use crate::config::RestartPolicy;

pub(crate) struct RestartTracker {
    timestamps: VecDeque<Instant>,
    max_restarts: u32,
    window: Duration,
    backoff_base: Duration,
    backoff_max: Duration,
    pub(crate) total_restarts: u64,
}

impl RestartTracker {
    pub(crate) fn new(policy: &RestartPolicy) -> Self {
        Self {
            timestamps: VecDeque::new(),
            max_restarts: policy.max_restarts,
            window: policy.window,
            backoff_base: policy.backoff_base,
            backoff_max: policy.backoff_max,
            total_restarts: 0,
        }
    }

    pub(crate) fn record(&mut self, now: Instant) {
        self.total_restarts += 1;
        self.timestamps.push_back(now);
        let cutoff = now - self.window;
        while self.timestamps.front().is_some_and(|&t| t < cutoff) {
            self.timestamps.pop_front();
        }
    }

    pub(crate) fn exceeded(&self) -> bool {
        self.timestamps.len() > self.max_restarts as usize
    }

    pub(crate) fn backoff_delay(&self) -> Duration {
        let n = self.timestamps.len().saturating_sub(1) as u32;
        let delay = self.backoff_base.saturating_mul(2u32.pow(n));
        delay.min(self.backoff_max)
    }
}

pub(crate) struct IntensityTracker {
    timestamps: VecDeque<Instant>,
    intensity: RestartIntensity,
    pub(crate) total_restarts: u64,
}

impl IntensityTracker {
    pub(crate) fn new(intensity: RestartIntensity) -> Self {
        Self {
            timestamps: VecDeque::new(),
            intensity,
            total_restarts: 0,
        }
    }

    pub(crate) fn record(&mut self, now: Instant) {
        self.total_restarts += 1;
        self.timestamps.push_back(now);
        let cutoff = now - self.intensity.window;
        while self.timestamps.front().is_some_and(|&t| t < cutoff) {
            self.timestamps.pop_front();
        }
    }

    pub(crate) fn exceeded(&self) -> bool {
        self.timestamps.len() > self.intensity.max_restarts as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RestartPolicy;
    use std::time::Duration;
    use tokio::time::Instant;

    fn default_policy() -> RestartPolicy {
        RestartPolicy {
            max_restarts: 3,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(16),
        }
    }

    #[tokio::test]
    async fn tracker_not_exceeded_within_limit() {
        let mut tracker = RestartTracker::new(&default_policy());
        let now = Instant::now();
        for i in 0..3 {
            tracker.record(now + Duration::from_secs(i));
        }
        assert!(!tracker.exceeded());
    }

    #[tokio::test]
    async fn tracker_exceeded_over_limit() {
        let mut tracker = RestartTracker::new(&default_policy());
        let now = Instant::now();
        for i in 0..4 {
            tracker.record(now + Duration::from_secs(i));
        }
        assert!(tracker.exceeded());
    }

    #[tokio::test]
    async fn tracker_evicts_old_timestamps_outside_window() {
        let policy = RestartPolicy {
            max_restarts: 3,
            window: Duration::from_secs(10),
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(16),
        };
        let mut tracker = RestartTracker::new(&policy);
        let now = Instant::now();
        // Record 3 restarts at t=0,1,2
        for i in 0..3 {
            tracker.record(now + Duration::from_secs(i));
        }
        // Record 1 more at t=15 (outside 10s window, old ones evicted)
        tracker.record(now + Duration::from_secs(15));
        assert!(!tracker.exceeded());
    }

    #[tokio::test]
    async fn tracker_backoff_exponential() {
        let mut tracker = RestartTracker::new(&default_policy());
        let now = Instant::now();

        // 0 timestamps → backoff_base * 2^0 = 1s (but len-1 = 0 when empty... let's record)
        tracker.record(now);
        // len=1, n=0 → 1 * 2^0 = 1s
        assert_eq!(tracker.backoff_delay(), Duration::from_secs(1));

        tracker.record(now + Duration::from_secs(1));
        // len=2, n=1 → 1 * 2^1 = 2s
        assert_eq!(tracker.backoff_delay(), Duration::from_secs(2));

        tracker.record(now + Duration::from_secs(2));
        // len=3, n=2 → 1 * 2^2 = 4s
        assert_eq!(tracker.backoff_delay(), Duration::from_secs(4));
    }

    #[tokio::test]
    async fn tracker_backoff_capped_at_max() {
        let policy = RestartPolicy {
            max_restarts: 10,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(16),
        };
        let mut tracker = RestartTracker::new(&policy);
        let now = Instant::now();
        // Record enough to make 2^n exceed 16
        for i in 0..6 {
            tracker.record(now + Duration::from_secs(i));
        }
        // len=6, n=5 → 1 * 2^5 = 32, capped to 16
        assert_eq!(tracker.backoff_delay(), Duration::from_secs(16));
    }

    #[tokio::test]
    async fn tracker_total_restarts_counts_all() {
        let policy = RestartPolicy {
            max_restarts: 3,
            window: Duration::from_secs(10),
            backoff_base: Duration::from_secs(1),
            backoff_max: Duration::from_secs(16),
        };
        let mut tracker = RestartTracker::new(&policy);
        let now = Instant::now();
        // Record 3 early, then 2 late (early ones evicted from window)
        for i in 0..3 {
            tracker.record(now + Duration::from_secs(i));
        }
        for i in 0..2 {
            tracker.record(now + Duration::from_secs(20 + i));
        }
        // total_restarts counts all 5, even though window only has 2
        assert_eq!(tracker.total_restarts, 5);
    }

    #[tokio::test]
    async fn intensity_tracker_exceeded() {
        let intensity = RestartIntensity {
            max_restarts: 2,
            window: Duration::from_secs(60),
        };
        let mut tracker = IntensityTracker::new(intensity);
        let now = Instant::now();
        tracker.record(now);
        tracker.record(now + Duration::from_secs(1));
        assert!(!tracker.exceeded());
        tracker.record(now + Duration::from_secs(2));
        assert!(tracker.exceeded());
    }
}
