use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentHandle, AgentReply, Task};
use mra::config::AgentConfig;
use mra::error::AgentError;
use mra::runtime::SwarmRuntime;
use mra::supervisor::{ChildContext, ChildSpec, SpawnedChild, SupervisorConfig};

struct EchoBehavior;

impl AgentBehavior for EchoBehavior {
    async fn handle(&mut self, _ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        Ok(AgentReply {
            task_id: input.id,
            output: input.instruction.clone(),
            tokens_used: 0,
        })
    }
}

fn echo_spec(name: &str) -> ChildSpec {
    let agent_name = name.to_string();
    ChildSpec::new(
        name,
        AgentConfig::new(name),
        Arc::new(move |ctx: ChildContext| {
            let agent_name = agent_name.clone();
            Box::pin(async move {
                Ok(AgentHandle::spawn_child(
                    ctx.id,
                    AgentConfig::new(&agent_name),
                    EchoBehavior,
                    ctx.peers,
                    ctx.llm,
                    ctx.cancel,
                ))
            })
                as Pin<
                    Box<dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>> + Send>,
                >
        }),
    )
}

#[tokio::test]
async fn test_runtime_spawn_and_execute() {
    let runtime = SwarmRuntime::new(SupervisorConfig::default());

    let handle = runtime.spawn(echo_spec("echo")).await.unwrap();

    let reply = handle.execute(Task::new("ping")).await.unwrap();
    assert_eq!(reply.output, "ping");

    runtime.shutdown().await;
}

#[tokio::test]
async fn test_runtime_shutdown_completes() {
    let runtime = SwarmRuntime::new(SupervisorConfig::default());
    runtime.spawn(echo_spec("a")).await.unwrap();
    runtime.spawn(echo_spec("b")).await.unwrap();

    let result = tokio::time::timeout(Duration::from_secs(5), runtime.shutdown()).await;
    assert!(result.is_ok(), "shutdown should complete within timeout");
}

#[tokio::test]
async fn test_runtime_get_handle_by_name() {
    let runtime = SwarmRuntime::new(SupervisorConfig::default());
    runtime.spawn(echo_spec("echo")).await.unwrap();

    let looked_up = runtime.get_handle_by_name("echo").await;
    assert!(looked_up.is_some());

    let missing = runtime.get_handle_by_name("nonexistent").await;
    assert!(missing.is_none());

    runtime.shutdown().await;
}
