//! Extensible tool system for agent-invoked operations.
//!
//! Provides the [`Tool`] trait, [`ToolSpec`] (what the LLM sees),
//! [`ToolOutput`] (what comes back), and [`ToolRegistry`] for name-based
//! lookup and invocation.

mod read_file;
mod shell;

pub use read_file::ReadFileTool;
pub use shell::ShellTool;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ToolError;

/// What the LLM sees — sent as part of the LLM request so the model
/// knows which tools are available and how to call them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// What comes back from a tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

/// The unified interface every tool backend implements.
///
/// Uses `Pin<Box<dyn Future>>` for dyn-safety, matching [`LlmProvider`](crate::llm::LlmProvider).
pub trait Tool: Send + Sync + 'static {
    fn spec(&self) -> &ToolSpec;
    fn invoke(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>>;
}

/// Name-based registry of tools. Cheap to clone (just `Arc` clones).
#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), crate::error::ToolError> {
        let name = tool.spec().name.clone();
        if self.tools.contains_key(&name) {
            return Err(crate::error::ToolError::InvalidArgs(
                format!("tool already registered: {name}"),
            ));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    pub fn specs(&self) -> Vec<&ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

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
