use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::error::AgentError;
use crate::ids::TaskId;

/// A unit of work submitted to an agent.
///
/// Contains a unique [`TaskId`] for correlation, a human-readable
/// instruction, and an optional JSON context blob for structured input.
#[derive(Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: TaskId,
    pub instruction: String,
    pub context: serde_json::Value,
}

impl Task {
    /// Creates a new task with a generated [`TaskId`] and null context.
    pub fn new(instruction: impl Into<String>) -> Self {
        Self {
            id: TaskId::new(),
            instruction: instruction.into(),
            context: serde_json::Value::Null,
        }
    }
}

/// The result produced by an agent after processing a [`Task`].
#[derive(Debug, Serialize, Deserialize)]
pub struct AgentReply {
    /// The id of the [`Task`] this reply corresponds to.
    pub task_id: TaskId,
    /// The agent's output text.
    pub output: String,
    /// Number of LLM tokens consumed while processing this task.
    pub tokens_used: u64,
}

/// Internal message envelope sent through the agent's bounded `mpsc` channel.
pub(crate) enum AgentMessage {
    Execute {
        task: Task,
        respond_to: oneshot::Sender<Result<AgentReply, AgentError>>,
    },
    /// Initiate graceful shutdown. The runner closes the receiver, drains
    /// buffered work until `deadline`, then fails any remaining callers
    /// with [`AgentError::Cancelled`].
    Shutdown {
        deadline: tokio::time::Instant,
    },
}
