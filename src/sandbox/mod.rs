//! Sandbox abstraction — isolated execution environments for agents.
//!
//! # Overview
//!
//! The core abstraction is the [`Sandbox`] trait: an object-safe interface for
//! running shell commands and reading/writing files within an isolated workspace.
//!
//! [`Workspace`] wraps a [`tempfile::TempDir`] and supports symlink-mounting real
//! host paths at named mount points. It is auto-deleted on drop.
//!
//! [`VirtualSandbox`] is the default implementation: it executes commands in the
//! workspace directory (via `sh -c`) and enforces path-traversal checks on all
//! file operations.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;

use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors that can occur inside a [`Sandbox`] operation.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// Wraps any [`std::io::Error`] from file or process operations.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Returned when a caller-supplied path would escape the workspace root.
    #[error("path traversal rejected: {0}")]
    PathTraversal(String),

    /// A catch-all for other errors.
    #[error("{0}")]
    Other(String),
}

// ── ExecOptions / ExecResult ──────────────────────────────────────────────────

/// Options for a sandbox [`Sandbox::exec`] call.
#[derive(Default)]
pub struct ExecOptions {
    /// Additional environment variables to inject into the child process.
    pub env: HashMap<String, String>,
    /// Optional data written to the child's stdin.
    pub stdin: Option<String>,
}

/// The result of a sandbox [`Sandbox::exec`] call.
///
/// A non-zero [`exit_code`](ExecResult::exit_code) does **not** cause an `Err`
/// return from `exec`; it is surfaced here so callers can decide what to do.
pub struct ExecResult {
    /// Captured stdout (UTF-8, lossily decoded).
    pub stdout: String,
    /// Captured stderr (UTF-8, lossily decoded).
    pub stderr: String,
    /// Process exit code (0 = success by convention).
    pub exit_code: i32,
}

// ── Sandbox trait ─────────────────────────────────────────────────────────────

/// Object-safe trait for an isolated execution environment.
///
/// All methods return boxed futures so the trait is usable as `dyn Sandbox`.
pub trait Sandbox: Send + 'static {
    /// Execute a shell command inside the sandbox.
    ///
    /// The command is run via `sh -c <cmd>` with the sandbox root as the
    /// working directory. A non-zero exit code is returned in
    /// [`ExecResult::exit_code`] rather than as an `Err`.
    fn exec<'a>(
        &'a mut self,
        cmd: &'a str,
        opts: ExecOptions,
    ) -> Pin<Box<dyn Future<Output = Result<ExecResult, SandboxError>> + Send + 'a>>;

    /// Read a workspace-relative file path.
    ///
    /// Returns [`SandboxError::PathTraversal`] if the resolved path lies
    /// outside the workspace root.
    fn read_file<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SandboxError>> + Send + 'a>>;

    /// Write content to a workspace-relative file path.
    ///
    /// Creates parent directories as needed. Returns
    /// [`SandboxError::PathTraversal`] if the resolved path lies outside the
    /// workspace root.
    fn write_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SandboxError>> + Send + 'a>>;

    /// Returns the absolute path to the sandbox root directory.
    fn root(&self) -> &Path;
}

// ── Workspace ─────────────────────────────────────────────────────────────────

/// A temporary directory that serves as the root of a sandbox workspace.
///
/// The underlying [`tempfile::TempDir`] is deleted automatically when `Workspace`
/// is dropped.
pub struct Workspace {
    dir: tempfile::TempDir,
}

impl Workspace {
    /// Create a new, empty workspace backed by a fresh temporary directory.
    pub fn new() -> Result<Self, SandboxError> {
        let dir = tempfile::TempDir::new()?;
        Ok(Self { dir })
    }

    /// Mount `real_path` into the workspace at the relative location `at`.
    ///
    /// Creates a symlink `<workspace_root>/<at>` → `real_path`. Intermediate
    /// parent directories inside the workspace are created automatically.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::PathTraversal`] if `at` would escape the
    /// workspace root, or [`SandboxError::Io`] on filesystem errors.
    pub fn mount(&mut self, at: &str, real_path: PathBuf) -> Result<(), SandboxError> {
        let link_path = self.dir.path().join(at);

        // Ensure the parent directory exists inside the workspace.
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Guard: the resolved link path must be inside the workspace.
        // We can't canonicalize the link path itself (it doesn't exist yet),
        // so we canonicalize the parent and re-append the final component.
        let parent = link_path
            .parent()
            .unwrap_or(self.dir.path())
            .canonicalize()?;
        let root = self.dir.path().canonicalize()?;
        if !parent.starts_with(&root) {
            return Err(SandboxError::PathTraversal(at.to_string()));
        }

        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_path, &link_path)?;

        #[cfg(not(unix))]
        {
            // On non-Unix platforms fall back to a directory junction for
            // directories and a hard-copy for files. For the purposes of this
            // codebase (macOS / Linux agents) the Unix branch is canonical.
            if real_path.is_dir() {
                std::os::windows::fs::symlink_dir(&real_path, &link_path)?;
            } else {
                std::os::windows::fs::symlink_file(&real_path, &link_path)?;
            }
        }

        Ok(())
    }

    /// Returns the path to the workspace root directory.
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

// ── VirtualSandbox ────────────────────────────────────────────────────────────

/// A [`Sandbox`] implementation backed by a [`Workspace`].
///
/// - **exec**: runs the command via `sh -c` with `cwd = workspace.path()`.
/// - **read_file / write_file**: enforces path-traversal checks before
///   touching the filesystem.
pub struct VirtualSandbox {
    workspace: Workspace,
}

