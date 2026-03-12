use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentHandle, AgentReply, Task};
use mra::config::AgentConfig;
use mra::error::AgentError;
use mra::supervisor::{
    ChildContext, ChildExit, ChildRestart, ChildSpec, RestartIntensity, ShutdownPolicy,
    SpawnedChild, Strategy, SupervisorConfig, SupervisorEvent, SupervisorHandle,
};

#[test]
fn child_exit_normal_is_not_failure() {
    assert!(!ChildExit::Normal.is_failure());
}

#[test]
fn child_exit_shutdown_is_not_failure() {
    assert!(!ChildExit::Shutdown.is_failure());
}

#[test]
fn child_exit_failed_is_failure() {
    assert!(ChildExit::Failed("boom".into()).is_failure());
}

#[test]
fn supervisor_config_defaults() {
    let config = SupervisorConfig::default();
    assert!(matches!(config.strategy, Strategy::OneForOne));
    assert_eq!(config.hang_check_interval, Duration::from_secs(1));
}

#[test]
fn child_restart_transient_is_default() {
    let r = ChildRestart::default();
    assert!(matches!(r, ChildRestart::Transient));
}

#[test]
fn child_restart_should_restart_logic() {
    assert!(ChildRestart::Permanent.should_restart(false));
    assert!(ChildRestart::Permanent.should_restart(true));
    assert!(!ChildRestart::Transient.should_restart(false));
    assert!(ChildRestart::Transient.should_restart(true));
    assert!(!ChildRestart::Temporary.should_restart(false));
    assert!(!ChildRestart::Temporary.should_restart(true));
}

#[test]
fn restart_intensity_default() {
    let ri = RestartIntensity::default();
    assert_eq!(ri.max_restarts, 10);
    assert_eq!(ri.window, Duration::from_secs(60));
}

#[test]
fn child_spec_with_defaults() {
    struct DummyBehavior;
    impl AgentBehavior for DummyBehavior {
        async fn handle(
            &mut self,
            _ctx: &mut AgentCtx,
            input: Task,
        ) -> Result<AgentReply, AgentError> {
            Ok(AgentReply {
                task_id: input.id,
                output: String::new(),
                tokens_used: 0,
            })
        }
    }

    let spec = ChildSpec::new(
        "test",
        AgentConfig::new("test"),
        Arc::new(|ctx| {
            Box::pin(async move {
                Ok(AgentHandle::spawn_child(
                    ctx.id,
                    AgentConfig::new("test"),
                    DummyBehavior,
                    ctx.peers,
                    ctx.llm,
                    ctx.cancel,
                ))
            })
                as Pin<
                    Box<
                        dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>>
                            + Send,
                    >,
                >
        }),
    );

    assert_eq!(spec.name, "test");
    assert!(matches!(spec.restart, ChildRestart::Transient));
    assert!(spec.hang_timeout.is_none());
}

#[test]
fn child_spec_builder_methods() {
    struct DummyBehavior;
    impl AgentBehavior for DummyBehavior {
        async fn handle(
            &mut self,
            _ctx: &mut AgentCtx,
            input: Task,
        ) -> Result<AgentReply, AgentError> {
            Ok(AgentReply {
                task_id: input.id,
                output: String::new(),
                tokens_used: 0,
            })
        }
    }

    let spec = ChildSpec::new(
        "worker",
        AgentConfig::new("worker"),
        Arc::new(|ctx| {
            Box::pin(async move {
                Ok(AgentHandle::spawn_child(
                    ctx.id,
                    AgentConfig::new("worker"),
                    DummyBehavior,
                    ctx.peers,
                    ctx.llm,
                    ctx.cancel,
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
    .with_shutdown_policy(ShutdownPolicy {
        grace: Duration::from_secs(10),
    })
    .with_hang_timeout(Duration::from_secs(30));

    assert_eq!(spec.name, "worker");
    assert!(matches!(spec.restart, ChildRestart::Permanent));
    assert_eq!(spec.shutdown_policy.grace, Duration::from_secs(10));
    assert_eq!(spec.hang_timeout, Some(Duration::from_secs(30)));
}

// --- Task 8: SupervisorHandle + Runner tests ---

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
                    Box<
                        dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>>
                            + Send,
                    >,
                >
        }),
    )
}

#[tokio::test]
async fn test_supervisor_start_child_and_execute() {
    let (handle, join) = SupervisorHandle::start(SupervisorConfig::default());

    let agent = handle.start_child(echo_spec("echo")).await.unwrap();
    let reply = agent.execute(Task::new("hello")).await.unwrap();
    assert_eq!(reply.output, "hello");

    handle.shutdown().await;
    let _ = join.await;
}

#[tokio::test]
async fn test_supervisor_stop_child() {
    let (handle, join) = SupervisorHandle::start(SupervisorConfig::default());

    let _agent = handle.start_child(echo_spec("echo")).await.unwrap();
    handle.stop_child("echo").await.unwrap();

    // Give the child time to exit after cancel
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Agent should now be unavailable (channel closed after cancel)
    let agent = handle.child("echo").await;
    // The child exists but is dead — execute should fail
    if let Some(a) = agent {
        let result = a.execute(Task::new("too late")).await;
        assert!(result.is_err());
    }

    handle.shutdown().await;
    let _ = join.await;
}

#[tokio::test]
async fn test_supervisor_emits_events() {
    let (handle, join) = SupervisorHandle::start(SupervisorConfig::default());
    let mut events = handle.subscribe();

    // Drain the SupervisorStarted event
    let ev = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, SupervisorEvent::SupervisorStarted));

    handle.start_child(echo_spec("echo")).await.unwrap();

    let ev = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, SupervisorEvent::ChildStarted { .. }));

    handle.shutdown().await;
    let _ = join.await;
}

