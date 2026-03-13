use mra::budget::BudgetTracker;

#[test]
fn test_charge_under_limit() {
    let tracker = BudgetTracker::builder()
        .global_limit(1000)
        .build_unconnected();

    assert!(tracker.charge_global(500).is_ok());
    let usage = tracker.run_usage();
    assert_eq!(usage.used, 500);
    assert_eq!(usage.limit, Some(1000));
}

#[test]
fn test_charge_exceeds_global_limit() {
    let tracker = BudgetTracker::builder()
        .global_limit(100)
        .build_unconnected();

    assert!(tracker.charge_global(60).is_ok());
    let result = tracker.charge_global(60);
    assert!(result.is_err());
}

#[test]
fn test_no_limit_means_unlimited() {
    let tracker = BudgetTracker::builder().build_unconnected();

    assert!(tracker.charge_global(999_999).is_ok());
    let usage = tracker.run_usage();
    assert_eq!(usage.used, 999_999);
    assert_eq!(usage.limit, None);
}

#[test]
fn test_multiple_charges_accumulate() {
    let tracker = BudgetTracker::builder()
        .global_limit(1000)
        .build_unconnected();

    tracker.charge_global(200).unwrap();
    tracker.charge_global(300).unwrap();
    assert_eq!(tracker.run_usage().used, 500);
}

// Per-agent tests

#[test]
fn test_per_agent_charge_under_limit() {
    let tracker = BudgetTracker::builder()
        .global_limit(10_000)
        .build_unconnected();

    tracker.register_agent("writer", Some(500));
    assert!(tracker.charge("writer", 200).is_ok());

    let usage = tracker.agent_usage("writer").unwrap();
    assert_eq!(usage.used, 200);
    assert_eq!(usage.limit, Some(500));
}

#[test]
fn test_per_agent_exceeds_own_limit() {
    let tracker = BudgetTracker::builder()
        .global_limit(10_000)
        .build_unconnected();

    tracker.register_agent("writer", Some(100));
    assert!(tracker.charge("writer", 60).is_ok());
    let result = tracker.charge("writer", 60);
    assert!(result.is_err());
}

#[test]
fn test_per_agent_also_charges_global() {
    let tracker = BudgetTracker::builder()
        .global_limit(10_000)
        .build_unconnected();

    tracker.register_agent("writer", Some(5000));
    tracker.charge("writer", 300).unwrap();

    assert_eq!(tracker.run_usage().used, 300);
    assert_eq!(tracker.agent_usage("writer").unwrap().used, 300);
}

#[test]
fn test_global_exceeded_via_per_agent_charges() {
    let tracker = BudgetTracker::builder()
        .global_limit(100)
        .build_unconnected();

    tracker.register_agent("a", None);
    tracker.register_agent("b", None);

    tracker.charge("a", 60).unwrap();
    let result = tracker.charge("b", 60);
    assert!(result.is_err());

    // Both counters should reflect actual spend even when global trips
    assert_eq!(tracker.agent_usage("b").unwrap().used, 60);
    assert_eq!(tracker.run_usage().used, 120);
}

#[test]
fn test_agent_no_limit_unlimited() {
    let tracker = BudgetTracker::builder().build_unconnected();

    tracker.register_agent("writer", None);
    assert!(tracker.charge("writer", 999_999).is_ok());
    assert_eq!(tracker.agent_usage("writer").unwrap().used, 999_999);
}

#[test]
fn test_unknown_agent_returns_none() {
    let tracker = BudgetTracker::builder().build_unconnected();
    assert!(tracker.agent_usage("ghost").is_none());
}
