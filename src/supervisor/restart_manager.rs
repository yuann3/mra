use std::collections::HashMap;
use std::time::Duration;

use tokio::time::Instant;

use super::config::{ChildRestart, Strategy, SupervisorConfig};
use super::tracker::{IntensityTracker, RestartTracker};
use super::ChildExit;
use crate::config::RestartPolicy;

/// Decision returned by RestartManager — tells supervisor what to do next.
#[derive(Debug, Clone)]
pub enum RestartDecision {
    /// Restart this child after the specified delay.
    RestartAfter { delay: Duration },
    /// Restart all children in order (OneForAll cascade).
    RestartAll,
    /// Don't restart — policy says no (Temporary, or Transient+Normal exit).
    NoRestart,
    /// Don't restart — child exceeded its per-child restart limit.
    ChildLimitExceeded { restarts: u64 },
    /// Don't restart — supervisor-wide intensity limit exceeded (fatal).
    IntensityExceeded { total_restarts: u64 },
}

struct ChildRestartState {
    policy: ChildRestart,
    restart_policy: RestartPolicy,
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
    pub(crate) fn register(&mut self, name: &str, restart: ChildRestart, restart_policy: &RestartPolicy) {
        self.children.insert(
            name.to_owned(),
            ChildRestartState {
                policy: restart,
                restart_policy: restart_policy.clone(),
                tracker: RestartTracker::new(restart_policy),
            },
        );
    }

    /// Removes a child from tracking (on explicit stop or permanent removal).
    pub(crate) fn unregister(&mut self, name: &str) {
        self.children.remove(name);
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
}
