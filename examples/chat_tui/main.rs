//! `chat_tui`: an interactive chat over an OpenAI-compatible endpoint.
//!
//! Run: `cargo run -p synonz-examples --bin chat_tui`
//!
//! The startup wizard asks for the endpoint and API key (pre-filled from
//! `SYNONZ_OPENAI_API_KEY` / `SYNONZ_OPENAI_BASE_URL` / `SYNONZ_OPENAI_MODEL`
//! when set), then for the model and thinking level. In chat: Enter sends,
//! Esc cancels the running turn, Tab cycles the focus (input → chat →
//! trace), the focused box scrolls with ↑/↓ and PgUp/PgDn, the mouse wheel
//! scrolls the box under the pointer, `/model <name>` and `/effort <level>`
//! apply at runtime (no rebuild), Ctrl+C quits. The right box shows the
//! latest model request (the bus observer's trace).

mod app;
mod setup;
mod tools;
mod ui;

use std::io;
use std::io::IsTerminal;
use std::panic;
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::style::Print;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use synonz::{Agent, Conversation, Subject, SubjectType, SynonzRuntime};
use synonz_openai::{Client, HeaderName, HeaderValue, ReasoningEffort, USER_AGENT};

use app::{ChatState, Entry, Focus, Selection, Status, TraceStore, selection_text};
use setup::{Outcome, Setup};

/// The agent's system prompt (tools + sandbox explained to the model).
///
/// Deliberately conservative: tools are declared but the model is told to
/// use them only on an explicit file/directory request and to ask on
/// ambiguous prompts (declaring tools alone already invites tool use).
const SYSTEM_PROMPT: &str = "You are a helpful assistant in a terminal chat. \
You have read-only tools for the working directory: read_file, list_dir, and file_info. \
Call a tool only when the user's request explicitly asks you to read a file or inspect \
the directory; for vague or general questions, ask what they mean instead of guessing. \
Keep answers concise.";

/// The terminal type used throughout.
type ChatTerminal = Terminal<CrosstermBackend<io::Stdout>>;

#[tokio::main]
async fn main() -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("chat_tui requires an interactive terminal (stdin and stdout must be a TTY)");
        return Ok(());
    }
    install_panic_hook();
    let mut terminal = setup_terminal()?;
    let result = run(&mut terminal).await;
    restore_terminal(&mut terminal)?;
    result
}

