//! Extensible tool system for agent-invoked operations.
//!
//! Provides the [`Tool`] trait, [`ToolSpec`] (what the LLM sees),
//! [`ToolOutput`] (what comes back), and [`ToolRegistry`] for name-based
//! lookup and invocation.

mod edit_file;
mod read_file;
mod shell;

pub use edit_file::EditFileTool;
pub use read_file::ReadFileTool;
pub use shell::ShellTool;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ToolError;

/// Tool metadata sent to the LLM so it knows what tools exist and
/// how to call them. Serialized into the `tools` array of the chat
/// completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique name the LLM uses to invoke this tool (e.g. `"shell"`).
    pub name: String,
    /// Short human-readable description shown to the model.
    pub description: String,
    /// JSON Schema describing the expected arguments.
    pub parameters: Value,
}

impl ToolSpec {
    pub(crate) fn from_schema<T: JsonSchema>(name: &str, description: &str) -> Self {
        Self {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: serde_json::to_value(schemars::schema_for!(T))
                .expect("schemars-generated schema should always serialize"),
        }
    }
}

/// Result of a tool invocation, returned to the LLM as a `tool` message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    /// The text payload (stdout, file contents, error message, etc.).
    pub content: String,
    /// `true` when the tool hit an error but still produced a message
    /// worth showing the model (e.g. a non-zero exit code with stderr).
    pub is_error: bool,
}

/// Async interface that every tool backend implements.
///
/// Returns `Pin<Box<dyn Future>>` for object-safety, same pattern as
/// [`LlmProvider`](crate::llm::LlmProvider). The boxing cost is
/// negligible compared to actual I/O.
pub trait Tool: Send + Sync + 'static {
    /// Returns the spec that gets forwarded to the LLM.
    fn spec(&self) -> &ToolSpec;

    /// Runs the tool with the given JSON arguments.
    fn invoke(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>>;
}

/// Name-keyed collection of tools available to an agent.
///
/// Cheap to clone (inner tools are `Arc`). Passed through
/// [`AgentCtx`](crate::agent::AgentCtx) so behaviors can call tools
/// via [`AgentCtx::call_tool`](crate::agent::AgentCtx::call_tool).
#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Creates an empty registry.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use mra::tool::{ToolRegistry, ShellTool};
    ///
    /// let mut registry = ToolRegistry::new();
    /// registry.register(Arc::new(ShellTool::new())).unwrap();
    /// assert_eq!(registry.specs().len(), 1);
    /// ```
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Adds a tool. Returns an error if a tool with the same name is
    /// already registered.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), crate::error::ToolError> {
        let name = tool.spec().name.clone();
        if self.tools.contains_key(&name) {
            return Err(crate::error::ToolError::InvalidArgs(format!(
                "tool already registered: {name}"
            )));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    /// Returns specs for all registered tools, sorted by name.
    pub fn specs(&self) -> Vec<&ToolSpec> {
        let mut specs: Vec<_> = self.tools.values().map(|t| t.spec()).collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    /// Looks up a tool by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Looks up a tool by name and invokes it. Returns
    /// [`ToolError::NotFound`] if no such tool exists.
    pub async fn invoke(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::NotFound(name.to_string()))?;
        tool.invoke(args).await
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))
}
