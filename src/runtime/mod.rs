//! Agent runtime — the top-level entry point for building and running agents.
//!
//! [`Runtime`] replaces `SwarmRuntime` as the primary public API.
//! Use [`Runtime::builder()`] to configure agents, select models, wire up a
//! session store, and start the supervisor. Call [`Runtime::run()`] to
//! dispatch based on command-line arguments.
//!
//! # Quick start
//!
//! ```no_run
//! use mra::runtime::{Runtime, AgentEntry};
//! use mra::llm::OpenRouterClient;
//! // (implement AgentBehavior for YourAgent)
//! # use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
//! # use mra::error::AgentError;
//! # struct YourAgent;
//! # impl AgentBehavior for YourAgent {
//! #     async fn handle(&mut self, _ctx: &mut AgentCtx, input: Task)
//! #         -> Result<AgentReply, AgentError> {
//! #         Ok(AgentReply { task_id: input.id, output: input.instruction,
//! #             self_tokens: 0, total_tokens: 0 })
//! #     }
//! # }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     Runtime::builder()
//!         .agent(AgentEntry::new("my-agent", YourAgent))
//!         .model("anthropic/claude-sonnet-4-6")
//!         .llm(OpenRouterClient::builder()
//!             .api_key("your-key")
//!             .build())
//!         .build()
//!         .await?
//!         .run()
//!         .await?;
//!     Ok(())
//! }
//! ```

mod cli;

use std::sync::Arc;

use thiserror::Error;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::agent::runner::ErasedAgentInit;
use crate::agent::{AgentBehavior, AgentHandle, DynAgentBehavior, Task};
use crate::budget::{AgentUsage, BudgetTracker, RunUsage};
use crate::config::AgentConfig;
use crate::error::SupervisorError;
use crate::llm::LlmProvider;
use crate::session::{MemorySessionStore, SessionStore};
use crate::supervisor::{
    ChildContext, ChildRestart, ChildSpec, ChildStatus, SpawnedChild, SupervisorConfig,
    SupervisorEvent, SupervisorHandle,
};

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors from the [`Runtime`].
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// Supervisor failed to start or a child could not be spawned.
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    /// Bad invocation — printed to stderr with usage hint.
    #[error("{0}")]
    Usage(String),
    /// The named agent does not exist in this runtime.
    #[error("unknown agent: {0}")]
    UnknownAgent(String),
    /// An agent returned an error.
    #[error(transparent)]
    Agent(#[from] crate::error::AgentError),
}

// ── AgentEntry ────────────────────────────────────────────────────────────────

/// A named agent registration used with [`RuntimeBuilder::agent()`].
///
/// Wraps a concrete behavior with an optional per-agent model override.
///
/// # Examples
///
/// ```no_run
/// use mra::runtime::AgentEntry;
/// # use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
/// # use mra::error::AgentError;
/// # struct Researcher;
/// # impl AgentBehavior for Researcher {
/// #     async fn handle(&mut self, _ctx: &mut AgentCtx, input: Task)
/// #         -> Result<AgentReply, AgentError> {
/// #         Ok(AgentReply { task_id: input.id, output: input.instruction,
/// #             self_tokens: 0, total_tokens: 0 })
/// #     }
/// # }
///
/// let entry = AgentEntry::new("researcher", Researcher)
///     .model("anthropic/claude-opus-4-6");
/// ```
pub struct AgentEntry {
    /// Human-readable name. Must be unique within a runtime.
    pub name: String,
    /// Type-erased behavior, consumed on first spawn.
    pub(crate) behavior: Box<dyn DynAgentBehavior>,
    /// Optional per-agent model override.
    pub model: Option<String>,
}

impl AgentEntry {
    /// Creates an entry from a concrete behavior.
    ///
    /// The behavior is consumed on first spawn. Agents registered via
    /// `AgentEntry` use `ChildRestart::Temporary` and do not restart.
    pub fn new(name: &str, behavior: impl AgentBehavior) -> Self {
        Self {
            name: name.to_string(),
            behavior: Box::new(behavior),
            model: None,
        }
    }

