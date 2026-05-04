//! Message types for the agent mailbox.
//!
//! [`Task`] is inbound work submitted via [`AgentHandle::execute`](super::AgentHandle::execute).
//! [`AgentReply`] is the outbound result returned through a `oneshot` channel.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::error::AgentError;
use crate::ids::TaskId;
use crate::session::{Message, SessionStore};

/// A unit of work submitted to an agent.
///
/// Contains a unique [`TaskId`] for correlation, a human-readable
/// instruction, and an optional JSON context blob for structured input.
///
/// The `session_id`, `history`, and `session_store` fields are populated by
/// [`Runtime`](crate::runtime::Runtime) before dispatch. User code creates
/// tasks with [`Task::new`] and does not interact with these fields directly.
#[derive(Serialize, Deserialize)]
pub struct Task {
    /// Unique identifier for correlation with [`AgentReply::task_id`].
    pub id: TaskId,
    /// Human-readable instruction describing the work.
    pub instruction: String,
    /// Optional structured input (default: `Value::Null`).
    pub context: serde_json::Value,
    /// Session ID injected by `Runtime`. `None` for stateless one-shot calls.
    #[serde(skip)]
    pub(crate) session_id: Option<String>,
    /// Conversation history loaded from the session store before dispatch.
    #[serde(skip)]
    pub(crate) history: Vec<Message>,
    /// Session store reference for saving history after each `ctx.chat()` call.
    #[serde(skip)]
    pub(crate) session_store: Option<Arc<dyn SessionStore>>,
    /// Optional role name to apply for this task. Validated at the HTTP layer.
    #[serde(skip)]
    pub(crate) role: Option<String>,
}

impl std::fmt::Debug for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Task")
            .field("id", &self.id)
            .field("instruction", &self.instruction)
            .field("context", &self.context)
            .field("session_id", &self.session_id)
            .field("history_len", &self.history.len())
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl Task {
    /// Creates a new task with a generated [`TaskId`] and null context.
    pub fn new(instruction: impl Into<String>) -> Self {
        Self {
            id: TaskId::new(),
            instruction: instruction.into(),
            context: serde_json::Value::Null,
            session_id: None,
            history: Vec::new(),
            session_store: None,
            role: None,
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
    /// This agent's direct LLM token usage (not including nested agents).
    pub self_tokens: u64,
    /// End-to-end pipeline total (self + nested agents). Telemetry only.
    pub total_tokens: u64,
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
    Shutdown { deadline: tokio::time::Instant },
}
