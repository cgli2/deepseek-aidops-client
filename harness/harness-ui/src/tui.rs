//! TuiUi（ratatui + crossterm）。交互式终端形态：
//! - 上区轮询 `SessionLog` 真相源渲染 transcript（自动滚动到底部）；
//! - 下区输入框：Enter 提交（经 `UiInputSink` 驱动后台回合）、Esc 取消当前回合、
//!   `q` 退出并还原终端。忙碌时禁用提交并提示"思考中"。

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use harness_core::event::EventBusView;
use harness_core::ui_input::UiInputSink;
use harness_session::{SessionEvent, SessionLog};

use crate::Ui;

/// 终端 TUI 渲染器（feature = "tui"）。持有反向输入通道（UI → 运行时）。
pub struct TuiUi {
    sink: Arc<dyn UiInputSink>,
}

impl TuiUi {
    pub fn new(sink: Arc<dyn UiInputSink>) -> Self {
        Self { sink }
    }
}

fn push_line(lines: &mut Vec<String>, e: &SessionEvent) {
    match e {
        SessionEvent::TurnStart { input, .. } => lines.push(format!(">>> {input}")),
        SessionEvent::Assistant { chunk, .. } => {
            if let Some(t) = &chunk.text {
                lines.push(t.clone());
            }
            for tc in &chunk.tool_calls {
                lines.push(format!("[tool_call {}]", tc.name));
            }
        }
        SessionEvent::ToolCall { call, .. } => lines.push(format!("→ calling {}", call.name)),
        SessionEvent::ToolResult { result, .. } => {
            let preview: String = result.content.chars().take(200).collect();
            lines.push(format!(
                "← tool {}: {}",
                if result.ok { "ok" } else { "ERR" },
                preview
            ));
        }
        SessionEvent::TurnEnd { .. } => lines.push("--- turn end ---".into()),
        _ => {}
    }
}

impl Ui for TuiUi {
    fn run(self: Arc<Self>, _bus: EventBusView, log: Arc<SessionLog>) {
        // UI 跑在独立 OS 线程；`run` 在此 `join` 等待线程结束后再返回，
        // 避免阻塞 tokio worker 导致进程无法退出。
        let sink = self.sink.clone();
        let handle = thread::spawn(move || {
            use crossterm::event::{self, Event as CEvent, KeyCode};
            use crossterm::terminal::{
                EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
            };
            use ratatui::Terminal;
            use ratatui::backend::CrosstermBackend;
            use ratatui::layout::{Constraint, Layout};
            use ratatui::style::{Color, Modifier, Style};
            use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

            let _ = enable_raw_mode();
            let mut stdout = std::io::stdout();
            let _ = crossterm::execute!(stdout, EnterAlternateScreen);
            let backend = CrosstermBackend::new(stdout);
            let mut term = match Terminal::new(backend) {
                Ok(t) => t,
                Err(_) => {
                    let _ = disable_raw_mode();
                    return;
                }
            };

            let mut lines: Vec<String> = Vec::new();
            let mut last = 0usize;
            let mut input = String::new();
            let mut quit = false;
            while !quit {
                let events = log.replay();
                if events.len() > last {
                    for e in &events[last..] {
                        push_line(&mut lines, e);
                    }
                    last = events.len();
                }
                let busy = sink.busy();
                let _ = term.draw(|f| {
                    let chunks = Layout::vertical([
                        Constraint::Min(1),
                        Constraint::Length(3),
                        Constraint::Length(1),
                    ])
                    .split(f.area());

                    let body = lines.join("\n");
                    let scroll = lines
                        .len()
                        .saturating_sub(chunks[0].height.saturating_sub(2) as usize);
                    let transcript = Paragraph::new(body)
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title("harness (tui)"),
                        )
                        .wrap(Wrap { trim: false })
                        .scroll((scroll as u16, 0));
                    f.render_widget(transcript, chunks[0]);

                    let editor = Paragraph::new(if input.is_empty() {
                        "输入消息后按 Enter 发送…".to_string()
                    } else {
                        input.clone()
                    })
                    .style(if input.is_empty() {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default()
                    })
                    .block(Block::default().borders(Borders::ALL).title(if busy {
                        "模型思考中…（Esc 停止）"
                    } else {
                        "输入"
                    }));
                    f.render_widget(editor, chunks[1]);

                    let status = if busy {
                        "● 正在处理，按 Esc 可随时停止"
                    } else {
                        "● 就绪 · Enter 发送 · Esc 停止 · q 退出"
                    };
                    let bar = Paragraph::new(status).style(
                        Style::default()
                            .fg(if busy { Color::Yellow } else { Color::Green })
                            .add_modifier(Modifier::DIM),
                    );
                    f.render_widget(bar, chunks[2]);
                });
                if event::poll(Duration::from_millis(60)).unwrap_or(false) {
                    if let Ok(CEvent::Key(k)) = event::read() {
                        match k.code {
                            KeyCode::Char('q') if !busy => quit = true,
                            KeyCode::Char(c) => {
                                if !busy {
                                    input.push(c);
                                }
                            }
                            KeyCode::Backspace => {
                                input.pop();
                            }
                            KeyCode::Enter => {
                                let text = input.trim().to_string();
                                if !busy && !text.is_empty() {
                                    sink.submit(text);
                                    input.clear();
                                }
                            }
                            KeyCode::Esc => {
                                if busy {
                                    sink.cancel();
                                } else {
                                    input.clear();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            let _ = disable_raw_mode();
            // 直接对 stdout 执行，避免依赖 backend 内部 `get_mut` API。
            let _ = crossterm::execute!(std::io::stdout(), LeaveAlternateScreen);
        });
        let _ = handle.join();
    }
}
