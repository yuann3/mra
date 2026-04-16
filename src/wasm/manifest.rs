//! WASM tool manifest (tool.toml) parsing.

use serde::Deserialize;
use serde_json::Value;

use super::WasmError;
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
    pub fn parse(toml_str: &str) -> Result<Self, WasmError> {
        let manifest: Self = toml::from_str(toml_str)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate the manifest fields and enforce hard caps.
    fn validate(&self) -> Result<(), WasmError> {
        if self.name.trim().is_empty() {
            return Err(WasmError::InvalidManifest(
                "manifest: name must not be empty".into(),
            ));
        }
        if self.description.trim().is_empty() {
            return Err(WasmError::InvalidManifest(
                "manifest: description must not be empty".into(),
            ));
        }
        if self.wasm.trim().is_empty() {
            return Err(WasmError::InvalidManifest(
                "manifest: wasm path must not be empty".into(),
            ));
        }
        if self.limits.max_memory_bytes > MAX_MEMORY_HARD_CAP {
            return Err(WasmError::InvalidManifest(format!(
                "manifest: max_memory_bytes ({}) exceeds hard cap ({})",
                self.limits.max_memory_bytes, MAX_MEMORY_HARD_CAP
            )));
        }
        Ok(())
    }
}
