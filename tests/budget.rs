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

// Phase 3: set_global_limit, set_agent_limit, reset_agent, list_agents

#[test]
fn test_set_global_limit_untrips_when_raised() {
    let tracker = BudgetTracker::builder()
        .global_limit(100)
        .build_unconnected();

    // Exceed the global limit
    tracker.charge_global(60).unwrap();
    let result = tracker.charge_global(60);
    assert!(result.is_err());
    assert!(tracker.is_global_exceeded());

    // Raise the limit above current usage (120)
    tracker.set_global_limit(200);

    // Should be un-tripped now
    assert!(!tracker.is_global_exceeded());

    // Further charges under new limit should succeed
    assert!(tracker.charge_global(50).is_ok());
    assert_eq!(tracker.run_usage().used, 170);
    assert_eq!(tracker.run_usage().limit, Some(200));
}

#[test]
fn test_set_global_limit_stays_tripped_when_insufficient() {
    let tracker = BudgetTracker::builder()
        .global_limit(100)
        .build_unconnected();

    // Exceed the global limit
    tracker.charge_global(60).unwrap();
    let _ = tracker.charge_global(60); // exceeds, used=120
    assert!(tracker.is_global_exceeded());

    // Set limit still below used (120)
    tracker.set_global_limit(110);

    // Should still be tripped
    assert!(tracker.is_global_exceeded());
    assert_eq!(tracker.run_usage().limit, Some(110));
}

#[test]
fn test_set_agent_limit_untrips_when_raised() {
    let tracker = BudgetTracker::builder()
        .global_limit(10_000)
        .build_unconnected();

    tracker.register_agent("writer", Some(100));

    // Exceed agent limit
    tracker.charge("writer", 60).unwrap();
    let result = tracker.charge("writer", 60);
    assert!(result.is_err());
    assert!(tracker.is_agent_exceeded("writer"));

    // Raise the agent limit above current usage (120)
    tracker.set_agent_limit("writer", Some(200));

    // Should be un-tripped
    assert!(!tracker.is_agent_exceeded("writer"));

    // Further charges under new limit should succeed
    assert!(tracker.charge("writer", 50).is_ok());
    let usage = tracker.agent_usage("writer").unwrap();
    assert_eq!(usage.used, 170);
    assert_eq!(usage.limit, Some(200));
}

#[test]
fn test_reset_agent_clears_usage() {
    let tracker = BudgetTracker::builder()
        .global_limit(10_000)
        .build_unconnected();

    tracker.register_agent("writer", Some(500));
    tracker.charge("writer", 300).unwrap();
    assert_eq!(tracker.agent_usage("writer").unwrap().used, 300);

    // Exceed the agent to trip it
    let _ = tracker.charge("writer", 300); // used=600, limit=500 -> tripped
    assert!(tracker.is_agent_exceeded("writer"));

    // Reset the agent
    tracker.reset_agent("writer");

    // used should be 0, not tripped, limit preserved
    let usage = tracker.agent_usage("writer").unwrap();
    assert_eq!(usage.used, 0);
    assert_eq!(usage.limit, Some(500));
    assert!(!tracker.is_agent_exceeded("writer"));
}

#[test]
fn test_list_agents_returns_all() {
    let tracker = BudgetTracker::builder()
        .global_limit(10_000)
        .build_unconnected();

    tracker.register_agent("alpha", Some(100));
    tracker.register_agent("beta", Some(200));
    tracker.register_agent("gamma", None);

    tracker.charge("alpha", 10).unwrap();
    tracker.charge("beta", 50).unwrap();

    let agents = tracker.list_agents();
    assert_eq!(agents.len(), 3);

    // Sort by name for deterministic assertions
    let mut agents = agents;
    agents.sort_by(|a, b| a.0.cmp(&b.0));

    assert_eq!(agents[0].0, "alpha");
    assert_eq!(agents[0].1.used, 10);
    assert_eq!(agents[0].1.limit, Some(100));

    assert_eq!(agents[1].0, "beta");
    assert_eq!(agents[1].1.used, 50);
    assert_eq!(agents[1].1.limit, Some(200));

    assert_eq!(agents[2].0, "gamma");
    assert_eq!(agents[2].1.used, 0);
    assert_eq!(agents[2].1.limit, None);
}
