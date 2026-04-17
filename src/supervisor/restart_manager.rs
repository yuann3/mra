//! Restart decision manager for the supervisor.
//!
//! This module consolidates all restart-related logic into a single struct:
//! - Per-child restart tracking (backoff, limits)
//! - Global restart intensity tracking
//! - Policy evaluation (Permanent/Transient/Temporary)
//! - Strategy dispatch (OneForOne/OneForAll)
//!
//! The `RestartManager::decide()` method is the single entry point for
//! restart decisions. It returns a `RestartDecision` enum that tells
//! the supervisor what to do — without blocking on backoff delays.

use std::collections::HashMap;
use std::time::Duration;

use tokio::time::Instant;

use super::ChildExit;
use super::config::{ChildRestart, Strategy, SupervisorConfig};
use super::tracker::{IntensityTracker, RestartTracker};
use crate::config::RestartPolicy;

/// What the supervisor should do after a child exits.
///
/// Returned synchronously by [`RestartManager::decide`]. The supervisor
/// is responsible for scheduling any backoff delay and executing the
/// restart or shutdown.
#[derive(Debug, Clone)]
pub enum RestartDecision {
    /// Restart this child after the specified backoff delay.
    RestartAfter { delay: Duration },
    /// Restart all children in insertion order (OneForAll cascade).
    RestartAll,
    /// Don't restart (Temporary child, or Transient with normal exit).
    NoRestart,
    /// Per-child restart limit exceeded; child will not be restarted again.
    ChildLimitExceeded { restarts: u64 },
    /// Supervisor-wide intensity limit exceeded; supervisor shuts down.
    IntensityExceeded { total_restarts: u64 },
}

struct ChildRestartState {
    policy: ChildRestart,
    tracker: RestartTracker,
}

/// Coordinates restart decisions, backoff calculation, and limit enforcement.
///
/// This struct owns all restart-related state and provides a single entry point
/// (`decide`) for the supervisor to determine what to do when a child exits.
/// Decisions are synchronous — the supervisor is responsible for scheduling
/// any backoff delays.
pub(crate) struct RestartManager {
    strategy: Strategy,
    children: HashMap<String, ChildRestartState>,
    intensity: IntensityTracker,
}

impl RestartManager {
    /// Creates a new RestartManager with supervisor-wide config.
    pub(crate) fn new(config: &SupervisorConfig) -> Self {
        Self {
            strategy: config.strategy,
            children: HashMap::new(),
            intensity: IntensityTracker::new(config.intensity.clone()),
        }
    }

    /// Registers a child for restart tracking. Call once on initial start.
    pub(crate) fn register(
        &mut self,
        name: &str,
        restart: ChildRestart,
        restart_policy: &RestartPolicy,
    ) {
        self.children.insert(
            name.to_owned(),
            ChildRestartState {
                policy: restart,
                tracker: RestartTracker::new(restart_policy),
            },
        );
    }

    /// The main entry point: "This child exited — what should I do?"
    ///
    /// Synchronously decides whether to restart, checking:
    /// - Restart policy (Permanent/Transient/Temporary)
    /// - Per-child restart limits
    /// - Supervisor-wide intensity limits
    /// - OneForOne vs OneForAll strategy
    ///
    /// Returns a decision with computed backoff delay (if applicable).
    /// **Does NOT sleep** — supervisor must schedule the delay.
    pub(crate) fn decide(
        &mut self,
        name: &str,
        exit: &ChildExit,
        hung: bool,
        now: Instant,
    ) -> RestartDecision {
        // Budget exhaustion is terminal — the counter is latched, so restarting
        // is futile. Return early before recording anything in trackers.
        if matches!(exit, ChildExit::BudgetExceeded) {
            return RestartDecision::NoRestart;
        }

        let Some(child) = self.children.get_mut(name) else {
            return RestartDecision::NoRestart;
        };

        // Hung children are treated as failures regardless of exit type
        let is_failure = hung || exit.is_failure();

        // 1. Evaluate restart policy
        if !child.policy.should_restart(is_failure) {
            return RestartDecision::NoRestart;
        }

        // 2. Record restart timestamp in per-child tracker
        child.tracker.record(now);

        // 3. Check per-child limit
        if child.tracker.exceeded() {
            return RestartDecision::ChildLimitExceeded {
                restarts: child.tracker.total_restarts,
            };
        }

        // 4. Record in global intensity tracker
        self.intensity.record(now);

        // 5. Check supervisor-wide intensity
        if self.intensity.exceeded() {
            return RestartDecision::IntensityExceeded {
                total_restarts: self.intensity.total_restarts,
            };
        }

        // 6. Apply strategy
        match self.strategy {
            Strategy::OneForOne => {
                let delay = child.tracker.backoff_delay();
                RestartDecision::RestartAfter { delay }
            }
            Strategy::OneForAll => RestartDecision::RestartAll,
        }
    }

