//! Role system — named system prompt overlays loaded from `.mra/roles/<name>.md`
//!
//! Roles are plain Markdown files whose content becomes a System message prepended
//! before the user prompt in a single LLM call. They are loaded synchronously at
//! [`Runtime`](crate::runtime::Runtime) build time (startup, not in hot paths).
//!
//! # Layout
//!
//! ```text
//! .mra/
//! └── roles/
//!     ├── analyst.md     → role name "analyst"
//!     └── reviewer.md   → role name "reviewer"
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use mra::agent::AgentCtx;
//! use mra::llm::LlmRequest;
//!
//! // Inside an AgentBehavior::handle implementation:
//! # async fn example(ctx: &mra::agent::AgentCtx, req: &LlmRequest) {
//! if let Some(req_with_role) = ctx.with_role("analyst", req) {
//!     // req_with_role has the analyst system prompt prepended
//! }
//! # }
//! ```

use std::collections::HashMap;
use std::path::Path;

/// A loaded role — its name and Markdown system prompt content.
#[derive(Clone, Debug)]
pub struct Role {
    /// The stem of the `.md` filename (e.g. `"analyst"` for `analyst.md`).
    pub name: String,
    /// The full Markdown content used as the System message.
    pub system_prompt: String,
}

/// Loaded role registry. Populated at Runtime build time from `.mra/roles/`.
///
/// Cheap to clone — the inner map is wrapped in an `Arc` via standard clone
/// semantics for `HashMap` (each `RoleRegistry` is independent).
#[derive(Default, Clone, Debug)]
pub struct RoleRegistry {
    roles: HashMap<String, Role>,
}

impl RoleRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Loads all `.md` files from `dir`. The filename stem becomes the role name.
    ///
    /// If the directory does not exist or cannot be read, returns an empty
    /// registry without emitting an error — missing roles are not fatal at startup.
    pub fn load_from_dir(dir: &Path) -> Self {
        let mut registry = Self::new();

        let read_dir = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => return registry,
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }

            let name = match path.file_stem().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            let system_prompt = match std::fs::read_to_string(&path) {
                Ok(content) => content,
                Err(_) => continue,
            };

            registry.roles.insert(name.clone(), Role { name, system_prompt });
        }

        registry
    }

    /// Looks up a role by name. Returns `None` if not found.
    pub fn get(&self, name: &str) -> Option<&Role> {
        self.roles.get(name)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn role_registry_load_from_nonexistent_dir_returns_empty() {
        let registry = RoleRegistry::load_from_dir(Path::new("/nonexistent/path/roles"));
        assert!(registry.get("anything").is_none());
    }

    #[test]
    fn role_registry_loads_md_files() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("analyst.md");
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(f, "You are a careful financial analyst.").unwrap();
        drop(f);

        let registry = RoleRegistry::load_from_dir(dir.path());
        let role = registry.get("analyst").expect("analyst role should be found");
        assert_eq!(role.name, "analyst");
        assert!(role.system_prompt.contains("financial analyst"));
    }

    #[test]
    fn role_registry_get_missing_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let registry = RoleRegistry::load_from_dir(dir.path());
        assert!(registry.get("unknown").is_none());
    }

    #[test]
    fn role_registry_ignores_non_md_files() {
        let dir = tempfile::tempdir().unwrap();
        // Create a .txt file — should not be loaded
        std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();
        // Create a .md file — should be loaded
        std::fs::write(dir.path().join("coder.md"), "You are a coder.").unwrap();

        let registry = RoleRegistry::load_from_dir(dir.path());
        assert!(registry.get("coder").is_some());
        assert!(registry.get("notes").is_none());
    }
}