/// The wizard, then the chat session.
async fn run(terminal: &mut ChatTerminal) -> io::Result<()> {
    let mut events = EventStream::new();

    // ---- wizard -------------------------------------------------------
    let mut wizard = Setup::from_env();
    let (base_url, api_key, model, effort) = loop {
        terminal.draw(|frame| ui::draw_setup(frame, &wizard))?;
        match events.next().await {
            Some(Ok(Event::Key(key))) => match wizard.handle_key(key) {
                Outcome::Redraw => {}
                Outcome::Quit => return Ok(()),
                Outcome::LoadModels => {
                    terminal.draw(|frame| ui::draw_setup(frame, &wizard))?;
                    load_models(&mut wizard).await;
                }
                Outcome::Launch => {
                    break (
                        wizard.base_url.text().trim().to_string(),
                        wizard.api_key.text().to_string(),
                        wizard.model.text().trim().to_string(),
                        wizard.effort(),
                    );
                }
            },
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return Ok(()),
        }
    };

    // ---- session ------------------------------------------------------
    let trace = TraceStore::default();
    let runtime = SynonzRuntime::builder().observer(trace.clone()).build();
    let client = Client::new(base_url, api_key, model.clone())
        // Gateways (e.g. OpenCode Go) ask for a dedicated client identity
        // and a stable per-session id for routing and prompt caching.
        .header(
            USER_AGENT,
            HeaderValue::from_static("synonz-chat-tui/0.4.0"),
        )
        .header(
            HeaderName::from_static("x-opencode-session"),
            HeaderValue::from_str(&session_id()).expect("session id is valid"),
        );
    if let Some(effort) = effort {
        let mut options = client.options();
        options.reasoning_effort = Some(effort);
        client.set_options(options);
    }
    let agent = Agent::builder()
        .runtime(&runtime)
        .model(client.clone())
        .system_prompt(SYSTEM_PROMPT)
        .tool(tools::ReadFile {
            path: String::new(),
        })
        .tool(tools::ListDir { path: None })
        .tool(tools::FileInfo {
            path: String::new(),
        })
        .build()
        .map_err(|error| io::Error::other(format!("agent setup failed: {error}")))?;

    let subject = Subject::of(SubjectType::User, "chat-tui");
    let mut conversation = Conversation::new(&runtime, &subject);
    let mut chat = ChatState::new(model, effort);
    chat.trace = trace;
    chat.transcript.push(Entry::Note(format!(
        "connected · {} · thinking {} · tools: read_file, list_dir, file_info (sandboxed to the working directory)",
        chat.model,
        app::effort_label(chat.effort),
    )));

    while !chat.should_quit {
        terminal.draw(|frame| ui::draw_chat(frame, &mut chat))?;
        match events.next().await {
            Some(Ok(Event::Key(key))) => {
                if is_ctrl_c(&key) {
                    break;
                }
                // Tab completes a visible slash command; otherwise it
                // cycles the focus (input → chat → trace).
                if key.code == KeyCode::Tab
                    && chat.focus == Focus::Input
                    && !chat.command_matches().is_empty()
                {
                    chat.complete_command();
                    continue;
                }
                if key.code == KeyCode::Tab {
                    chat.cycle_focus();
                    continue;
                }
                if key.code == KeyCode::Esc && chat.focus != Focus::Input {
                    chat.focus = Focus::Input;
                    continue;
                }
                if chat.focus == Focus::Input {
                    if key.code == KeyCode::Enter && chat.status == Status::Idle {
                        let line = chat.input.take();
                        let line = line.trim().to_string();
                        if line.is_empty() {
                            continue;
                        }
                        if let Some(command) = line.strip_prefix('/') {
                            if let Some(captured) = handle_command(command, &client, &mut chat) {
                                let _ = set_mouse_capture(captured);
                            }
                            continue;
                        }
                        chat.push_user(&line);
                        run_turn(
                            terminal,
                            &agent,
                            &mut conversation,
                            &mut chat,
                            &mut events,
                            &line,
                        )
                        .await?;
                    } else {
                        apply_input_key(&mut chat, key);
                    }
                } else {
                    handle_scroll_key(&mut chat, &key);
                }
            }
            Some(Ok(Event::Mouse(mouse))) => handle_mouse(&mut chat, mouse),
            Some(Ok(Event::Resize(..))) => {}
            Some(Ok(_)) => {}
            Some(Err(_)) | None => break,
        }
    }

    runtime.shutdown().await;
    Ok(())
}

/// A stable per-run session id (the conversation's routing identity).
fn session_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("synonz-chat-tui-{}-{millis}", std::process::id())
}

/// Fetches the endpoint's model list for the wizard (best effort; the
/// wizard keeps accepting a hand-typed model on failure).
async fn load_models(wizard: &mut Setup) {
    let client = Client::new(
        wizard.base_url.text().trim(),
        wizard.api_key.text(),
        "unused",
    )
    .header(
        USER_AGENT,
        HeaderValue::from_static("synonz-chat-tui/0.4.0"),
    );
    match client.list_models().await {
        Ok(models) => wizard.set_models(models),
        Err(error) => wizard.set_models_error(error.to_string()),
    }
}

