use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{Value, json};

use mra::error::{AgentError, LlmError, ToolError};
use mra::llm::{ChatMessage, LlmProvider, LlmRequest, LlmResponse, Role, ToolCall};
use mra::tool::{ReadFileTool, ShellTool};
use mra::tool::{Tool, ToolOutput, ToolRegistry, ToolSpec};

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
    let result = registry
        .invoke("echo", json!({"text": "world"}))
        .await
        .unwrap();
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
    let result = tool
        .invoke(json!({"path": "/nonexistent/file.txt"}))
        .await
        .unwrap();
    assert!(result.is_error);
}

#[tokio::test]
async fn read_file_tool_invalid_args() {
    let tool = ReadFileTool::new();
    let result = tool.invoke(json!({"wrong": "field"})).await;
    assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
}

// --- EditFileTool tests ---

#[test]
fn edit_file_tool_spec() {
    let tool = mra::tool::EditFileTool::new();
    let spec = tool.spec();
    assert_eq!(spec.name, "edit_file");
    assert!(spec.parameters.is_object());
    assert!(!spec.description.is_empty());
}

#[tokio::test]
async fn edit_file_tool_successful_replace() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, "hello world").unwrap();

    let tool = mra::tool::EditFileTool::new();
    let result = tool
        .invoke(json!({
            "path": path.to_str().unwrap(),
            "old_text": "world",
            "new_text": "rust"
        }))
        .await
        .unwrap();

    assert!(!result.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello rust");
}

#[tokio::test]
async fn edit_file_tool_file_not_found() {
    let tool = mra::tool::EditFileTool::new();
    let result = tool
        .invoke(json!({
            "path": "/nonexistent/file.txt",
            "old_text": "a",
            "new_text": "b"
        }))
        .await
        .unwrap();

    assert!(result.is_error);
    assert!(result.content.contains("/nonexistent/file.txt"));
}

#[tokio::test]
async fn edit_file_tool_old_text_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, "hello world").unwrap();

    let tool = mra::tool::EditFileTool::new();
    let result = tool
        .invoke(json!({
            "path": path.to_str().unwrap(),
            "old_text": "nonexistent",
            "new_text": "anything"
        }))
        .await
        .unwrap();

    assert!(result.is_error);
    assert!(result.content.contains("not found"));
    // File should be unchanged
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world");
}

#[tokio::test]
async fn edit_file_tool_ambiguous_match() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, "aaa bbb aaa").unwrap();

    let tool = mra::tool::EditFileTool::new();
    let result = tool
        .invoke(json!({
            "path": path.to_str().unwrap(),
            "old_text": "aaa",
            "new_text": "ccc"
        }))
        .await
        .unwrap();

    assert!(result.is_error);
    assert!(result.content.contains("2 times"));
    // File should be unchanged
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "aaa bbb aaa");
}

#[tokio::test]
async fn edit_file_tool_deny_unknown_fields() {
    let tool = mra::tool::EditFileTool::new();
    let result = tool
        .invoke(json!({
            "path": "test.txt",
            "old_text": "a",
            "new_text": "b",
            "extra": "field"
        }))
        .await;

    assert!(matches!(result, Err(ToolError::InvalidArgs(_))));
}

#[tokio::test]
async fn edit_file_tool_unicode_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test.txt");
    std::fs::write(&path, "日本語のテスト文字列").unwrap();

    let tool = mra::tool::EditFileTool::new();
    let result = tool
        .invoke(json!({
            "path": path.to_str().unwrap(),
            "old_text": "テスト",
            "new_text": "サンプル"
        }))
        .await
        .unwrap();

    assert!(!result.is_error);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "日本語のサンプル文字列"
    );
}

// --- AgentCtx + spawn path integration ---

use std::collections::HashMap;

use tokio_util::sync::CancellationToken;

use mra::agent::{AgentBehavior, AgentCtx, AgentHandle, AgentReply, Task};
use mra::config::AgentConfig;
use mra::ids::AgentId;

