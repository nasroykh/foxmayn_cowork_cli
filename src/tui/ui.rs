use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tui_textarea::TextArea;

use super::widgets;
use crate::app::{App, InputMode};

pub fn render(f: &mut Frame, app: &App, textarea: &TextArea) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),    // main content
            Constraint::Length(3), // input bar
        ])
        .split(f.area());

    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25), // file tree
            Constraint::Percentage(75), // chat
        ])
        .split(root[0]);

    widgets::file_tree::render(f, app, content[0]);
    widgets::chat::render(f, app, content[1]);

    if app.input_mode == InputMode::Confirming {
        widgets::confirmation::render(f, app, content[1]);
    }

    render_input_bar(f, app, textarea, root[1]);
}

fn render_input_bar(f: &mut Frame, app: &App, textarea: &TextArea, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(13), // status indicator
        ])
        .split(area);

    f.render_widget(textarea, chunks[0]);

    let (dot, dot_color, label) = if app.health_status {
        ("●", Color::Green, " Online")
    } else {
        ("●", Color::Red, "Offline")
    };

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(dot, Style::default().fg(dot_color)),
            Span::raw(" "),
            Span::styled(label, Style::default().fg(Color::DarkGray)),
            Span::raw(" "),
        ]))
        .block(Block::default().borders(Borders::ALL)),
        chunks[1],
    );
}
