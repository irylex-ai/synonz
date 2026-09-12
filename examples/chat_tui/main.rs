//! `chat_tui`: an interactive chat over an OpenAI-compatible endpoint.
//!
//! Run: `cargo run -p synonz-examples --bin chat_tui`
//!
//! The startup wizard asks for the endpoint and API key (pre-filled from
//! `SYNONZ_OPENAI_API_KEY` / `SYNONZ_OPENAI_BASE_URL` / `SYNONZ_OPENAI_MODEL`
//! when set), then for the model and thinking level. In chat: Enter sends,
//! Esc cancels the running turn, `/model <name>` and `/effort <level>`
//! apply at runtime (no rebuild), Ctrl+C quits.

mod app;
mod setup;
mod tools;
mod ui;

use std::io;
use std::panic;
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use synonz::{Agent, Conversation, Subject, SubjectType, SynonzRuntime};
use synonz_openai::{Client, ReasoningEffort};

use app::{ChatState, Entry, Status};
use setup::{Outcome, Setup};

/// The agent's system prompt (tools + sandbox explained to the model).
const SYSTEM_PROMPT: &str = "You are a helpful assistant in a terminal chat. \
You have read-only tools for the working directory: read_file, list_dir, and file_info. \
Use them when the user asks about files or the directory. Keep answers concise.";

/// The terminal type used throughout.
type ChatTerminal = Terminal<CrosstermBackend<io::Stdout>>;

#[tokio::main]
async fn main() -> io::Result<()> {
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
    let runtime = SynonzRuntime::builder().build();
    let client = Client::new(base_url, api_key, model.clone());
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
    chat.transcript.push(Entry::Note(format!(
        "connected · {} · thinking {} · tools: read_file, list_dir, file_info (sandboxed to the working directory)",
        chat.model,
        app::effort_label(chat.effort),
    )));

    while !chat.should_quit {
        terminal.draw(|frame| ui::draw_chat(frame, &chat))?;
        match events.next().await {
            Some(Ok(Event::Key(key))) => {
                if is_ctrl_c(&key) {
                    break;
                }
                if key.code == KeyCode::Enter && chat.status == Status::Idle {
                    let line = chat.input.take();
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if let Some(command) = line.strip_prefix('/') {
                        handle_command(command, &client, &mut chat);
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
            }
            Some(Ok(Event::Resize(..))) => {}
            Some(Ok(_)) => {}
            Some(Err(_)) | None => break,
        }
    }

    runtime.shutdown().await;
    Ok(())
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
    chat.status = Status::Running;
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
                        execution.cancel();
                    } else if key.code == KeyCode::Esc {
                        execution.cancel();
                    } else {
                        chat.handle_scroll_key(key.code);
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) | None => {
                    chat.should_quit = true;
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
    chat.status = Status::Idle;
    Ok(())
}

/// Handles a `/` command; commands never reach the model.
fn handle_command(command: &str, client: &Client, chat: &mut ChatState) {
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
        }
        "quit" | "exit" => chat.should_quit = true,
        _ => chat
            .transcript
            .push(Entry::Error(format!("unknown command: /{name}"))),
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
        KeyCode::PageUp | KeyCode::PageDown => chat.handle_scroll_key(key.code),
        _ => {}
    }
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c')
}

fn setup_terminal() -> io::Result<ChatTerminal> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    terminal.hide_cursor()?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut ChatTerminal) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, Show)?;
    terminal.show_cursor()
}

/// Restores the terminal if the process panics.
fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
        original(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use synonz::MockModel;

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