    /// Sets the per-agent model ID, overriding the global default.
    ///
    /// Use OpenRouter-style `provider/model-name` strings.
    pub fn model(mut self, model_id: &str) -> Self {
        self.model = Some(model_id.to_string());
        self
    }
}

// ── RuntimeBuilder ────────────────────────────────────────────────────────────

/// Builder for [`Runtime`].
///
/// Collect agents, set the global model, wire up the LLM provider,
/// optionally configure a session store and budget, then call [`build()`](Self::build).
pub struct RuntimeBuilder {
    agents: Vec<AgentEntry>,
    global_model: Option<String>,
    llm: Option<Arc<dyn LlmProvider>>,
    session_store: Option<Arc<dyn SessionStore>>,
    budget: Option<Arc<BudgetTracker>>,
    port: u16,
}

impl RuntimeBuilder {
    fn new() -> Self {
        Self {
            agents: Vec::new(),
            global_model: None,
            llm: None,
            session_store: None,
            budget: None,
            port: 3000,
        }
    }

    /// Registers an agent with the runtime.
    pub fn agent(mut self, entry: AgentEntry) -> Self {
        self.agents.push(entry);
        self
    }

    /// Sets the global default model ID for agents that don't specify their own.
    pub fn model(mut self, model_id: impl Into<String>) -> Self {
        self.global_model = Some(model_id.into());
        self
    }

    /// Sets the shared LLM provider used by all agents.
    pub fn llm(mut self, llm: impl LlmProvider) -> Self {
        self.llm = Some(Arc::new(llm));
        self
    }

    /// Sets the session store. Defaults to [`MemorySessionStore`] for CLI mode.
    pub fn session_store(mut self, store: impl SessionStore) -> Self {
        self.session_store = Some(Arc::new(store));
        self
    }

    /// Configures a global token budget.
    pub fn budget(mut self, budget: BudgetTracker) -> Self {
        self.budget = Some(Arc::new(budget));
        self
    }

    /// Sets the HTTP server port (default: 3000). Only used with `serve` mode.
    pub fn port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    /// Builds the [`Runtime`], starting the supervisor and spawning all agents.
    pub async fn build(self) -> Result<Runtime, RuntimeError> {
        let llm: Option<Arc<dyn LlmProvider>> = self.llm;
        let budget = self.budget;

        let mut sup_builder = SupervisorConfig::builder();
        if let Some(ref l) = llm {
            sup_builder = sup_builder.llm(Arc::clone(l));
        }
        if let Some(ref b) = budget {
            sup_builder = sup_builder.budget(Arc::clone(b));
        }
        let sup_config = sup_builder.build();

        let (supervisor, join) =
            SupervisorHandle::start_with_budget(sup_config, budget.clone());

        let mut agent_names: Vec<String> = self.agents.iter().map(|e| e.name.clone()).collect();
        // Track registered agent names for validation
        agent_names.dedup();

        // Spawn each agent under the supervisor with ChildRestart::Temporary.
        // Behaviors are consumed on first spawn; restarts are not supported.
        for entry in self.agents {
            let effective_model = entry.model.or_else(|| self.global_model.clone());
            let cfg = AgentConfig::new(&entry.name);
            let name = entry.name.clone();

            // Wrap behavior in a Mutex<Option> so the factory can take it once.
            let behavior_slot: Arc<tokio::sync::Mutex<Option<Box<dyn DynAgentBehavior>>>> =
                Arc::new(tokio::sync::Mutex::new(Some(entry.behavior)));

            let cfg_clone = cfg.clone();
            let model_clone = effective_model.clone();
            let llm_clone = llm.clone();

            let factory: crate::supervisor::ChildFactory = Arc::new(move |ctx: ChildContext| {
                let slot = Arc::clone(&behavior_slot);
                let cfg = cfg_clone.clone();
                let model = model_clone.clone();
                let effective_llm = llm_clone.clone().or(ctx.llm);
                Box::pin(async move {
                    let behavior = slot.lock().await.take().ok_or_else(|| {
                        SupervisorError::SpawnFailed(
                            "AgentEntry behavior already consumed (restart not supported)".into(),
                        )
                    })?;

                    let child = AgentHandle::spawn_child_erased(ErasedAgentInit {
                        id: ctx.id,
                        config: cfg,
                        behavior,
                        peers: ctx.peers,
                        llm: effective_llm,
                        cancel: ctx.cancel,
                        budget: ctx.budget,
                        tools: ctx.tools,
                        model,
                    });
                    Ok(child)
                }) as std::pin::Pin<
                    Box<
                        dyn std::future::Future<
                                Output = Result<SpawnedChild, SupervisorError>,
                            > + Send,
                    >,
                >
            });

            let spec = ChildSpec::new(&name, cfg, factory)
                .with_restart(ChildRestart::Temporary);

            supervisor.start_child(spec).await?;
        }

        Ok(Runtime {
            supervisor,
            join,
            session_store: self.session_store,
            budget,
            port: self.port,
        })
    }
}

