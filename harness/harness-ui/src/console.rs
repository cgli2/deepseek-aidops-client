//! ConsoleUi：headless 骨架的可见渲染器。
//!
//! 设计说明：骨架里 `agent_loop` 只写 `SessionLog`（`log.append`），并没有把 `SessionEvent`
//! 回流到事件总线（bus）的 emit 通道，所以 UI 直接轮询 `SessionLog::replay()` 即可拿到完整真相源。
//!
//! 退出语义：headless 只需跑通一回合，`run` 轮询到 `TurnEnd` 即自然返回（不 park、不泄漏线程），
//! 进程随 `main` 返回而结束，不会出现「黑屏卡死」。

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use harness_core::event::EventBusView;
use harness_session::{SessionEvent, SessionLog};

use crate::Ui;

/// headless / 终端可见渲染器。
pub struct ConsoleUi;

fn render(e: &SessionEvent) {
    match e {
        SessionEvent::TurnStart { input, .. } => {
            println!("\n\x1b[36m>>>\x1b[0m {input}");
        }
        SessionEvent::Assistant { chunk, .. } => {
            if let Some(t) = &chunk.text {
                print!("{t}");
            }
            for tc in &chunk.tool_calls {
                println!("\n\x1b[35m[tool_call {}]\x1b[0m", tc.name);
            }
        }
        SessionEvent::ToolCall { call, .. } => {
            println!("\n\x1b[33m→ calling {}\x1b[0m", call.name);
        }
        SessionEvent::ToolResult { result, .. } => {
            let tag = if result.ok {
                "\x1b[32mok\x1b[0m"
            } else {
                "\x1b[31mERR\x1b[0m"
            };
            let preview: String = result.content.chars().take(200).collect();
            println!("\n\x1b[34m← tool {tag}\x1b[0m: {preview}");
        }
        SessionEvent::TurnEnd { .. } => {
            println!("\n\x1b[36m--- turn end ---\x1b[0m");
        }
        _ => {}
    }
    let _ = std::io::stdout().flush();
}

impl Ui for ConsoleUi {
    fn run(self: Arc<Self>, _bus: EventBusView, log: Arc<SessionLog>) {
        // 先补印已存在的历史（重放）。
        for e in log.replay() {
            render(&e);
        }
        // 轮询新事件并打印，直到回合结束（TurnEnd）即自然返回，让进程可以退出。
        let mut last = 0usize;
        loop {
            let events = log.replay();
            if events.len() > last {
                for e in &events[last..] {
                    render(e);
                }
                last = events.len();
            }
            if events
                .iter()
                .any(|e| matches!(e, SessionEvent::TurnEnd { .. }))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
    }
}
