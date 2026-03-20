use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Value, json};

use mra::error::ToolError;
use mra::tool::{Tool, ToolOutput, ToolRegistry, ToolSpec};
use mra::tool::{ShellTool, ReadFileTool};

/// A simple test tool that echoes its input.
struct EchoTool {
    spec: ToolSpec,
}

impl EchoTool {
    fn new() -> Self {
        Self {
            spec: ToolSpec {
                name: "echo".into(),
                description: "Echoes the input".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "text": { "type": "string" }
                    },
                    "required": ["text"]
                }),
            },
        }
    }
}

impl Tool for EchoTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        Box::pin(async move {
            let text = args["text"].as_str().unwrap_or("no text");
            Ok(ToolOutput {
                content: text.to_string(),
                is_error: false,
            })
        })
    }
}

#[test]
fn tool_spec_fields() {
    let tool = EchoTool::new();
    let spec = tool.spec();
    assert_eq!(spec.name, "echo");
    assert_eq!(spec.description, "Echoes the input");
    assert!(spec.parameters.is_object());
}

#[test]
fn tool_output_success() {
    let output = ToolOutput {
        content: "hello".into(),
        is_error: false,
    };
    assert_eq!(output.content, "hello");
    assert!(!output.is_error);
}

#[test]
fn tool_output_error() {
    let output = ToolOutput {
        content: "something went wrong".into(),
        is_error: true,
    };
    assert!(output.is_error);
}

#[tokio::test]
async fn tool_invoke_returns_echoed_text() {
    let tool = EchoTool::new();
    let result = tool.invoke(json!({"text": "hi"})).await.unwrap();
    assert_eq!(result.content, "hi");
    assert!(!result.is_error);
}

#[test]
fn registry_register_and_get() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool::new())).unwrap();
    assert!(registry.get("echo").is_some());
    assert!(registry.get("nonexistent").is_none());
}

#[test]
fn registry_specs_returns_all_specs() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool::new())).unwrap();
    let specs = registry.specs();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "echo");
}

#[tokio::test]
async fn registry_invoke_existing_tool() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool::new())).unwrap();
    let result = registry.invoke("echo", json!({"text": "world"})).await.unwrap();
    assert_eq!(result.content, "world");
}

#[tokio::test]
async fn registry_invoke_not_found() {
    let registry = ToolRegistry::new();
    let result = registry.invoke("missing", json!({})).await;
    assert!(matches!(result, Err(ToolError::NotFound(_))));
}

#[test]
fn registry_clone_shares_tools() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool::new())).unwrap();
    let cloned = registry.clone();
    assert!(cloned.get("echo").is_some());
}

#[test]
fn tool_is_dyn_safe() {
    let tool = EchoTool::new();
    let _dyn_tool: Arc<dyn Tool> = Arc::new(tool);
}

// --- ShellTool tests ---

#[test]
fn shell_tool_spec() {
    let tool = ShellTool::new();
    let spec = tool.spec();
    assert_eq!(spec.name, "shell");
    assert!(spec.parameters.is_object());
}

#[tokio::test]
async fn shell_tool_runs_command() {
    let tool = ShellTool::new();
    let result = tool.invoke(json!({"command": "echo hello"})).await.unwrap();
    assert_eq!(result.content.trim(), "hello");
    assert!(!result.is_error);
}

#[tokio::test]
async fn shell_tool_returns_error_on_failure() {
    let tool = ShellTool::new();
    let result = tool.invoke(json!({"command": "false"})).await.unwrap();
    assert!(result.is_error);
}

#[tokio::test]
async fn shell_tool_invalid_args() {
    let tool = ShellTool::new();
    let result = tool.invoke(json!({"wrong_field": 123})).await;
    assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
}

// --- ReadFileTool tests ---

#[test]
fn read_file_tool_spec() {
    let tool = ReadFileTool::new();
    let spec = tool.spec();
    assert_eq!(spec.name, "read_file");
    assert!(spec.parameters.is_object());
}

#[tokio::test]
async fn read_file_tool_reads_existing_file() {
    let tool = ReadFileTool::new();
    let result = tool.invoke(json!({"path": "Cargo.toml"})).await.unwrap();
    assert!(!result.is_error);
    assert!(result.content.contains("[package]"));
}

#[tokio::test]
async fn read_file_tool_error_on_missing_file() {
    let tool = ReadFileTool::new();
    let result = tool.invoke(json!({"path": "/nonexistent/file.txt"})).await.unwrap();
    assert!(result.is_error);
}

#[tokio::test]
async fn read_file_tool_invalid_args() {
    let tool = ReadFileTool::new();
    let result = tool.invoke(json!({"wrong": "field"})).await;
    assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
}

// --- AgentCtx + spawn path integration ---

use std::collections::HashMap;

use tokio_util::sync::CancellationToken;

use mra::agent::{AgentBehavior, AgentCtx, AgentHandle, AgentReply, Task};
use mra::config::AgentConfig;
use mra::error::AgentError;
use mra::ids::AgentId;

struct ToolUsingBehavior;

impl AgentBehavior for ToolUsingBehavior {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let result = ctx.call_tool("echo", json!({"text": &input.instruction})).await?;
        Ok(AgentReply {
            task_id: input.id,
            output: result.content,
            self_tokens: 0,
            total_tokens: 0,
        })
    }
}

#[tokio::test]
async fn agent_ctx_call_tool_works() {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool::new())).unwrap();

    let spawned = AgentHandle::spawn(
        AgentId::new(),
        AgentConfig::new("test"),
        ToolUsingBehavior,
        HashMap::new(),
        None,
        CancellationToken::new(),
        None,
        registry,
    );

    let reply = spawned.handle.execute(Task::new("hello from tool")).await.unwrap();
    assert_eq!(reply.output, "hello from tool");
}
