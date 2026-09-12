//! Application state: the transcript, input editing, scrolling, focus, and
//! the latest model-request trace.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use ratatui::layout::Rect;
use synonz::{
    ExecutionEvent, Message, ModelDelta, ModelEvent, Observer, ObserverContext, SynonzEvent,
    ToolContent, ToolResult, TurnEvent,
};
use synonz_openai::ReasoningEffort;

/// Lines moved by one page-scroll key.
const SCROLL_PAGE: u16 = 5;

/// A character-safe single-line text input with a cursor.
#[derive(Debug, Clone)]
pub struct TextInput {
    text: String,
    cursor: usize,
}

impl TextInput {
    /// Creates an input with the cursor at the end.
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.len();
        Self { text, cursor }
    }

    /// The current text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor position (a byte offset at a char boundary).
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replaces the text; the cursor moves to the end.
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.text.len();
    }

    /// Inserts one character at the cursor.
    pub fn insert(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    /// Deletes the character before the cursor.
    pub fn backspace(&mut self) {
        if let Some(previous) = self.text[..self.cursor].chars().next_back() {
            let start = self.cursor - previous.len_utf8();
            self.text.replace_range(start..self.cursor, "");
            self.cursor = start;
        }
    }

    /// Deletes the character at the cursor.
    pub fn delete(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.text
                .replace_range(self.cursor..self.cursor + next.len_utf8(), "");
        }
    }

    /// Moves the cursor one character left.
    pub fn move_left(&mut self) {
        if let Some(previous) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= previous.len_utf8();
        }
    }

    /// Moves the cursor one character right.
    pub fn move_right(&mut self) {
        if let Some(next) = self.text[self.cursor..].chars().next() {
            self.cursor += next.len_utf8();
        }
    }

    /// Moves the cursor to the start.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Moves the cursor to the end.
    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Takes the text, leaving the input empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }
}

/// One transcript entry.
#[derive(Debug, Clone)]
pub enum Entry {
    /// The user's message.
    User(String),
    /// The assistant's streamed text.
    Assistant(String),
    /// The model's streamed reasoning (display-only narration).
    Reasoning(String),
    /// A tool call the model requested.
    ToolCall {
        /// The tool's name.
        name: String,
        /// The compact JSON arguments.
        arguments: String,
    },
    /// A finished tool invocation.
    ToolResult {
        /// Whether the tool succeeded (soft failures are `false`).
        ok: bool,
        /// A short one-line summary.
        summary: String,
    },
    /// A neutral note.
    Note(String),
    /// An error surfaced to the user.
    Error(String),
}

/// Whether a turn is in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Waiting for input.
    Idle,
    /// A turn is running.
    Running,
}

/// Which box receives scrolling keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The message input box.
    Input,
    /// The transcript box.
    Chat,
    /// The trace box.
    Trace,
}

/// One recorded model request (the bus observer's trace payload).
pub struct TraceEntry {
    /// Monotonic request number (1-based).
    pub number: usize,
    /// The reasoning-loop round, when inside the loop.
    pub round: Option<usize>,
    /// The call purpose (a debug label).
    pub purpose: String,
    /// The full canonical message list sent to the model.
    pub messages: Vec<Message>,
}

/// The latest model request, shared between the bus observer and the UI.
#[derive(Clone, Default)]
pub struct TraceStore {
    inner: Arc<Mutex<Option<Arc<TraceEntry>>>>,
    counter: Arc<AtomicUsize>,
}

impl TraceStore {
    /// Records one model request as the latest trace entry.
    pub fn record(&self, round: Option<usize>, purpose: String, messages: Vec<Message>) {
        let number = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let entry = Arc::new(TraceEntry {
            number,
            round,
            purpose,
            messages,
        });
        *self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(entry);
    }

