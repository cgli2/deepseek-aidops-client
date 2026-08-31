//! ACP stdio JSON-RPC 边界：请求驱动 AgentLoop，会话结果从 SessionLog 投影返回。

use harness_core::{types::UserInput, AppContext};
use harness_runtime::AgentLoop;
use harness_session::SessionLog;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Deserialize)]
pub struct AcpRequest {
    #[serde(default)]
    pub jsonrpc: Option<String>,
    #[serde(default)]
    pub id: Option<u64>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct AcpResponse {
    pub jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
}

pub struct AcpServer;
impl Default for AcpServer {
    fn default() -> Self {
        Self::new()
    }
}
impl AcpServer {
    pub fn new() -> Self {
        Self
    }

    async fn dispatch(
        &self,
        req: &AcpRequest,
        ctx: &AppContext,
        log: &Arc<SessionLog>,
    ) -> Result<Value, String> {
        match req.method.as_str() {
            "initialize" => Ok(
                json!({"name":"harness","protocolVersion":1,"capabilities":{"prompt":true,"replay":true}}),
            ),
            "session/status" => {
                Ok(json!({"sessionId":log.id().to_string(),"events":log.replay().len()}))
            }
            "session/replay" => serde_json::to_value(log.replay()).map_err(|e| e.to_string()),
            "session/prompt" => {
                let text = req
                    .params
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|v| !v.trim().is_empty())
                    .ok_or("params.text is required")?;
                let start = log.replay().len();
                AgentLoop::new()
                    .run_turn(
                        ctx,
                        UserInput {
                            text: text.into(),
                            attachments: vec![],
                        },
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let (_, events) = log.replay_from(start);
                serde_json::to_value(events).map_err(|e| e.to_string())
            }
            _ => Err(format!("method not found: {}", req.method)),
        }
    }

    pub async fn run(&self, ctx: AppContext, log: Arc<SessionLog>) -> io::Result<()> {
        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        let mut out = tokio::io::stdout();
        while let Some(line) = lines.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let response = match serde_json::from_str::<AcpRequest>(&line) {
                Ok(req) => match self.dispatch(&req, &ctx, &log).await {
                    Ok(result) => AcpResponse {
                        jsonrpc: "2.0",
                        id: req.id,
                        result: Some(result),
                        error: None,
                    },
                    Err(message) => AcpResponse {
                        jsonrpc: "2.0",
                        id: req.id,
                        result: None,
                        error: Some(json!({"code":-32601,"message":message})),
                    },
                },
                Err(error) => AcpResponse {
                    jsonrpc: "2.0",
                    id: None,
                    result: None,
                    error: Some(json!({"code":-32700,"message":error.to_string()})),
                },
            };
            out.write_all(
                serde_json::to_string(&response)
                    .unwrap_or_default()
                    .as_bytes(),
            )
            .await?;
            out.write_all(b"\n").await?;
            out.flush().await?;
        }
        Ok(())
    }
}