/// Runs one turn to its terminal event, live-updating the transcript.
async fn run_turn(
    terminal: &mut ChatTerminal,
    agent: &Agent,
    conversation: &mut Conversation,
    chat: &mut ChatState,
    events: &mut EventStream,
    input: &str,
) -> io::Result<()> {
    chat.begin_turn();
    let mut execution = agent.run(conversation.turn_input(input));
    let mut ticker = tokio::time::interval(Duration::from_millis(120));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        terminal.draw(|frame| ui::draw_chat(frame, chat))?;
        tokio::select! {
            maybe_event = events.next() => match maybe_event {
                Some(Ok(Event::Key(key))) => {
                    if is_ctrl_c(&key) {
                        chat.should_quit = true;
                        chat.request_cancel();
                        execution.cancel();
                    } else if key.code == KeyCode::Tab {
                        chat.cycle_focus();
                    } else if key.code == KeyCode::Esc {
                        if chat.focus != Focus::Input {
                            chat.focus = Focus::Input;
                        } else {
                            chat.request_cancel();
                            execution.cancel();
                        }
                    } else {
                        handle_scroll_key(chat, &key);
                    }
                }
                Some(Ok(Event::Mouse(mouse))) => handle_mouse(chat, mouse),
                Some(Ok(_)) => {}
                Some(Err(_)) | None => {
                    chat.should_quit = true;
                    chat.request_cancel();
                    execution.cancel();
                }
            },
            maybe_item = execution.next() => match maybe_item {
                Some(event) => {
                    if chat.apply_event(&event) {
                        break;
                    }
                }
                None => break,
            },
            _ = ticker.tick() => {
                chat.spinner = chat.spinner.wrapping_add(1);
            }
        }
    }
    chat.end_turn();
    Ok(())
}

/// Handles a `/` command; commands never reach the model. Returns a mouse
/// capture change when `/mouse` asks for one (the caller drives the
/// terminal).
fn handle_command(command: &str, client: &Client, chat: &mut ChatState) -> Option<bool> {
    let mut parts = command.splitn(2, char::is_whitespace);
    let name = parts.next().unwrap_or_default();
    let argument = parts.next().map(str::trim).unwrap_or_default();
    match name {
        "model" => {
            if argument.is_empty() {
                chat.transcript
                    .push(Entry::Error("usage: /model <name>".to_string()));
            } else {
                client.set_model(argument);
                chat.model = argument.to_string();
                chat.transcript.push(Entry::Note(format!(
                    "model → {argument} (applies to the next message)"
                )));
            }
            None
        }
        "effort" => {
            if argument.is_empty() {
                chat.transcript.push(Entry::Error(
                    "usage: /effort <default|off|minimal|low|medium|high|xhigh|max>".to_string(),
                ));
            } else {
                match parse_effort(argument) {
                    Some(effort) => {
                        let mut options = client.options();
                        options.reasoning_effort = effort;
                        client.set_options(options);
                        chat.effort = effort;
                        chat.transcript.push(Entry::Note(format!(
                            "thinking → {argument} (applies to the next message)"
                        )));
                    }
                    None => chat.transcript.push(Entry::Error(format!(
                        "unknown effort {argument:?}: use default/off/minimal/low/medium/high/xhigh/max"
                    ))),
                }
            }
            None
        }
        "quit" | "exit" => {
            chat.should_quit = true;
            None
        }
        "think" => {
            match argument {
                "" => {
                    let visible = chat.toggle_thinking();
                    chat.transcript.push(Entry::Note(format!(
                        "thinking display {}",
                        if visible { "on" } else { "off" }
                    )));
                }
                "on" => {
                    chat.show_thinking = true;
                    chat.transcript
                        .push(Entry::Note("thinking display on".to_string()));
                }
                "off" => {
                    chat.show_thinking = false;
                    chat.transcript
                        .push(Entry::Note("thinking display off".to_string()));
                }
                other => chat.transcript.push(Entry::Error(format!(
                    "unknown /think argument {other:?}: use on or off"
                ))),
            }
            None
        }
        "mouse" => {
            let target = match argument {
                "" => !chat.mouse_captured,
                "on" => true,
                "off" => false,
                other => {
                    chat.transcript.push(Entry::Error(format!(
                        "unknown /mouse argument {other:?}: use on or off"
                    )));
                    return None;
                }
            };
            chat.mouse_captured = target;
            chat.transcript.push(Entry::Note(format!(
                "mouse capture {} — {}",
                if target { "on" } else { "off" },
                if target {
                    "wheel + in-app selection (drag, copies on release)"
                } else {
                    "native terminal selection"
                }
            )));
            Some(target)
        }
        _ => {
            chat.transcript
                .push(Entry::Error(format!("unknown command: /{name}")));
            None
        }
    }
}

