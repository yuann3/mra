use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
use mra::config::AgentConfig;
use mra::error::AgentError;
use mra::supervisor::{ChildSpec, SupervisorConfig, SupervisorHandle};

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

fn echo_spec(name: &str) -> ChildSpec {
    ChildSpec::from_behavior(AgentConfig::new(name), |_| EchoBehavior)
}

#[tokio::test]
async fn test_runtime_spawn_and_execute() {
    let (supervisor, _join) = SupervisorHandle::start(SupervisorConfig::default());

    let handle = supervisor.start_child(echo_spec("echo")).await.unwrap();

    let reply = handle.execute(Task::new("ping")).await.unwrap();
    assert_eq!(reply.output, "ping");

    supervisor.shutdown().await;
}

#[tokio::test]
async fn test_runtime_shutdown_completes() {
    let (supervisor, join) = SupervisorHandle::start(SupervisorConfig::default());
    supervisor.start_child(echo_spec("a")).await.unwrap();
    supervisor.start_child(echo_spec("b")).await.unwrap();

    supervisor.shutdown().await;
    let result = tokio::time::timeout(Duration::from_secs(5), join).await;
    assert!(result.is_ok(), "shutdown should complete within timeout");
}

#[tokio::test]
async fn test_runtime_get_handle_by_name() {
    let (supervisor, _join) = SupervisorHandle::start(SupervisorConfig::default());
    supervisor.start_child(echo_spec("echo")).await.unwrap();

    let looked_up = supervisor.child("echo").await;
    assert!(looked_up.is_some());

    let missing = supervisor.child("nonexistent").await;
    assert!(missing.is_none());

    supervisor.shutdown().await;
}
