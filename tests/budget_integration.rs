use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use mra::agent::{AgentBehavior, AgentCtx, AgentHandle, AgentReply, Task};
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
            }],
            temperature: None,
            max_tokens: None,
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
                Ok(AgentHandle::spawn_child(
                    ctx.id,
                    AgentConfig::new(&name),
                    SimpleBehavior,
                    ctx.peers,
                    Some(llm),
                    ctx.cancel,
                    ctx.budget,
                ))
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
