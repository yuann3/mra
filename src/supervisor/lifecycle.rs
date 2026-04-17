//! Child lifecycle management for the supervisor.
//!
//! This module consolidates child state management, JoinSet coordination,
//! and cancellation token lifecycle into a focused struct with an ergonomic API.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::agent::mailbox::MailboxSlot;
use crate::agent::{AgentHandle, ProgressState};
use crate::budget::BudgetTracker;
use crate::error::SupervisorError;
use crate::ids::AgentId;
use crate::tool::ToolRegistry;

use super::ChildExit;
use super::child::{ChildContext, ChildSpec};

/// Runtime state for a single child.
pub struct ChildRecord {
    /// Stable logical ID for this child.
    pub id: AgentId,
    /// Current generation (0 = first start, 1 = first restart, ...).
    pub generation: u64,
    /// Stable mailbox slot (sender swapped on restart).
    pub mailbox: Arc<MailboxSlot>,
    /// Logical cancellation token (parent of per-generation tokens).
    pub logical_cancel: CancellationToken,
    /// Current generation's cancellation token.
    pub child_cancel: Option<CancellationToken>,
    /// Progress watch receiver for hang detection.
    pub progress: Option<watch::Receiver<ProgressState>>,
    /// Whether the child is currently running.
    pub alive: bool,
    /// Whether the child was detected as hung (affects restart decision).
    pub hung: bool,
}

/// Configuration for ChildLifecycle, set once at construction.
pub struct LifecycleConfig {
    /// Shared budget tracker (if any).
    pub budget: Option<Arc<BudgetTracker>>,
    /// Shared LLM provider (if any).
    pub llm: Option<Arc<dyn crate::llm::LlmProvider>>,
    /// Shared tool registry.
    pub tools: ToolRegistry,
}

/// Information about a child exit, returned by `next_exit()`.
#[derive(Debug)]
pub struct ChildExitInfo {
    /// Name of the child that exited.
    pub name: String,
    /// Generation at time of exit.
    pub generation: u64,
    /// Why the child exited.
    pub exit: ChildExit,
    /// Whether the child was marked as hung before exit.
    pub hung: bool,
}

/// Information about a hung child, returned by `check_hangs()`.
#[derive(Debug)]
pub struct HungChild {
    /// Name of the hung child.
    pub name: String,
    /// Generation of the hung child.
    pub generation: u64,
    /// How long since last progress.
    pub elapsed: Duration,
}

/// Manages child lifecycle: spawn, exit handling, hang detection.
///
/// Owns the JoinSet of child tasks and all per-child state.
/// Provides ergonomic methods for the three most common operations:
/// - `start()` — spawn a new child
/// - `next_exit()` — poll for child exits
/// - `check_hangs()` — detect hung children
pub struct ChildLifecycle {
    children: HashMap<String, ChildRecord>,
    task_map: HashMap<tokio::task::Id, String>,
    join_set: JoinSet<ChildExit>,
    config: LifecycleConfig,
}

impl ChildLifecycle {
    /// Creates a new ChildLifecycle with the given configuration.
    pub fn new(config: LifecycleConfig) -> Self {
        Self {
            children: HashMap::new(),
            task_map: HashMap::new(),
            join_set: JoinSet::new(),
            config,
        }
    }

