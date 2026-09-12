//! Rendering: the setup wizard and the chat screen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{ChatState, CommandSpec, Entry, Status, TextInput, effort_label};
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
                "Model   ",
                &setup.model,
                setup.field == 0,
                width,
                false,
            ));
            rows.push((Line::default(), None));
            rows.push(effort_line(setup, width));
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
        Step::Model => {
            "Tab: model/thinking · ←/→ or space: pick · Enter: start chatting · Esc: back"
        }
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
}

/// Draws the chat screen.
pub fn draw_chat(frame: &mut Frame, chat: &ChatState) {
    let area = frame.area();
    let matches = chat.command_matches();
    let suggestions_height = if matches.is_empty() {
        0
    } else {
        (matches.len() as u16 + 2).min(8)
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(suggestions_height),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);
    draw_transcript(frame, chat, chunks[0]);
    if !matches.is_empty() {
        draw_suggestions(frame, &matches, chunks[1]);
    }
    draw_input(frame, chat, chunks[2]);
    draw_status(frame, chat, chunks[3]);
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

fn draw_transcript(frame: &mut Frame, chat: &ChatState, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" chat ");
    let inner_width = area.width.saturating_sub(2).max(8) as usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    for entry in &chat.transcript {
        if matches!(entry, Entry::Reasoning(_)) && !chat.show_thinking {
            continue;
        }
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
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let cursor_chars = chat.input.text()[..chat.input.cursor()].chars().count();
    let (visible, column) = input_window(chat.input.text(), cursor_chars, inner.width as usize);
    frame.render_widget(Paragraph::new(visible), inner);
    frame.set_cursor_position((inner.x + column, inner.y));
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
    // The masked form changes the character count, so its caret sits at
    // the end of the visible value.
    let cursor_chars = if masked {
        value.chars().count()
    } else {
        input.text()[..input.cursor()].chars().count()
    };
    let available = width.saturating_sub(prefix_len + 2).max(1);
    let (visible, column) = input_window(&value, cursor_chars, available);
    let line = Line::from(vec![
        Span::styled(prefix, style),
        Span::styled("[", hint_style()),
        Span::raw(visible),
        Span::styled("]", hint_style()),
    ]);
    let caret = focused.then_some(prefix_len as u16 + 1 + column);
    (line, caret)
}

fn effort_line(setup: &Setup, width: usize) -> (Line<'static>, Option<u16>) {
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
    let caret = focused.then_some((prefix.chars().count() + selection.chars().count()) as u16);
    (Line::from(spans), caret)
}

/// The visible window of a single-line input and the caret's column in it.
///
/// The window scrolls horizontally so the caret stays visible.
fn input_window(text: &str, cursor_chars: usize, width: usize) -> (String, u16) {
    let width = width.max(1);
    let chars: Vec<char> = text.chars().collect();
    let scroll = cursor_chars
        .saturating_sub(width.saturating_sub(1))
        .min(chars.len());
    let end = (scroll + width).min(chars.len());
    let visible: String = chars[scroll..end].iter().collect();
    let column = (cursor_chars - scroll).min(width - 1) as u16;
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

fn entry_lines(entry: &Entry, width: usize) -> Vec<Line<'static>> {
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

        let (visible, column) = input_window("", 0, 10);
        assert_eq!(visible, "");
        assert_eq!(column, 0);
    }
}
