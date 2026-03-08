use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentHandle, AgentReply, Task};
use mra::config::AgentConfig;
use mra::error::AgentError;
use mra::ids::AgentId;
use tokio_util::sync::CancellationToken;

/// A simple behavior that echoes the instruction back as output.
struct EchoBehavior;

impl AgentBehavior for EchoBehavior {
    async fn handle(
        &mut self,
        _ctx: &mut AgentCtx,
        input: Task,
    ) -> Result<AgentReply, AgentError> {
        Ok(AgentReply {
            task_id: input.id,
            output: input.instruction.clone(),
            tokens_used: 0,
        })
    }
}

/// A behavior that sleeps for a fixed duration before replying.
struct SlowBehavior {
    delay: Duration,
}

impl AgentBehavior for SlowBehavior {
    async fn handle(
        &mut self,
        _ctx: &mut AgentCtx,
        input: Task,
    ) -> Result<AgentReply, AgentError> {
        tokio::time::sleep(self.delay).await;
        Ok(AgentReply {
            task_id: input.id,
            output: format!("done after {:?}", self.delay),
            tokens_used: 0,
        })
    }
}

#[tokio::test]
async fn test_agent_execute() {
    let config = AgentConfig::new("echo");
    let ctx = AgentCtx { id: AgentId::new() };
    let cancel = CancellationToken::new();

    let spawned = AgentHandle::spawn(config, EchoBehavior, ctx, cancel);

    let task = Task::new("hello world");
    let reply = spawned.handle.execute(task).await.unwrap();
    assert_eq!(reply.output, "hello world");

    spawned.handle.cancel();
    spawned.join.await.unwrap();
}

#[tokio::test]
async fn test_agent_shutdown() {
    let config = AgentConfig::new("echo");
    let ctx = AgentCtx { id: AgentId::new() };
    let cancel = CancellationToken::new();

    let spawned = AgentHandle::spawn(config, EchoBehavior, ctx, cancel);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    spawned.handle.shutdown(deadline).await;

    // JoinHandle should complete within a reasonable time
    let result = tokio::time::timeout(Duration::from_secs(2), spawned.join).await;
    assert!(result.is_ok(), "agent task should complete after shutdown");
}

#[tokio::test]
async fn test_agent_cancel() {
    let config = AgentConfig::new("echo");
    let ctx = AgentCtx { id: AgentId::new() };
    let cancel = CancellationToken::new();

    let spawned = AgentHandle::spawn(config, EchoBehavior, ctx, cancel);

    spawned.handle.cancel();

    let result = tokio::time::timeout(Duration::from_secs(2), spawned.join).await;
    assert!(result.is_ok(), "agent task should complete after cancel");
}

#[tokio::test]
async fn test_agent_backpressure() {
    let config = AgentConfig::new("slow").with_mailbox_size(1);
    let ctx = AgentCtx { id: AgentId::new() };
    let cancel = CancellationToken::new();

    let spawned = AgentHandle::spawn(
        config,
        SlowBehavior {
            delay: Duration::from_millis(100),
        },
        ctx,
        cancel,
    );

    let handle1 = spawned.handle.clone();
    let handle2 = spawned.handle.clone();

    let t1 = tokio::spawn(async move {
        handle1.execute(Task::new("task1")).await
    });
    let t2 = tokio::spawn(async move {
        handle2.execute(Task::new("task2")).await
    });

    // Both should eventually complete (second waits for channel space)
    let timeout = Duration::from_secs(5);
    let (r1, r2) = tokio::join!(
        tokio::time::timeout(timeout, t1),
        tokio::time::timeout(timeout, t2),
    );

    assert!(r1.is_ok(), "task1 should complete within timeout");
    assert!(r2.is_ok(), "task2 should complete within timeout");

    let r1 = r1.unwrap().unwrap().unwrap();
    let r2 = r2.unwrap().unwrap().unwrap();

    // Both completed
    assert!(!r1.output.is_empty());
    assert!(!r2.output.is_empty());

    spawned.handle.cancel();
    spawned.join.await.unwrap();
}

#[tokio::test]
async fn test_agent_progress_updates() {
    let config = AgentConfig::new("slow");
    let ctx = AgentCtx { id: AgentId::new() };
    let cancel = CancellationToken::new();

    let spawned = AgentHandle::spawn(
        config,
        SlowBehavior {
            delay: Duration::from_millis(50),
        },
        ctx,
        cancel,
    );

    // Initially not busy
    let state = *spawned.progress.borrow();
    assert!(!state.busy, "agent should start idle");

    // Send a task and give it a moment to start
    let handle = spawned.handle.clone();
    let task_handle = tokio::spawn(async move {
        handle.execute(Task::new("work")).await
    });

    // Wait for reply
    task_handle.await.unwrap().unwrap();

    // After completion, should no longer be busy
    let state = *spawned.progress.borrow();
    assert!(!state.busy, "agent should be idle after completing task");

    spawned.handle.cancel();
    spawned.join.await.unwrap();
}

#[tokio::test]
async fn test_agent_execute_after_channel_closed() {
    let config = AgentConfig::new("echo");
    let ctx = AgentCtx { id: AgentId::new() };
    let cancel = CancellationToken::new();

    let spawned = AgentHandle::spawn(config, EchoBehavior, ctx, cancel);

    // Cancel the agent so it stops
    spawned.handle.cancel();
    spawned.join.await.unwrap();

    // Now try to execute — should get Cancelled error
    let result = spawned.handle.execute(Task::new("too late")).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AgentError::Cancelled));
}

#[tokio::test]
async fn test_task_new_generates_unique_ids() {
    let t1 = Task::new("a");
    let t2 = Task::new("b");
    assert_ne!(t1.id, t2.id);
}

#[tokio::test]
async fn test_task_context_defaults_to_null() {
    let t = Task::new("hello");
    assert_eq!(t.context, serde_json::Value::Null);
}
