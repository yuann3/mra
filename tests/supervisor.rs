use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use mra::agent::{AgentBehavior, AgentCtx, AgentHandle, AgentReply, Task};
use mra::config::AgentConfig;
use mra::error::AgentError;
use mra::supervisor::{
    ChildExit, ChildRestart, ChildSpec, RestartIntensity, ShutdownPolicy, SpawnedChild, Strategy,
    SupervisorConfig,
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
                    Box<dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>> + Send>,
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
                    Box<dyn Future<Output = Result<SpawnedChild, mra::error::SupervisorError>> + Send>,
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