    /// For OneForAll: records restart for all children except Temporary.
    /// Call AFTER canceling all children, BEFORE respawning.
    /// Returns true if restart should proceed, false if intensity exceeded.
    pub(crate) fn record_all(&mut self, now: Instant) -> bool {
        for child in self.children.values_mut() {
            if !matches!(child.policy, ChildRestart::Temporary) {
                child.tracker.record(now);
            }
        }
        // Record one global restart for the OneForAll cascade
        self.intensity.record(now);
        !self.intensity.exceeded()
    }

    /// Returns the total restarts tracked by the intensity tracker.
    pub(crate) fn intensity_total_restarts(&self) -> u64 {
        self.intensity.total_restarts
    }

    /// Returns the total restart count for a child.
    pub(crate) fn child_restart_count(&self, name: &str) -> u64 {
        self.children
            .get(name)
            .map(|c| c.tracker.total_restarts)
            .unwrap_or(0)
    }

    /// Returns current backoff delay for a child (useful for diagnostics).
    pub(crate) fn backoff_delay(&self, name: &str) -> Duration {
        self.children
            .get(name)
            .map(|c| c.tracker.backoff_delay())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::config::RestartIntensity;

    fn test_config(strategy: Strategy) -> SupervisorConfig {
        SupervisorConfig::builder()
            .strategy(strategy)
            .intensity(RestartIntensity {
                max_restarts: 3,
                window: Duration::from_secs(60),
            })
            .build()
    }

    fn test_restart_policy() -> RestartPolicy {
        RestartPolicy {
            max_restarts: 2,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(100),
            backoff_max: Duration::from_secs(1),
        }
    }

    #[test]
    fn decide_no_restart_for_temporary() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("temp", ChildRestart::Temporary, &test_restart_policy());

        let decision = mgr.decide(
            "temp",
            &ChildExit::Failed("err".into()),
            false,
            Instant::now(),
        );
        assert!(matches!(decision, RestartDecision::NoRestart));
    }

    #[test]
    fn decide_no_restart_for_transient_normal_exit() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("trans", ChildRestart::Transient, &test_restart_policy());

