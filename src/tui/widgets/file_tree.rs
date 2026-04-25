use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
};

use crate::app::{App, Panel};

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let focused = app.focused_panel == Panel::FileTree;
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let title = match &app.working_dir {
        Some(dir) => format!(
            " {} ",
            dir.file_name().unwrap_or_default().to_string_lossy()
        ),
        None => " Files ".to_string(),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title);

    if app.file_tree.is_empty() {
        let msg = if app.working_dir.is_none() {
            "No directory"
        } else {
            "Empty"
        };
        f.render_widget(
            Paragraph::new(msg)
                .style(Style::default().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .file_tree
        .iter()
        .map(|entry| {
            let indent = "  ".repeat(entry.depth);
            let (prefix, style) = if entry.is_dir {
                let arrow = if entry.expanded { "▼ " } else { "▶ " };
                (
                    format!("{indent}{arrow}"),
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                (format!("{indent}  "), Style::default().fg(Color::White))
            };
            ListItem::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(entry.name.as_str(), style),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.file_tree_scroll.min(app.file_tree.len() - 1)));

    f.render_stateful_widget(
        List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED)),
        area,
        &mut state,
    );
}