// --- Task 9: Crash detection and OneForOne restart with backoff ---

#[tokio::test]
async fn test_supervisor_restarts_crashed_transient_child() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let start_count = Arc::new(AtomicU32::new(0));
    let count = start_count.clone();

    let spec = ChildSpec::new(
        "crasher",
        AgentConfig::new("crasher").with_restart_policy(mra::config::RestartPolicy {
            max_restarts: 5,
            window: Duration::from_secs(60),
            backoff_base: Duration::from_millis(10),
            backoff_max: Duration::from_millis(100),
        }),
        Arc::new(move |ctx: ChildContext| {
            let count = count.clone();
            Box::pin(async move {
                let generation = ctx.generation;
                count.fetch_add(1, Ordering::SeqCst);

                if generation < 2 {
                    Ok(SpawnedChild::from_future(Box::pin(async {
                        ChildExit::Failed("crash".into())
                    })))
                } else {
                    Ok(AgentHandle::spawn_child(
                        ctx.id,
                        AgentConfig::new("crasher"),
                        EchoBehavior,
                        ctx.peers,
                        ctx.llm,
                        ctx.cancel,
                    ))
                }
            })
                as Pin<
                    Box<
                        dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>>
                            + Send,
                    >,
                >
        }),
    );

    let (handle, join) = SupervisorHandle::start(SupervisorConfig::default());
    let agent = handle.start_child(spec).await.unwrap();

    // Wait for restarts (backoff is 10ms base, so should be fast)
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Should have been started 3 times (gen 0: crash, gen 1: crash, gen 2: ok)
    assert!(
        start_count.load(Ordering::SeqCst) >= 3,
        "expected >= 3 starts, got {}",
        start_count.load(Ordering::SeqCst),
    );

    // After gen 2 starts successfully, agent should work
    let reply = agent.execute(Task::new("after restart")).await.unwrap();
    assert_eq!(reply.output, "after restart");

    handle.shutdown().await;
    let _ = join.await;
}

#[tokio::test]
async fn test_supervisor_temporary_child_not_restarted() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let start_count = Arc::new(AtomicU32::new(0));
    let count = start_count.clone();

    let spec = ChildSpec::new(
        "temp",
        AgentConfig::new("temp"),
        Arc::new(move |_ctx: ChildContext| {
            let count = count.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(SpawnedChild::from_future(Box::pin(async {
                    ChildExit::Failed("crash".into())
                })))
            })
                as Pin<
                    Box<
                        dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>>
                            + Send,
                    >,
                >
        }),
    )
    .with_restart(ChildRestart::Temporary);

    let (handle, join) = SupervisorHandle::start(SupervisorConfig::default());
    handle.start_child(spec).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Should only have started once — no restart for Temporary
    assert_eq!(start_count.load(Ordering::SeqCst), 1);

    handle.shutdown().await;
    let _ = join.await;
}

#[tokio::test]
async fn test_supervisor_transient_child_not_restarted_on_normal_exit() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let start_count = Arc::new(AtomicU32::new(0));
    let count = start_count.clone();

    let spec = ChildSpec::new(
        "normal-exit",
        AgentConfig::new("normal-exit"),
        Arc::new(move |_ctx: ChildContext| {
            let count = count.clone();
            Box::pin(async move {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(SpawnedChild::from_future(Box::pin(async {
                    ChildExit::Normal
                })))
            })
                as Pin<
                    Box<
                        dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>>
                            + Send,
                    >,
                >
        }),
    );

    let (handle, join) = SupervisorHandle::start(SupervisorConfig::default());
    handle.start_child(spec).await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Transient: should not restart on normal exit
    assert_eq!(start_count.load(Ordering::SeqCst), 1);

    handle.shutdown().await;
    let _ = join.await;
}

#[tokio::test]
async fn test_supervisor_child_lookup() {
    let (handle, join) = SupervisorHandle::start(SupervisorConfig::default());

    handle.start_child(echo_spec("worker")).await.unwrap();

    let found = handle.child("worker").await;
    assert!(found.is_some());

    let not_found = handle.child("nonexistent").await;
    assert!(not_found.is_none());

    handle.shutdown().await;
    let _ = join.await;
}

