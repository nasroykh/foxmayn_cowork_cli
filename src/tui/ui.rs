use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};
use tui_textarea::TextArea;

use super::commands;
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
    render_slash_popup(f, app, content[1], root[1]);
}

fn render_slash_popup(f: &mut Frame, app: &App, chat_area: Rect, input_area: Rect) {
    if let Some(picker) = &app.slash_picker {
        render_picker(f, picker, chat_area, input_area);
    } else if !app.slash_completions.is_empty() {
        render_completions(f, app, chat_area, input_area);
    }
}

fn render_completions(f: &mut Frame, app: &App, chat_area: Rect, input_area: Rect) {
    let n = app.slash_completions.len() as u16;
    let popup_height = (n + 2).min(chat_area.height);
    let popup_width = 58u16.min(chat_area.width);
    let popup_y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect::new(chat_area.x, popup_y, popup_width, popup_height);

    let items: Vec<ListItem> = app
        .slash_completions
        .iter()
        .enumerate()
        .map(|(i, &cmd_idx)| {
            let cmd = &commands::COMMANDS[cmd_idx];
            let name = if cmd.has_picker {
                format!("{} …", cmd.name)
            } else if cmd.has_arg {
                format!("{} <…>", cmd.name)
            } else {
                cmd.name.to_string()
            };
            let line = Line::from(vec![
                Span::styled(
                    format!(" {:<20}", name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(cmd.description),
                Span::raw(" "),
            ]);
            let style = if i == app.slash_selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(line).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                " Commands — Tab/Enter to accept · Esc to dismiss ",
                Style::default().fg(Color::DarkGray),
            )),
    );

    f.render_widget(list, popup_area);
}

fn render_picker(
    f: &mut Frame,
    picker: &crate::app::SlashPicker,
    chat_area: Rect,
    input_area: Rect,
) {
    let n = picker.items.len() as u16;
    let popup_height = (n + 2).min(chat_area.height);
    let popup_width = 70u16.min(chat_area.width);
    let popup_y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect::new(chat_area.x, popup_y, popup_width, popup_height);

    let items: Vec<ListItem> = picker
        .items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let style = if i == picker.selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(format!(" {} ", item.display)).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(
                " ↑/↓ navigate · Enter select · Esc cancel ",
                Style::default().fg(Color::DarkGray),
            )),
    );

    f.render_widget(list, popup_area);
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
