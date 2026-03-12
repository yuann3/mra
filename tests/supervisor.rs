use mra::supervisor::ChildExit;

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
