use std::time::Duration;

use mra::config::{AgentConfig, RestartPolicy, RuntimeConfig};

#[test]
fn restart_policy_defaults() {
    let policy = RestartPolicy::default();
    assert_eq!(policy.max_restarts, 5);
    assert_eq!(policy.window, Duration::from_secs(60));
    assert_eq!(policy.backoff_base, Duration::from_secs(1));
    assert_eq!(policy.backoff_max, Duration::from_secs(30));
}

#[test]
fn agent_config_defaults() {
    let config = AgentConfig::new("test-agent");
    assert_eq!(config.name, "test-agent");
    assert_eq!(config.mailbox_size, 32);
}

#[test]
fn agent_config_custom_mailbox() {
    let config = AgentConfig::new("custom").with_mailbox_size(64);
    assert_eq!(config.mailbox_size, 64);
}

#[test]
fn agent_config_custom_restart_policy() {
    let policy = RestartPolicy {
        max_restarts: 10,
        ..Default::default()
    };
    let config = AgentConfig::new("custom").with_restart_policy(policy);
    assert_eq!(config.restart_policy.max_restarts, 10);
}

#[test]
fn runtime_config_defaults() {
    let config = RuntimeConfig::default();
    assert_eq!(config.max_agents, 100);
    assert_eq!(config.shutdown_timeout, Duration::from_secs(30));
}
