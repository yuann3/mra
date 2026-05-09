//! Runtime context passed to agent behaviors.
//!
//! Owns LLM access, tool dispatch, budget enforcement, peer handles,
//! and heartbeat reporting. The supervisor injects these at spawn time;
//! behaviors receive the context as `&mut AgentCtx` in every `handle` call.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::watch;

use std::future::Future;

use serde_json::Value;

use crate::budget::BudgetTracker;
use crate::error::{AgentError, ToolError};
use crate::ids::AgentId;
use crate::llm::{ChatMessage, LlmProvider, LlmRequest, LlmResponse, Role};
use crate::session::{Message, Role as SessionRole, SessionStore};
use crate::tool::{ToolOutput, ToolRegistry};

use super::handle::AgentHandle;
use super::runner::ProgressState;

/// What [`AgentCtx::chat_with_tools`] hands back when the loop finishes.
///
/// Contains the last LLM response, how many rounds it took, and the
/// total token spend across every LLM call in the loop. If the loop
/// hit `max_iterations` while the LLM was still requesting tool calls,
/// `response.tool_calls` will be non-empty -- the caller decides what
/// to do with that.
pub struct ToolLoopResult {
    /// The last response from the LLM. Usually plain text, but may
    /// still contain `tool_calls` if the iteration cap was reached.
    pub response: LlmResponse,
    /// How many LLM calls were made (1 = no tool use, just a direct answer).
    pub iterations: usize,
    /// Sum of `prompt_tokens` across every LLM call in the loop.
    pub total_prompt_tokens: u64,
    /// Sum of `completion_tokens` across every LLM call in the loop.
    pub total_completion_tokens: u64,
}

/// Runtime context passed to [`AgentBehavior::handle`](super::AgentBehavior::handle).
///
/// Provides the agent's identity, peer handles for delegation, an
/// optional LLM provider, budget tracking, and a tool registry.
pub struct AgentCtx {
    /// This agent's unique identifier.
    pub id: AgentId,
    /// Human-readable name, used as the key for budget tracking.
    pub name: String,
    /// Named handles to peer agents, injected at spawn time.
    /// Agents call `ctx.peers["writer"].execute(task)` to delegate.
    pub peers: HashMap<String, AgentHandle>,
    /// Shared LLM provider. `None` for agents that don't need LLM access.
    /// Private to enforce budget tracking via [`Self::chat()`].
    pub(crate) llm: Option<Arc<dyn LlmProvider>>,
    /// Shared budget tracker. `None` if no budget is configured.
    pub(crate) budget: Option<Arc<BudgetTracker>>,
    /// Progress sender for cooperative heartbeat updates.
    pub(crate) progress_tx: watch::Sender<ProgressState>,
    /// Registered tools available to this agent.
    pub tools: ToolRegistry,
    /// Per-agent model override resolved at spawn time.
    /// `None` → use whatever model is set on the `LlmRequest`.
    pub(crate) model: Option<String>,
    /// Conversation history for the current session.
    /// Loaded from the session store before each task and saved after each `chat()`.
    pub(crate) history: Vec<Message>,
    /// Session ID for the current task. `None` for stateless one-shot calls.
    pub(crate) session_id: Option<String>,
    /// Session store for persisting history. `None` for stateless one-shot calls.
    pub(crate) session_store: Option<Arc<dyn SessionStore>>,
    /// Role registry for system prompt overlays. Populated at runtime build time.
    pub(crate) role_registry: crate::runtime::roles::RoleRegistry,
    /// Active role for the current task. Set by the runner from `Task::role`.
    /// When `Some`, `chat()` automatically calls `with_role()` to prepend the
    /// system message. Cleared between tasks.
    pub(crate) active_role: Option<String>,
}

impl AgentCtx {
    /// Returns `true` if this agent has an LLM provider configured.
    pub fn has_llm(&self) -> bool {
        self.llm.is_some()
    }