// ── Runtime ───────────────────────────────────────────────────────────────────

/// The top-level agent runtime.
///
/// Wraps a supervisor and a session store. Created via [`Runtime::builder()`]
/// and started via [`Runtime::run()`].
pub struct Runtime {
    supervisor: SupervisorHandle,
    join: JoinHandle<Result<(), crate::error::SupervisorError>>,
    session_store: Option<Arc<dyn SessionStore>>,
    budget: Option<Arc<BudgetTracker>>,
    port: u16,
}

impl Runtime {
    /// Returns a [`RuntimeBuilder`] for constructing a new runtime.
    pub fn builder() -> RuntimeBuilder {
        RuntimeBuilder::new()
    }

    /// Dispatches based on command-line arguments.
    ///
    /// | `argv[1]`              | Mode                          |
    /// |------------------------|-------------------------------|
    /// | `serve`                | HTTP server on default port   |
    /// | `serve --port N`       | HTTP server on port N         |
    /// | `<agent-name> <prompt>`| CLI one-shot                  |
    /// | absent / unrecognised  | Prints usage, exits 1         |
    pub async fn run(self) -> Result<(), RuntimeError> {
        let args: Vec<String> = std::env::args().collect();
        match args.get(1).map(String::as_str) {
            Some("serve") => {
                let _port = if args.get(2).is_some_and(|a| a == "--port") {
                    args.get(3)
                        .and_then(|p| p.parse::<u16>().ok())
                        .unwrap_or(self.port)
                } else {
                    self.port
                };
                Err(RuntimeError::Usage(
                    "HTTP server mode requires the `http` feature flag".into(),
                ))
            }
            Some(name) => {
                let prompt = args
                    .get(2)
                    .map(String::as_str)
                    .unwrap_or("")
                    .to_string();
                cli::run_cli(self, name.to_string(), prompt).await
            }
            None => {
                eprintln!(
                    "usage: {} serve | {} <agent-name> <prompt>",
                    args[0], args[0]
                );
                std::process::exit(1);
            }
        }
    }

    /// Looks up an agent handle by name.
    pub async fn get_handle_by_name(&self, name: &str) -> Option<AgentHandle> {
        self.supervisor.child(name).await
    }

    /// Returns status snapshots of all supervised children.
    pub async fn list_children(&self) -> Vec<ChildStatus> {
        self.supervisor.list_children().await
    }

    /// Subscribes to supervisor events.
    pub fn subscribe(&self) -> broadcast::Receiver<SupervisorEvent> {
        self.supervisor.subscribe()
    }

    /// Returns current global token usage, if a budget is configured.
    pub fn token_usage(&self) -> Option<RunUsage> {
        self.budget.as_ref().map(|b| b.run_usage())
    }

    /// Returns per-agent token usage, if a budget is configured.
    pub fn agent_token_usage(&self, name: &str) -> Option<AgentUsage> {
        self.budget.as_ref().and_then(|b| b.agent_usage(name))
    }

