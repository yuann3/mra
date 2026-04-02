//! WASM tool manifest (tool.toml) parsing.

use serde::Deserialize;
use serde_json::Value;

use super::limits::{DEFAULT_EPOCH_DEADLINE_TICKS, DEFAULT_MAX_MEMORY_BYTES, MAX_MEMORY_HARD_CAP};

/// Parsed contents of a `tool.toml` manifest file.
#[derive(Debug, Clone, Deserialize)]
pub struct WasmToolManifest {
    /// Tool name (used for registry lookup).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Tool version string.
    pub version: String,
    /// Path to the `.wasm` binary, relative to the manifest directory.
    pub wasm: String,
    /// Resource limit overrides.
    #[serde(default)]
    pub limits: WasmToolLimits,
    /// JSON Schema for tool parameters.
    #[serde(default)]
    pub parameters: Option<Value>,
}

/// Optional resource limit overrides from the manifest.
#[derive(Debug, Clone, Deserialize)]
pub struct WasmToolLimits {
    /// Maximum linear memory in bytes (clamped to hard cap).
    #[serde(default = "default_max_memory")]
    pub max_memory_bytes: usize,
    /// Epoch deadline in ticks.
    #[serde(default = "default_epoch_deadline")]
    pub epoch_deadline_ticks: u64,
}

impl Default for WasmToolLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            epoch_deadline_ticks: DEFAULT_EPOCH_DEADLINE_TICKS,
        }
    }
}

fn default_max_memory() -> usize {
    DEFAULT_MAX_MEMORY_BYTES
}

fn default_epoch_deadline() -> u64 {
    DEFAULT_EPOCH_DEADLINE_TICKS
}

/// Validated configuration derived from a manifest.
#[derive(Debug, Clone)]
pub struct WasmToolConfig {
    /// The parsed manifest.
    pub manifest: WasmToolManifest,
    /// Resolved absolute path to the `.wasm` binary.
    pub wasm_path: std::path::PathBuf,
}

impl WasmToolManifest {
    /// Parse and validate a manifest from TOML string content.
    pub fn parse(toml_str: &str) -> Result<Self, anyhow::Error> {
        let manifest: Self = toml::from_str(toml_str)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate the manifest fields and enforce hard caps.
    fn validate(&self) -> Result<(), anyhow::Error> {
        if self.name.is_empty() {
            anyhow::bail!("manifest: name must not be empty");
        }
        if self.description.is_empty() {
            anyhow::bail!("manifest: description must not be empty");
        }
        if self.wasm.is_empty() {
            anyhow::bail!("manifest: wasm path must not be empty");
        }
        if self.limits.max_memory_bytes > MAX_MEMORY_HARD_CAP {
            anyhow::bail!(
                "manifest: max_memory_bytes ({}) exceeds hard cap ({})",
                self.limits.max_memory_bytes,
                MAX_MEMORY_HARD_CAP
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_manifest() {
        let toml = r#"
            name = "my-tool"
            description = "A test tool"
            version = "1.0.0"
            wasm = "my_tool.wasm"

            [limits]
            max_memory_bytes = 134217728
            epoch_deadline_ticks = 100

            [parameters]
            type = "object"
            properties = { text = { type = "string" } }
        "#;

        let manifest = WasmToolManifest::parse(toml).unwrap();
        assert_eq!(manifest.name, "my-tool");
        assert_eq!(manifest.description, "A test tool");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.wasm, "my_tool.wasm");
        assert_eq!(manifest.limits.max_memory_bytes, 128 * 1024 * 1024);
        assert_eq!(manifest.limits.epoch_deadline_ticks, 100);
        assert!(manifest.parameters.is_some());
    }

    #[test]
    fn parse_minimal_manifest_applies_defaults() {
        let toml = r#"
            name = "minimal"
            description = "Minimal tool"
            version = "0.1.0"
            wasm = "minimal.wasm"
        "#;

        let manifest = WasmToolManifest::parse(toml).unwrap();
        assert_eq!(manifest.limits.max_memory_bytes, DEFAULT_MAX_MEMORY_BYTES);
        assert_eq!(
            manifest.limits.epoch_deadline_ticks,
            DEFAULT_EPOCH_DEADLINE_TICKS
        );
        assert!(manifest.parameters.is_none());
    }

    #[test]
    fn reject_memory_exceeding_hard_cap() {
        let toml = r#"
            name = "big"
            description = "Too much memory"
            version = "0.1.0"
            wasm = "big.wasm"

            [limits]
            max_memory_bytes = 536870912
        "#;

        let err = WasmToolManifest::parse(toml).unwrap_err();
        assert!(
            err.to_string().contains("hard cap"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn reject_missing_name() {
        let toml = r#"
            description = "No name"
            version = "0.1.0"
            wasm = "tool.wasm"
        "#;

        assert!(WasmToolManifest::parse(toml).is_err());
    }

    #[test]
    fn reject_empty_name() {
        let toml = r#"
            name = ""
            description = "Empty name"
            version = "0.1.0"
            wasm = "tool.wasm"
        "#;

        let err = WasmToolManifest::parse(toml).unwrap_err();
        assert!(err.to_string().contains("name"), "unexpected error: {err}");
    }
}
