//! Supervisor system — Erlang/OTP-style agent lifecycle management.

pub(crate) mod child;
mod config;
mod event;
mod handle;
pub(crate) mod lifecycle;
pub(crate) mod restart_manager;
pub(crate) mod runner;
pub(crate) mod tracker;

pub use child::{ChildContext, ChildFactory, ChildSpec, SpawnedChild};
pub use config::{
    ChildRestart, RestartIntensity, ShutdownPolicy, Strategy, SupervisorConfig,
    SupervisorConfigBuilder,
};
pub use event::SupervisorEvent;
pub use handle::SupervisorHandle;

/// Point-in-time snapshot of a supervised child's status.
#[derive(Debug, Clone)]
pub struct ChildStatus {
    /// Human-readable name of the child.
    pub name: String,
    /// Whether the child is currently running.
    pub alive: bool,
    /// Current generation (0 = first start, incremented on each restart).
    pub generation: u64,
    /// Total number of restarts recorded for this child.
    pub restart_count: u64,
}

/// Why a child agent exited.
#[derive(Debug, Clone)]
pub enum ChildExit {
    /// Clean return from the agent's run loop (channel closed, no senders left).
    Normal,
    /// Explicit shutdown via cancellation token or shutdown message.
    Shutdown,
    /// The agent's behavior handler returned an error.
    Failed(String),
    /// The agent's token/cost budget was exceeded. Terminal — restarting is futile.
    BudgetExceeded,
}

impl ChildExit {
    /// Returns `true` if this exit should trigger a restart
    /// (for `Transient` or `Permanent` restart policies).
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}
