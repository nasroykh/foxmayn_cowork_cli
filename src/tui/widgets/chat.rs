use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::app::{App, ChatRole};

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let title = match &app.working_dir {
        Some(dir) => format!(" {} ", dir.display()),
        None => " Chat ".to_string(),
    };

    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.chat_messages.is_empty() {
        let hint = if app.working_dir.is_none() {
            "No directory open. Use --dir <path> at startup or type /dir <path> here."
        } else {
            "Type a message and press Enter to chat with the assistant."
        };
        f.render_widget(
            Paragraph::new(hint)
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    for entry in &app.chat_messages {
        let (prefix, prefix_style, content_style) = match entry.role {
            ChatRole::User => (
                " You  ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(Color::White),
            ),
            ChatRole::Assistant => (
                " AI   ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(Color::White),
            ),
            ChatRole::Tool => (
                " Tool ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(Color::DarkGray),
            ),
            ChatRole::Error => (
                " Err  ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                Style::default().fg(Color::Red),
            ),
            ChatRole::Warning => (
                " Warn ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(Color::Yellow),
            ),
        };

        let content_lines: Vec<&str> = entry.content.lines().collect();
        if content_lines.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(prefix, prefix_style),
                Span::raw("│ "),
            ]));
        } else {
            for (i, content_line) in content_lines.iter().enumerate() {
                if i == 0 {
                    lines.push(Line::from(vec![
                        Span::styled(prefix, prefix_style),
                        Span::raw("│ "),
                        Span::styled(*content_line, content_style),
                    ]));
                } else {
                    lines.push(Line::from(vec![
                        Span::raw("       │ "),
                        Span::styled(*content_line, content_style),
                    ]));
                }
            }
        }

        lines.push(Line::from(""));
    }

    if let Some(text) = &app.streaming_text {
        let prefix_style = Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD);
        let content_style = Style::default().fg(Color::White);
        let content_lines: Vec<&str> = text.lines().collect();

        if content_lines.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(" AI   ", prefix_style),
                Span::raw("│ "),
                Span::styled("▌", content_style),
            ]));
        } else {
            for (i, line) in content_lines.iter().enumerate() {
                let is_last = i == content_lines.len() - 1;
                let mut spans = if i == 0 {
                    vec![
                        Span::styled(" AI   ", prefix_style),
                        Span::raw("│ "),
                        Span::styled(*line, content_style),
                    ]
                } else {
                    vec![
                        Span::raw("       │ "),
                        Span::styled(*line, content_style),
                    ]
                };
                if is_last {
                    spans.push(Span::styled("▌", content_style));
                }
                lines.push(Line::from(spans));
            }
        }
    } else if app.is_loading {
        lines.push(Line::from(vec![
            Span::styled(
                " AI   ",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("│ "),
            Span::styled(
                "thinking...",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
    }

    // Compute actual display rows accounting for line wrapping.
    let col_width = (inner.width.max(1)) as usize;
    let total: u16 = lines
        .iter()
        .map(|line| {
            let w = line.width();
            if w == 0 { 1usize } else { w.div_ceil(col_width) }
        })
        .sum::<usize>()
        .min(u16::MAX as usize) as u16;

    let visible = inner.height;
    // auto-scroll offset: keeps bottom visible when chat_scroll == 0
    let bottom_offset = total.saturating_sub(visible);
    let scroll = bottom_offset.saturating_sub(app.chat_scroll as u16);

    f.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false }),
        inner,
    );
}
