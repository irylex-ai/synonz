//! Rendering: the setup wizard and the chat screen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{ChatState, Entry, Status, TextInput, effort_label};
use crate::setup::{Setup, Step};

const SPINNER: [&str; 4] = ["|", "/", "-", "\\"];

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

fn cursor_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

fn hint_style() -> Style {
    Style::default().fg(Color::DarkGray)
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
    let mut lines: Vec<Line<'static>> = Vec::new();
    match setup.step {
        Step::Api => {
            lines.push(field_line(
                "Base URL",
                &setup.base_url,
                setup.field == 0,
                width,
                false,
            ));
            lines.push(Line::default());
            lines.push(field_line(
                "API key ",
                &setup.api_key,
                setup.field == 1,
                width,
                true,
            ));
        }
        Step::Model => {
            lines.push(field_line(
                "Model   ",
                &setup.model,
                setup.field == 0,
                width,
                false,
            ));
            lines.push(Line::default());
            lines.push(effort_line(setup, width));
        }
        Step::Chat => {}
    }
    lines.push(Line::default());
    if let Some(error) = &setup.error {
        lines.push(Line::from(Span::styled(
            format!("! {error}"),
            error_style(),
        )));
    }
    let hint = match setup.step {
        Step::Api => "Tab: next field · Enter: continue · Esc: quit",
        Step::Model => {
            "Tab: model/thinking · ←/→ or space: pick · Enter: start chatting · Esc: back"
        }
        Step::Chat => "",
    };
    lines.push(Line::from(Span::styled(hint.to_string(), hint_style())));

    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        inner,
    );
}

/// Draws the chat screen.
pub fn draw_chat(frame: &mut Frame, chat: &ChatState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);
    draw_transcript(frame, chat, chunks[0]);
    draw_input(frame, chat, chunks[1]);
    draw_status(frame, chat, chunks[2]);
}

fn draw_transcript(frame: &mut Frame, chat: &ChatState, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" chat ");
    let inner_width = area.width.saturating_sub(2).max(8) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for entry in &chat.transcript {
        lines.extend(entry_lines(entry, inner_width));
    }
    let max_scroll = lines.len().saturating_sub(inner_height);
    let offset = if chat.follow {
        max_scroll
    } else {
        (chat.scroll as usize).min(max_scroll)
    };
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .block(block)
            .scroll((offset.min(u16::MAX as usize) as u16, 0)),
        area,
    );
}

fn draw_input(frame: &mut Frame, chat: &ChatState, area: Rect) {
    let title = match chat.status {
        Status::Idle => " message ",
        Status::Running => " message (running — Esc cancels) ",
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width.max(1) as usize;
    let cursor_chars = chat.input.text()[..chat.input.cursor()].chars().count();
    let spans = input_spans(chat.input.text(), cursor_chars, width);
    frame.render_widget(Paragraph::new(Line::from(spans)), inner);
}

fn draw_status(frame: &mut Frame, chat: &ChatState, area: Rect) {
    let state = match chat.status {
        Status::Idle => "idle".to_string(),
        Status::Running => format!("running {}", SPINNER[chat.spinner % SPINNER.len()]),
    };
    let state_style = if chat.status == Status::Running {
        tool_style()
    } else {
        note_style()
    };
    let line = Line::from(vec![
        Span::styled(format!(" {} ", chat.model), assistant_style()),
        Span::styled("· ", hint_style()),
        Span::styled(
            format!("thinking {} ", effort_label(chat.effort)),
            note_style(),
        ),
        Span::styled("· ", hint_style()),
        Span::styled(format!("{state} "), state_style),
        Span::styled(" · Enter send · Esc cancel · Ctrl+C quit", hint_style()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn field_line(
    label: &str,
    input: &TextInput,
    focused: bool,
    width: usize,
    masked: bool,
) -> Line<'static> {
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
    let cursor_chars = if masked {
        value.chars().count()
    } else {
        input.text()[..input.cursor()].chars().count()
    };
    let available = width.saturating_sub(prefix_len + 2).max(1);
    let mut spans = vec![Span::styled(prefix, style)];
    spans.push(Span::styled("[", hint_style()));
    spans.extend(input_spans(&value, cursor_chars, available));
    spans.push(Span::styled("]", hint_style()));
    Line::from(spans)
}

fn effort_line(setup: &Setup, width: usize) -> Line<'static> {
    let focused = setup.field == 1;
    let prefix = "  Thinking  ";
    let selection = format!("‹ {} ›", setup.effort_label());
    let hint = "   (←/→ or space)";
    let mut spans = vec![Span::styled(
        prefix.to_string(),
        if focused {
            focused_style()
        } else {
            label_style()
        },
    )];
    spans.push(Span::styled(
        selection.clone(),
        if focused {
            focused_style()
        } else {
            Style::default()
        },
    ));
    let used = prefix.chars().count() + selection.chars().count() + hint.chars().count();
    if used < width {
        spans.push(Span::styled(hint.to_string(), hint_style()));
    }
    Line::from(spans)
}

/// Renders `text` with the cursor rendered reversed; the view scrolls
/// horizontally so the cursor stays visible within `width` columns.
fn input_spans(text: &str, cursor_chars: usize, width: usize) -> Vec<Span<'static>> {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    let scroll = cursor_chars.saturating_sub(width.saturating_sub(1));
    let start = scroll.min(chars.len());
    let end = (start + width).min(chars.len());
    let mut spans = Vec::new();
    let mut buffer = String::new();
    for (index, ch) in chars.iter().enumerate().take(end).skip(start) {
        if index == cursor_chars {
            if !buffer.is_empty() {
                spans.push(Span::raw(std::mem::take(&mut buffer)));
            }
            spans.push(Span::styled(ch.to_string(), cursor_style()));
        } else {
            buffer.push(*ch);
        }
    }
    if !buffer.is_empty() {
        spans.push(Span::raw(buffer));
    }
    if cursor_chars >= end && cursor_chars - start < width {
        spans.push(Span::styled("▌".to_string(), cursor_style()));
    }
    spans
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

fn entry_lines(entry: &Entry, width: usize) -> Vec<Line<'static>> {
    let (label, style, body) = match entry {
        Entry::User(text) => ("you", user_style(), text.clone()),
        Entry::Assistant(text) => ("assistant", assistant_style(), text.clone()),
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
        if index == 0 {
            lines.push(Line::from(vec![
                Span::styled(format!("{label} "), style.add_modifier(Modifier::BOLD)),
                Span::raw(part),
            ]));
        } else {
            lines.push(Line::from(Span::raw(format!("{:indent$}{part}", ""))));
        }
    }
    lines
}

/// Wraps text at `width` characters (spaces preferred; long words are
/// hard-broken). Newlines in the input always start a new line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let mut current = String::new();
        let mut current_len = 0usize;
        for word in raw.split(' ') {
            let word_len = word.chars().count();
            if current_len > 0 && current_len + 1 + word_len > width {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            if word_len > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                let mut chunk = String::new();
                let mut chunk_len = 0usize;
                for ch in word.chars() {
                    if chunk_len == width {
                        lines.push(std::mem::take(&mut chunk));
                        chunk_len = 0;
                    }
                    chunk.push(ch);
                    chunk_len += 1;
                }
                current = chunk;
                current_len = chunk_len;
                continue;
            }
            if current_len > 0 {
                current.push(' ');
                current_len += 1;
            }
            current.push_str(word);
            current_len += word_len;
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
    fn input_spans_show_a_cursor_when_empty() {
        let spans = input_spans("", 0, 10);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "▌");
    }
}
