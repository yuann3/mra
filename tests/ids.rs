use mra::ids::{AgentId, TaskId};

#[test]
fn agent_id_new_generates_unique_ids() {
    let a = AgentId::new();
    let b = AgentId::new();
    assert_ne!(a, b);
}

#[test]
fn task_id_new_generates_unique_ids() {
    let a = TaskId::new();
    let b = TaskId::new();
    assert_ne!(a, b);
}

#[test]
fn agent_id_display_shows_uuid() {
    let id = AgentId::new();
    let s = id.to_string();
    assert!(!s.is_empty());
    // Display should be just the UUID, not wrapped in "AgentId(...)"
    assert!(!s.contains("AgentId"));
}

#[test]
fn agent_id_clone_preserves_equality() {
    let id = AgentId::new();
    let cloned = id;
    assert_eq!(id, cloned);
}

#[test]
fn agent_id_debug_contains_uuid() {
    let id = AgentId::new();
    let debug = format!("{id:?}");
    assert!(debug.contains("AgentId"));
}

#[test]
fn ids_serialize_roundtrip() {
    let agent_id = AgentId::new();
    let json = serde_json::to_string(&agent_id).unwrap();
    let back: AgentId = serde_json::from_str(&json).unwrap();
    assert_eq!(agent_id, back);

    let task_id = TaskId::new();
    let json = serde_json::to_string(&task_id).unwrap();
    let back: TaskId = serde_json::from_str(&json).unwrap();
    assert_eq!(task_id, back);
}

#[test]
fn ids_usable_as_hash_keys() {
    use std::collections::HashMap;
    let mut map = HashMap::new();
    let id = AgentId::new();
    map.insert(id, "test");
    assert_eq!(map[&id], "test");
}
