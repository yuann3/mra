//! Shell command tool -- runs a command via `/bin/sh -c` with a
//! configurable timeout, output truncation, and `kill_on_drop`.

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::ToolError;

use super::{Tool, ToolOutput, ToolSpec, parse_args};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShellArgs {
    /// The shell command to execute
    command: String,
}

const MAX_OUTPUT_BYTES: usize = 32_768;

fn truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
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

/// Runs shell commands on the host. The child process is killed if
/// the timeout expires or the future is dropped.
///
/// Output (stdout on success, stderr on failure) is capped at
/// `max_output` bytes (default 32 KB) with UTF-8-safe truncation.
pub struct ShellTool {
    spec: ToolSpec,
    timeout: Duration,
    max_output: usize,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for [`ShellTool`] with configurable timeout and output limit.
pub struct ShellToolBuilder {
    timeout: Duration,
    max_output: usize,
}

impl ShellToolBuilder {
    /// Sets the command timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sets the maximum output size in bytes before truncation.
    pub fn max_output(mut self, max_output: usize) -> Self {
        self.max_output = max_output;
        self
    }

    /// Builds the [`ShellTool`].
    pub fn build(self) -> ShellTool {
        ShellTool {
            spec: ToolSpec::from_schema::<ShellArgs>("shell", "Run a shell command"),
            timeout: self.timeout,
            max_output: self.max_output,
        }
    }
}

impl ShellTool {
    /// Creates a `ShellTool` with a 30-second timeout.
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(30))
    }

    /// Creates a `ShellTool` with a custom timeout.
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            spec: ToolSpec::from_schema::<ShellArgs>("shell", "Run a shell command"),
            timeout,
            max_output: MAX_OUTPUT_BYTES,
        }
    }

    /// Returns a builder with default settings.
    pub fn builder() -> ShellToolBuilder {
        ShellToolBuilder {
            timeout: Duration::from_secs(30),
            max_output: MAX_OUTPUT_BYTES,
        }
    }
}

impl Tool for ShellTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn invoke(
        &self,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + '_>> {
        Box::pin(async move {
            let parsed: ShellArgs = parse_args(args)?;

            let result = tokio::time::timeout(self.timeout, async {
                let child = tokio::process::Command::new("/bin/sh")
                    .arg("-c")
                    .arg(&parsed.command)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                child
                    .wait_with_output()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
            })
            .await;

            let max = self.max_output;
            match result {
                Ok(Ok(output)) => {
                    if output.status.success() {
                        Ok(ToolOutput {
                            content: truncate(&String::from_utf8_lossy(&output.stdout), max),
                            is_error: false,
                        })
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let content = if stderr.is_empty() {
                            truncate(&stdout, max)
                        } else {
                            truncate(&stderr, max)
                        };
                        Ok(ToolOutput {
                            content,
                            is_error: true,
                        })
                    }
                }
                Ok(Err(e)) => Err(e),
                Err(_) => Err(ToolError::ExecutionFailed("command timed out".into())),
            }
        })
    }
}
