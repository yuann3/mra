//! Error types for the mra framework.
//!
//! Each subsystem has its own error enum with a `classification()` method
//! returning an [`ErrorClass`] used by the supervisor and retry logic to
//! decide how to react: retry, restart, shed load, or give up.

use crate::ids::AgentId;
use thiserror::Error;

/// How the runtime should react to an error.
///
/// Drives retry policies, supervisor restart decisions, and admission control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Retryable — the operation may succeed on a subsequent attempt.
    Transient,
    /// Fatal — retrying will not help; propagate the error.
    Permanent,
    /// The system is under pressure — back off or shed load.
    Overload,
    /// The operation was explicitly cancelled (e.g. shutdown).
    Cancelled,
    /// A token or cost budget has been exceeded.
    BudgetExceeded,
}

/// Errors originating from agent behavior handlers.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The agent's behavior handler returned an error.
    #[error("handler failed: {0}")]
    HandlerFailed(String),
    /// The agent did not respond within the configured timeout.
    #[error("agent timed out")]
    Timeout,
    /// The agent was cancelled via its `CancellationToken`.
    #[error("agent cancelled")]
    Cancelled,
    /// The agent's token/cost budget was exceeded.
    #[error("budget exceeded")]
    BudgetExceeded,
}

impl AgentError {
    /// Returns the [`ErrorClass`] for this error, used to decide retry/restart behavior.
    pub fn classification(&self) -> ErrorClass {
        match self {
            Self::HandlerFailed(_) => ErrorClass::Permanent,
            Self::Timeout => ErrorClass::Transient,
            Self::Cancelled => ErrorClass::Cancelled,
            Self::BudgetExceeded => ErrorClass::BudgetExceeded,
        }
    }
}

/// Errors from the supervision system.
#[derive(Debug, Error)]
pub enum SupervisorError {
    /// An agent has been restarted too many times within its restart window.
    #[error("restart limit exceeded for agent {agent_id}: {restarts} restarts")]
    RestartLimitExceeded { agent_id: AgentId, restarts: u32 },
    /// Failed to spawn a new agent task.
    #[error("failed to spawn agent: {0}")]
    SpawnFailed(String),
}

impl SupervisorError {
    /// Returns the [`ErrorClass`] for this error.
    pub fn classification(&self) -> ErrorClass {
        match self {
            Self::RestartLimitExceeded { .. } => ErrorClass::Permanent,
            Self::SpawnFailed(_) => ErrorClass::Transient,
        }
    }
}

/// Errors from tool execution (native or WASM).
#[derive(Debug, Error)]
pub enum ToolError {
    /// The WASM module triggered a trap (e.g. unreachable, out-of-bounds).
    #[error("WASM trap: {0}")]
    WasmTrap(String),
    /// The WASM module exhausted its fuel budget (likely an infinite loop).
    #[error("WASM fuel exhausted")]
    FuelExhausted,
    /// A native or WASM tool returned a runtime error.
    #[error("tool execution failed: {0}")]
    ExecutionFailed(String),
    /// No tool with the given name is registered.
    #[error("tool not found: {0}")]
    NotFound(String),
}

impl ToolError {
    /// Returns the [`ErrorClass`] for this error.
    pub fn classification(&self) -> ErrorClass {
        match self {
            Self::WasmTrap(_) => ErrorClass::Permanent,
            Self::FuelExhausted => ErrorClass::Overload,
            Self::ExecutionFailed(_) => ErrorClass::Transient,
            Self::NotFound(_) => ErrorClass::Permanent,
        }
    }
}

/// Errors from LLM provider calls.
#[derive(Debug, Error)]
pub enum LlmError {
    /// The LLM API returned an HTTP error. 5xx is transient, 4xx is permanent.
    #[error("API error (status {status}): {message}")]
    ApiError { status: u16, message: String },
    /// The LLM request timed out.
    #[error("LLM request timed out")]
    Timeout,
    /// The provider is rate-limiting requests.
    #[error("rate limited")]
    RateLimit,
    /// The API response could not be parsed.
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

impl LlmError {
    /// Returns the [`ErrorClass`] for this error.
    ///
    /// Server errors (5xx) are transient; client errors (4xx) are permanent.
    pub fn classification(&self) -> ErrorClass {
        match self {
            Self::ApiError { status, .. } if *status >= 500 => ErrorClass::Transient,
            Self::ApiError { .. } => ErrorClass::Permanent,
            Self::Timeout => ErrorClass::Transient,
            Self::RateLimit => ErrorClass::Overload,
            Self::InvalidResponse(_) => ErrorClass::Permanent,
        }
    }
}

/// Errors from the budget/quota system.
#[derive(Debug, Error)]
pub enum BudgetError {
    /// The total token limit for this run has been exceeded.
    #[error("token limit exceeded")]
    TokenLimitExceeded,
    /// The cost ceiling for this run has been exceeded.
    #[error("cost limit exceeded")]
    CostLimitExceeded,
    /// The admission semaphore denied the request (too many in-flight tasks).
    #[error("admission denied")]
    AdmissionDenied,
}

impl BudgetError {
    /// Returns the [`ErrorClass`] for this error.
    pub fn classification(&self) -> ErrorClass {
        match self {
            Self::TokenLimitExceeded | Self::CostLimitExceeded => ErrorClass::BudgetExceeded,
            Self::AdmissionDenied => ErrorClass::Overload,
        }
    }
}

/// Configuration validation errors.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

/// Top-level error type for the mra framework.
///
/// Each variant wraps a subsystem error and supports `?` conversion via `#[from]`.
#[derive(Debug, Error)]
pub enum MraError {
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Supervisor(#[from] SupervisorError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Budget(#[from] BudgetError),
    #[error(transparent)]
    Config(#[from] ConfigError),
}
