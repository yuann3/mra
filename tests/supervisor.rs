use std::time::Duration;

use mra::supervisor::{ChildExit, ChildRestart, RestartIntensity, Strategy, SupervisorConfig};

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
