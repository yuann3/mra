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
//! Use [`AgentSpawn`] to create both halves and get back a
//! [`SpawnedAgent`] containing the handle, a progress watcher, and the
//! task's `JoinHandle`.
//!
//! ```ignore
//! let spawned = AgentSpawn::new("my-agent", MyBehavior).spawn();
//! ```

mod ctx;
mod handle;
pub(crate) mod mailbox;
mod message;
pub(crate) mod runner;
mod spawn;

use std::future::Future;
use std::pin::Pin;

use crate::error::AgentError;

pub use ctx::{AgentCtx, ToolLoopResult};
pub use handle::AgentHandle;
pub(crate) use message::AgentMessage;
pub use message::{AgentReply, Task};
pub use runner::{ProgressState, SpawnedAgent};
pub use spawn::AgentSpawn;

/// Defines how an agent processes incoming [`Task`]s.
///
/// Implement this trait to create custom agent behaviors. The runner
/// calls [`handle`](Self::handle) for each [`Task`] received through
/// the agent's mailbox.
///
/// Uses native `async fn` in traits (RPITIT, Rust 1.75+). The runner
/// dispatches via a blanket-impl'd object-safe wrapper so that a
/// single event loop serves all spawn paths.
///
/// # Examples
///
/// ```
/// use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
/// use mra::error::AgentError;
///
/// struct Echo;
///
/// impl AgentBehavior for Echo {
///     async fn handle(
///         &mut self,
///         _ctx: &mut AgentCtx,
///         input: Task,
///     ) -> Result<AgentReply, AgentError> {
///         Ok(AgentReply {
///             task_id: input.id,
///             output: input.instruction,
///             self_tokens: 0,
///             total_tokens: 0,
///         })
///     }
/// }
/// ```
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

// ── Type-erased behavior for Runtime storage ──────────────────────────────────

/// Object-safe version of [`AgentBehavior`] using boxed futures.
///
/// Implemented automatically for all `AgentBehavior` types via a blanket impl.
/// Provides a dyn-safe dispatch path so behaviors of different concrete types
/// can be stored and invoked uniformly (e.g. in `AgentEntry` / `AgentRunner`).
pub(crate) trait DynAgentBehavior: Send + 'static {
    fn handle_dyn<'a>(
        &'a mut self,
        ctx: &'a mut AgentCtx,
        input: Task,
    ) -> Pin<Box<dyn Future<Output = Result<AgentReply, AgentError>> + Send + 'a>>;
}

impl<B: AgentBehavior> DynAgentBehavior for B {
    fn handle_dyn<'a>(
        &'a mut self,
        ctx: &'a mut AgentCtx,
        input: Task,
    ) -> Pin<Box<dyn Future<Output = Result<AgentReply, AgentError>> + Send + 'a>> {
        Box::pin(self.handle(ctx, input))
    }
}
