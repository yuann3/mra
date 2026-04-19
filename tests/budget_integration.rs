use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, AgentSpawn, Task};
use mra::config::AgentConfig;
use mra::error::AgentError;
use mra::llm::{ChatMessage, LlmProvider, LlmRequest, LlmResponse, Role};
use mra::runtime::SwarmRuntime;
use mra::supervisor::{ChildContext, ChildRestart, ChildSpec, SpawnedChild, SupervisorConfig};

/// Mock LLM that returns a fixed number of tokens per call.
struct MockLlm(u64);

impl LlmProvider for MockLlm {
    fn chat<'a>(
        &'a self,
        _req: &'a LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, mra::error::LlmError>> + Send + 'a>> {
        let tokens = self.0;
        Box::pin(async move {
            Ok(LlmResponse {
                content: "mock".into(),
                prompt_tokens: tokens / 2,
                completion_tokens: tokens - tokens / 2,
                tool_calls: vec![],
            })
        })
    }
}

struct SimpleBehavior;

impl AgentBehavior for SimpleBehavior {
    async fn handle(&mut self, ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        let request = LlmRequest {
            model: None,
            messages: vec![ChatMessage {
                role: Role::User,
                content: input.instruction.clone(),
                tool_calls: vec![],
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            tools: None,
        };
        let response = ctx.chat(&request).await?;
        let tokens = response.total_tokens();
        Ok(AgentReply {
            task_id: input.id,
            output: response.content,
            self_tokens: tokens,
            total_tokens: tokens,
        })
    }
}

fn test_spec(name: &str, llm: Arc<dyn LlmProvider>) -> ChildSpec {
    let agent_name = name.to_string();
    ChildSpec::new(
        name,
        AgentConfig::new(name),
        Arc::new(move |ctx: ChildContext| {
            let llm = llm.clone();
            let name = agent_name.clone();
            Box::pin(async move {
                Ok(
                    AgentSpawn::from_config(AgentConfig::new(&name), SimpleBehavior)
                        .id(ctx.id)
                        .llm(llm)
                        .cancel(ctx.cancel)
                        .peers(ctx.peers)
                        .tools(ctx.tools)
                        .budget_opt(ctx.budget)
                        .spawn_child(),
                )
            })
                as Pin<
                    Box<
                        dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>>
                            + Send,
                    >,
                >
        }),
    )
    .with_restart(ChildRestart::Permanent)
}

#[tokio::test]
async fn test_budget_kill_switch() {
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlm(100)); // 100 tokens per call

    let runtime = SwarmRuntime::with_budget(SupervisorConfig::default(), 250);

    runtime.spawn(test_spec("agent", llm)).await.unwrap();

    let handle = runtime.get_handle_by_name("agent").await.unwrap();

    // First call: 100 tokens — under budget
    let r1 = handle.execute(Task::new("call1")).await;
    assert!(r1.is_ok(), "first call should succeed");

    // Second call: 200 tokens total — under budget
    let r2 = handle.execute(Task::new("call2")).await;
    assert!(r2.is_ok(), "second call should succeed");

    // Third call: 300 tokens total — exceeds 250 limit
    let r3 = handle.execute(Task::new("call3")).await;
    assert!(
        matches!(r3, Err(AgentError::BudgetExceeded)),
        "third call should fail with BudgetExceeded, got: {r3:?}"
    );

    // Verify usage
    let usage = runtime.token_usage().unwrap();
    assert!(usage.used >= 250, "global usage should be >= 250");

    runtime.shutdown().await;
}

#[tokio::test]
async fn test_per_agent_budget_limit() {
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlm(100));

    let runtime = SwarmRuntime::with_budget(SupervisorConfig::default(), 50_000);

    runtime
        .spawn(test_spec("agent", llm).with_token_budget(150))
        .await
        .unwrap();

    let handle = runtime.get_handle_by_name("agent").await.unwrap();

    // First call: 100 tokens — under per-agent limit of 150
    let r1 = handle.execute(Task::new("call1")).await;
    assert!(r1.is_ok());

    // Second call: 200 tokens — exceeds per-agent limit of 150
    let r2 = handle.execute(Task::new("call2")).await;
    assert!(matches!(r2, Err(AgentError::BudgetExceeded)));

