//! Stdio transport: newline-delimited JSON-RPC over one child process.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;

use super::{JsonRpcChannel, MAX_MESSAGE_BYTES, McpError};

/// One live stdio server session.
///
/// The child is killed when the channel drops; stdout stays bounded by
/// [`MAX_MESSAGE_BYTES`] per line and stderr is discarded.
pub struct StdioChannel {
    child: Mutex<Child>,
    stdin: Mutex<tokio::process::ChildStdin>,
    stdout: Mutex<BufReader<tokio::process::ChildStdout>>,
    timeout: Duration,
}

impl StdioChannel {
    /// Spawns one server command.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Transport`] when the command cannot start or its
    /// pipes are missing.
    pub async fn spawn(
        command: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<Self, McpError> {
        let mut child = tokio::process::Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| McpError::Transport)?;
        let stdin = child.stdin.take().ok_or(McpError::Transport)?;
        let stdout = child.stdout.take().ok_or(McpError::Transport)?;
        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            timeout,
        })
    }
}

impl Drop for StdioChannel {
    fn drop(&mut self) {
        // Best-effort kill; the process may have already exited.
        let _ = self.child.get_mut().start_kill();
    }
}

#[async_trait::async_trait]
impl JsonRpcChannel for StdioChannel {
    async fn request(&self, id: u64, method: &str, params: Value) -> Result<Value, McpError> {
        let line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|_| McpError::Protocol)?;
        if line.len() > MAX_MESSAGE_BYTES {
            return Err(McpError::Oversized);
        }
        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(&line)
                .await
                .map_err(|_| McpError::Transport)?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|_| McpError::Transport)?;
            stdin.flush().await.map_err(|_| McpError::Transport)?;
        }
        let fut = self.wait_for(id);
        match tokio::time::timeout(self.timeout, fut).await {
            Ok(outcome) => outcome,
            Err(_) => Err(McpError::Timeout),
        }
    }

    async fn notify(&self, method: &str) -> Result<(), McpError> {
        let line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "method": method,
        }))
        .map_err(|_| McpError::Protocol)?;
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(&line)
            .await
            .map_err(|_| McpError::Transport)?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|_| McpError::Transport)?;
        stdin.flush().await.map_err(|_| McpError::Transport)?;
        Ok(())
    }
}

impl StdioChannel {
    async fn wait_for(&self, id: u64) -> Result<Value, McpError> {
        loop {
            let mut raw = String::new();
            let count = self
                .stdout
                .lock()
                .await
                .read_line(&mut raw)
                .await
                .map_err(|_| McpError::Transport)?;
            if count == 0 {
                return Err(McpError::Transport);
            }
            if raw.len() > MAX_MESSAGE_BYTES {
                return Err(McpError::Oversized);
            }
            if raw.trim().is_empty() {
                continue;
            }
            let value: Value = serde_json::from_str(raw.trim()).map_err(|_| McpError::Protocol)?;
            if value["id"].as_u64() != Some(id) {
                // Notifications and stale replies are skipped.
                continue;
            }
            if value["error"].as_object().is_some() {
                return Err(McpError::ServerError);
            }
            return Ok(value["result"].clone());
        }
    }
}
