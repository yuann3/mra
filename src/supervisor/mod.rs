//! Supervisor system — Erlang/OTP-style agent lifecycle management.

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
