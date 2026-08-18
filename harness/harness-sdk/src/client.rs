//! 进程外 JSON-RPC 客户端：请求 id 严格关联，自动跳过通知帧并传播协议错误。

use std::io::{self, BufRead, Write};

use serde::Serialize;
use serde_json::Value;

/// 一条 JSON-RPC 请求帧（用于构造发往服务器的行）。
#[derive(Debug, Serialize)]
pub struct RpcRequest {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

/// 进程外客户端。泛型 `R`/`W` 解耦传输（stdin/stdout、TCP 流、管道均可）。
pub struct SdkClient<R, W> {
    id: u64,
    reader: R,
    writer: W,
}

impl<R: BufRead, W: Write> SdkClient<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            id: 0,
            reader,
            writer,
        }
    }

    /// 发出请求并持续读取，直到收到 id 匹配的响应；无 id 帧视为通知并跳过。
    pub fn call(&mut self, method: &str, params: Value) -> io::Result<Value> {
        self.id += 1;
        let req = RpcRequest {
            id: self.id,
            method: method.to_string(),
            params,
        };
        let line = serde_json::to_string(&req)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        writeln!(self.writer, "{line}")?;
        loop {
            let mut s = String::new();
            if self.reader.read_line(&mut s)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "RPC peer closed before response",
                ));
            }
            let value: Value = serde_json::from_str(&s)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            if value.get("id").and_then(Value::as_u64) != Some(self.id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(io::Error::new(io::ErrorKind::Other, error.to_string()));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn correlates_response_and_skips_notification() {
        let input = b"{\"jsonrpc\":\"2.0\",\"method\":\"progress\"}\n{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n";
        let mut output = Vec::new();
        let mut client = SdkClient::new(&input[..], &mut output);
        assert_eq!(client.call("initialize", Value::Null).unwrap()["ok"], true);
        assert!(!output.is_empty());
    }
}
