//! File editing tool -- search-and-replace within a file.
//!
//! Reads a file, finds exactly one occurrence of `old_text`, swaps it
//! for `new_text`, and writes back. If the match is missing or ambiguous,
//! the tool returns an error message (not a Rust error) so the LLM can
//! try again with better arguments.

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::ToolError;

use super::{Tool, ToolOutput, ToolSpec};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditFileArgs {
    /// Absolute or relative path to the file.
    path: String,
    /// The exact text to find. Must appear exactly once in the file;
    /// the tool refuses ambiguous edits.
    old_text: String,
    /// The text that replaces `old_text`.
    new_text: String,
}

/// Search-and-replace tool for surgical file edits.
///
/// Two kinds of errors:
///
/// - **Domain errors** (file not found, text not found, multiple matches):
///   returned as `Ok(ToolOutput { is_error: true })` so the LLM sees the
///   message and can adjust its next call.
/// - **Bad arguments** (unknown fields, wrong types): returned as
///   `Err(ToolError::InvalidArgs)`. These are programming errors.
pub struct EditFileTool {
    spec: ToolSpec,
}

impl Default for EditFileTool {
    fn default() -> Self {
        Self::new()
    }
}

impl EditFileTool {
    /// Creates a new `EditFileTool` with an auto-generated JSON Schema.
    pub fn new() -> Self {
        let schema = schemars::schema_for!(EditFileArgs);
        Self {
            spec: ToolSpec {
                name: "edit_file".into(),
                description: "Edit a file by replacing an exact text match with new text".into(),
                parameters: serde_json::to_value(schema).unwrap(),
            },
        }
    }
}

impl Tool for EditFileTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        Box::pin(async move {
            let parsed: EditFileArgs =
                serde_json::from_value(args).map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

            if parsed.old_text.is_empty() {
                return Ok(ToolOutput {
                    content: "old_text must be non-empty".into(),
                    is_error: true,
                });
            }

            let content = match tokio::fs::read_to_string(&parsed.path).await {
                Ok(c) => c,
                Err(e) => {
                    return Ok(ToolOutput {
                        content: format!("Failed to read {}: {e}", parsed.path),
                        is_error: true,
                    });
                }
            };

            // Count overlapping occurrences by advancing one character at a time.
            let mut count = 0usize;
            for (idx, _) in content.char_indices() {
                if content[idx..].starts_with(&parsed.old_text) {
                    count += 1;
                    if count > 1 {
                        break;
                    }
                }
            }

            if count == 0 {
                return Ok(ToolOutput {
                    content: format!("old_text not found in {}", parsed.path),
                    is_error: true,
                });
            }

            if count > 1 {
                return Ok(ToolOutput {
                    content: format!(
                        "old_text appears {count} times in {} (must be unique)",
                        parsed.path
                    ),
                    is_error: true,
                });
            }

            let new_content = content.replacen(&parsed.old_text, &parsed.new_text, 1);

            if let Err(e) = tokio::fs::write(&parsed.path, &new_content).await {
                return Ok(ToolOutput {
                    content: format!("Failed to write {}: {e}", parsed.path),
                    is_error: true,
                });
            }

            Ok(ToolOutput {
                content: format!(
                    "Replaced {} bytes with {} bytes in {}",
                    parsed.old_text.len(),
                    parsed.new_text.len(),
                    parsed.path
                ),
                is_error: false,
            })
        })
    }
}
