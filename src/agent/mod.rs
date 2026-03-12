//! Actor-based agent system.
//!
//! Each agent is split into two halves following the
//! [Tokio actor pattern](https://ryhl.io/blog/actors-with-tokio/):
//!
//! - [`AgentHandle`] — the cloneable, `Send + Sync` external API that
//!   communicates with the agent through a bounded `mpsc` channel.
//! - An internal runner (not public) that owns mutable state, receives
//!   messages, and calls [`AgentBehavior::handle`].
//!
//! Use [`AgentHandle::spawn`] to create both halves and get back a
//! [`SpawnedAgent`] containing the handle, a progress watcher, and the
//! task's `JoinHandle`.

mod ctx;
mod handle;
pub(crate) mod mailbox;
mod message;
mod runner;

use crate::error::AgentError;

pub use ctx::AgentCtx;
pub use handle::AgentHandle;
pub(crate) use message::AgentMessage;
pub use message::{AgentReply, Task};
pub use runner::{ProgressState, SpawnedAgent};

/// Defines how an agent processes incoming [`Task`]s.
///
/// Implement this trait to create custom agent behaviors. The runner
/// calls [`handle`](Self::handle) for each [`Task`] received through
/// the agent's mailbox.
///
/// Uses native `async fn` in traits (RPITIT, Rust 1.75+). The runner
/// is generic over `B: AgentBehavior` rather than using `dyn` dispatch,
/// so there is no per-call heap allocation.
pub trait AgentBehavior: Send + 'static {
    /// Processes a single task and returns a reply.
    ///
    /// Called sequentially — the runner will not invoke `handle` again
    /// until the previous call completes. Cancellation is observed via
    /// `tokio::select!` around this future; cooperative cancellation
    /// points (e.g. in LLM or tool calls) will respond promptly.
    fn handle(
        &mut self,
        ctx: &mut AgentCtx,
        input: Task,
    ) -> impl Future<Output = Result<AgentReply, AgentError>> + Send;
}
