//! CLI trigger — dispatches a single task to a named agent from argv.
//!
//! Invocation: `<binary> <agent-name> <prompt>`
//!
//! Sessions are in-memory and lost when the process exits. Each CLI
//! invocation is one-shot: no conversation continuity across runs.

use std::sync::Arc;

use crate::session::{MemorySessionStore, SessionStore};

use super::{Runtime, RuntimeError};

/// Handles the CLI dispatch path for `Runtime::run()`.
///
/// Dispatches `prompt` to `agent_name`, prints the result to stdout, and shuts
/// down the runtime. Returns an error if the agent name is unknown or the
/// task fails.
pub(crate) async fn run_cli(
    runtime: Runtime,
    agent_name: String,
    prompt: String,
) -> Result<(), RuntimeError> {
    if prompt.is_empty() {
        eprintln!("usage: <binary> {} <prompt>", agent_name);
        runtime.shutdown().await;
        std::process::exit(1);
    }

    let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());

    let reply = runtime.dispatch(&agent_name, &prompt, None, None, Arc::clone(&store)).await;

    runtime.shutdown().await;

    match reply {
        Ok(r) => {
            println!("{}", r.output);
            Ok(())
        }
        Err(RuntimeError::UnknownAgent(name)) => {
            eprintln!("error: no agent named '{name}'");
            std::process::exit(1);
        }
        Err(e) => Err(e),
    }
}