// --- Task 10: OneForAll restart strategy ---

#[tokio::test]
async fn test_supervisor_one_for_all_restarts_all_children() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let a_count = Arc::new(AtomicU32::new(0));
    let b_count = Arc::new(AtomicU32::new(0));

    let a_c = a_count.clone();
    let spec_a = ChildSpec::new(
        "a",
        AgentConfig::new("a"),
        Arc::new(move |ctx: ChildContext| {
            let a_c = a_c.clone();
            Box::pin(async move {
                a_c.fetch_add(1, Ordering::SeqCst);
                Ok(AgentHandle::spawn_child(
                    ctx.id,
                    AgentConfig::new("a"),
                    EchoBehavior,
                    ctx.peers,
                    ctx.llm,
                    ctx.cancel,
                ))
            })
                as Pin<
                    Box<
                        dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>>
                            + Send,
                    >,
                >
        }),
    );

    let b_c = b_count.clone();
    let spec_b = ChildSpec::new(
        "b",
        AgentConfig::new("b"),
        Arc::new(move |ctx: ChildContext| {
            let b_c = b_c.clone();
            Box::pin(async move {
                b_c.fetch_add(1, Ordering::SeqCst);
                // First start: fail immediately to trigger OneForAll
                if ctx.generation == 0 {
                    Ok(SpawnedChild::from_future(Box::pin(async {
                        ChildExit::Failed("crash".into())
                    })))
                } else {
                    Ok(AgentHandle::spawn_child(
                        ctx.id,
                        AgentConfig::new("b"),
                        EchoBehavior,
                        ctx.peers,
                        ctx.llm,
                        ctx.cancel,
                    ))
                }
            })
                as Pin<
                    Box<
                        dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>>
                            + Send,
                    >,
                >
        }),
    );

    let config = SupervisorConfig {
        strategy: Strategy::OneForAll,
        ..Default::default()
    };
    let (handle, join) = SupervisorHandle::start(config);

    // Start "a" first (healthy), then "b" (will crash immediately)
    handle.start_child(spec_a).await.unwrap();
    handle.start_child(spec_b).await.unwrap();

    // Wait for the crash + OneForAll restart
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Both should have been started at least 2 times (initial + restart)
    assert!(
        a_count.load(Ordering::SeqCst) >= 2,
        "expected a started >= 2 times, got {}",
        a_count.load(Ordering::SeqCst),
    );
    assert!(
        b_count.load(Ordering::SeqCst) >= 2,
        "expected b started >= 2 times, got {}",
        b_count.load(Ordering::SeqCst),
    );

    handle.shutdown().await;
    let _ = join.await;
}

// --- Task 11: Hang detection via ProgressState polling ---

#[tokio::test]
async fn test_supervisor_detects_hung_agent() {
    struct HangBehavior;
    impl AgentBehavior for HangBehavior {
        async fn handle(
            &mut self,
            _ctx: &mut AgentCtx,
            _input: Task,
        ) -> Result<AgentReply, AgentError> {
            // Hang forever without reporting progress
            tokio::time::sleep(Duration::from_secs(60)).await;
            unreachable!()
        }
    }

    let config = SupervisorConfig {
        hang_check_interval: Duration::from_millis(50),
        ..Default::default()
    };
    let (handle, join) = SupervisorHandle::start(config);
    let mut events = handle.subscribe();

    // Drain SupervisorStarted
    let _ = tokio::time::timeout(Duration::from_secs(1), events.recv()).await;

    let spec = ChildSpec::new(
        "hanger",
        AgentConfig::new("hanger"),
        Arc::new(move |ctx: ChildContext| {
            Box::pin(async move {
                Ok(AgentHandle::spawn_child(
                    ctx.id,
                    AgentConfig::new("hanger"),
                    HangBehavior,
                    ctx.peers,
                    ctx.llm,
                    ctx.cancel,
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
    .with_hang_timeout(Duration::from_millis(100));

    let agent = handle.start_child(spec).await.unwrap();

    // Drain ChildStarted event
    let _ = tokio::time::timeout(Duration::from_secs(1), events.recv()).await;

    // Send a task to make the agent busy (don't await — it will hang)
    let h = agent.clone();
    tokio::spawn(async move {
        let _ = h.execute(Task::new("hang forever")).await;
    });

    // Wait for hang detection
    let mut found_hang = false;
    for _ in 0..30 {
        match tokio::time::timeout(Duration::from_millis(200), events.recv()).await {
            Ok(Ok(SupervisorEvent::HangDetected { .. })) => {
                found_hang = true;
                break;
            }
            Ok(Ok(_)) => continue,
            _ => continue,
        }
    }
    assert!(found_hang, "should detect hung agent");

    handle.shutdown().await;
    let _ = join.await;
}
