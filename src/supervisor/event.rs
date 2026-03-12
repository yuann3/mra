use std::time::Duration;

use super::ChildExit;

#[derive(Debug, Clone)]
pub enum SupervisorEvent {
    SupervisorStarted,
    SupervisorStopping,
    ChildStarted { name: String, generation: u64 },
    ChildExited { name: String, generation: u64, exit: ChildExit },
    ChildRestarted { name: String, old_gen: u64, new_gen: u64, delay: Duration },
    HangDetected { name: String, generation: u64, elapsed: Duration },
    ChildRestartLimitExceeded { name: String, restarts: u64 },
    RestartIntensityExceeded { total_restarts: u64 },
}
