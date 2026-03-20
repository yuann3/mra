//! LLM provider abstraction.
//!
//! Defines the [`LlmProvider`] trait and types for chat-based LLM
//! interactions. The trait returns `Pin<Box<dyn Future>>` for dyn-safety
//! so providers can be shared as `Arc<dyn LlmProvider>`.
//!
//! [`AgentBehavior`](crate::agent::AgentBehavior) stays generic (RPITIT)
//! for zero-cost dispatch in the hot actor loop.

mod openrouter;

pub use openrouter::OpenRouterClient;

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::LlmError;
use crate::tool::ToolSpec;

/// Role in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// A tool call requested by the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// A single message in a chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Request payload for an LLM chat completion.
///
/// `model` is optional — if `None`, the provider uses its configured default.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub tools: Option<Vec<ToolSpec>>,
}

/// Response from an LLM chat completion.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub tool_calls: Vec<ToolCall>,
}

impl LlmResponse {
    /// Total tokens consumed (prompt + completion).
    pub fn total_tokens(&self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// Trait for LLM provider implementations.
///
/// Returns `Pin<Box<dyn Future>>` for dyn-safety — allows `Arc<dyn LlmProvider>`.
/// The per-call boxing cost is negligible relative to network I/O.
pub trait LlmProvider: Send + Sync + 'static {
    /// Sends a chat completion request and returns the response.
    fn chat<'a>(
        &'a self,
        request: &'a LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>>;
}