/// Parses a `/effort` argument; the outer `Option` is "was it recognized".
fn parse_effort(value: &str) -> Option<Option<ReasoningEffort>> {
    match value.to_ascii_lowercase().as_str() {
        "default" => Some(None),
        "off" | "none" => Some(Some(ReasoningEffort::Off)),
        "minimal" => Some(Some(ReasoningEffort::Minimal)),
        "low" => Some(Some(ReasoningEffort::Low)),
        "medium" => Some(Some(ReasoningEffort::Medium)),
        "high" => Some(Some(ReasoningEffort::High)),
        "xhigh" | "extrahigh" | "extra-high" => Some(Some(ReasoningEffort::ExtraHigh)),
        "max" => Some(Some(ReasoningEffort::Max)),
        _ => None,
    }
}

/// Applies an editing/navigation key to the input line.
fn apply_input_key(chat: &mut ChatState, key: KeyEvent) {
    match key.code {
        KeyCode::Char(ch) => chat.input.insert(ch),
        KeyCode::Backspace => chat.input.backspace(),
        KeyCode::Delete => chat.input.delete(),
        KeyCode::Left => chat.input.move_left(),
        KeyCode::Right => chat.input.move_right(),
        KeyCode::Home => chat.input.home(),
        KeyCode::End => chat.input.end(),
        KeyCode::Up => chat.history_prev(),
        KeyCode::Down => chat.history_next(),
        _ => {}
    }
}

/// Routes a scrolling key to the focused box.
fn handle_scroll_key(chat: &mut ChatState, key: &KeyEvent) {
    match chat.focus {
        Focus::Chat => chat.scroll_key(key.code),
        Focus::Trace => chat.trace_scroll_key(key.code),
        Focus::Input => {}
    }
}

/// Routes mouse events: the wheel scrolls the box under the pointer;
/// dragging selects text and releasing copies it (OSC 52).
fn handle_mouse(chat: &mut ChatState, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => scroll_under_pointer(chat, mouse.column, mouse.row, -3),
        MouseEventKind::ScrollDown => scroll_under_pointer(chat, mouse.column, mouse.row, 3),
        MouseEventKind::Down(MouseButton::Left) => begin_selection(chat, mouse.column, mouse.row),
        MouseEventKind::Drag(MouseButton::Left) => update_selection(chat, mouse.column, mouse.row),
        MouseEventKind::Up(MouseButton::Left) => finish_selection(chat),
        _ => {}
    }
}

fn scroll_under_pointer(chat: &mut ChatState, column: u16, row: u16, delta: i32) {
    let over_trace = chat
        .trace_area
        .is_some_and(|area| area.contains(Position::new(column, row)));
    if over_trace {
        chat.trace_scroll_lines(delta);
    } else {
        chat.scroll_lines(delta);
    }
}

/// Starts a selection in the box under the pointer (and focuses it).
fn begin_selection(chat: &mut ChatState, column: u16, row: u16) {
    let target = if chat
        .trace_area
        .is_some_and(|area| area.contains(Position::new(column, row)))
    {
        Focus::Trace
    } else if chat.chat_area.contains(Position::new(column, row)) {
        Focus::Chat
    } else {
        return;
    };
    let inner = match target {
        Focus::Trace => chat.trace_inner,
        Focus::Chat => chat.chat_inner,
        Focus::Input => return,
    };
    let point = clamp_to(inner, column, row);
    chat.selection = Some(Selection {
        target,
        start: point,
        end: point,
    });
    chat.focus = target;
}

/// Extends the active selection to the pointer.
fn update_selection(chat: &mut ChatState, column: u16, row: u16) {
    let Some(selection) = chat.selection else {
        return;
    };
    let inner = match selection.target {
        Focus::Trace => chat.trace_inner,
        Focus::Chat => chat.chat_inner,
        Focus::Input => return,
    };
    chat.selection = Some(Selection {
        end: clamp_to(inner, column, row),
        ..selection
    });
}

/// Copies the finished selection to the terminal clipboard (OSC 52).
fn finish_selection(chat: &mut ChatState) {
    let Some(selection) = chat.selection.take() else {
        return;
    };
    if selection.is_empty() {
        return;
    }
    let (view, inner, offset) = match selection.target {
        Focus::Chat => (&chat.chat_view, chat.chat_inner, chat.chat_offset),
        Focus::Trace => (&chat.trace_view, chat.trace_inner, chat.trace_offset),
        Focus::Input => return,
    };
    let text = selection_text(view, inner, offset, &selection);
    if text.trim().is_empty() {
        return;
    }
    let chars = text.chars().count();
    match copy_to_clipboard(&text) {
        Ok(()) => chat
            .transcript
            .push(Entry::Note(format!("copied {chars} chars (OSC 52)"))),
        Err(error) => chat
            .transcript
            .push(Entry::Error(format!("clipboard copy failed: {error}"))),
    }
}

