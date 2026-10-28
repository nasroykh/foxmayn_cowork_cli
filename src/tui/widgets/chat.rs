use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::wrap::wrap_line;
use crate::app::{App, ChatEntry, ChatRole};
use crate::config::{Provider, ThinkingDisplay};
use crate::llm::types::{OllamaThink, OllamaThinkLevel, ReasoningEffort};

/// Width of the `" AI   │ "` gutter drawn before every row of an entry.
const GUTTER: usize = 8;
const CONT_GUTTER: &str = "      │ ";

/// Wrapped lines of every finalized `ChatEntry`, so history is parsed (markdown) and wrapped
/// once per width instead of once per frame. `chat_messages` is append-only apart from the
/// replacements in `App` that call `invalidate`.
#[derive(Default)]
pub struct ChatCache {
    width: u16,
    entries_done: usize,
    lines: Vec<Line<'static>>,
    /// Total rows (cache + live tail) of the previous frame, to anchor a scrolled-up view.
    last_total: usize,
}

impl ChatCache {
    pub fn invalidate(&mut self) {
        *self = Self::default();
    }
}

pub fn render(f: &mut Frame, app: &mut App, area: Rect) {
    let title = match &app.working_dir {
        Some(dir) => format!(" {} ", dir.display()),
        None => " Chat ".to_string(),
    };

    let mut status = vec![Span::styled(
        status_line(app),
        Style::default().fg(Color::DarkGray),
    )];
    if app.config.skip_confirmations {
        status.insert(
            0,
            Span::styled(
                " CONFIRMATIONS OFF ",
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
        );
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_top(Line::from(status).alignment(Alignment::Right));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.chat_messages.is_empty() && app.streaming_text.is_none() && !app.is_loading {
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
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = inner.width as usize;
    let cache = &mut app.chat_cache;
    if cache.width != inner.width || cache.entries_done > app.chat_messages.len() {
        cache.invalidate();
        cache.width = inner.width;
    }
    for entry in &app.chat_messages[cache.entries_done..] {
        cache.lines.extend(entry_lines(entry, width));
        cache.lines.push(Line::from(""));
    }
    cache.entries_done = app.chat_messages.len();

    let tail = live_tail(app, width);

    let cache = &mut app.chat_cache;
    let total = cache.lines.len() + tail.len();
    let visible = inner.height as usize;
    let (scroll, start, end) = scroll_window(total, visible, app.chat_scroll, cache.last_total);
    cache.last_total = total;
    app.chat_scroll = scroll;

    let window: Vec<Line> = (start..end)
        .map(|i| match cache.lines.get(i) {
            Some(l) => l.clone(),
            None => tail[i - cache.lines.len()].clone(),
        })
        .collect();

    f.render_widget(Paragraph::new(window), inner);
}

/// Resolve the visible row range. `scroll` is the distance from the bottom; it is clamped to the
/// content, and a scrolled-up view is anchored (shifted by the rows added since `last_total`).
/// Returns `(scroll, start, end)`.
fn scroll_window(
    total: usize,
    visible: usize,
    scroll: usize,
    last_total: usize,
) -> (usize, usize, usize) {
    let mut scroll = scroll;
    if scroll > 0 && last_total > 0 && total > last_total {
        scroll += total - last_total;
    }
    let scroll = scroll.min(total.saturating_sub(visible));
    let end = total - scroll;
    (scroll, end.saturating_sub(visible), end)
}

/// Lay out `content` lines behind the role gutter, wrapping each to the remaining width.
fn gutter_lines(
    prefix: &'static str,
    prefix_style: Style,
    content: Vec<Line>,
    width: usize,
) -> Vec<Line<'static>> {
    let cw = width.saturating_sub(GUTTER).max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    for line in &content {
        for row in wrap_line(line, cw) {
            let mut spans = if out.is_empty() {
                vec![Span::styled(prefix, prefix_style), Span::raw("│ ")]
            } else {
                vec![Span::raw(CONT_GUTTER)]
            };
            spans.extend(row);
            out.push(Line::from(spans));
        }
    }
    if out.is_empty() {
        out.push(Line::from(vec![
            Span::styled(prefix, prefix_style),
            Span::raw("│ "),
        ]));
    }
    out
}

fn plain_lines(text: &str, style: Style) -> Vec<Line<'static>> {
    text.lines()
        .map(|l| Line::from(Span::styled(l.to_string(), style)))
        .collect()
}

fn entry_lines(entry: &ChatEntry, width: usize) -> Vec<Line<'static>> {
    let bold = |c: Color| Style::default().fg(c).add_modifier(Modifier::BOLD);
    match entry.role {
        ChatRole::Assistant => {
            let content = tui_markdown::from_str(&entry.content).lines;
            gutter_lines(" AI   ", bold(Color::Green), content, width)
        }
        ChatRole::User => gutter_lines(
            " You  ",
            bold(Color::Cyan),
            plain_lines(&entry.content, Style::default().fg(Color::White)),
            width,
        ),
        ChatRole::Tool => gutter_lines(
            " Tool ",
            bold(Color::Yellow),
            plain_lines(&entry.content, Style::default().fg(Color::DarkGray)),
            width,
        ),
        ChatRole::Thinking => gutter_lines(
            " Think",
            bold(Color::Magenta),
            plain_lines(
                &entry.content,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ),
            width,
        ),
        ChatRole::Error => gutter_lines(
            " Err  ",
            bold(Color::Red),
            plain_lines(&entry.content, Style::default().fg(Color::Red)),
            width,
        ),
        ChatRole::Warning => gutter_lines(
            " Warn ",
            bold(Color::Yellow),
            plain_lines(&entry.content, Style::default().fg(Color::Yellow)),
            width,
        ),
    }
}

