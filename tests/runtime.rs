use std::collections::HashMap;
use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, Task};
use mra::config::{AgentConfig, RuntimeConfig};
use mra::error::AgentError;
use mra::runtime::SwarmRuntime;

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

#[tokio::test]
async fn test_runtime_spawn_and_execute() {
    let mut runtime = SwarmRuntime::new(RuntimeConfig::default());

    let handle = runtime.spawn(
        "echo",
        AgentConfig::new("echo"),
        EchoBehavior,
        HashMap::new(),
        None,
    );

    let reply = handle.execute(Task::new("ping")).await.unwrap();
    assert_eq!(reply.output, "ping");

    runtime.shutdown().await;
}

#[tokio::test]
async fn test_runtime_shutdown_completes() {
    let mut runtime = SwarmRuntime::new(RuntimeConfig::default());
    runtime.spawn(
        "a",
        AgentConfig::new("a"),
        EchoBehavior,
        HashMap::new(),
        None,
    );
    runtime.spawn(
        "b",
        AgentConfig::new("b"),
        EchoBehavior,
        HashMap::new(),
        None,
    );

    let result = tokio::time::timeout(Duration::from_secs(5), runtime.shutdown()).await;
    assert!(result.is_ok(), "shutdown should complete within timeout");
}

#[tokio::test]
async fn test_runtime_get_handle_by_id() {
    let mut runtime = SwarmRuntime::new(RuntimeConfig::default());
    let handle = runtime.spawn(
        "echo",
        AgentConfig::new("echo"),
        EchoBehavior,
        HashMap::new(),
        None,
    );
    let id = handle.id();

    let looked_up = runtime.get_handle(id);
    assert!(looked_up.is_some());
    assert_eq!(looked_up.unwrap().id(), id);
}

#[tokio::test]
async fn test_runtime_get_handle_by_name() {
    let mut runtime = SwarmRuntime::new(RuntimeConfig::default());
    runtime.spawn(
        "echo",
        AgentConfig::new("echo"),
        EchoBehavior,
        HashMap::new(),
        None,
    );

    let looked_up = runtime.get_handle_by_name("echo");
    assert!(looked_up.is_some());

    let missing = runtime.get_handle_by_name("nonexistent");
    assert!(missing.is_none());
}
