//! Shell command tool.

use std::future::Future;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::ToolError;

use super::{Tool, ToolOutput, ToolSpec};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShellArgs {
    /// The shell command to execute
    command: String,
}

const MAX_OUTPUT_BYTES: usize = 32_768;

fn truncate(s: &str) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        s.to_string()
    } else {
        let mut out = s[..MAX_OUTPUT_BYTES].to_string();
        out.push_str("\n...[truncated]");
        out
    }
}

pub struct ShellTool {
    spec: ToolSpec,
    timeout: Duration,
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellTool {
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(30))
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        let schema = schemars::schema_for!(ShellArgs);
        Self {
            spec: ToolSpec {
                name: "shell".into(),
                description: "Run a shell command".into(),
                parameters: serde_json::to_value(schema).unwrap(),
            },
            timeout,
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
            let parsed: ShellArgs = serde_json::from_value(args)
                .map_err(|e| ToolError::InvalidArgs(e.to_string()))?;

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

            match result {
                Ok(Ok(output)) => {
                    if output.status.success() {
                        Ok(ToolOutput {
                            content: truncate(
                                &String::from_utf8_lossy(&output.stdout),
                            ),
                            is_error: false,
                        })
                    } else {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let content = if stderr.is_empty() {
                            truncate(&stdout)
                        } else {
                            truncate(&stderr)
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