    /// The latest recorded request, if any.
    pub fn latest(&self) -> Option<Arc<TraceEntry>> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Observer for TraceStore {
    fn on_event(&self, _ctx: &ObserverContext, event: &SynonzEvent) {
        if let SynonzEvent::Turn(TurnEvent::Model(ModelEvent::Requested {
            purpose,
            messages,
            round,
        })) = event
        {
            self.record(*round, format!("{purpose:?}"), messages.clone());
        }
    }
}

/// The chat state.
pub struct ChatState {
    /// The rendered transcript.
    pub transcript: Vec<Entry>,
    /// The input line.
    pub input: TextInput,
    /// The current model name (chat box title + `/model`).
    pub model: String,
    /// The current thinking option (chat box title + `/effort`).
    pub effort: Option<ReasoningEffort>,
    /// Whether the model's thinking output is displayed.
    pub show_thinking: bool,
    /// The box that receives scrolling keys.
    pub focus: Focus,
    /// Sent messages (for history recall).
    pub history: Vec<String>,
    /// The history position while recalling (`None` = editing).
    history_pos: Option<usize>,
    /// The in-progress input saved while recalling history.
    draft: String,
    /// The transcript's top line offset (used when not following).
    pub scroll: u16,
    /// Whether the view follows new output.
    pub follow: bool,
    /// Whether the view is paused above the bottom while content arrived.
    pub paused: bool,
    /// The last rendered transcript max-scroll (for anchoring).
    pub last_max_scroll: u16,
    /// The transcript area (mouse hit-testing).
    pub chat_area: Rect,
    /// The trace area (mouse hit-testing; `None` when hidden).
    pub trace_area: Option<Rect>,
    /// The latest model-request trace.
    pub trace: TraceStore,
    /// The trace box's top line offset.
    pub trace_scroll: u16,
    /// The trace request number currently rendered.
    pub trace_seen: usize,
    /// The last rendered trace max-scroll.
    pub last_trace_max: u16,
    /// The turn status.
    pub status: Status,
    /// The spinner frame.
    pub spinner: usize,
    /// Set when the user asked to quit.
    pub should_quit: bool,
    /// The transcript index of the assistant entry being streamed.
    streaming: Option<usize>,
    /// The transcript index of the reasoning entry being streamed.
    reasoning_streaming: Option<usize>,
}

impl ChatState {
    /// Creates an empty chat state.
    pub fn new(model: String, effort: Option<ReasoningEffort>) -> Self {
        Self {
            transcript: Vec::new(),
            input: TextInput::new(""),
            model,
            effort,
            show_thinking: true,
            focus: Focus::Input,
            history: Vec::new(),
            history_pos: None,
            draft: String::new(),
            scroll: 0,
            follow: true,
            paused: false,
            last_max_scroll: 0,
            chat_area: Rect::new(0, 0, 0, 0),
            trace_area: None,
            trace: TraceStore::default(),
            trace_scroll: 0,
            trace_seen: 0,
            last_trace_max: 0,
            status: Status::Idle,
            spinner: 0,
            should_quit: false,
            streaming: None,
            reasoning_streaming: None,
        }
    }

    /// Records a sent user message (transcript + history) and follows it.
    pub fn push_user(&mut self, text: &str) {
        self.transcript.push(Entry::User(text.to_string()));
        self.history.push(text.to_string());
        self.history_pos = None;
        self.draft.clear();
        self.follow = true;
        self.paused = false;
    }

    /// Applies one execution event; returns `true` at the terminal event.
    pub fn apply_event(&mut self, event: &ExecutionEvent) -> bool {
        match event {
            ExecutionEvent::Delta(ModelDelta::Text { text }) => {
                match self.streaming {
                    Some(index) => {
                        if let Some(Entry::Assistant(buffer)) = self.transcript.get_mut(index) {
                            buffer.push_str(text);
                        }
                    }
                    None => {
                        self.transcript.push(Entry::Assistant(text.clone()));
                        self.streaming = Some(self.transcript.len() - 1);
                    }
                }
                self.mark_new_content();
                false
            }
            ExecutionEvent::Delta(ModelDelta::Reasoning { text }) => {
                match self.reasoning_streaming {
                    Some(index) => {
                        if let Some(Entry::Reasoning(buffer)) = self.transcript.get_mut(index) {
                            buffer.push_str(text);
                        }
                    }
                    None => {
                        self.transcript.push(Entry::Reasoning(text.clone()));
                        self.reasoning_streaming = Some(self.transcript.len() - 1);
                    }
                }
                self.mark_new_content();
                false
            }
            ExecutionEvent::ToolRequested(call) => {
                self.transcript.push(Entry::ToolCall {
                    name: call.name.clone(),
                    arguments: summarize_arguments(&call.arguments),
                });
                self.mark_new_content();
                false
            }
            ExecutionEvent::ToolCompleted { result, .. } => {
                let (ok, summary) = match result {
                    ToolResult::Ok { content } => (true, summarize_content(content)),
                    ToolResult::Err { message } => (false, message.clone()),
                    _ => (true, "done".to_string()),
                };
                self.transcript.push(Entry::ToolResult { ok, summary });
                self.mark_new_content();
                false
            }
            ExecutionEvent::Completed(output) => {
                self.reasoning_streaming = None;
                let text = output.text().unwrap_or_default().to_string();
                match self.streaming.take() {
                    Some(index) => {
                        self.transcript[index] = Entry::Assistant(text);
                    }
                    None if !text.is_empty() => {
                        self.transcript.push(Entry::Assistant(text));
                    }
                    None => {}
                }
                true
            }
            ExecutionEvent::Failed(error) => {
                self.streaming = None;
                self.reasoning_streaming = None;
                self.transcript.push(Entry::Error(error.to_string()));
                true
            }
            ExecutionEvent::Cancelled(reason) => {
                self.streaming = None;
                self.reasoning_streaming = None;
                self.transcript
                    .push(Entry::Note(format!("cancelled ({reason})")));
                true
            }
            _ => false,
        }
    }

