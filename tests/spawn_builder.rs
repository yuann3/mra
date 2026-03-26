use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, AgentSpawn, Task};
use mra::budget::BudgetTracker;
use mra::config::AgentConfig;
use mra::error::{AgentError, LlmError};
use mra::llm::{LlmProvider, LlmRequest, LlmResponse};
use mra::supervisor::{ChildContext, ChildSpec, SpawnedChild, SupervisorConfig, SupervisorHandle};

struct EchoBehavior;

impl AgentBehavior for EchoBehavior {
    async fn handle(&mut self, _ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        Ok(AgentReply {
            task_id: input.id,
            output: input.instruction.clone(),
            self_tokens: 0,
            total_tokens: 0,
        })
    }
}

#[tokio::test]
async fn spawn_new_creates_working_agent() {
    let spawned = AgentSpawn::new("echo", EchoBehavior).spawn();
    let reply = spawned.handle.execute(Task::new("hello")).await.unwrap();
    assert_eq!(reply.output, "hello");
    spawned.handle.cancel();
    spawned.join.await.unwrap();
}

#[tokio::test]
async fn spawn_with_cancel_token_is_respected() {
    let cancel = CancellationToken::new();
    let spawned = AgentSpawn::new("echo", EchoBehavior)
        .cancel(cancel.clone())
        .spawn();

    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), spawned.join).await;
    assert!(result.is_ok(), "agent should exit when cancel token fires");
}

#[tokio::test]
async fn spawn_with_peers_enables_delegation() {
    struct DelegateBehavior;
    impl AgentBehavior for DelegateBehavior {
        async fn handle(
            &mut self,
            ctx: &mut AgentCtx,
            input: Task,
        ) -> Result<AgentReply, AgentError> {
            let echo = ctx.peers.get("echo").unwrap();
            let reply = echo.execute(Task::new(format!("via: {}", input.instruction))).await?;
            Ok(AgentReply {
                task_id: input.id,
                output: reply.output,
                self_tokens: 0,
                total_tokens: 0,
            })
        }
    }

    let cancel = CancellationToken::new();
    let echo = AgentSpawn::new("echo", EchoBehavior)
        .cancel(cancel.clone())
        .spawn();

    let delegator = AgentSpawn::new("delegator", DelegateBehavior)
        .peers(HashMap::from([("echo".into(), echo.handle.clone())]))
        .cancel(cancel.clone())
        .spawn();

    let reply = delegator.handle.execute(Task::new("hi")).await.unwrap();
    assert_eq!(reply.output, "via: hi");

    cancel.cancel();
    echo.join.await.unwrap();
    delegator.join.await.unwrap();
}

struct MockLlm;

impl LlmProvider for MockLlm {
    fn chat<'a>(
        &'a self,
        _req: &'a LlmRequest,
    ) -> Pin<Box<dyn Future<Output = Result<LlmResponse, LlmError>> + Send + 'a>> {
        Box::pin(async {
            Ok(LlmResponse {
                content: "mock reply".into(),
                prompt_tokens: 10,
                completion_tokens: 5,
                tool_calls: vec![],
            })
        })
    }
}

#[tokio::test]
async fn spawn_with_llm_and_budget() {
    struct ChatBehavior;
    impl AgentBehavior for ChatBehavior {
        async fn handle(
            &mut self,
            ctx: &mut AgentCtx,
            input: Task,
        ) -> Result<AgentReply, AgentError> {
            let req = LlmRequest {
                model: None,
                messages: vec![],
                temperature: None,
                max_tokens: None,
                tools: None,
            };
            let resp = ctx.chat(&req).await?;
            let tokens = resp.total_tokens();
            Ok(AgentReply {
                task_id: input.id,
                output: resp.content,
                self_tokens: tokens,
                total_tokens: tokens,
            })
        }
    }

    let budget = Arc::new(
        BudgetTracker::builder()
            .global_limit(1000)
            .build_unconnected(),
    );
    budget.register_agent("chat", None);

    let spawned = AgentSpawn::new("chat", ChatBehavior)
        .llm(Arc::new(MockLlm))
        .budget(budget.clone())
        .spawn();

    let reply = spawned.handle.execute(Task::new("test")).await.unwrap();
    assert_eq!(reply.output, "mock reply");
    assert_eq!(budget.run_usage().used, 15);

    spawned.handle.cancel();
    spawned.join.await.unwrap();
}

#[tokio::test]
async fn spawn_child_works_under_supervisor() {
    let (sup, join) = SupervisorHandle::start(SupervisorConfig::default());

    let spec = ChildSpec::new(
        "echo",
        AgentConfig::new("echo"),
        Arc::new(|ctx: ChildContext| {
            Box::pin(async move {
                Ok(AgentSpawn::from_config(AgentConfig::new("echo"), EchoBehavior)
                    .id(ctx.id)
                    .cancel(ctx.cancel)
                    .peers(ctx.peers)
                    .tools(ctx.tools)
                    .spawn_child())
            })
                as Pin<Box<dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>> + Send>>
        }),
    );

    let agent = sup.start_child(spec).await.unwrap();
    let reply = agent.execute(Task::new("supervised")).await.unwrap();
    assert_eq!(reply.output, "supervised");

    sup.shutdown().await;
    let _ = join.await;
}

#[tokio::test]
async fn from_behavior_creates_working_supervised_agent() {
    let (sup, join) = SupervisorHandle::start(SupervisorConfig::default());

    let spec = ChildSpec::from_behavior(AgentConfig::new("echo"), |_| EchoBehavior);
    let agent = sup.start_child(spec).await.unwrap();

    let reply = agent.execute(Task::new("from_behavior")).await.unwrap();
    assert_eq!(reply.output, "from_behavior");

    sup.shutdown().await;
    let _ = join.await;
}

#[tokio::test]
async fn from_behavior_closure_receives_child_context() {
    use std::sync::atomic::{AtomicU64, Ordering};

    let observed_gen = Arc::new(AtomicU64::new(999));
    let gen_ref = observed_gen.clone();

    let spec = ChildSpec::from_behavior(AgentConfig::new("gen-test"), move |ctx| {
        gen_ref.store(ctx.generation, Ordering::SeqCst);
        EchoBehavior
    });

    let (sup, join) = SupervisorHandle::start(SupervisorConfig::default());
    let agent = sup.start_child(spec).await.unwrap();

    // Factory was called with generation 0
    assert_eq!(observed_gen.load(Ordering::SeqCst), 0);

    let reply = agent.execute(Task::new("works")).await.unwrap();
    assert_eq!(reply.output, "works");

    sup.shutdown().await;
    let _ = join.await;
}

#[tokio::test]
async fn spawn_from_config_uses_custom_mailbox_size() {
    let config = AgentConfig::new("echo").with_mailbox_size(2);
    let spawned = AgentSpawn::from_config(config, EchoBehavior).spawn();
    let reply = spawned.handle.execute(Task::new("hi")).await.unwrap();
    assert_eq!(reply.output, "hi");
    spawned.handle.cancel();
    spawned.join.await.unwrap();
}
