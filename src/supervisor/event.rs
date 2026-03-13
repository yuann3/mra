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
    ChildStarted { name: String, generation: u64 },
    /// A child's run future completed.
    ChildExited {
        name: String,
        generation: u64,
        exit: ChildExit,
    },
    /// A child was restarted after a failure, with the backoff `delay` applied.
    ChildRestarted {
        name: String,
        old_gen: u64,
        new_gen: u64,
        delay: Duration,
    },
    /// A child has been busy without reporting progress longer than its timeout.
    HangDetected {
        name: String,
        generation: u64,
        elapsed: Duration,
    },
    /// A single child hit its per-child restart limit and will not be restarted again.
    ChildRestartLimitExceeded { name: String, restarts: u64 },
    /// Supervisor-wide restart intensity exceeded — the supervisor shuts down.
    RestartIntensityExceeded { total_restarts: u64 },
    /// A token budget was exceeded (reserved for future event-emitter wiring).
    BudgetExceeded { name: String, used: u64, limit: u64 },
}