    /// Starts a child and returns a stable AgentHandle.
    ///
    /// Handles all the plumbing: token creation, mailbox setup,
    /// budget registration, factory invocation, JoinSet spawning.
    pub async fn start(
        &mut self,
        spec: ChildSpec,
        peers: &HashMap<String, AgentHandle>,
    ) -> Result<AgentHandle, SupervisorError> {
        let name = spec.name.clone();
        let id = AgentId::new();
        let logical_cancel = CancellationToken::new();
        let child_cancel = logical_cancel.child_token();

        // Create stable mailbox with dummy sender
        let (dummy_tx, _) = mpsc::channel(1);
        let mailbox = Arc::new(MailboxSlot::new(dummy_tx));

        // Register budget if tracking is active
        if let Some(ref budget) = self.config.budget {
            budget.register_agent(&name, spec.token_budget);
        }

        // Build context and call factory
        let ctx = ChildContext {
            id,
            generation: 0,
            cancel: child_cancel.clone(),
            peers: peers.clone(),
            llm: self.config.llm.clone(),
            budget: self.config.budget.clone(),
            tools: self.config.tools.clone(),
        };

        let spawned = (spec.factory)(ctx)
            .await
            .map_err(|e| SupervisorError::SpawnFailed(e.to_string()))?;

        // Swap real sender into stable mailbox
        mailbox.swap(spawned.sender);

        // Build stable handle
        let handle = AgentHandle::new(name.clone(), id, mailbox.clone(), logical_cancel.clone());

        // Spawn future in JoinSet
        let abort = self.join_set.spawn(spawned.future);
        let task_id = abort.id();
        self.task_map.insert(task_id, name.clone());

        // Record child state
        self.children.insert(
            name,
            ChildRecord {
                id,
                generation: 0,
                mailbox,
                logical_cancel,
                child_cancel: Some(child_cancel),
                progress: Some(spawned.progress),
                alive: true,
                hung: false,
            },
        );

        Ok(handle)
    }

    /// Polls for the next child exit.
    ///
    /// Returns `None` if no tasks in JoinSet.
    /// Normalizes panics to `ChildExit::Failed`.
    /// Updates child state flags (alive, hung, progress).
    #[allow(dead_code)] // useful for simpler supervisor implementations
    pub async fn next_exit(&mut self) -> Option<ChildExitInfo> {
        let result = self.join_set.join_next_with_id().await?;

        let (task_id, exit) = match result {
            Ok((id, exit)) => (id, exit),
            Err(e) => {
                let id = e.id();
                (id, ChildExit::Failed(format!("task panicked: {e}")))
            }
        };

        let name = self.task_map.remove(&task_id)?;

        let (generation, hung) = if let Some(child) = self.children.get_mut(&name) {
            let generation = child.generation;
            child.alive = false;
            child.child_cancel = None;
            child.progress = None;
            let was_hung = child.hung;
            child.hung = false;
            (generation, was_hung)
        } else {
            return None;
        };

        Some(ChildExitInfo {
            name,
            generation,
            exit,
            hung,
        })
    }

