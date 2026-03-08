//! Strongly-typed identifiers for agents and tasks.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for an agent within a swarm.
///
/// Wraps a v4 UUID. `Clone`, `Copy`, `Hash`, and serializable.
/// `Display` outputs the raw UUID string for use in logs and APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(Uuid);

impl AgentId {
    /// Generates a new random agent identifier.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a task submitted to an agent.
///
/// Wraps a v4 UUID. Used to correlate requests with replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskId(Uuid);

impl TaskId {
    /// Generates a new random task identifier.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
