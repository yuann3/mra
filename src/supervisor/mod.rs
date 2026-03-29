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
pub use config::{ChildRestart, RestartIntensity, ShutdownPolicy, Strategy, SupervisorConfig};
pub use event::SupervisorEvent;
pub use handle::SupervisorHandle;

/// Why a child agent exited.
#[derive(Debug, Clone)]
pub enum ChildExit {
    /// Clean return from the agent's run loop (channel closed, no senders left).
    Normal,
    /// Explicit shutdown via cancellation token or shutdown message.
    Shutdown,
    /// The agent's behavior handler returned an error.
    Failed(String),
}

impl ChildExit {
    /// Returns `true` if this exit should trigger a restart
    /// (for `Transient` or `Permanent` restart policies).
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}