    /// Recalls the previous history entry.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let position = match self.history_pos {
            None => {
                self.draft = self.input.text().to_string();
                self.history.len() - 1
            }
            Some(0) => 0,
            Some(position) => position - 1,
        };
        self.history_pos = Some(position);
        self.input.set(self.history[position].clone());
    }

    /// Moves to the next history entry (or back to the draft).
    pub fn history_next(&mut self) {
        match self.history_pos {
            None => {}
            Some(position) if position + 1 < self.history.len() => {
                self.history_pos = Some(position + 1);
                self.input.set(self.history[position + 1].clone());
            }
            Some(_) => {
                self.history_pos = None;
                self.input.set(self.draft.clone());
            }
        }
    }

    /// Advances the focus cycle: input → chat → trace → input.
    pub fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Input => Focus::Chat,
            Focus::Chat => Focus::Trace,
            Focus::Trace => Focus::Input,
        };
    }

    /// Marks that content arrived while the view was paused.
    pub fn mark_new_content(&mut self) {
        if !self.follow {
            self.paused = true;
        }
    }

    /// Scrolls the transcript by `delta` lines (negative = up).
    ///
    /// Scrolling up from the bottom anchors at the current position first;
    /// scrolling back to the bottom resumes follow.
    pub fn scroll_lines(&mut self, delta: i32) {
        if delta < 0 {
            let step = (-delta) as u16;
            if self.follow {
                self.follow = false;
                self.scroll = self.last_max_scroll.saturating_sub(step);
            } else {
                self.scroll = self.scroll.saturating_sub(step);
            }
        } else {
            self.scroll = self.scroll.saturating_add(delta as u16);
            if self.scroll >= self.last_max_scroll {
                self.scroll_end();
            }
        }
    }

    /// Jumps the transcript to the top and pauses follow.
    pub fn scroll_home(&mut self) {
        self.follow = false;
        self.scroll = 0;
    }

    /// Jumps the transcript to the bottom and resumes follow.
    pub fn scroll_end(&mut self) {
        self.follow = true;
        self.scroll = self.last_max_scroll;
        self.paused = false;
    }

    /// Applies one transcript scroll key.
    pub fn scroll_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up => self.scroll_lines(-1),
            KeyCode::Down => self.scroll_lines(1),
            KeyCode::PageUp => self.scroll_lines(-(SCROLL_PAGE as i32)),
            KeyCode::PageDown => self.scroll_lines(SCROLL_PAGE as i32),
            KeyCode::Home => self.scroll_home(),
            KeyCode::End => self.scroll_end(),
            _ => {}
        }
    }

    /// Scrolls the trace by `delta` lines (negative = up).
    pub fn trace_scroll_lines(&mut self, delta: i32) {
        if delta < 0 {
            self.trace_scroll = self.trace_scroll.saturating_sub((-delta) as u16);
        } else {
            self.trace_scroll = self.trace_scroll.saturating_add(delta as u16);
        }
    }

    /// Applies one trace scroll key.
    pub fn trace_scroll_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up => self.trace_scroll_lines(-1),
            KeyCode::Down => self.trace_scroll_lines(1),
            KeyCode::PageUp => self.trace_scroll_lines(-(SCROLL_PAGE as i32)),
            KeyCode::PageDown => self.trace_scroll_lines(SCROLL_PAGE as i32),
            KeyCode::Home => self.trace_scroll = 0,
            KeyCode::End => self.trace_scroll = self.last_trace_max,
            _ => {}
        }
    }

    /// Resets the trace scroll to the top when a new request arrives.
    pub fn sync_trace(&mut self, entry: &TraceEntry) {
        if self.trace_seen != entry.number {
            self.trace_seen = entry.number;
            self.trace_scroll = 0;
        }
    }

    /// The commands matching the current input (empty when not applicable).
    pub fn command_matches(&self) -> Vec<&'static CommandSpec> {
        let text = self.input.text();
        if !text.starts_with('/') || text.contains(' ') {
            return Vec::new();
        }
        COMMANDS
            .iter()
            .filter(|command| command.name.starts_with(text))
            .collect()
    }

    /// Completes the input to the first matching command; returns whether
    /// anything was completed.
    pub fn complete_command(&mut self) -> bool {
        let Some(command) = self.command_matches().first().copied() else {
            return false;
        };
        if command.args.is_empty() {
            self.input.set(command.name);
        } else {
            self.input.set(format!("{} ", command.name));
        }
        true
    }

    /// Toggles thinking display; returns the new visibility.
    pub fn toggle_thinking(&mut self) -> bool {
        self.show_thinking = !self.show_thinking;
        self.show_thinking
    }
}

