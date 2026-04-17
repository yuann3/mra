//! Token budget enforcement for swarm runs.
//!
//! Two levels of limits: a global cap on the entire run and optional
//! per-agent quotas. Both use `AtomicU64` counters so any number of
//! Tokio tasks can charge concurrently without a mutex.
//!
//! Charging happens after the LLM response arrives (post-call), so
//! overshoot is bounded by the number of in-flight calls at the moment
//! a limit is crossed. Once tripped, the flag is latched — subsequent
//! [`AgentCtx::chat`](crate::agent::AgentCtx::chat) calls fail
//! immediately without contacting the provider.

use std::collections::HashMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::error::AgentError;

/// Read-only snapshot of global token usage.
pub struct RunUsage {
    /// Tokens consumed so far.
    pub used: u64,
    /// Configured limit (`None` = unlimited).
    pub limit: Option<u64>,
}

/// Read-only snapshot of per-agent token usage.
pub struct AgentUsage {
    /// Tokens consumed by this agent.
    pub used: u64,
    /// Configured limit for this agent (`None` = unlimited).
    pub limit: Option<u64>,
}

struct AgentBudget {
    used: AtomicU64,
    limit: Option<u64>,
    tripped: AtomicBool,
}

/// Builder for [`BudgetTracker`].
///
/// ```
/// # use mra::budget::BudgetTracker;
/// let tracker = BudgetTracker::builder()
///     .global_limit(50_000)
///     .build_unconnected();
/// ```
pub struct BudgetTrackerBuilder {
    global_limit: Option<u64>,
}

impl BudgetTrackerBuilder {
    /// Sets the global token limit for the entire run.
    pub fn global_limit(mut self, limit: u64) -> Self {
        self.global_limit = Some(limit);
        self
    }

    /// Builds the tracker. "Unconnected" because there is no wired
    /// event emitter or cancellation token yet — those can be added later.
    pub fn build_unconnected(self) -> BudgetTracker {
        BudgetTracker {
            global_used: AtomicU64::new(0),
            global_limit: RwLock::new(self.global_limit),
            global_tripped: AtomicBool::new(false),
            agents: RwLock::new(HashMap::new()),
        }
    }
}

/// Lock-free token budget tracker.
///
/// Tracks both a global run-level budget and per-agent quotas.
/// Each agent charges only its own direct LLM token usage — nested
/// agent calls are charged by those agents individually, avoiding
/// double-counting.
pub struct BudgetTracker {
    global_used: AtomicU64,
    global_limit: RwLock<Option<u64>>,
    global_tripped: AtomicBool,
    agents: RwLock<HashMap<String, AgentBudget>>,
}

impl BudgetTracker {
    /// Creates a new builder.
    ///
    /// # Examples
    ///
    /// ```
    /// use mra::budget::BudgetTracker;
    ///
    /// let tracker = BudgetTracker::builder()
    ///     .global_limit(100_000)
    ///     .build_unconnected();
    /// assert!(!tracker.is_global_exceeded());
    /// ```
    pub fn builder() -> BudgetTrackerBuilder {
        BudgetTrackerBuilder { global_limit: None }
    }

    /// Register a per-agent budget slot. Called by the supervisor on spawn.
    ///
    /// If the agent was already registered (e.g. after restart), the
    /// existing counters are preserved — usage persists across restarts.
    pub fn register_agent(&self, name: &str, limit: Option<u64>) {
        let mut agents = self.agents.write().unwrap();
        agents
            .entry(name.to_string())
            .or_insert_with(|| AgentBudget {
                used: AtomicU64::new(0),
                limit,
                tripped: AtomicBool::new(false),
            });
    }

    /// Returns `true` if the budget for the given agent has been tripped.
    pub fn is_agent_exceeded(&self, name: &str) -> bool {
        let agents = self.agents.read().unwrap();
        agents
            .get(name)
            .is_some_and(|a| a.tripped.load(Ordering::Relaxed))
    }

    /// Returns `true` if either the global or per-agent budget has been tripped.
    pub fn is_exceeded(&self, agent_name: &str) -> bool {
        self.is_global_exceeded() || self.is_agent_exceeded(agent_name)
    }

