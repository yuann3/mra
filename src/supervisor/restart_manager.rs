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
}
