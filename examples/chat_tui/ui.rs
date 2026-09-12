//! Rendering: the setup wizard and the chat screen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{ChatState, CommandSpec, Entry, Status, TextInput, effort_label};
use crate::setup::{PopupKind, Setup, Step};

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
    let (visible, column) =
        input_window(chat.input.text(), chat.input.cursor(), inner.width as usize);
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