    // Global usage should still show 200 (both calls charged)
    let usage = runtime.token_usage().unwrap();
    assert_eq!(usage.used, 200);

    // Per-agent usage
    let agent_usage = runtime.agent_token_usage("agent").unwrap();
    assert_eq!(agent_usage.used, 200);

    runtime.shutdown().await;
}

/// Verifies that supervisor-level LLM injection flows through to children
/// spawned via `from_behavior`, and `ctx.chat()` works. (Issue #10, test plan item 2)
#[tokio::test]
async fn test_supervisor_level_llm_chat_works_via_from_behavior() {
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlm(100));

    let config = SupervisorConfig::default().with_llm(llm);
    let (supervisor, _join) = mra::supervisor::SupervisorHandle::start(config);

    // from_behavior child — no manual LLM wiring needed
    let spec = ChildSpec::from_behavior(AgentConfig::new("writer"), |_ctx| SimpleBehavior);

    supervisor.start_child(spec).await.unwrap();
    let handle = supervisor.child("writer").await.unwrap();

    // ctx.chat() should succeed because LLM flows from supervisor config
    let reply = handle.execute(Task::new("hello")).await.unwrap();
    assert_eq!(reply.output, "mock");

    supervisor.shutdown().await;
}

/// Verifies that supervisor-level tools are available in child's ctx.tools.
/// (Issue #10, test plan item 3)
#[tokio::test]
async fn test_supervisor_level_tools_available_in_child() {
    use mra::error::ToolError;
    use mra::tool::{Tool, ToolOutput, ToolSpec};
    use serde_json::Value;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct DummyTool;
    impl Tool for DummyTool {
        fn spec(&self) -> &ToolSpec {
            // Leak a static spec for simplicity in test
            Box::leak(Box::new(ToolSpec {
                name: "dummy".into(),
                description: "test tool".into(),
                parameters: Value::Null,
            }))
        }
        fn invoke(
            &self,
            _args: Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<ToolOutput, ToolError>> + Send + '_>,
        > {
            Box::pin(async {
                Ok(ToolOutput {
                    content: "tool-ok".into(),
                    is_error: false,
                })
            })
        }
    }

    struct ToolCheckBehavior {
        found_tool: Arc<AtomicBool>,
    }

    impl AgentBehavior for ToolCheckBehavior {
        async fn handle(
            &mut self,
            ctx: &mut mra::agent::AgentCtx,
            task: Task,
        ) -> Result<AgentReply, AgentError> {
            self.found_tool
                .store(ctx.tools.get("dummy").is_some(), Ordering::SeqCst);
            Ok(AgentReply {
                task_id: task.id,
                output: "checked".into(),
                self_tokens: 0,
                total_tokens: 0,
            })
        }
    }

    let tools = mra::tool::ToolRegistry::new();
    tools.register(Arc::new(DummyTool)).unwrap();

    let found_tool = Arc::new(AtomicBool::new(false));
    let found_tool2 = found_tool.clone();

    let config = SupervisorConfig::default().with_tools(tools);
    let (supervisor, _join) = mra::supervisor::SupervisorHandle::start(config);

    let spec = ChildSpec::from_behavior(AgentConfig::new("tool-user"), move |_ctx| {
        ToolCheckBehavior {
            found_tool: found_tool2.clone(),
        }
    });

    supervisor.start_child(spec).await.unwrap();
    let handle = supervisor.child("tool-user").await.unwrap();
    handle.execute(Task::new("check")).await.unwrap();

    assert!(
        found_tool.load(Ordering::SeqCst),
        "from_behavior child should have supervisor-level tools in ctx.tools"
    );

    supervisor.shutdown().await;
}

#[tokio::test]
async fn test_no_budget_means_unlimited() {
    let llm: Arc<dyn LlmProvider> = Arc::new(MockLlm(100));

    let runtime = SwarmRuntime::new(SupervisorConfig::default());

    runtime.spawn(test_spec("agent", llm)).await.unwrap();

    let handle = runtime.get_handle_by_name("agent").await.unwrap();

    // Should work fine without any budget
    for i in 0..10 {
        let result = handle.execute(Task::new(format!("call{i}"))).await;
        assert!(result.is_ok(), "call {i} should succeed without budget");
    }

    // No budget configured
    assert!(runtime.token_usage().is_none());

    runtime.shutdown().await;
}