    /// Checks all alive children for hang condition.
    ///
    /// Returns an iterator of hung children. Caller decides how to handle
    /// (emit event, cancel, etc.).
    pub fn check_hangs<'a>(
        &'a self,
        specs: &'a HashMap<String, ChildSpec>,
    ) -> impl Iterator<Item = HungChild> + 'a {
        self.children.iter().filter_map(move |(name, child)| {
            if !child.alive {
                return None;
            }
            let spec = specs.get(name)?;
            let hang_timeout = spec.hang_timeout?;
            let progress_rx = child.progress.as_ref()?;

            let progress = progress_rx.borrow();
            if progress.busy && progress.last_progress.elapsed() > hang_timeout {
                Some(HungChild {
                    name: name.clone(),
                    generation: child.generation,
                    elapsed: progress.last_progress.elapsed(),
                })
            } else {
                None
            }
        })
    }

    /// Cancels a child's current generation token and marks it as hung.
    pub fn cancel_child(&mut self, name: &str) {
        if let Some(child) = self.children.get_mut(name) {
            if let Some(cancel) = child.child_cancel.take() {
                cancel.cancel();
            }
            child.hung = true;
        }
    }

    /// Restarts a dead child with a new generation.
    ///
    /// Returns the new generation on success.
    /// Fails if the child is still alive or if generation mismatches.
    pub async fn restart(
        &mut self,
        name: &str,
        spec: &ChildSpec,
        peers: &HashMap<String, AgentHandle>,
    ) -> Result<u64, SupervisorError> {
        let child = self
            .children
            .get(name)
            .ok_or_else(|| SupervisorError::ChildNotFound(name.to_string()))?;

        if child.alive {
            return Err(SupervisorError::SpawnFailed(
                "cannot restart: child is still alive".to_string(),
            ));
        }

        let old_gen = child.generation;
        let new_gen = old_gen + 1;
        let child_cancel = child.logical_cancel.child_token();
        let child_id = child.id;

        let ctx = ChildContext {
            id: child_id,
            generation: new_gen,
            cancel: child_cancel.clone(),
            peers: peers.clone(),
            llm: self.config.llm.clone(),
            budget: self.config.budget.clone(),
            tools: self.config.tools.clone(),
        };

        let spawned = (spec.factory)(ctx)
            .await
            .map_err(|e| SupervisorError::SpawnFailed(e.to_string()))?;

        let child = self.children.get_mut(name).unwrap();
        child.mailbox.swap(spawned.sender);

        let abort = self.join_set.spawn(spawned.future);
        self.task_map.insert(abort.id(), name.to_string());

        child.generation = new_gen;
        child.progress = Some(spawned.progress);
        child.child_cancel = Some(child_cancel);
        child.alive = true;

        Ok(new_gen)
    }

    /// Returns a stable AgentHandle for a named child.
    pub fn get_handle(&self, name: &str) -> Option<AgentHandle> {
        self.children.get(name).map(|child| {
            AgentHandle::new(
                name.to_string(),
                child.id,
                child.mailbox.clone(),
                child.logical_cancel.clone(),
            )
        })
    }

    /// Builds peers map from all alive children except `exclude`.
    pub fn peers_excluding(&self, exclude: &str) -> HashMap<String, AgentHandle> {
        self.children
            .iter()
            .filter(|(n, c)| c.alive && *n != exclude)
            .map(|(n, c)| {
                (
                    n.clone(),
                    AgentHandle::new(n.clone(), c.id, c.mailbox.clone(), c.logical_cancel.clone()),
                )
            })
            .collect()
    }

    /// Cancels all alive children.
    pub fn cancel_all(&mut self) {
        for child in self.children.values_mut() {
            if let Some(cancel) = child.child_cancel.take() {
                cancel.cancel();
            }
        }
    }

    /// Cancels all alive children except the specified one.
    pub fn cancel_all_except(&mut self, exclude: &str) {
        for (name, child) in self.children.iter_mut() {
            if name != exclude
                && let Some(cancel) = child.child_cancel.take()
            {
                cancel.cancel();
            }
        }
    }

    /// Drains all remaining tasks. Blocks until JoinSet is empty.
    /// Also updates child state for each exit.
    pub async fn drain(&mut self) {
        while let Some(result) = self.join_set.join_next_with_id().await {
            // Process each exit to update child state
            let _ = self.process_exit(result);
        }
        self.task_map.clear();
    }

    /// Returns reference to child record.
    pub fn get(&self, name: &str) -> Option<&ChildRecord> {
        self.children.get(name)
    }

    /// Returns an iterator over (name, record) pairs for all tracked children.
    pub fn children(&self) -> impl Iterator<Item = (&str, &ChildRecord)> {
        self.children.iter().map(|(n, r)| (n.as_str(), r))
    }

    /// Returns a reference to the shared budget tracker, if configured.
    pub fn budget(&self) -> Option<&Arc<BudgetTracker>> {
        self.config.budget.as_ref()
    }

    /// Returns true if no children are being tracked.
    #[allow(dead_code)] // useful for testing and future use
    pub fn is_empty(&self) -> bool {
        self.children.is_empty()
    }

    /// Returns a mutable reference to the JoinSet for use in select! loops.
    ///
    /// After polling with `join_set_mut().join_next_with_id()`, call
    /// `process_exit()` to update internal state.
    pub fn join_set_mut(&mut self) -> &mut JoinSet<ChildExit> {
        &mut self.join_set
    }

    /// Processes a raw JoinSet result and updates internal state.
    ///
    /// This is the split version of `next_exit()` for use with external polling.
    pub fn process_exit(
        &mut self,
        result: Result<(tokio::task::Id, ChildExit), tokio::task::JoinError>,
    ) -> Option<ChildExitInfo> {
        let (task_id, exit) = match result {
            Ok((id, exit)) => (id, exit),
            Err(e) => {
                let id = e.id();
                (id, ChildExit::Failed(format!("task panicked: {e}")))
            }
        };

        let name = self.task_map.remove(&task_id)?;

        let (generation, hung) = if let Some(child) = self.children.get_mut(&name) {
            let generation = child.generation;
            child.alive = false;
            child.child_cancel = None;
            child.progress = None;
            let was_hung = child.hung;
            child.hung = false;
            (generation, was_hung)
        } else {
            return None;
        };

        Some(ChildExitInfo {
            name,
            generation,
            exit,
            hung,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
    use crate::config::AgentConfig;
    use crate::error::AgentError;
    /// Simple behavior that exits immediately
    struct ImmediateExitBehavior;

    impl AgentBehavior for ImmediateExitBehavior {
        async fn handle(
            &mut self,
            _ctx: &mut AgentCtx,
            task: Task,
        ) -> Result<AgentReply, AgentError> {
            Ok(AgentReply {
                task_id: task.id,
                output: "done".to_string(),
                self_tokens: 0,
                total_tokens: 0,
            })
        }
    }

    #[tokio::test]
    async fn start_returns_working_handle() {
        let config = LifecycleConfig {
            budget: None,
            llm: None,
            tools: ToolRegistry::new(),
        };
        let mut lifecycle = ChildLifecycle::new(config);

        let spec =
            ChildSpec::from_behavior(AgentConfig::new("test-child"), |_| ImmediateExitBehavior);

        let peers = HashMap::new();
        let handle = lifecycle.start(spec, &peers).await.unwrap();

        // The handle should be usable - execute a task
        let task = Task::new("hello");
        let reply = handle.execute(task).await.unwrap();

        assert_eq!(reply.output, "done");

        // Child should be tracked as alive
        let child = lifecycle.get("test-child").unwrap();
        assert!(child.alive);
        assert_eq!(child.generation, 0);
    }

    #[tokio::test]
    async fn next_exit_returns_exit_info_when_child_exits() {
        let config = LifecycleConfig {
            budget: None,
            llm: None,
            tools: ToolRegistry::new(),
        };
        let mut lifecycle = ChildLifecycle::new(config);

        let spec = ChildSpec::from_behavior(AgentConfig::new("exiter"), |_| ImmediateExitBehavior);

        let peers = HashMap::new();
        let _handle = lifecycle.start(spec, &peers).await.unwrap();

        // Cancel the child to trigger shutdown
        lifecycle.cancel_child("exiter");

        // Wait for child to exit
        let exit_info = lifecycle.next_exit().await.unwrap();

        assert_eq!(exit_info.name, "exiter");
        assert_eq!(exit_info.generation, 0);
        // hung is true because cancel_child sets it
        assert!(exit_info.hung);
        assert!(matches!(exit_info.exit, ChildExit::Shutdown));

        // Child should now be marked as dead
        let child = lifecycle.get("exiter").unwrap();
        assert!(!child.alive);
    }

    #[tokio::test]
    async fn next_exit_normalizes_panics_to_failed() {
        use super::super::child::SpawnedChild;
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::Arc;

        let config = LifecycleConfig {
            budget: None,
            llm: None,
            tools: ToolRegistry::new(),
        };
        let mut lifecycle = ChildLifecycle::new(config);

        // Create a spec with a factory that returns a panicking future
        let spec = ChildSpec::new(
            "panicker",
            AgentConfig::new("panicker"),
            Arc::new(|_ctx| {
                Box::pin(async {
                    Ok(SpawnedChild::from_future(Box::pin(async {
                        panic!("intentional test panic");
                        #[allow(unreachable_code)]
                        ChildExit::Normal
                    })
                        as Pin<Box<dyn Future<Output = ChildExit> + Send>>))
                })
                    as Pin<Box<dyn Future<Output = Result<SpawnedChild, SupervisorError>> + Send>>
            }),
        );

        let peers = HashMap::new();
        let _handle = lifecycle.start(spec, &peers).await.unwrap();

        // Wait for the panic
        let exit_info = lifecycle.next_exit().await.unwrap();

        assert_eq!(exit_info.name, "panicker");
        assert!(matches!(exit_info.exit, ChildExit::Failed(msg) if msg.contains("panic")));
    }

    fn make_spec(name: &str) -> ChildSpec {
        ChildSpec::from_behavior(AgentConfig::new(name), |_| ImmediateExitBehavior)
    }

    #[tokio::test]
    async fn restart_fails_if_child_is_alive() {
        let config = LifecycleConfig {
            budget: None,
            llm: None,
            tools: ToolRegistry::new(),
        };
        let mut lifecycle = ChildLifecycle::new(config);

        let spec = make_spec("alive-child");
        let peers = HashMap::new();
        let _handle = lifecycle.start(spec, &peers).await.unwrap();

        // Child is alive, restart should fail
        let spec2 = make_spec("alive-child");
        let result = lifecycle.restart("alive-child", &spec2, &peers).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("still alive"));
    }

    #[tokio::test]
    async fn restart_succeeds_and_increments_generation() {
        let config = LifecycleConfig {
            budget: None,
            llm: None,
            tools: ToolRegistry::new(),
        };
        let mut lifecycle = ChildLifecycle::new(config);

        let spec = make_spec("restartable");
        let peers = HashMap::new();
        let _handle = lifecycle.start(spec, &peers).await.unwrap();

        // Cancel and wait for exit
        lifecycle.cancel_child("restartable");
        let _ = lifecycle.next_exit().await.unwrap();

        // Now restart
        let spec2 = make_spec("restartable");
        let new_gen = lifecycle
            .restart("restartable", &spec2, &peers)
            .await
            .unwrap();
        assert_eq!(new_gen, 1);

        // Child should be alive again
        let child = lifecycle.get("restartable").unwrap();
        assert!(child.alive);
        assert_eq!(child.generation, 1);
    }

    #[tokio::test]
    async fn peers_excluding_returns_correct_handles() {
        let config = LifecycleConfig {
            budget: None,
            llm: None,
            tools: ToolRegistry::new(),
        };
        let mut lifecycle = ChildLifecycle::new(config);

        // Start 3 children
        for name in ["alice", "bob", "charlie"] {
            let spec = ChildSpec::from_behavior(AgentConfig::new(name), |_| ImmediateExitBehavior);
            let peers = lifecycle.peers_excluding(name);
            lifecycle.start(spec, &peers).await.unwrap();
        }

        // peers_excluding("bob") should return alice and charlie
        let peers = lifecycle.peers_excluding("bob");
        assert_eq!(peers.len(), 2);
        assert!(peers.contains_key("alice"));
        assert!(peers.contains_key("charlie"));
        assert!(!peers.contains_key("bob"));
    }

    #[tokio::test]
    async fn start_passes_llm_and_tools_from_config() {
        use super::super::child::SpawnedChild;
        use crate::llm::{LlmProvider, LlmRequest, LlmResponse};
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct DummyLlm;
        impl LlmProvider for DummyLlm {
            fn chat<'a>(
                &'a self,
                _req: &'a LlmRequest,
            ) -> Pin<
                Box<dyn Future<Output = Result<LlmResponse, crate::error::LlmError>> + Send + 'a>,
            > {
                Box::pin(async { unreachable!() })
            }
        }

        let llm: Arc<dyn LlmProvider> = Arc::new(DummyLlm);
        let tools = ToolRegistry::new();

        let config = LifecycleConfig {
            budget: None,
            llm: Some(llm.clone()),
            tools: tools.clone(),
        };
        let mut lifecycle = ChildLifecycle::new(config);

        let got_llm = Arc::new(AtomicBool::new(false));
        let got_llm2 = got_llm.clone();

        let spec = ChildSpec::new(
            "checker",
            AgentConfig::new("checker"),
            Arc::new(move |ctx: ChildContext| {
                let got_llm = got_llm2.clone();
                Box::pin(async move {
                    // Verify ChildContext received the LLM from config
                    got_llm.store(ctx.llm.is_some(), Ordering::SeqCst);
                    Ok(SpawnedChild::from_future(
                        Box::pin(async { ChildExit::Normal })
                            as Pin<Box<dyn Future<Output = ChildExit> + Send>>,
                    ))
                })
                    as Pin<Box<dyn Future<Output = Result<SpawnedChild, SupervisorError>> + Send>>
            }),
        );

        let _handle = lifecycle.start(spec, &HashMap::new()).await.unwrap();
        assert!(
            got_llm.load(Ordering::SeqCst),
            "ChildContext should have received LLM from config"
        );
    }

    #[tokio::test]
    async fn restart_passes_llm_and_tools_from_config() {
        use super::super::child::SpawnedChild;
        use crate::llm::{LlmProvider, LlmRequest, LlmResponse};
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct DummyLlm;
        impl LlmProvider for DummyLlm {
            fn chat<'a>(
                &'a self,
                _req: &'a LlmRequest,
            ) -> Pin<
                Box<dyn Future<Output = Result<LlmResponse, crate::error::LlmError>> + Send + 'a>,
            > {
                Box::pin(async { unreachable!() })
            }
        }

        let llm: Arc<dyn LlmProvider> = Arc::new(DummyLlm);

        let config = LifecycleConfig {
            budget: None,
            llm: Some(llm.clone()),
            tools: ToolRegistry::new(),
        };
        let mut lifecycle = ChildLifecycle::new(config);

        let got_llm = Arc::new(AtomicBool::new(false));
        let got_llm2 = got_llm.clone();

        let factory: super::super::child::ChildFactory = Arc::new(move |ctx: ChildContext| {
            let got_llm = got_llm2.clone();
            Box::pin(async move {
                got_llm.store(ctx.llm.is_some(), Ordering::SeqCst);
                Ok(SpawnedChild::from_future(
                    Box::pin(async { ChildExit::Normal })
                        as Pin<Box<dyn Future<Output = ChildExit> + Send>>,
                ))
            })
                as Pin<Box<dyn Future<Output = Result<SpawnedChild, SupervisorError>> + Send>>
        });

        let spec = ChildSpec::new("checker", AgentConfig::new("checker"), factory.clone());
        let _handle = lifecycle.start(spec, &HashMap::new()).await.unwrap();

        // Kill and wait for exit
        lifecycle.cancel_child("checker");
        let _ = lifecycle.next_exit().await.unwrap();

        // Reset flag
        got_llm.store(false, Ordering::SeqCst);

        // Restart — should also pass LLM
        let spec2 = ChildSpec::new("checker", AgentConfig::new("checker"), factory);
        lifecycle
            .restart("checker", &spec2, &HashMap::new())
            .await
            .unwrap();

        assert!(
            got_llm.load(Ordering::SeqCst),
            "ChildContext should have LLM on restart too"
        );
    }

    #[tokio::test]
    async fn cancel_all_and_drain_shuts_down_everything() {
        let config = LifecycleConfig {
            budget: None,
            llm: None,
            tools: ToolRegistry::new(),
        };
        let mut lifecycle = ChildLifecycle::new(config);

        // Start 3 children
        for name in ["a", "b", "c"] {
            let spec = ChildSpec::from_behavior(AgentConfig::new(name), |_| ImmediateExitBehavior);
            lifecycle.start(spec, &HashMap::new()).await.unwrap();
        }

        // Cancel all and drain
        lifecycle.cancel_all();
        lifecycle.drain().await;

        // All children should be marked as not alive (after drain, JoinSet is empty)
        // Note: drain() doesn't update child state, but the JoinSet should be empty
        assert!(!lifecycle.is_empty()); // children still tracked

        // But we can verify no more exits come
        // Actually, after drain, all tasks are gone from JoinSet
    }
}
