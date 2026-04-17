//! File reading tool -- reads a file by path and returns its contents,
//! truncated to a configurable limit (default 64 KB) with UTF-8-safe
//! boundary handling.

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::ToolError;

use super::{Tool, ToolOutput, ToolSpec, parse_args};

const MAX_OUTPUT_BYTES: usize = 65_536;

fn truncate(s: String, limit: usize) -> String {
    if s.len() <= limit {
        s
    } else {
        let mut end = limit;
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
/// Output is capped at `max_size` bytes (default 64 KB). I/O errors
/// (missing file, permission denied, etc.) are returned as
/// `ToolOutput { is_error: true }` so the LLM sees the error message
/// instead of crashing the tool call.
pub struct ReadFileTool {
    spec: ToolSpec,
    max_size: usize,
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for [`ReadFileTool`] with a configurable output size limit.
pub struct ReadFileToolBuilder {
    max_size: usize,
}

impl ReadFileToolBuilder {
    /// Sets the maximum file size in bytes before truncation.
    pub fn max_size(mut self, max_size: usize) -> Self {
        self.max_size = max_size;
        self
    }

    /// Builds the [`ReadFileTool`].
    pub fn build(self) -> ReadFileTool {
        ReadFileTool {
            spec: ToolSpec::from_schema::<ReadFileArgs>(
                "read_file",
                "Read a file and return its contents",
            ),
            max_size: self.max_size,
        }
    }
}

impl ReadFileTool {
    /// Creates a new `ReadFileTool` with the default 64 KB limit.
    pub fn new() -> Self {
        Self {
            spec: ToolSpec::from_schema::<ReadFileArgs>(
                "read_file",
                "Read a file and return its contents",
            ),
            max_size: MAX_OUTPUT_BYTES,
        }
    }

    /// Creates a `ReadFileTool` with a custom max size.
    pub fn with_max_size(max_size: usize) -> Self {
        Self::builder().max_size(max_size).build()
    }

    /// Returns a builder with default settings.
    pub fn builder() -> ReadFileToolBuilder {
        ReadFileToolBuilder {
            max_size: MAX_OUTPUT_BYTES,
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
            let parsed: ReadFileArgs = parse_args(args)?;

            match tokio::fs::read_to_string(&parsed.path).await {
                Ok(content) => Ok(ToolOutput {
                    content: truncate(content, self.max_size),
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