    /// Prepends the named role's system prompt as a System message before the
    /// user message(s) in a single `chat()` call.
    ///
    /// The injected System message is **not** persisted to session history —
    /// it only affects the request passed to `chat()` for this one call.
    ///
    /// Returns `None` if the role name is not found in the registry.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use mra::agent::AgentCtx;
    /// # use mra::llm::LlmRequest;
    /// # async fn example(ctx: &mut AgentCtx, req: &LlmRequest) -> Result<(), mra::error::AgentError> {
    /// if let Some(req_with_role) = ctx.with_role("analyst", req) {
    ///     let _response = ctx.chat(&req_with_role).await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn with_role(&self, role_name: &str, request: &LlmRequest) -> Option<LlmRequest> {
        let role = self.role_registry.get(role_name)?;
        let mut messages = vec![ChatMessage {
            role: Role::System,
            content: role.system_prompt.clone(),
            tool_calls: vec![],
            tool_call_id: None,
        }];
        messages.extend_from_slice(&request.messages);
        Some(LlmRequest {
            model: request.model.clone(),
            messages,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            tools: request.tools.clone(),
        })
    }

    /// Reports progress to the supervisor, resetting the hang-detection timer.
    ///
    /// Call this during long operations (e.g. LLM calls, tool invocations)
    /// to prevent the supervisor from treating the agent as hung.
    pub fn report_progress(&self) {
        let _ = self.progress_tx.send(ProgressState {
            last_progress: tokio::time::Instant::now(),
            busy: true,
        });
    }

    /// Awaits `fut` while sending heartbeats every second so the supervisor
    /// doesn't mistake a long-running operation for a hung agent.
    async fn with_heartbeat<F: Future>(&self, fut: F) -> F::Output {
        tokio::pin!(fut);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                result = &mut fut => break result,
                _ = heartbeat.tick() => self.report_progress(),
            }
        }
    }

    /// Invokes a tool by name, sending heartbeats every second so the
    /// supervisor doesn't mistake a long-running tool for a hung agent.
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        self.with_heartbeat(self.tools.invoke(name, args)).await
    }

    /// Call LLM with automatic budget enforcement and session history management.
    ///
    /// The full message list sent to the LLM is built as `history + request.messages`.
    /// After a successful call, the user turn(s) from `request.messages` and the
    /// assistant response are appended to the in-memory history and flushed to the
    /// session store (if a `session_id` is set).
    ///
    /// Pre-checks whether the budget has already been tripped before calling the LLM,
    /// then charges this agent's direct token usage against both per-agent and global budgets.
    ///
    /// Returns `Err(AgentError::BudgetExceeded)` if the budget was already tripped
    /// or if this call crosses a limit.
    pub async fn chat(&mut self, request: &LlmRequest) -> Result<LlmResponse, AgentError> {
        if let Some(ref budget) = self.budget
            && budget.is_exceeded(&self.name)
        {
            return Err(AgentError::BudgetExceeded);
        }

        // 1. Apply active role (if set) — inject system message before everything.
        // The system message is NOT stored in session history.
        let request = if let Some(ref role_name) = self.active_role {
            match self.with_role(role_name, request) {
                Some(req_with_role) => std::borrow::Cow::Owned(req_with_role),
                None => std::borrow::Cow::Borrowed(request),
            }
        } else {
            std::borrow::Cow::Borrowed(request)
        };

        // 2. Build full message list: history + request.messages
        let mut full_messages: Vec<ChatMessage> =
            self.history.iter().map(|m| m.to_chat_message()).collect();
        full_messages.extend_from_slice(&request.messages);

        // 3. Resolve model: per-agent override takes precedence over request.model
        let effective_model = self.model.clone().or_else(|| request.model.clone());

        let full_request = LlmRequest {
            model: effective_model,
            messages: full_messages,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
            tools: request.tools.clone(),
        };

        let llm = self.llm.as_ref().ok_or(AgentError::LlmNotConfigured)?;
        let response = self
            .with_heartbeat(llm.chat(&full_request))
            .await
            .map_err(AgentError::Llm)?;

        if let Some(ref budget) = self.budget {
            budget.charge(&self.name, response.total_tokens())?;
        }

        // 3. Append user turn(s) + assistant turn to history
        for msg in &request.messages {
            let session_role = match msg.role {
                Role::User => Some(SessionRole::User),
                Role::Assistant => Some(SessionRole::Assistant),
                Role::System => Some(SessionRole::System),
                Role::Tool => None, // tool round-trips are not persisted
            };
            if let Some(role) = session_role {
                self.history.push(Message {
                    role,
                    content: msg.content.clone(),
                });
            }
        }
        self.history.push(Message {
            role: SessionRole::Assistant,
            content: response.content.clone(),
        });

        // 4. Flush to session store if session_id is set
        if let (Some(session_id), Some(store)) = (&self.session_id, &self.session_store) {
            store
                .save(session_id, &self.history)
                .await
                .map_err(|e| AgentError::HandlerFailed(format!("session save failed: {e}")))?;
        }

        Ok(response)
    }

    /// Runs the standard tool-use loop that most tool-calling agents need.
    ///
    /// Calls the LLM, checks if it wants to use tools, executes them,
    /// feeds the results back, and repeats. Stops when either the LLM
    /// responds with plain text (no tool calls) or `max_iterations` LLM
    /// calls have been made -- whichever comes first.
    ///
    /// Each LLM call goes through [`Self::chat()`], so budget enforcement,
    /// heartbeats, and history management happen automatically.
    /// Tool execution errors are caught and sent back to the LLM as error
    /// messages rather than crashing the loop.
    pub async fn chat_with_tools(
        &mut self,
        request: &LlmRequest,
        max_iterations: usize,
    ) -> Result<ToolLoopResult, AgentError> {
        let mut messages = request.messages.clone();
        let mut total_prompt = 0u64;
        let mut total_completion = 0u64;
        let mut iterations = 0usize;

        for _ in 0..max_iterations {
            let req = LlmRequest {
                model: request.model.clone(),
                messages: messages.clone(),
                temperature: request.temperature,
                max_tokens: request.max_tokens,
                tools: request.tools.clone(),
            };

            let response = self.chat(&req).await?;
            iterations += 1;
            total_prompt += response.prompt_tokens;
            total_completion += response.completion_tokens;

            if response.tool_calls.is_empty() || iterations == max_iterations {
                // Either the LLM is done, or we've hit the iteration cap.
                // Don't execute tools on the final iteration.
                return Ok(ToolLoopResult {
                    response,
                    iterations,
                    total_prompt_tokens: total_prompt,
                    total_completion_tokens: total_completion,
                });
            }

            // Append assistant message with tool calls
            messages.push(ChatMessage {
                role: Role::Assistant,
                content: response.content.clone(),
                tool_calls: response.tool_calls.clone(),
                tool_call_id: None,
            });

            // Execute each tool call and append results
            for call in &response.tool_calls {
                let tool_result = match self.call_tool(&call.name, call.arguments.clone()).await {
                    Ok(output) => output,
                    Err(err) => ToolOutput {
                        content: format!("Tool error: {err}"),
                        is_error: true,
                    },
                };

                messages.push(ChatMessage {
                    role: Role::Tool,
                    content: tool_result.content,
                    tool_calls: vec![],
                    tool_call_id: Some(call.id.clone()),
                });
            }

            self.report_progress();
        }

        // max_iterations == 0: nothing to do
        Err(AgentError::HandlerFailed(
            "chat_with_tools called with max_iterations = 0".into(),
        ))
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tokio::sync::watch;

    use crate::llm::{ChatMessage, LlmRequest, Role};
    use crate::runtime::roles::RoleRegistry;
    use crate::tool::ToolRegistry;

    use super::super::runner::ProgressState;
    use super::AgentCtx;

    fn make_ctx_with_registry(registry: RoleRegistry) -> AgentCtx {
        let (progress_tx, _) = watch::channel(ProgressState::idle_now());
        AgentCtx {
            id: crate::ids::AgentId::new(),
            name: "test-agent".to_string(),
            peers: HashMap::new(),
            llm: None,
            budget: None,
            progress_tx,
            tools: ToolRegistry::new(),
            model: None,
            history: Vec::new(),
            session_id: None,
            session_store: None,
            role_registry: registry,
            active_role: None,
        }
    }

    #[test]
    fn with_role_prepends_system_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("test.md"),
            "You are a helpful test assistant.",
        )
        .unwrap();
        let registry = RoleRegistry::load_from_dir(dir.path());

        let ctx = make_ctx_with_registry(registry);

        let req = LlmRequest::builder()
            .message(ChatMessage {
                role: Role::User,
                content: "Hello!".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            })
            .build();

        let result = ctx
            .with_role("test", &req)
            .expect("role 'test' should exist");

        assert_eq!(
            result.messages.len(),
            2,
            "should have system + user message"
        );
        assert!(matches!(result.messages[0].role, Role::System));
        assert!(
            result.messages[0]
                .content
                .contains("helpful test assistant")
        );
        assert!(matches!(result.messages[1].role, Role::User));
        assert_eq!(result.messages[1].content, "Hello!");
    }

    #[test]
    fn with_role_missing_returns_none() {
        let ctx = make_ctx_with_registry(RoleRegistry::new());

        let req = LlmRequest::builder()
            .message(ChatMessage {
                role: Role::User,
                content: "Hi".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            })
            .build();

        assert!(ctx.with_role("nonexistent", &req).is_none());
    }

    #[test]
    fn with_role_preserves_request_fields() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("coder.md"), "You are an expert coder.").unwrap();
        let registry = RoleRegistry::load_from_dir(dir.path());

        let ctx = make_ctx_with_registry(registry);

        let req = LlmRequest::builder()
            .message(ChatMessage {
                role: Role::User,
                content: "Write code".to_string(),
                tool_calls: vec![],
                tool_call_id: None,
            })
            .model("test-model")
            .temperature(0.5)
            .max_tokens(256)
            .build();

        let result = ctx.with_role("coder", &req).unwrap();

        assert_eq!(result.model.as_deref(), Some("test-model"));
        assert_eq!(result.temperature, Some(0.5));
        assert_eq!(result.max_tokens, Some(256));
    }
}
