//! File reading tool -- reads a file by path and returns its contents,
//! truncated to 64 KB with UTF-8-safe boundary handling.

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::ToolError;

use super::{Tool, ToolOutput, ToolSpec};

const MAX_OUTPUT_BYTES: usize = 65_536;

fn truncate(s: String) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        s
    } else {
        let mut end = MAX_OUTPUT_BYTES;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        let mut out = s[..end].to_string();
        out.push_str("\n...[truncated]");
        out
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    /// The file path to read
    path: String,
}

/// Reads a file from disk and returns its contents as a string.
///
/// Output is capped at 64 KB. I/O errors (missing file, permission
/// denied, etc.) are returned as `ToolOutput { is_error: true }` so
/// the LLM sees the error message instead of crashing the tool call.
pub struct ReadFileTool {
    spec: ToolSpec,
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadFileTool {
    /// Creates a new `ReadFileTool`.
    pub fn new() -> Self {
        let schema = schemars::schema_for!(ReadFileArgs);
        Self {
            spec: ToolSpec {
                name: "read_file".into(),
                description: "Read a file and return its contents".into(),
                parameters: serde_json::to_value(schema).unwrap(),
            },
        }
    }
}

impl Tool for ReadFileTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        Box::pin(async move {
            let parsed: ReadFileArgs =
                serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

            match tokio::fs::read_to_string(&parsed.path).await {
                Ok(content) => Ok(ToolOutput {
                    content: truncate(content),
                    is_error: false,
                }),
                Err(e) => Ok(ToolOutput {
                    content: e.to_string(),
                    is_error: true,
                }),
            }
        })
    }
}