    /// Returns the session store in use (or a fresh `MemorySessionStore` if none was configured).
    #[allow(dead_code)]
    pub(crate) fn session_store(&self) -> Arc<dyn SessionStore> {
        self.session_store
            .clone()
            .unwrap_or_else(|| Arc::new(MemorySessionStore::new()))
    }

    /// Gracefully shuts down all agents and the supervisor.
    pub async fn shutdown(self) {
        self.supervisor.shutdown().await;
        let _ = self.join.await;
    }

    /// Dispatches a single task to the named agent using the given session store.
    ///
    /// Used internally by CLI and HTTP triggers.
    pub(crate) async fn dispatch(
        &self,
        agent_name: &str,
        prompt: &str,
        session_id: Option<String>,
        store: Arc<dyn SessionStore>,
    ) -> Result<crate::agent::AgentReply, RuntimeError> {
        let handle = self
            .get_handle_by_name(agent_name)
            .await
            .ok_or_else(|| RuntimeError::UnknownAgent(agent_name.to_string()))?;

        // Load history from session store
        let history = if let Some(ref sid) = session_id {
            store
                .load(sid)
                .await
                .map_err(|e| RuntimeError::Agent(crate::error::AgentError::HandlerFailed(
                    format!("session load failed: {e}"),
                )))?
        } else {
            Vec::new()
        };

        let mut task = Task::new(prompt);
        task.session_id = session_id;
        task.history = history;
        task.session_store = Some(Arc::clone(&store));

        let reply = handle.execute(task).await?;
        Ok(reply)
    }
}

// ── Existing compat types ─────────────────────────────────────────────────────

/// Thin wrapper around the root supervisor.
///
/// # Deprecated
///
/// Use [`Runtime::builder()`] for new code.
#[deprecated(note = "Use Runtime::builder() instead")]
pub struct SwarmRuntime {
    supervisor: SupervisorHandle,
    join: JoinHandle<Result<(), crate::error::SupervisorError>>,
    budget: Option<Arc<BudgetTracker>>,
}

#[allow(deprecated)]
impl SwarmRuntime {
    /// Creates a new runtime backed by a supervisor with the given config.
    pub fn new(config: SupervisorConfig) -> Self {
        let (supervisor, join) = SupervisorHandle::start(config);
        Self { supervisor, join, budget: None }
    }

    /// Creates a new runtime with a global token budget.
    pub fn with_budget(config: SupervisorConfig, global_limit: u64) -> Self {
        let budget = Arc::new(
            BudgetTracker::builder()
                .global_limit(global_limit)
                .build_unconnected(),
        );
        let (supervisor, join) =
            SupervisorHandle::start_with_budget(config, Some(budget.clone()));
        Self { supervisor, join, budget: Some(budget) }
    }

    /// Spawns a child agent via the supervisor.
    pub async fn spawn(
        &self,
        spec: ChildSpec,
    ) -> Result<AgentHandle, SupervisorError> {
        self.supervisor.start_child(spec).await
    }

    /// Looks up a child handle by name.
    pub async fn get_handle_by_name(&self, name: &str) -> Option<AgentHandle> {
        self.supervisor.child(name).await
    }

    /// Returns status snapshots of all supervised children.
    pub async fn list_children(&self) -> Vec<ChildStatus> {
        self.supervisor.list_children().await
    }

    /// Subscribes to supervisor events.
    pub fn subscribe(&self) -> broadcast::Receiver<SupervisorEvent> {
        self.supervisor.subscribe()
    }

    /// Returns current global token usage, if a budget is configured.
    pub fn token_usage(&self) -> Option<RunUsage> {
        self.budget.as_ref().map(|b| b.run_usage())
    }

    /// Returns per-agent token usage, if a budget is configured.
    pub fn agent_token_usage(&self, name: &str) -> Option<AgentUsage> {
        self.budget.as_ref().and_then(|b| b.agent_usage(name))
    }