/// Clamps a screen position to the box's content rect.
fn clamp_to(inner: Rect, column: u16, row: u16) -> (u16, u16) {
    let last_col = inner.x + inner.width.saturating_sub(1);
    let last_row = inner.y + inner.height.saturating_sub(1);
    (
        column.clamp(inner.x, last_col),
        row.clamp(inner.y, last_row),
    )
}

/// Writes text to the terminal clipboard with OSC 52 (tmux-aware).
fn copy_to_clipboard(text: &str) -> io::Result<()> {
    let osc = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let sequence = if std::env::var_os("TMUX").is_some() {
        format!("\x1bPtmux;\x1b{osc}\x1b\\")
    } else {
        osc
    };
    execute!(io::stdout(), Print(sequence))
}

/// Toggles the terminal mouse capture (wheel + in-app selection).
fn set_mouse_capture(enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(io::stdout(), EnableMouseCapture)
    } else {
        execute!(io::stdout(), DisableMouseCapture)
    }
}

/// Standard base64 with padding (the OSC 52 payload encoding).
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c')
}

fn setup_terminal() -> io::Result<ChatTerminal> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut ChatTerminal) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        Show
    )?;
    terminal.show_cursor()
}

/// Restores the terminal if the process panics.
fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            Show
        );
        original(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use synonz::MockModel;

    #[test]
    fn base64_encodes_osc52_payloads() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"hello"), "aGVsbG8=");
        assert_eq!(base64("你好".as_bytes()), "5L2g5aW9");
    }

    /// The trace must record the exact outgoing prompt: system first, then
    /// the conversation messages.
    #[tokio::test]
    async fn the_trace_records_the_full_prompt_including_system() {
        let trace = TraceStore::default();
        let runtime = SynonzRuntime::builder().observer(trace.clone()).build();
        let agent = Agent::builder()
            .runtime(&runtime)
            .model(MockModel::finishing_with_text("pong"))
            .system_prompt("SYSTEM-MARKER")
            .build()
            .expect("model is set");
        let subject = Subject::of(SubjectType::User, "test");
        let mut conversation = Conversation::new(&runtime, &subject);
        let mut execution = agent.run(conversation.turn_input("ping"));
        while let Some(event) = execution.next().await {
            if matches!(event, synonz::ExecutionEvent::Completed(_)) {
                break;
            }
        }
        let entry = trace.latest().expect("the trace recorded the request");
        assert_eq!(entry.messages[0].role, synonz::Role::System);
        assert!(entry.messages[0].blocks.iter().any(|block| {
            matches!(block, synonz::ContentBlock::Text { text } if text == "SYSTEM-MARKER")
        }));
        assert_eq!(entry.role_counts.0, 1, "one system message");
    }

    /// A scripted model drives one real turn into the transcript — the
    /// assembly path, with no terminal involved.
    #[tokio::test]
    async fn a_scripted_run_fills_the_transcript() {
        let runtime = SynonzRuntime::builder().build();
        let agent = Agent::builder()
            .runtime(&runtime)
            .model(MockModel::finishing_with_text("pong"))
            .build()
            .expect("model is set");
        let subject = Subject::of(SubjectType::User, "test");
        let mut conversation = Conversation::new(&runtime, &subject);
        let mut chat = ChatState::new("mock".to_string(), None);
        chat.push_user("ping");

        let mut terminal = false;
        let mut execution = agent.run(conversation.turn_input("ping"));
        while let Some(event) = execution.next().await {
            if chat.apply_event(&event) {
                terminal = true;
                break;
            }
        }
        assert!(terminal, "the run must reach a terminal event");
        assert!(matches!(
            chat.transcript.last(),
            Some(Entry::Assistant(text)) if text == "pong"
        ));
        assert_eq!(chat.status, Status::Idle);
        runtime.shutdown().await;
    }
}
