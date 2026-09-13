//! Rendering: the setup wizard and the chat screen.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use synonz::{ContentBlock, Message, Role, ToolResult};

use crate::app::{
    ChatState, CommandSpec, Entry, Focus, Selection, Status, TextInput, effort_label,
    summarize_arguments, summarize_content,
};
use crate::setup::{PopupKind, Setup, Step};

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];
/// Below this width the trace box is hidden (the chat needs the room).
const TRACE_MIN_WIDTH: u16 = 80;

fn user_style() -> Style {
    Style::default().fg(Color::Cyan)
}

fn assistant_style() -> Style {
    Style::default().fg(Color::Green)
}

fn tool_style() -> Style {
    Style::default().fg(Color::Yellow)
}

fn note_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn error_style() -> Style {
    Style::default().fg(Color::Red)
}

fn label_style() -> Style {
    Style::default().fg(Color::Gray)
}

fn focused_style() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

fn hint_style() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn reasoning_style() -> Style {
    Style::default()
        .fg(Color::Magenta)
        .add_modifier(Modifier::ITALIC)
}

/// Draws the setup wizard.
pub fn draw_setup(frame: &mut Frame, setup: &Setup) {
    let area = frame.area();
    let phase = match setup.step {
        Step::Api => "1/2 · endpoint",
        Step::Model => "2/2 · model & thinking",
        Step::Chat => "done",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Synonz chat — setup ({phase}) "));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let width = inner.width.max(1) as usize;
    let mut rows: Vec<(Line<'static>, Option<u16>)> = Vec::new();
    match setup.step {
        Step::Api => {
            rows.push(field_line(
                "Base URL",
                &setup.base_url,
                setup.field == 0,
                width,
                false,
            ));
            rows.push((Line::default(), None));
            rows.push(field_line(
                "API key ",
                &setup.api_key,
                setup.field == 1,
                width,
                true,
            ));
        }
        Step::Model => {
            rows.push(field_line(
                "Model    ",
                &setup.model,
                setup.field == 0,
                width,
                false,
            ));
            if setup.models_loading {
                rows.push((
                    Line::from(Span::styled("  loading models…", hint_style())),
                    None,
                ));
            } else if let Some(error) = &setup.models_error {
                rows.push((
                    Line::from(Span::styled(
                        format!("  models unavailable: {error}"),
                        note_style(),
                    )),
                    None,
                ));
            }
            rows.push((Line::default(), None));
            rows.push(reasoning_row(setup));
            rows.push(effort_row(setup));
            rows.push(start_row(setup));
        }
        Step::Chat => {}
    }
    rows.push((Line::default(), None));
    if let Some(error) = &setup.error {
        rows.push((
            Line::from(Span::styled(format!("! {error}"), error_style())),
            None,
        ));
    }
    let hint = match setup.step {
        Step::Api => "Tab: next field · Enter: continue · Esc: quit",
        Step::Model => "Tab: field · Enter: pick model / toggle / pick level / start · Esc: back",
        Step::Chat => "",
    };
    rows.push((
        Line::from(Span::styled(hint.to_string(), hint_style())),
        None,
    ));

    for (index, (line, caret)) in rows.into_iter().enumerate() {
        let y = inner.y.saturating_add(index as u16);
        if y >= inner.y.saturating_add(inner.height) {
            break;
        }
        frame.render_widget(Paragraph::new(line), Rect::new(inner.x, y, inner.width, 1));
        if let Some(column) = caret {
            frame.set_cursor_position((inner.x + column, y));
        }
    }

    if setup.popup.is_some() {
        draw_popup(frame, setup);
    }
}

/// Draws the open selection popup (models or effort levels).
fn draw_popup(frame: &mut Frame, setup: &Setup) {
    let Some(popup) = &setup.popup else {
        return;
    };
    let items = setup.popup_items();
    let area = centered_rect(frame.area(), 64, 60);
    frame.render_widget(Clear, area);
    let title = match popup.kind {
        PopupKind::Models => " select model ",
        PopupKind::Levels => " thinking level ",
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines: Vec<Line<'static>> = Vec::new();
    if popup.kind == PopupKind::Models {
        let query = if popup.query.is_empty() {
            "(type to filter)".to_string()
        } else {
            popup.query.clone()
        };
        lines.push(Line::from(vec![
            Span::styled(" filter: ", hint_style()),
            Span::styled(query, focused_style()),
        ]));
    }
    let list_height = inner.height.saturating_sub(lines.len() as u16 + 2).max(1) as usize;
    let offset = popup.selected.saturating_sub(list_height.saturating_sub(1));
    if items.is_empty() {
        lines.push(Line::from(Span::styled(" (no matches)", hint_style())));
    }
    for (index, item) in items.iter().enumerate().skip(offset).take(list_height) {
        let selected = index == popup.selected;
        let marker = if selected { "▶ " } else { "  " };
        lines.push(Line::from(Span::styled(
            format!("{marker}{item}"),
            if selected {
                focused_style()
            } else {
                Style::default()
            },
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " ↑↓ move · Enter select · Esc cancel",
        hint_style(),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

/// A rectangle centered in `area` (percent of width/height).
fn centered_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// Draws the chat screen: transcript and input on the left, the latest
/// model request on the right (hidden on narrow terminals).
pub fn draw_chat(frame: &mut Frame, chat: &mut ChatState) {
    let area = frame.area();
    let (left, trace_area) = if area.width >= TRACE_MIN_WIDTH {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(40), Constraint::Percentage(32)])
            .split(area);
        (columns[0], Some(columns[1]))
    } else {
        (area, None)
    };
    chat.trace_area = trace_area;

    let matches = chat.command_matches();
    let suggestions_height = if matches.is_empty() {
        0
    } else {
        (matches.len() as u16 + 2).min(8)
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(suggestions_height),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(left);
    chat.chat_area = rows[0];
    draw_transcript(frame, chat, rows[0]);
    if !matches.is_empty() {
        draw_suggestions(frame, &matches, rows[1]);
    }
    draw_input(frame, chat, rows[2]);
    draw_status_line(frame, chat, rows[3]);
    if let Some(area) = trace_area {
        draw_trace(frame, chat, area);
    }
}

/// The status line: state animation, timing, throughput, and context size.
fn draw_status_line(frame: &mut Frame, chat: &ChatState, area: Rect) {
    if area.width == 0 {
        return;
    }
    let context = match chat.trace.latest() {
        Some(entry) => format!("ctx ~{} tok", format_count(entry.context_tokens)),
        None => "ctx –".to_string(),
    };
    let spinner = SPINNER[chat.spinner % SPINNER.len()];
    let state = match chat.status {
        Status::Running if chat.is_cancelling() => format!("cancelling… {spinner}"),
        Status::Running => format!("running {spinner}"),
        Status::Idle => "idle".to_string(),
    };
    let mut parts = vec![state];
    if let Some(elapsed) = chat.elapsed() {
        parts.push(format_duration(elapsed));
    }
    if let Some(think) = chat.think_time() {
        parts.push(format!("think {}", format_duration(think)));
    }
    let rate = match chat.status {
        Status::Running => chat.live_rate(),
        Status::Idle => chat.last_rate(),
    };
    if let Some(rate) = rate {
        let prefix = if chat.status == Status::Running {
            "~"
        } else {
            ""
        };
        parts.push(format!("{prefix}{rate:.0} tok/s"));
    }
    if chat.status == Status::Running {
        parts.push(format!("out ~{} tok", format_count(chat.output_tokens())));
    }
    parts.push(context);
    let state_style = if chat.status == Status::Running {
        tool_style()
    } else {
        note_style()
    };
    let mut spans = vec![Span::styled(format!(" {} ", parts[0]), state_style)];
    if parts.len() > 1 {
        spans.push(Span::styled(
            format!("· {}", parts[1..].join(" · ")),
            note_style(),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A compact duration: `12.3s` or `1m02s`.
fn format_duration(duration: std::time::Duration) -> String {
    let seconds = duration.as_secs_f64();
    if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else {
        let minutes = (seconds / 60.0) as u64;
        format!("{minutes}m{:02.0}s", seconds - minutes as f64 * 60.0)
    }
}

/// A compact count: `123`, `1.23k`, or `12.3k`.
fn format_count(value: usize) -> String {
    if value >= 10_000 {
        format!("{:.1}k", value as f64 / 1000.0)
    } else if value >= 1_000 {
        format!("{:.2}k", value as f64 / 1000.0)
    } else {
        value.to_string()
    }
}

fn draw_suggestions(frame: &mut Frame, matches: &[&'static CommandSpec], area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" commands ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let lines: Vec<Line<'static>> = matches
        .iter()
        .take(inner.height as usize)
        .map(|command| {
            let usage = if command.args.is_empty() {
                command.name.to_string()
            } else {
                format!("{} {}", command.name, command.args)
            };
            Line::from(vec![
                Span::styled(format!(" {usage:<24}"), focused_style()),
                Span::raw(command.description.to_string()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The border style of a box: highlighted while it owns the focus.
fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Reverse-highlights the selected cells of one box.
struct SelectionHighlight<'a> {
    selection: &'a Selection,
    inner: Rect,
}

impl Widget for SelectionHighlight<'_> {
    fn render(self, _area: Rect, buf: &mut Buffer) {
        let ((start_col, start_row), (end_col, end_row)) = self.selection.normalized();
        let inner = self.inner;
        let last_col = inner.x + inner.width.saturating_sub(1);
        for row in start_row..=end_row {
            if row < inner.y || row >= inner.y.saturating_add(inner.height) {
                continue;
            }
            let (from, to) = if start_row == end_row {
                (start_col.min(last_col), end_col.min(last_col))
            } else if row == start_row {
                (start_col.min(last_col), last_col)
            } else if row == end_row {
                (inner.x, end_col.min(last_col))
            } else {
                (inner.x, last_col)
            };
            for column in from..=to {
                if let Some(cell) = buf.cell_mut((column, row)) {
                    cell.modifier |= Modifier::REVERSED;
                }
            }
        }
    }
}

fn draw_transcript(frame: &mut Frame, chat: &mut ChatState, area: Rect) {
    let focused = chat.focus == Focus::Chat;
    let title = format!(
        " chat │ {} · thinking {} ",
        chat.model,
        effort_label(chat.effort)
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    chat.chat_inner = inner;
    if inner.width == 0 || inner.height == 0 {
        chat.last_max_scroll = 0;
        return;
    }
    let inner_width = inner.width.max(8) as usize;
    let inner_height = inner.height as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut plains: Vec<String> = Vec::new();
    for entry in &chat.transcript {
        if matches!(entry, Entry::Reasoning(_)) && !chat.show_thinking {
            continue;
        }
        for (line, plain) in entry_lines(entry, inner_width) {
            lines.push(line);
            plains.push(plain);
        }
    }
    chat.chat_view = plains;
    let max_scroll = lines.len().saturating_sub(inner_height);
    chat.last_max_scroll = max_scroll.min(u16::MAX as usize) as u16;
    let offset = if chat.follow {
        max_scroll
    } else {
        (chat.scroll as usize).min(max_scroll)
    };
    chat.chat_offset = offset.min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(Text::from(lines)).scroll((offset.min(u16::MAX as usize) as u16, 0)),
        inner,
    );
    if chat.paused {
        let marker = Rect::new(
            inner.x + inner.width.saturating_sub(8),
            inner.y + inner.height.saturating_sub(1),
            inner.width.min(8),
            1,
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                " ↓ new ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            marker,
        );
    }
    let selection = chat
        .selection
        .filter(|selection| selection.target == Focus::Chat);
    if let Some(selection) = selection {
        frame.render_widget(
            SelectionHighlight {
                selection: &selection,
                inner,
            },
            inner,
        );
    }
}

fn draw_input(frame: &mut Frame, chat: &ChatState, area: Rect) {
    let focused = chat.focus == Focus::Input;
    let title = match chat.status {
        Status::Idle => format!(
            " message │ Enter send · Esc cancel · Tab focus · mouse {} ",
            if chat.mouse_captured { "on" } else { "off" }
        ),
        Status::Running => " message (running — Esc cancels) ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let (visible, column) =
        input_window(chat.input.text(), chat.input.cursor(), inner.width as usize);
    frame.render_widget(Paragraph::new(visible), inner);
    if focused {
        frame.set_cursor_position((inner.x + column, inner.y));
    }
}

/// Draws the latest model request (the bus observer's trace) as a readable
/// transcript of the complete prompt: role-labelled, wrapped messages.
fn draw_trace(frame: &mut Frame, chat: &mut ChatState, area: Rect) {
    let focused = chat.focus == Focus::Trace;
    let entry = chat.trace.latest();
    let title = match &entry {
        Some(entry) => format!(
            " trace #{} │ round {} · {} ",
            entry.number,
            entry
                .round
                .map(|round| round.to_string())
                .unwrap_or_else(|| "–".to_string()),
            entry.purpose
        ),
        None => " trace │ no request yet ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    chat.trace_inner = inner;
    if inner.width == 0 || inner.height == 0 {
        chat.last_trace_max = 0;
        return;
    }
    let Some(entry) = entry else {
        chat.last_trace_max = 0;
        chat.trace_view.clear();
        frame.render_widget(
            Paragraph::new(Span::styled(
                "(no model request yet — the trace appears on the first turn)",
                hint_style(),
            )),
            inner,
        );
        return;
    };
    chat.sync_trace(&entry);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut plains: Vec<String> = Vec::new();
    let (system, user, assistant, tool) = entry.role_counts;
    let header = format!(
        "#{} · round {} · {} · {} messages (system {system} · user {user} · assistant {assistant} · tool {tool}) · ctx ~{} tok / {} chars",
        entry.number,
        entry
            .round
            .map(|round| round.to_string())
            .unwrap_or_else(|| "–".to_string()),
        entry.purpose,
        entry.messages.len(),
        format_count(entry.context_tokens),
        format_count(entry.context_chars),
    );
    lines.push(Line::from(Span::styled(header.clone(), hint_style())));
    plains.push(header);
    lines.push(Line::default());
    plains.push(String::new());
    for message in &entry.messages {
        for (line, plain) in trace_message_lines(message, inner.width as usize) {
            lines.push(line);
            plains.push(plain);
        }
        lines.push(Line::default());
        plains.push(String::new());
    }
    chat.trace_view = plains;
    let max_scroll = lines.len().saturating_sub(inner.height as usize);
    chat.last_trace_max = max_scroll.min(u16::MAX as usize) as u16;
    let offset = (chat.trace_scroll as usize).min(max_scroll);
    chat.trace_offset = offset.min(u16::MAX as usize) as u16;
    frame.render_widget(
        Paragraph::new(lines).scroll((offset.min(u16::MAX as usize) as u16, 0)),
        inner,
    );
    let selection = chat
        .selection
        .filter(|selection| selection.target == Focus::Trace);
    if let Some(selection) = selection {
        frame.render_widget(
            SelectionHighlight {
                selection: &selection,
                inner,
            },
            inner,
        );
    }
}

/// One trace message: (styled line, plain text) pairs, wrapped by width.
fn trace_message_lines(message: &Message, width: usize) -> Vec<(Line<'static>, String)> {
    let (label, style) = match message.role {
        Role::System => ("system", reasoning_style()),
        Role::User => ("user", user_style()),
        Role::Assistant => ("assistant", assistant_style()),
        Role::Tool => ("tool", tool_style()),
        _ => ("?", hint_style()),
    };
    let indent = label.chars().count() + 2;
    let body_width = width.saturating_sub(indent).max(8);
    let mut parts = Vec::new();
    for block in &message.blocks {
        match block {
            ContentBlock::Text { text } => parts.push(text.clone()),
            ContentBlock::ToolCall(call) => parts.push(format!(
                "⚙ {}({})",
                call.name,
                summarize_arguments(&call.arguments)
            )),
            ContentBlock::ToolResult { result, .. } => match result {
                ToolResult::Ok { content } => {
                    parts.push(format!("→ {}", summarize_content(content)));
                }
                ToolResult::Err { message } => parts.push(format!("✗ {message}")),
                _ => parts.push("→ done".to_string()),
            },
            other => parts.push(format!("{other:?}")),
        }
    }
    let body = parts.join("\n");
    let mut lines = Vec::new();
    for (index, part) in wrap(&body, body_width).into_iter().enumerate() {
        let plain = if index == 0 {
            format!("{label} {part}")
        } else {
            format!("{:indent$}{part}", "")
        };
        let line = if index == 0 {
            Line::from(vec![
                Span::styled(format!("{label} "), style.add_modifier(Modifier::BOLD)),
                Span::raw(part),
            ])
        } else {
            Line::from(Span::raw(format!("{:indent$}{part}", "")))
        };
        lines.push((line, plain));
    }
    lines
}

fn field_line(
    label: &str,
    input: &TextInput,
    focused: bool,
    width: usize,
    masked: bool,
) -> (Line<'static>, Option<u16>) {
    let style = if focused {
        focused_style()
    } else {
        label_style()
    };
    let prefix = format!("  {label}  ");
    let prefix_len = prefix.chars().count();
    let value: String = if masked {
        mask_secret(input.text())
    } else {
        input.text().to_string()
    };
    // The masked form changes the byte length, so its caret sits at the
    // end of the visible value.
    let cursor_bytes = if masked { value.len() } else { input.cursor() };
    let available = width.saturating_sub(prefix_len + 2).max(1);
    let (visible, column) = input_window(&value, cursor_bytes, available);
    let line = Line::from(vec![
        Span::styled(prefix, style),
        Span::styled("[", hint_style()),
        Span::raw(visible),
        Span::styled("]", hint_style()),
    ]);
    let caret = focused.then_some(prefix_len as u16 + 1 + column);
    (line, caret)
}

/// The reasoning on/off row (Enter or ←/→ toggles).
fn reasoning_row(setup: &Setup) -> (Line<'static>, Option<u16>) {
    let focused = setup.field == 1;
    let value = if setup.reasoning_enabled { "On" } else { "Off" };
    let value_style = if !setup.reasoning_enabled {
        note_style()
    } else if focused {
        focused_style()
    } else {
        Style::default()
    };
    let line = Line::from(vec![
        Span::styled(
            "  Reasoning ",
            if focused {
                focused_style()
            } else {
                label_style()
            },
        ),
        Span::styled(format!("[{value}]"), value_style),
        Span::styled("    (Enter: toggle; Off sends \"none\")", hint_style()),
    ]);
    (line, None)
}

/// The effort level row (Enter opens the ladder popup).
fn effort_row(setup: &Setup) -> (Line<'static>, Option<u16>) {
    let focused = setup.field == 2;
    if !setup.reasoning_enabled {
        let line = Line::from(vec![
            Span::styled("  Effort    ", label_style()),
            Span::styled("(disabled — enable reasoning to choose)", hint_style()),
        ]);
        return (line, None);
    }
    let selection = format!("‹ {} ›", setup.reasoning_label());
    let line = Line::from(vec![
        Span::styled(
            "  Effort    ",
            if focused {
                focused_style()
            } else {
                label_style()
            },
        ),
        Span::styled(
            selection,
            if focused {
                focused_style()
            } else {
                Style::default()
            },
        ),
        Span::styled("    (Enter: pick from the list · ←/→: cycle)", hint_style()),
    ]);
    (line, None)
}

/// The explicit start row (Enter launches the chat).
fn start_row(setup: &Setup) -> (Line<'static>, Option<u16>) {
    let focused = setup.field == 3;
    let style = if focused {
        focused_style().add_modifier(Modifier::BOLD)
    } else {
        label_style()
    };
    let line = Line::from(vec![
        Span::raw("  ".to_string()),
        Span::styled("▶ Start chatting", style),
        Span::styled("    (Enter)", hint_style()),
    ]);
    (line, None)
}

/// The visible window of a single-line input and the caret's column in it.
///
/// Columns are display cells (wide characters count two), and the window
/// scrolls horizontally so the caret stays visible. `cursor_bytes` is a
/// byte offset at a char boundary.
fn input_window(text: &str, cursor_bytes: usize, width: usize) -> (String, u16) {
    let width = width.max(1);
    let cursor_width = UnicodeWidthStr::width(&text[..cursor_bytes]);
    let scroll = cursor_width.saturating_sub(width - 1);
    let mut visible = String::new();
    let mut visible_width = 0usize;
    let mut skipped = 0usize;
    let mut started = false;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if !started && skipped + ch_width <= scroll {
            skipped += ch_width;
            continue;
        }
        started = true;
        if visible_width + ch_width > width {
            break;
        }
        visible.push(ch);
        visible_width += ch_width;
    }
    let column = cursor_width.saturating_sub(skipped).min(width - 1) as u16;
    (visible, column)
}

/// Masks a secret, keeping the last four characters visible.
fn mask_secret(secret: &str) -> String {
    let len = secret.chars().count();
    if len == 0 {
        return String::new();
    }
    let tail: String = secret.chars().skip(len.saturating_sub(4)).collect();
    format!("••••{tail}")
}

fn entry_lines(entry: &Entry, width: usize) -> Vec<(Line<'static>, String)> {
    let (label, style, body) = match entry {
        Entry::User(text) => ("you", user_style(), text.clone()),
        Entry::Assistant(text) => ("assistant", assistant_style(), text.clone()),
        Entry::Reasoning(text) => ("thinking", reasoning_style(), text.clone()),
        Entry::ToolCall { name, arguments } => {
            ("tool", tool_style(), format!("{name}({arguments})"))
        }
        Entry::ToolResult { ok, summary } => {
            let marker = if *ok { "→" } else { "✗" };
            ("tool", tool_style(), format!("{marker} {summary}"))
        }
        Entry::Note(text) => ("note", note_style(), text.clone()),
        Entry::Error(text) => ("error", error_style(), text.clone()),
    };
    let indent = label.chars().count() + 2;
    let body_width = width.saturating_sub(indent).max(8);
    let mut lines = Vec::new();
    for (index, part) in wrap(&body, body_width).into_iter().enumerate() {
        let plain = if index == 0 {
            format!("{label} {part}")
        } else {
            format!("{:indent$}{part}", "")
        };
        let line = if index == 0 {
            Line::from(vec![
                Span::styled(format!("{label} "), style.add_modifier(Modifier::BOLD)),
                Span::raw(part),
            ])
        } else {
            Line::from(Span::raw(format!("{:indent$}{part}", "")))
        };
        lines.push((line, plain));
    }
    lines
}

/// Wraps text at `width` display columns (spaces preferred; long words are
/// hard-broken). Newlines in the input always start a new line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let mut current = String::new();
        let mut current_width = 0usize;
        for word in raw.split(' ') {
            let word_width = UnicodeWidthStr::width(word);
            if current_width > 0 && current_width + 1 + word_width > width {
                lines.push(std::mem::take(&mut current));
                current_width = 0;
            }
            if word_width > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                let mut chunk = String::new();
                let mut chunk_width = 0usize;
                for ch in word.chars() {
                    let ch_width = ch.width().unwrap_or(0);
                    if chunk_width + ch_width > width {
                        lines.push(std::mem::take(&mut chunk));
                        chunk_width = 0;
                    }
                    chunk.push(ch);
                    chunk_width += ch_width;
                }
                current = chunk;
                current_width = chunk_width;
                continue;
            }
            if current_width > 0 {
                current.push(' ');
                current_width += 1;
            }
            current.push_str(word);
            current_width += word_width;
        }
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_at_spaces() {
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc"]);
    }

    #[test]
    fn wrap_hard_breaks_long_words() {
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
    }

    #[test]
    fn input_window_scrolls_to_keep_the_caret_visible() {
        // Caret mid-text: the window starts at zero, caret on "d".
        let (visible, column) = input_window("abcdef", 3, 4);
        assert_eq!(visible, "abcd");
        assert_eq!(column, 3);

        // Caret at the end: the window scrolls so the caret sits right
        // after the last visible character.
        let (visible, column) = input_window("abcdef", 6, 4);
        assert_eq!(visible, "def");
        assert_eq!(column, 3);

        // Double-width characters occupy two display columns.
        let (visible, column) = input_window("你好", 6, 10);
        assert_eq!(visible, "你好");
        assert_eq!(column, 4);

        let (visible, column) = input_window("你好世界", 12, 4);
        assert_eq!(visible, "世界");
        assert_eq!(column, 3);

        let (visible, column) = input_window("", 0, 10);
        assert_eq!(visible, "");
        assert_eq!(column, 0);
    }

    #[test]
    fn wrap_counts_double_width_characters() {
        assert_eq!(wrap("你好世界", 4), vec!["你好", "世界"]);
    }
}