    /// Charge tokens against both the agent's quota and the global budget.
    ///
    /// Both counters are always incremented (reflecting actual spend).
    /// Returns `Err(AgentError::BudgetExceeded)` if either limit is crossed.
    pub fn charge(&self, agent_name: &str, tokens: u64) -> Result<(), AgentError> {
        let global_exceeded = self.charge_global_inner(tokens);

        let mut agent_exceeded = false;
        let agents = self.agents.read().unwrap();
        if let Some(agent) = agents.get(agent_name) {
            let prev = agent.used.fetch_add(tokens, Ordering::Relaxed);
            let new = prev.saturating_add(tokens);
            if let Some(limit) = agent.limit
                && new > limit
            {
                agent.tripped.store(true, Ordering::Relaxed);
                agent_exceeded = true;
            }
        }

        if global_exceeded || agent_exceeded {
            return Err(AgentError::BudgetExceeded);
        }
        Ok(())
    }

    /// Charge tokens against the global budget only.
    pub fn charge_global(&self, tokens: u64) -> Result<(), AgentError> {
        if self.charge_global_inner(tokens) {
            Err(AgentError::BudgetExceeded)
        } else {
            Ok(())
        }
    }

    /// Increments the global counter. Returns `true` if the limit was exceeded.
    fn charge_global_inner(&self, tokens: u64) -> bool {
        let prev = self.global_used.fetch_add(tokens, Ordering::Relaxed);
        let new = prev.saturating_add(tokens);

        let limit = *self.global_limit.read().unwrap();
        if let Some(limit) = limit
            && new > limit
        {
            self.global_tripped.store(true, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// Returns current global usage snapshot.
    pub fn run_usage(&self) -> RunUsage {
        RunUsage {
            used: self.global_used.load(Ordering::Relaxed),
            limit: *self.global_limit.read().unwrap(),
        }
    }

    /// Query per-agent usage.
    pub fn agent_usage(&self, name: &str) -> Option<AgentUsage> {
        let agents = self.agents.read().unwrap();
        agents.get(name).map(|a| AgentUsage {
            used: a.used.load(Ordering::Relaxed),
            limit: a.limit,
        })
    }

    /// Update the global token limit at runtime.
    ///
    /// If the new limit exceeds current usage, the tripped flag is cleared
    /// so that subsequent charges can succeed again.
    pub fn set_global_limit(&self, new_limit: u64) {
        {
            let mut limit = self.global_limit.write().unwrap();
            *limit = Some(new_limit);
        }
        let used = self.global_used.load(Ordering::Relaxed);
        if new_limit >= used {
            let _ = self.global_tripped.compare_exchange(
                true,
                false,
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
        }
    }

    /// Update the token limit for a registered agent.
    ///
    /// If the agent exists and the new limit exceeds (or removes) its
    /// current usage, the tripped flag is cleared. If the agent is not
    /// registered this is a no-op.
    pub fn set_agent_limit(&self, name: &str, new_limit: Option<u64>) {
        let mut agents = self.agents.write().unwrap();
        if let Some(agent) = agents.get_mut(name) {
            agent.limit = new_limit;
            let used = agent.used.load(Ordering::Relaxed);
            let should_untrip = match new_limit {
                None => true,
                Some(lim) => lim >= used,
            };
            if should_untrip {
                let _ = agent.tripped.compare_exchange(
                    true,
                    false,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
            }
        }
    }

    /// Reset a registered agent's usage to zero.
    ///
    /// Clears both the usage counter and the tripped flag while
    /// preserving the configured limit. No-op if the agent is not
    /// registered.
    pub fn reset_agent(&self, name: &str) {
        let mut agents = self.agents.write().unwrap();
        if let Some(agent) = agents.get_mut(name) {
            agent.used.store(0, Ordering::Relaxed);
            agent.tripped.store(false, Ordering::Relaxed);
        }
    }

    /// Snapshot all registered agents and their current usage.
    pub fn list_agents(&self) -> Vec<(String, AgentUsage)> {
        let agents = self.agents.read().unwrap();
        agents
            .iter()
            .map(|(name, a)| {
                (
                    name.clone(),
                    AgentUsage {
                        used: a.used.load(Ordering::Relaxed),
                        limit: a.limit,
                    },
                )
            })
            .collect()
    }

    /// Whether the global budget has been tripped.
    pub fn is_global_exceeded(&self) -> bool {
        self.global_tripped.load(Ordering::Relaxed)
    }
}