/// One entry in the slash-command completion list.
pub struct CommandSpec {
    /// The command including the slash, e.g. `/model`.
    pub name: &'static str,
    /// The argument placeholder (empty when the command takes none).
    pub args: &'static str,
    /// A one-line description.
    pub description: &'static str,
}

/// The commands offered by completion.
pub const COMMANDS: [CommandSpec; 4] = [
    CommandSpec {
        name: "/model",
        args: "<name>",
        description: "switch model (next message)",
    },
    CommandSpec {
        name: "/effort",
        args: "<level>",
        description: "set thinking level (next message)",
    },
    CommandSpec {
        name: "/think",
        args: "[on|off]",
        description: "show or hide the model's thinking output",
    },
    CommandSpec {
        name: "/quit",
        args: "",
        description: "exit the chat",
    },
];

/// A short status label for a thinking option.
pub fn effort_label(effort: Option<ReasoningEffort>) -> &'static str {
    match effort {
        None => "default",
        Some(ReasoningEffort::Off) => "off",
        Some(ReasoningEffort::Minimal) => "minimal",
        Some(ReasoningEffort::Low) => "low",
        Some(ReasoningEffort::Medium) => "medium",
        Some(ReasoningEffort::High) => "high",
        Some(ReasoningEffort::ExtraHigh) => "xhigh",
        Some(ReasoningEffort::Max) => "max",
        Some(_) => "custom",
    }
}

/// One compact line of tool-call arguments.
fn summarize_arguments(arguments: &serde_json::Value) -> String {
    truncate_chars(&arguments.to_string(), 160)
}

/// One compact line of tool result content.
fn summarize_content(content: &ToolContent) -> String {
    let text = match content {
        ToolContent::Text { text } => text.clone(),
        ToolContent::Json { value } => value.to_string(),
        _ => "ok".to_string(),
    };
    truncate_chars(&text, 160)
}

