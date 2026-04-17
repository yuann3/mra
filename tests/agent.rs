use std::collections::HashMap;
use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentReply, AgentSpawn, Task};
use mra::config::AgentConfig;
use mra::error::AgentError;
use tokio_util::sync::CancellationToken;

/// A simple behavior that echoes the instruction back as output.
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

/// A behavior that sleeps for a fixed duration before replying.
struct SlowBehavior {
    delay: Duration,
}

impl AgentBehavior for SlowBehavior {
    async fn handle(&mut self, _ctx: &mut AgentCtx, input: Task) -> Result<AgentReply, AgentError> {
        tokio::time::sleep(self.delay).await;
        Ok(AgentReply {
            task_id: input.id,
            output: format!("done after {:?}", self.delay),
            self_tokens: 0,
            total_tokens: 0,
        })
    }
}

#[tokio::test]
async fn test_agent_execute() {
    let cancel = CancellationToken::new();

    let spawned = AgentSpawn::new("echo", EchoBehavior).cancel(cancel).spawn();

    let task = Task::new("hello world");
    let reply = spawned.handle.execute(task).await.unwrap();
    assert_eq!(reply.output, "hello world");

    spawned.handle.cancel();
    spawned.join.await.unwrap();
}

#[tokio::test]
async fn test_agent_shutdown() {
    let cancel = CancellationToken::new();

    let spawned = AgentSpawn::new("echo", EchoBehavior).cancel(cancel).spawn();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    spawned.handle.shutdown(deadline).await;

    // JoinHandle should complete within a reasonable time
    let result = tokio::time::timeout(Duration::from_secs(2), spawned.join).await;
    assert!(result.is_ok(), "agent task should complete after shutdown");
}

#[tokio::test]
async fn test_agent_cancel() {
    let cancel = CancellationToken::new();

    let spawned = AgentSpawn::new("echo", EchoBehavior).cancel(cancel).spawn();

    spawned.handle.cancel();

    let result = tokio::time::timeout(Duration::from_secs(2), spawned.join).await;
    assert!(result.is_ok(), "agent task should complete after cancel");
}

#[tokio::test]
async fn test_agent_backpressure() {
    let cancel = CancellationToken::new();

    let spawned = AgentSpawn::from_config(
        AgentConfig::new("slow").with_mailbox_size(1),
        SlowBehavior {
            delay: Duration::from_millis(100),
        },
    )
    .cancel(cancel)
    .spawn();

    let handle1 = spawned.handle.clone();
    let handle2 = spawned.handle.clone();

    let t1 = tokio::spawn(async move { handle1.execute(Task::new("task1")).await });
    let t2 = tokio::spawn(async move { handle2.execute(Task::new("task2")).await });

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
    let cancel = CancellationToken::new();

    let spawned = AgentSpawn::new(
        "slow",
        SlowBehavior {
            delay: Duration::from_millis(50),
        },
    )
    .cancel(cancel)
    .spawn();

    // Initially not busy
    let state = *spawned.progress.borrow();
    assert!(!state.busy, "agent should start idle");

    // Send a task and give it a moment to start
    let handle = spawned.handle.clone();
    let task_handle = tokio::spawn(async move { handle.execute(Task::new("work")).await });

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
    let cancel = CancellationToken::new();

    let spawned = AgentSpawn::new("echo", EchoBehavior).cancel(cancel).spawn();

    // Cancel the agent so it stops
    spawned.handle.cancel();
    spawned.join.await.unwrap();

    // Now try to execute — should get Unavailable error (retry also fails)
    let result = spawned.handle.execute(Task::new("too late")).await;
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AgentError::Unavailable));
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

#[tokio::test]
async fn test_agent_delegates_to_peer() {
    struct DelegateBehavior;
    impl AgentBehavior for DelegateBehavior {
        async fn handle(
            &mut self,
            ctx: &mut AgentCtx,
            input: Task,
        ) -> Result<AgentReply, AgentError> {
            let echo_handle = ctx.peers.get("echo").expect("echo peer not found");
            let sub_task = Task::new(format!("delegated: {}", input.instruction));
            let reply = echo_handle.execute(sub_task).await?;
            Ok(AgentReply {
                task_id: input.id,
                output: format!("via-delegate: {}", reply.output),
                self_tokens: 0,
                total_tokens: 0,
            })
        }
    }

    let cancel = CancellationToken::new();

    let echo = AgentSpawn::new("echo", EchoBehavior)
        .cancel(cancel.clone())
        .spawn();

    let mut peers = HashMap::new();
    peers.insert("echo".into(), echo.handle.clone());
    let delegator = AgentSpawn::new("delegator", DelegateBehavior)
        .peers(peers)
        .cancel(cancel.clone())
        .spawn();

    let reply = delegator.handle.execute(Task::new("hello")).await.unwrap();
    assert_eq!(reply.output, "via-delegate: delegated: hello");

    cancel.cancel();
    echo.join.await.unwrap();
    delegator.join.await.unwrap();
}

#[tokio::test]
async fn test_agent_report_progress() {
    struct ProgressBehavior;
    impl AgentBehavior for ProgressBehavior {
        async fn handle(
            &mut self,
            ctx: &mut AgentCtx,
            input: Task,
        ) -> Result<AgentReply, AgentError> {
            tokio::time::sleep(Duration::from_millis(20)).await;
            ctx.report_progress();
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok(AgentReply {
                task_id: input.id,
                output: "done".into(),
                self_tokens: 0,
                total_tokens: 0,
            })
        }
    }

    let cancel = CancellationToken::new();
    let spawned = AgentSpawn::new("progress", ProgressBehavior)
        .cancel(cancel)
        .spawn();

    let handle = spawned.handle.clone();
    let task_handle = tokio::spawn(async move { handle.execute(Task::new("work")).await });

    tokio::time::sleep(Duration::from_millis(30)).await;
    let state = *spawned.progress.borrow();
    assert!(state.last_progress.elapsed() < Duration::from_millis(50));

    task_handle.await.unwrap().unwrap();
    spawned.handle.cancel();
    spawned.join.await.unwrap();
}

#[tokio::test]
async fn test_execute_with_timeout_succeeds_within_deadline() {
    let cancel = CancellationToken::new();

    let spawned = AgentSpawn::new("echo", EchoBehavior).cancel(cancel).spawn();

    let task = Task::new("hello timeout");
    let reply = spawned
        .handle
        .execute_with_timeout(task, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(reply.output, "hello timeout");

    spawned.handle.cancel();
    spawned.join.await.unwrap();
}

#[tokio::test]
async fn test_execute_with_timeout_returns_timeout_error() {
    struct HangBehavior;
    impl AgentBehavior for HangBehavior {
        async fn handle(
            &mut self,
            _ctx: &mut AgentCtx,
            _input: Task,
        ) -> Result<AgentReply, AgentError> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            unreachable!()
        }
    }

    let cancel = CancellationToken::new();

    let spawned = AgentSpawn::new("hanger", HangBehavior)
        .cancel(cancel)
        .spawn();

    let task = Task::new("will timeout");
    let result = spawned
        .handle
        .execute_with_timeout(task, Duration::from_millis(50))
        .await;

    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), AgentError::Timeout));

    spawned.handle.cancel();
    spawned.join.await.unwrap();
}