        let decision = mgr.decide("trans", &ChildExit::Normal, false, Instant::now());
        assert!(matches!(decision, RestartDecision::NoRestart));
    }

    #[test]
    fn decide_restart_for_transient_failure() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("trans", ChildRestart::Transient, &test_restart_policy());

        let decision = mgr.decide(
            "trans",
            &ChildExit::Failed("err".into()),
            false,
            Instant::now(),
        );
        assert!(matches!(decision, RestartDecision::RestartAfter { .. }));
    }

    #[test]
    fn decide_restart_for_permanent_normal_exit() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("perm", ChildRestart::Permanent, &test_restart_policy());

        let decision = mgr.decide("perm", &ChildExit::Normal, false, Instant::now());
        assert!(matches!(decision, RestartDecision::RestartAfter { .. }));
    }

    #[test]
    fn decide_hung_child_treated_as_failure() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("trans", ChildRestart::Transient, &test_restart_policy());

        // Shutdown exit is normally not a failure, but hung=true overrides
        let decision = mgr.decide("trans", &ChildExit::Shutdown, true, Instant::now());
        assert!(matches!(decision, RestartDecision::RestartAfter { .. }));
    }

    #[test]
    fn decide_child_limit_exceeded() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        let policy = RestartPolicy {
            max_restarts: 1,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(10),
            backoff_max: Duration::from_millis(100),
        };
        mgr.register("child", ChildRestart::Permanent, &policy);

        let now = Instant::now();
        // First restart
        let d1 = mgr.decide("child", &ChildExit::Failed("".into()), false, now);
        assert!(matches!(d1, RestartDecision::RestartAfter { .. }));

        // Second restart - should exceed
        let d2 = mgr.decide(
            "child",
            &ChildExit::Failed("".into()),
            false,
            now + Duration::from_millis(1),
        );
        assert!(matches!(d2, RestartDecision::ChildLimitExceeded { .. }));
    }

    #[test]
    fn decide_intensity_exceeded() {
        let config = SupervisorConfig::builder()
            .strategy(Strategy::OneForOne)
            .intensity(RestartIntensity {
                max_restarts: 1,
                window: Duration::from_secs(60),
            })
            .build();
        let mut mgr = RestartManager::new(&config);
        let policy = RestartPolicy {
            max_restarts: 10,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(10),
            backoff_max: Duration::from_millis(100),
        };
        mgr.register("a", ChildRestart::Permanent, &policy);
        mgr.register("b", ChildRestart::Permanent, &policy);

        let now = Instant::now();
        let d1 = mgr.decide("a", &ChildExit::Failed("".into()), false, now);
        assert!(matches!(d1, RestartDecision::RestartAfter { .. }));

        let d2 = mgr.decide(
            "b",
            &ChildExit::Failed("".into()),
            false,
            now + Duration::from_millis(1),
        );
        assert!(matches!(d2, RestartDecision::IntensityExceeded { .. }));
    }

    #[test]
    fn decide_one_for_all_returns_restart_all() {
        let config = test_config(Strategy::OneForAll);
        let mut mgr = RestartManager::new(&config);
        mgr.register("child", ChildRestart::Permanent, &test_restart_policy());

        let decision = mgr.decide(
            "child",
            &ChildExit::Failed("".into()),
            false,
            Instant::now(),
        );
        assert!(matches!(decision, RestartDecision::RestartAll));
    }

    #[test]
    fn decide_unknown_child_returns_no_restart() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);

        let decision = mgr.decide(
            "unknown",
            &ChildExit::Failed("".into()),
            false,
            Instant::now(),
        );
        assert!(matches!(decision, RestartDecision::NoRestart));
    }

    #[test]
    fn backoff_delay_increases_exponentially() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        let policy = RestartPolicy {
            max_restarts: 10,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(100),
            backoff_max: Duration::from_secs(10),
        };
        mgr.register("child", ChildRestart::Permanent, &policy);

        let now = Instant::now();

        if let RestartDecision::RestartAfter { delay } =
            mgr.decide("child", &ChildExit::Failed("".into()), false, now)
        {
            assert_eq!(delay, Duration::from_millis(100));
        } else {
            panic!("expected RestartAfter");
        }

        if let RestartDecision::RestartAfter { delay } = mgr.decide(
            "child",
            &ChildExit::Failed("".into()),
            false,
            now + Duration::from_millis(1),
        ) {
            assert_eq!(delay, Duration::from_millis(200));
        } else {
            panic!("expected RestartAfter");
        }

        if let RestartDecision::RestartAfter { delay } = mgr.decide(
            "child",
            &ChildExit::Failed("".into()),
            false,
            now + Duration::from_millis(2),
        ) {
            assert_eq!(delay, Duration::from_millis(400));
        } else {
            panic!("expected RestartAfter");
        }
    }

    #[test]
    fn record_all_tracks_intensity_and_returns_false_when_exceeded() {
        // Supervisor-wide intensity: max 2 restarts in 60s
        let config = SupervisorConfig::builder()
            .strategy(Strategy::OneForAll)
            .intensity(RestartIntensity {
                max_restarts: 2,
                window: Duration::from_secs(60),
            })
            .build();
        let mut mgr = RestartManager::new(&config);
        let policy = test_restart_policy();

        mgr.register("a", ChildRestart::Permanent, &policy);
        mgr.register("b", ChildRestart::Permanent, &policy);
        mgr.register("temp", ChildRestart::Temporary, &policy);

        let now = Instant::now();

        // First cascade — should succeed
        assert!(mgr.record_all(now));
        assert_eq!(mgr.intensity_total_restarts(), 1);

        // Second cascade — should succeed
        assert!(mgr.record_all(now + Duration::from_millis(1)));
        assert_eq!(mgr.intensity_total_restarts(), 2);

        // Third cascade — should fail (exceeded)
        assert!(!mgr.record_all(now + Duration::from_millis(2)));
        assert_eq!(mgr.intensity_total_restarts(), 3);
    }

    #[test]
    fn decide_no_restart_for_budget_exceeded_permanent() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("perm", ChildRestart::Permanent, &test_restart_policy());

        let decision = mgr.decide("perm", &ChildExit::BudgetExceeded, false, Instant::now());
        assert!(matches!(decision, RestartDecision::NoRestart));
    }

    #[test]
    fn decide_no_restart_for_budget_exceeded_transient() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("trans", ChildRestart::Transient, &test_restart_policy());

        let decision = mgr.decide("trans", &ChildExit::BudgetExceeded, false, Instant::now());
        assert!(matches!(decision, RestartDecision::NoRestart));
    }

    #[test]
    fn decide_no_restart_for_budget_exceeded_temporary() {
        let config = test_config(Strategy::OneForOne);
        let mut mgr = RestartManager::new(&config);
        mgr.register("temp", ChildRestart::Temporary, &test_restart_policy());

        let decision = mgr.decide("temp", &ChildExit::BudgetExceeded, false, Instant::now());
        assert!(matches!(decision, RestartDecision::NoRestart));
    }

    #[test]
    fn decide_budget_exceeded_does_not_increment_counters() {
        let config = SupervisorConfig::builder()
            .strategy(Strategy::OneForOne)
            .intensity(RestartIntensity {
                max_restarts: 1,
                window: Duration::from_secs(60),
            })
            .build();
        let mut mgr = RestartManager::new(&config);
        let policy = RestartPolicy {
            max_restarts: 1,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(10),
            backoff_max: Duration::from_millis(100),
        };
        mgr.register("child", ChildRestart::Permanent, &policy);

        let now = Instant::now();

        // BudgetExceeded should NOT consume restart quota
        mgr.decide("child", &ChildExit::BudgetExceeded, false, now);
        mgr.decide(
            "child",
            &ChildExit::BudgetExceeded,
            false,
            now + Duration::from_millis(1),
        );

        // A real failure should still be allowed (limit is 1)
        let decision = mgr.decide(
            "child",
            &ChildExit::Failed("err".into()),
            false,
            now + Duration::from_millis(2),
        );
        assert!(
            matches!(decision, RestartDecision::RestartAfter { .. }),
            "budget exits should not have consumed the restart quota"
        );

        // Now a second real failure should exceed the limit
        let decision2 = mgr.decide(
            "child",
            &ChildExit::Failed("err".into()),
            false,
            now + Duration::from_millis(3),
        );
        assert!(matches!(
            decision2,
            RestartDecision::ChildLimitExceeded { .. }
        ));
    }

    #[test]
    fn record_all_does_not_count_temporary_children() {
        let config = test_config(Strategy::OneForAll);
        let mut mgr = RestartManager::new(&config);
        let policy = test_restart_policy();

        mgr.register("perm", ChildRestart::Permanent, &policy);
        mgr.register("temp", ChildRestart::Temporary, &policy);

        let now = Instant::now();
        let base_delay = mgr.backoff_delay("perm"); // base delay before any restarts

        // Two restarts to see backoff increase (first restart = 2^0 = base, second = 2^1 = 2*base)
        mgr.record_all(now);
        mgr.record_all(now + Duration::from_millis(1));

        // Permanent child's tracker was updated — backoff doubled (2 restarts = 2^1 * base)
        let delay_perm = mgr.backoff_delay("perm");
        assert_eq!(
            delay_perm,
            base_delay * 2,
            "Permanent child backoff should double"
        );

        // Temporary child's tracker was NOT updated — backoff stays at base
        let delay_temp = mgr.backoff_delay("temp");
        assert_eq!(
            delay_temp, base_delay,
            "Temporary child backoff should not change"
        );
    }
}