    /// Gracefully shuts down all agents and the supervisor.
    pub async fn shutdown(self) {
        self.supervisor.shutdown().await;
        let _ = self.join.await;
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use crate::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
    use crate::error::{AgentError, LlmError};
    use crate::llm::{ChatMessage, LlmProvider, LlmRequest, LlmResponse, Role};
    use crate::session::{MemorySessionStore, Message, Role as SRole};

    use super::*;

    /// Mock LLM that echoes the last user message back with token counts.
    struct MockLlm;

    impl LlmProvider for MockLlm {
        fn chat<'a>(
            &'a self,
            req: &'a LlmRequest,
        ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
            let last = req
                .messages
                .iter()
                .rev()
                .find(|m| matches!(m.role, Role::User))
                .map(|m| m.content.clone())
                .unwrap_or_default();
            Box::pin(async move {
                Ok(LlmResponse {
                    content: format!("echo: {last}"),
                    prompt_tokens: req.messages.len() as u64,
                    completion_tokens: 1,
                    tool_calls: vec![],
                })
            })
        }
    }

    struct Echo;
    impl AgentBehavior for Echo {
        async fn handle(
            &mut self,
            ctx: &mut AgentCtx,
            input: Task,
        ) -> Result<AgentReply, AgentError> {
            let req = LlmRequest::builder()
                .message(ChatMessage {
                    role: Role::User,
                    content: input.instruction.clone(),
                    tool_calls: vec![],
                    tool_call_id: None,
                })
                .build();
            let resp = ctx.chat(&req).await?;
            let tokens = resp.total_tokens();
            Ok(AgentReply {
                task_id: input.id,
                output: resp.content,
                self_tokens: tokens,
                total_tokens: tokens,
            })
        }
    }

    #[tokio::test]
    async fn runtime_builder_cli_dispatch() {
        let runtime = Runtime::builder()
            .agent(AgentEntry::new("echo", Echo))
            .llm(MockLlm)
            .build()
            .await
            .unwrap();

        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
        let reply = runtime
            .dispatch("echo", "hello", None, Arc::clone(&store))
            .await
            .unwrap();

        assert!(reply.output.contains("hello"));
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn history_prepended_in_chat() {
        let store = Arc::new(MemorySessionStore::new());

        // Pre-seed history for session "s1"
        store
            .save(
                "s1",
                &[
                    Message { role: SRole::User, content: "first question".into() },
                    Message { role: SRole::Assistant, content: "first answer".into() },
                ],
            )
            .await
            .unwrap();

        let runtime = Runtime::builder()
            .agent(AgentEntry::new("echo", Echo))
            .llm(MockLlm)
            .build()
            .await
            .unwrap();

        // MockLlm echoes the last user message; the prompt_tokens count tells us
        // how many messages were in the context (history + current).
        let store_arc: Arc<dyn SessionStore> = store.clone();
        let reply = runtime
            .dispatch("echo", "second question", Some("s1".into()), Arc::clone(&store_arc))
            .await
            .unwrap();

        // The echo agent just returns the LLM response
        assert!(reply.output.contains("second question"));

        // History should now have: prev 2 + new user + new assistant = 4
        let updated = store.load("s1").await.unwrap();
        assert_eq!(updated.len(), 4, "history should grow after each dispatch");

        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn per_agent_model_override() {
        // Just verify the runtime builds without error when a model is specified
        let runtime = Runtime::builder()
            .agent(AgentEntry::new("echo", Echo).model("anthropic/claude-opus-4-6"))
            .model("anthropic/claude-sonnet-4-6")
            .llm(MockLlm)
            .build()
            .await
            .unwrap();

        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
        let reply = runtime
            .dispatch("echo", "hi", None, store)
            .await
            .unwrap();

        assert!(!reply.output.is_empty());
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn unknown_agent_returns_error() {
        let runtime = Runtime::builder()
            .agent(AgentEntry::new("echo", Echo))
            .llm(MockLlm)
            .build()
            .await
            .unwrap();

        let store: Arc<dyn SessionStore> = Arc::new(MemorySessionStore::new());
        let err = runtime
            .dispatch("nonexistent", "hi", None, store)
            .await
            .unwrap_err();

        assert!(matches!(err, RuntimeError::UnknownAgent(_)));
        runtime.shutdown().await;
    }
}