impl VirtualSandbox {
    /// Create a `VirtualSandbox` with an empty workspace.
    pub fn new() -> Result<Self, SandboxError> {
        Ok(Self {
            workspace: Workspace::new()?,
        })
    }

    /// Create a `VirtualSandbox` and immediately mount `path` at `at`.
    ///
    /// Equivalent to `VirtualSandbox::new()` followed by
    /// `sandbox.workspace.mount(at, path)`.
    pub fn with_mount(at: &str, path: PathBuf) -> Result<Self, SandboxError> {
        let mut sandbox = Self::new()?;
        sandbox.workspace.mount(at, path)?;
        Ok(sandbox)
    }

    /// Resolve a caller-supplied relative path to an absolute path inside the
    /// workspace root, and verify it does not escape the root.
    ///
    /// For *existing* paths we canonicalize; for paths whose parent exists but
    /// the leaf does not (new files), we canonicalize the parent and re-append
    /// the leaf.
    fn safe_path(&self, rel: &str) -> Result<PathBuf, SandboxError> {
        let root = self.workspace.path();
        let joined = root.join(rel);

        let canonical = if joined.exists() {
            joined.canonicalize()?
        } else {
            // File doesn't exist yet (e.g., a write target).
            let parent = joined.parent().unwrap_or(root);
            let canon_parent = if parent.exists() {
                parent.canonicalize()?
            } else {
                // Create parents so we can canonicalize them.
                std::fs::create_dir_all(parent)?;
                parent.canonicalize()?
            };
            let file_name = joined
                .file_name()
                .ok_or_else(|| SandboxError::Other("path has no file name".into()))?;
            canon_parent.join(file_name)
        };

        let canon_root = root.canonicalize()?;
        if !canonical.starts_with(&canon_root) {
            return Err(SandboxError::PathTraversal(rel.to_string()));
        }

        Ok(canonical)
    }
}

impl Sandbox for VirtualSandbox {
    fn exec<'a>(
        &'a mut self,
        cmd: &'a str,
        opts: ExecOptions,
    ) -> Pin<Box<dyn Future<Output = Result<ExecResult, SandboxError>> + Send + 'a>> {
        Box::pin(async move {
            let mut child = Command::new("/bin/sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(self.workspace.path())
                .envs(&opts.env)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .stdin(if opts.stdin.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .kill_on_drop(true)
                .spawn()?;

            // Write stdin if provided.
            if let Some(input) = opts.stdin {
                if let Some(mut stdin_handle) = child.stdin.take() {
                    stdin_handle.write_all(input.as_bytes()).await?;
                    // Drop closes the pipe, signalling EOF to the child.
                }
            }

            let output = child.wait_with_output().await?;

            Ok(ExecResult {
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                exit_code: output.status.code().unwrap_or(-1),
            })
        })
    }

    fn read_file<'a>(
        &'a self,
        path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<String, SandboxError>> + Send + 'a>> {
        Box::pin(async move {
            let abs = self.safe_path(path)?;
            let content = tokio::fs::read_to_string(&abs).await?;
            Ok(content)
        })
    }

    fn write_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SandboxError>> + Send + 'a>> {
        Box::pin(async move {
            let abs = self.safe_path(path)?;
            if let Some(parent) = abs.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&abs, content).await?;
            Ok(())
        })
    }

    fn root(&self) -> &Path {
        self.workspace.path()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn virtual_sandbox_exec_runs_command() {
        let mut sb = VirtualSandbox::new().expect("sandbox creation");
        let result = sb
            .exec("echo hello", ExecOptions::default())
            .await
            .expect("exec");
        assert!(
            result.stdout.contains("hello"),
            "stdout was: {:?}",
            result.stdout
        );
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn virtual_sandbox_read_write_file() {
        let sb = VirtualSandbox::new().expect("sandbox creation");
        sb.write_file("greet.txt", "hello sandbox")
            .await
            .expect("write");
        let content = sb.read_file("greet.txt").await.expect("read");
        assert_eq!(content, "hello sandbox");
    }

    #[tokio::test]
    async fn virtual_sandbox_rejects_path_traversal() {
        let sb = VirtualSandbox::new().expect("sandbox creation");
        let err = sb
            .read_file("../secret")
            .await
            .expect_err("should have rejected traversal");
        assert!(
            matches!(err, SandboxError::PathTraversal(_)),
            "expected PathTraversal, got: {err}"
        );
    }

    #[tokio::test]
    async fn workspace_mount_creates_symlink() {
        // Create a real directory to mount.
        let real_dir = tempfile::TempDir::new().expect("real dir");
        std::fs::write(real_dir.path().join("hello.txt"), "hi").expect("write sentinel");

        let mut ws = Workspace::new().expect("workspace");
        ws.mount("mydir", real_dir.path().to_path_buf())
            .expect("mount");

        let link = ws.path().join("mydir");
        assert!(link.exists(), "mount point should exist");
        assert!(
            link.is_symlink() || link.is_dir(),
            "mount point should be a symlink or directory"
        );

        // Verify we can reach the sentinel file through the mount.
        let sentinel = link.join("hello.txt");
        assert!(sentinel.exists(), "sentinel file reachable through mount");
    }

    #[test]
    fn workspace_cleaned_up_on_drop() {
        let ws = Workspace::new().expect("workspace");
        let path = ws.path().to_path_buf();
        assert!(path.exists(), "workspace should exist before drop");
        drop(ws);
        assert!(!path.exists(), "workspace should be deleted after drop");
    }
}