/// Rows for in-progress content (reasoning, streamed text, spinner), rebuilt every frame.
fn live_tail(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let bold = |c: Color| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let cursor = |style: Style| Span::styled("▌", style);

    if let Some(thinking) = &app.thinking_text {
        let content_style = Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC);
        match app.config.thinking_display {
            ThinkingDisplay::Off => {}
            ThinkingDisplay::Inline => {
                let label = format!("[Thinking… ({} chars)]", thinking.chars().count());
                lines.extend(gutter_lines(
                    " Think",
                    bold(Color::Magenta),
                    vec![Line::from(Span::styled(label, content_style))],
                    width,
                ));
            }
            ThinkingDisplay::Full => {
                let mut content = plain_lines(thinking, content_style);
                match content.last_mut() {
                    Some(last) => last.spans.push(cursor(content_style)),
                    None => content.push(Line::from(cursor(content_style))),
                }
                lines.extend(gutter_lines(" Think", bold(Color::Magenta), content, width));
            }
        }
    }

    if let Some(text) = &app.streaming_text {
        let content_style = Style::default().fg(Color::White);
        let mut content = plain_lines(text, content_style);
        match content.last_mut() {
            Some(last) => last.spans.push(cursor(content_style)),
            None => content.push(Line::from(cursor(content_style))),
        }
        lines.extend(gutter_lines(" AI   ", bold(Color::Green), content, width));
    } else if app.is_loading {
        lines.extend(gutter_lines(
            " AI   ",
            bold(Color::Green),
            vec![Line::from(Span::styled(
                "thinking...",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ))],
            width,
        ));
    }
    lines
}

fn status_line(app: &App) -> String {
    let provider = match app.config.provider {
        Provider::OpenRouter => "openrouter",
        Provider::Ollama => "ollama",
        Provider::Local => "local",
    };

    // Strip provider prefix from model name (e.g. "google/gemini-…" → "gemini-…").
    let model = match app.config.model.split_once('/') {
        Some((_, short)) => short,
        None => &app.config.model,
    };
    // Truncate long model names.
    let model: String = if model.chars().count() > 24 {
        format!("{}…", model.chars().take(23).collect::<String>())
    } else {
        model.to_string()
    };

    let reasoning = match app.config.provider {
        Provider::OpenRouter => match app
            .config
            .openrouter_reasoning
            .as_ref()
            .and_then(|r| r.effort)
        {
            None => "off",
            Some(ReasoningEffort::XHigh) => "xhigh",
            Some(ReasoningEffort::High) => "high",
            Some(ReasoningEffort::Medium) => "medium",
            Some(ReasoningEffort::Low) => "low",
            Some(ReasoningEffort::Minimal) => "minimal",
            Some(ReasoningEffort::None) => "none",
        },
        Provider::Ollama => match app.config.ollama_think {
            None | Some(OllamaThink::OnOff(false)) => "off",
            Some(OllamaThink::OnOff(true)) | Some(OllamaThink::Level(OllamaThinkLevel::High)) => {
                "high"
            }
            Some(OllamaThink::Level(OllamaThinkLevel::Medium)) => "medium",
            Some(OllamaThink::Level(OllamaThinkLevel::Low)) => "low",
        },
        Provider::Local => "—",
    };

    format!(" {provider} · {model} · {reasoning} ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_view_shows_last_rows() {
        assert_eq!(scroll_window(100, 10, 0, 100), (0, 90, 100));
    }

    #[test]
    fn scroll_is_clamped_to_content() {
        assert_eq!(scroll_window(100, 10, 1_000_000, 100), (90, 0, 10));
        assert_eq!(scroll_window(5, 10, 3, 5), (0, 0, 5));
    }

    #[test]
    fn scrolled_view_stays_anchored_while_rows_grow() {
        assert_eq!(scroll_window(110, 10, 20, 100), (30, 70, 80));
        // At the bottom the view just follows.
        assert_eq!(scroll_window(110, 10, 0, 100), (0, 100, 110));
    }
}
