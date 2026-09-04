use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use harness_capability::lsp::Lsp;
use harness_core::error::{Error, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

struct LspState {
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

/// 标准 stdio JSON-RPC LSP 客户端。命令可通过 `HARNESS_LSP_COMMAND` 配置。
pub struct LocalLsp {
    command: String,
    state: Mutex<Option<LspState>>,
    next_id: AtomicU64,
}

impl LocalLsp {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            command: command.into(),
            state: Mutex::new(None),
            next_id: AtomicU64::new(1),
        }
    }

    async fn write_message(input: &mut ChildStdin, value: &Value) -> Result<()> {
        let body = serde_json::to_vec(value)?;
        input
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await?;
        input.write_all(&body).await?;
        input.flush().await?;
        Ok(())
    }

    async fn read_message<R: AsyncBufRead + Unpin>(output: &mut R) -> Result<Value> {
        let mut content_len = None;
        loop {
            let mut line = String::new();
            if output.read_line(&mut line).await? == 0 {
                return Err(Error::Lsp("language server closed stdout".into()));
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                content_len = value.trim().parse::<usize>().ok();
            }
        }
        let len = content_len.ok_or_else(|| Error::Lsp("missing Content-Length".into()))?;
        let mut body = vec![0; len];
        output.read_exact(&mut body).await?;
        Ok(serde_json::from_slice(&body)?)
    }
}

#[async_trait]
impl Lsp for LocalLsp {
    async fn start(&self, root: &Path) -> Result<()> {
        let mut parts = self.command.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| Error::Lsp("empty language server command".into()))?;
        let mut child = Command::new(program)
            .args(parts)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Lsp(format!("cannot start {program}: {e}")))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| Error::Lsp("language server stdin unavailable".into()))?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| Error::Lsp("language server stdout unavailable".into()))?,
        );
        *self.state.lock().await = Some(LspState {
            _child: child,
            input,
            output,
        });
        let root_uri = format!(
            "file:///{}",
            root.canonicalize()?.to_string_lossy().replace('\\', "/")
        );
        self.request(
            "initialize",
            json!({"processId": std::process::id(), "rootUri": root_uri, "capabilities": {}}),
        )
        .await?;
        let mut state = self.state.lock().await;
        let state = state
            .as_mut()
            .ok_or_else(|| Error::Lsp("language server not started".into()))?;
        Self::write_message(
            &mut state.input,
            &json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
        )
        .await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut guard = self.state.lock().await;
        let state = guard
            .as_mut()
            .ok_or_else(|| Error::Lsp("language server not started".into()))?;
        Self::write_message(
            &mut state.input,
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        )
        .await?;
        loop {
            let message = Self::read_message(&mut state.output).await?;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    return Err(Error::Lsp(error.to_string()));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            // 服务端通知在这里被安全消费；后续可投影到 EventBus。
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn parses_content_length_frame() {
        let body = br#"{"jsonrpc":"2.0","id":1,"result":{}}"#;
        let data = format!("Content-Length: {}\r\n\r\n", body.len())
            .into_bytes()
            .into_iter()
            .chain(body.iter().copied())
            .collect::<Vec<_>>();
        let mut reader = BufReader::new(&data[..]);
        let value = LocalLsp::read_message(&mut reader).await.unwrap();
        assert_eq!(value["id"], 1);
    }
}
