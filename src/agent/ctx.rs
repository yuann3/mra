use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::watch;

use serde_json::Value;

use crate::budget::BudgetTracker;
use crate::error::{AgentError, ToolError};
use crate::ids::AgentId;
use crate::llm::{ChatMessage, LlmProvider, LlmRequest, LlmResponse, Role};
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
}

impl AgentCtx {
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

    /// Invokes a tool by name, sending heartbeats every second so the
    /// supervisor doesn't mistake a long-running tool for a hung agent.
    pub async fn call_tool(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let invoke = self.tools.invoke(name, args);
        tokio::pin!(invoke);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(1));

        loop {
            tokio::select! {
                result = &mut invoke => break result,
                _ = heartbeat.tick() => self.report_progress(),
            }
        }
    }

    /// Call LLM with automatic budget enforcement.
    ///
    /// Pre-checks whether the budget has already been tripped before
    /// calling the LLM, then charges this agent's direct token usage
    /// against both per-agent and global budgets.
    ///
    /// Returns `Err(AgentError::BudgetExceeded)` if the budget was
    /// already tripped or if this call crosses a limit.
    pub async fn chat(&self, request: &LlmRequest) -> Result<LlmResponse, AgentError> {
        if let Some(ref budget) = self.budget
            && budget.is_exceeded(&self.name)
        {
            return Err(AgentError::BudgetExceeded);
        }

        let llm = self.llm.as_ref().expect("no llm configured");
        let chat_fut = llm.chat(request);
        tokio::pin!(chat_fut);
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(1));

        let response = loop {
            tokio::select! {
                result = &mut chat_fut => break result.map_err(AgentError::Llm)?,
                _ = heartbeat.tick() => self.report_progress(),
            }
        };

        if let Some(ref budget) = self.budget {
            budget.charge(&self.name, response.total_tokens())?;
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
    /// Each LLM call goes through [`Self::chat()`], so budget enforcement
    /// and heartbeats happen automatically. Tool execution errors are
    /// caught and sent back to the LLM as error messages rather than
    /// crashing the loop.
    pub async fn chat_with_tools(
        &self,
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