/// Truncates to `limit` characters, appending an ellipsis when cut.
fn truncate_chars(text: &str, limit: usize) -> String {
    let mut out: String = text.chars().take(limit).collect();
    if text.chars().nth(limit).is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use synonz::{AgentOutput, CallId, TokenUsage, ToolCall};

    fn completed(text: &str) -> ExecutionEvent {
        ExecutionEvent::Completed(AgentOutput::new(
            Message::assistant_text(text),
            TokenUsage::new(1, 1),
        ))
    }

    #[test]
    fn deltas_coalesce_and_completion_reconciles() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        assert!(!chat.apply_event(&ExecutionEvent::Delta(ModelDelta::Text {
            text: "po".into()
        })));
        assert!(!chat.apply_event(&ExecutionEvent::Delta(ModelDelta::Text {
            text: "ng".into()
        })));
        assert!(chat.apply_event(&completed("pong")));
        assert_eq!(chat.transcript.len(), 1);
        assert!(matches!(&chat.transcript[0], Entry::Assistant(text) if text == "pong"));
    }

    #[test]
    fn completion_without_deltas_still_records_text() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        assert!(chat.apply_event(&completed("pong")));
        assert!(matches!(chat.transcript.last(), Some(Entry::Assistant(text)) if text == "pong"));
    }

    #[test]
    fn reasoning_fragments_coalesce() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        assert!(
            !chat.apply_event(&ExecutionEvent::Delta(ModelDelta::Reasoning {
                text: "hmm ".into()
            }))
        );
        assert!(
            !chat.apply_event(&ExecutionEvent::Delta(ModelDelta::Reasoning {
                text: "ok".into()
            }))
        );
        assert!(chat.apply_event(&completed("pong")));
        assert!(matches!(&chat.transcript[0], Entry::Reasoning(text) if text == "hmm ok"));
        assert!(matches!(&chat.transcript[1], Entry::Assistant(text) if text == "pong"));
    }

    #[test]
    fn command_completion_matches_prefixes() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        chat.input.set("/mo");
        let names: Vec<&str> = chat.command_matches().iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["/model"]);
        assert!(chat.complete_command());
        assert_eq!(chat.input.text(), "/model ");

        chat.input.set("/think");
        assert_eq!(chat.command_matches().len(), 1);
        assert!(chat.complete_command());
        assert_eq!(chat.input.text(), "/think ");
        assert!(
            chat.command_matches().is_empty(),
            "arguments end completion"
        );

        chat.input.set("hello");
        assert!(chat.command_matches().is_empty());
        assert!(!chat.complete_command());

        chat.input.set("/z");
        assert!(chat.command_matches().is_empty());
    }

    #[test]
    fn thinking_display_toggles() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        assert!(chat.show_thinking);
        assert!(!chat.toggle_thinking());
        assert!(chat.toggle_thinking());
    }

    #[test]
    fn tool_events_become_entries() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        let call = ToolCall::new("c1", "read_file", serde_json::json!({"path": "x"}));
        assert!(!chat.apply_event(&ExecutionEvent::ToolRequested(call)));
        assert!(!chat.apply_event(&ExecutionEvent::ToolCompleted {
            call_id: CallId::new("c1"),
            result: ToolResult::Err {
                message: "nope".into(),
            },
        }));
        assert!(matches!(&chat.transcript[0], Entry::ToolCall { name, .. } if name == "read_file"));
        assert!(matches!(
            &chat.transcript[1],
            Entry::ToolResult { ok: false, summary } if summary == "nope"
        ));
    }

    #[test]
    fn text_input_editing_is_char_safe() {
        let mut input = TextInput::new("");
        input.insert('你');
        input.insert('好');
        input.move_left();
        input.insert('很');
        assert_eq!(input.text(), "你很好");
        input.backspace();
        assert_eq!(input.text(), "你好");
        input.delete();
        assert_eq!(input.text(), "你");
    }

    #[test]
    fn history_recall_round_trips() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        chat.push_user("one");
        chat.push_user("two");
        chat.input.set("draft");
        chat.history_prev();
        assert_eq!(chat.input.text(), "two");
        chat.history_prev();
        assert_eq!(chat.input.text(), "one");
        chat.history_next();
        assert_eq!(chat.input.text(), "two");
        chat.history_next();
        assert_eq!(chat.input.text(), "draft");
    }

    #[test]
    fn focus_cycles_input_chat_trace() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        assert_eq!(chat.focus, Focus::Input);
        chat.cycle_focus();
        assert_eq!(chat.focus, Focus::Chat);
        chat.cycle_focus();
        assert_eq!(chat.focus, Focus::Trace);
        chat.cycle_focus();
        assert_eq!(chat.focus, Focus::Input);
    }

    #[test]
    fn scrolling_anchors_and_resumes_follow() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        chat.last_max_scroll = 100;
        chat.scroll_lines(-5);
        assert!(!chat.follow);
        assert_eq!(chat.scroll, 95, "scrolling up anchors at the bottom first");
        chat.scroll_lines(5);
        assert!(chat.follow, "reaching the bottom resumes follow");
        assert_eq!(chat.scroll, 100);
        chat.scroll_home();
        assert!(!chat.follow);
        assert_eq!(chat.scroll, 0);
        chat.scroll_end();
        assert!(chat.follow);
        assert_eq!(chat.scroll, 100);
    }

    #[test]
    fn new_content_while_paused_sets_the_indicator() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        chat.last_max_scroll = 50;
        chat.scroll_lines(-1);
        assert!(!chat.follow);
        assert!(!chat.paused);
        chat.mark_new_content();
        assert!(chat.paused);
        chat.scroll_end();
        assert!(!chat.paused);
    }

    #[test]
    fn trace_scroll_resets_on_a_new_request() {
        let mut chat = ChatState::new("test-model".to_string(), None);
        chat.last_trace_max = 30;
        chat.trace_scroll_key(KeyCode::PageDown);
        assert_eq!(chat.trace_scroll, 5);
        chat.trace_scroll_key(KeyCode::End);
        assert_eq!(chat.trace_scroll, 30);
        chat.trace_scroll_key(KeyCode::Up);
        assert_eq!(chat.trace_scroll, 29);

        let entry = TraceEntry {
            number: 1,
            round: Some(1),
            purpose: "Reasoning".to_string(),
            messages: Vec::new(),
        };
        chat.sync_trace(&entry);
        assert_eq!(chat.trace_scroll, 0, "a new request resets to the top");
        chat.trace_scroll_key(KeyCode::Down);
        chat.sync_trace(&entry);
        assert_eq!(chat.trace_scroll, 1, "the same request keeps the scroll");
    }
}