struct ToolUsingBehavior;

impl AgentBehavior for ToolUsingBehavior {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let result = ctx
            .call_tool("echo", json!({"text": &input.instruction}))
            .await?;
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

    let reply = spawned
        .handle
        .execute(Task::new("hello from tool"))
        .await
        .unwrap();
    assert_eq!(reply.output, "hello from tool");
}

// --- chat_with_tools tests ---

/// Mock LLM that returns a scripted sequence of responses.
struct MockLlm {
    responses: std::sync::Mutex<Vec<LlmResponse>>,
    call_count: AtomicUsize,
}

impl MockLlm {
    fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses),
            call_count: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

impl LlmProvider for MockLlm {
    fn chat<'a>(
        &'a self,
        _request: &'a LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
        Box::pin(async move {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err(LlmError::InvalidResponse(
                    "no more scripted responses".into(),
                ));
            }
            Ok(responses.remove(0))
        })
    }
}

fn plain_response(content: &str, prompt: u64, completion: u64) -> LlmResponse {
    LlmResponse {
        content: content.into(),
        prompt_tokens: prompt,
        completion_tokens: completion,
        tool_calls: vec![],
    }
}

fn tool_call_response(calls: Vec<ToolCall>, prompt: u64, completion: u64) -> LlmResponse {
    LlmResponse {
        content: String::new(),
        prompt_tokens: prompt,
        completion_tokens: completion,
        tool_calls: calls,
    }
}

struct ChatWithToolsBehavior;

impl AgentBehavior for ChatWithToolsBehavior {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let tools_specs: Vec<_> = ctx.tools.specs().into_iter().cloned().collect();
        let request = LlmRequest {
            model: None,
            messages: vec![ChatMessage {
                role: Role::User,
                content: input.instruction,
                tool_calls: vec![],
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            tools: Some(tools_specs),
        };

        let result = ctx.chat_with_tools(&request, 10).await?;
        Ok(AgentReply {
            task_id: input.id,
            output: result.response.content,
            self_tokens: result.total_prompt_tokens + result.total_completion_tokens,
            total_tokens: result.total_prompt_tokens + result.total_completion_tokens,
        })
    }
}

fn spawn_tool_agent(llm: Arc<dyn LlmProvider>, registry: ToolRegistry) -> mra::agent::SpawnedAgent {
    AgentHandle::spawn(
        AgentId::new(),
        AgentConfig::new("test"),
        ChatWithToolsBehavior,
        HashMap::new(),
        Some(llm),
        CancellationToken::new(),
        None,
        registry,
    )
}

#[tokio::test]
async fn chat_with_tools_immediate_text() {
    let llm = Arc::new(MockLlm::new(vec![plain_response("hello", 10, 5)]));
    let registry = ToolRegistry::new();
    let spawned = spawn_tool_agent(llm.clone(), registry);

    let reply = spawned.handle.execute(Task::new("hi")).await.unwrap();
    assert_eq!(reply.output, "hello");
    assert_eq!(reply.self_tokens, 15); // 10 + 5
    assert_eq!(llm.calls(), 1);
}

#[tokio::test]
async fn chat_with_tools_one_tool_round() {
    // LLM calls echo tool, then returns final text
    let llm = Arc::new(MockLlm::new(vec![
        tool_call_response(
            vec![ToolCall {
                id: "call_1".into(),
                name: "echo".into(),
                arguments: json!({"text": "tool result"}),
            }],
            10,
            5,
        ),
        plain_response("done", 20, 10),
    ]));

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool::new())).unwrap();
    let spawned = spawn_tool_agent(llm.clone(), registry);

    let reply = spawned.handle.execute(Task::new("test")).await.unwrap();
    assert_eq!(reply.output, "done");
    // Total tokens: (10+5) + (20+10) = 45
    assert_eq!(reply.self_tokens, 45);
    assert_eq!(llm.calls(), 2);
}

