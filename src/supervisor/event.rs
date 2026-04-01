//! Supervisor lifecycle events broadcast to subscribers.
//!
//! Subscribe via [`SupervisorHandle::subscribe`](super::SupervisorHandle::subscribe)
//! to observe starts, stops, restarts, hangs, and limit breaches.

use std::time::Duration;

use super::ChildExit;

/// Lifecycle events emitted by the supervisor via a `broadcast` channel.
///
/// Subscribe with [`SupervisorHandle::subscribe`](super::SupervisorHandle::subscribe)
/// to observe starts, stops, restarts, and hang detections.
#[derive(Debug, Clone)]
pub enum SupervisorEvent {
    /// The supervisor's event loop has started.
    SupervisorStarted,
    /// The supervisor is draining children before shutting down.
    SupervisorStopping,
    /// A child was spawned (or respawned) successfully.
    ChildStarted {
        /// Child name.
        name: String,
        /// Restart generation (0 = first start).
        generation: u64,
    },
    /// A child's run future completed.
    ChildExited {
        /// Child name.
        name: String,
        /// Generation at time of exit.
        generation: u64,
        /// Why the child exited.
        exit: ChildExit,
    },
    /// A child was restarted after a failure, with the backoff `delay` applied.
    ChildRestarted {
        /// Child name.
        name: String,
        /// Generation before restart.
        old_gen: u64,
        /// Generation after restart.
        new_gen: u64,
        /// Backoff delay that was applied before restarting.
        delay: Duration,
    },
    /// A child has been busy without reporting progress longer than its timeout.
    HangDetected {
        /// Child name.
        name: String,
        /// Generation of the hung child.
        generation: u64,
        /// Time since last progress report.
        elapsed: Duration,
    },
    /// A single child hit its per-child restart limit and will not be restarted again.
    ChildRestartLimitExceeded {
        /// Child name.
        name: String,
        /// Total number of restarts before the limit was hit.
        restarts: u64,
    },
    /// Supervisor-wide restart intensity exceeded — the supervisor shuts down.
    RestartIntensityExceeded {
        /// Total restarts across all children.
        total_restarts: u64,
    },
    /// A token budget was exceeded (reserved for future event-emitter wiring).
    BudgetExceeded {
        /// Agent name that exceeded the budget.
        name: String,
        /// Tokens used.
        used: u64,
        /// Token limit.
        limit: u64,
    },
}