#[tokio::test]
async fn chat_with_tools_max_iterations() {
    // LLM always returns tool calls — should stop at max_iterations
    let responses: Vec<_> = (0..10)
        .map(|i| {
            tool_call_response(
                vec![ToolCall {
                    id: format!("call_{i}"),
                    name: "echo".into(),
                    arguments: json!({"text": "loop"}),
                }],
                10,
                5,
            )
        })
        .collect();

    let llm = Arc::new(MockLlm::new(responses));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool::new())).unwrap();
    let spawned = spawn_tool_agent(llm.clone(), registry);

    let reply = spawned.handle.execute(Task::new("loop")).await.unwrap();
    // Should have made exactly 10 LLM calls (max_iterations)
    assert_eq!(llm.calls(), 10);
    // Output is empty because last response was a tool call
    assert_eq!(reply.output, "");
}

#[tokio::test]
async fn chat_with_tools_tool_error_is_non_fatal() {
    // LLM calls a nonexistent tool, then gets the error and responds with text
    let llm = Arc::new(MockLlm::new(vec![
        tool_call_response(
            vec![ToolCall {
                id: "call_1".into(),
                name: "nonexistent".into(),
                arguments: json!({}),
            }],
            10,
            5,
        ),
        plain_response("recovered", 20, 10),
    ]));

    let registry = ToolRegistry::new(); // empty — no tools registered
    let spawned = spawn_tool_agent(llm.clone(), registry);

    let reply = spawned.handle.execute(Task::new("test")).await.unwrap();
    assert_eq!(reply.output, "recovered");
    assert_eq!(llm.calls(), 2);
}

#[tokio::test]
async fn chat_with_tools_token_aggregation() {
    // Three iterations: tool call, tool call, final text
    let llm = Arc::new(MockLlm::new(vec![
        tool_call_response(
            vec![ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                arguments: json!({"text": "a"}),
            }],
            100,
            50,
        ),
        tool_call_response(
            vec![ToolCall {
                id: "c2".into(),
                name: "echo".into(),
                arguments: json!({"text": "b"}),
            }],
            200,
            100,
        ),
        plain_response("final", 300, 150),
    ]));

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool::new())).unwrap();
    let spawned = spawn_tool_agent(llm.clone(), registry);

    let reply = spawned.handle.execute(Task::new("test")).await.unwrap();
    assert_eq!(reply.output, "final");
    // Total: (100+50) + (200+100) + (300+150) = 900
    assert_eq!(reply.self_tokens, 900);
    assert_eq!(llm.calls(), 3);
}

#[tokio::test]
async fn chat_with_tools_budget_exceeded_mid_loop() {
    use mra::budget::BudgetTracker;

    // First LLM call returns tool calls (150 tokens, exceeds budget of 100).
    // Second LLM call should be blocked by budget pre-check.
    let llm = Arc::new(MockLlm::new(vec![
        tool_call_response(
            vec![ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                arguments: json!({"text": "a"}),
            }],
            100,
            50,
        ),
        // This should never be reached
        plain_response("unreachable", 100, 50),
    ]));

    let budget = Arc::new(
        BudgetTracker::builder()
            .global_limit(100)
            .build_unconnected(),
    );
    budget.register_agent("test", None);

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool::new())).unwrap();

    let spawned = AgentHandle::spawn(
        AgentId::new(),
        AgentConfig::new("test"),
        ChatWithToolsBehavior,
        HashMap::new(),
        Some(llm.clone()),
        CancellationToken::new(),
        Some(budget),
        registry,
    );

    let result = spawned.handle.execute(Task::new("test")).await;
    // First chat() charges 150 tokens against a 100 limit — trips budget.
    // Second chat() in the loop hits the pre-check and returns BudgetExceeded.
    assert!(matches!(result, Err(AgentError::BudgetExceeded)));
    assert_eq!(llm.calls(), 1); // only first call went through
}
