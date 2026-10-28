use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::wrap::wrap_line;
use crate::app::App;

/// Rows taken by chrome inside the dialog: 2 border + 1 spacer + 1 y/n prompt.
const CHROME_ROWS: usize = 4;

pub fn render(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(pending) = &app.pending_confirmation else {
        return;
    };

    let width = dialog_width(area.width, 76);
    let text_width = width.saturating_sub(2).max(1) as usize;
    let rows: Vec<Vec<Span<'static>>> = pending
        .description
        .lines()
        .flat_map(|l| wrap_line(&Line::from(l.to_string()), text_width))
        .collect();

    let height = dialog_height(area.height, rows.len());
    let text_height = (height as usize).saturating_sub(CHROME_ROWS);
    let max_scroll = rows.len().saturating_sub(text_height);
    app.confirm_scroll = app.confirm_scroll.min(max_scroll);
    let scroll = app.confirm_scroll;
    let hidden_below = rows.len().saturating_sub(scroll + text_height);

    let popup = centered_rect(width, height, area);
    f.render_widget(Clear, popup);

    let title = if max_scroll > 0 {
        format!(
            " Confirm action — rows {}-{} of {} (↑/↓ scroll) ",
            scroll + 1,
            (scroll + text_height).min(rows.len()),
            rows.len()
        )
    } else {
        " Confirm action ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Line::from(Span::styled(
            title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));

    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),    // description
            Constraint::Length(1), // spacer
            Constraint::Length(1), // y/n prompt
        ])
        .split(inner);

    let window: Vec<Line> = rows
        .into_iter()
        .skip(scroll)
        .take(text_height)
        .map(Line::from)
        .collect();
    f.render_widget(
        Paragraph::new(window).style(Style::default().fg(Color::White)),
        chunks[0],
    );

    let mut prompt = Vec::new();
    if hidden_below > 0 {
        prompt.push(Span::styled(
            format!("↓ …and {hidden_below} more   "),
            Style::default().fg(Color::Yellow),
        ));
    }
    prompt.extend([
        Span::styled(
            "[y]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Confirm   "),
        Span::styled(
            "[n]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" / "),
        Span::styled(
            "[Esc]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Cancel"),
    ]);
    f.render_widget(
        Paragraph::new(Line::from(prompt)).alignment(Alignment::Center),
        chunks[2],
    );
}

/// `percent` of `area_width`, computed in u32 so wide terminals cannot overflow u16.
fn dialog_width(area_width: u16, percent: u32) -> u16 {
    (area_width as u32 * percent / 100).min(area_width as u32) as u16
}

/// Height for `rows` description rows plus chrome, never more than the area.
fn dialog_height(area_height: u16, rows: usize) -> u16 {
    (rows.saturating_add(CHROME_ROWS)).min(area_height as usize) as u16
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_does_not_overflow_on_huge_terminals() {
        // 65535 * 76 overflows u16 (and u32 would be fine); result is 76 %.
        assert_eq!(dialog_width(u16::MAX, 76), 49_806);
        assert_eq!(dialog_width(100, 76), 76);
        assert_eq!(dialog_width(0, 76), 0);
    }

    #[test]
    fn height_fits_content_but_never_exceeds_area() {
        assert_eq!(dialog_height(40, 3), 7);
        assert_eq!(dialog_height(10, 500), 10);
        assert_eq!(dialog_height(0, 5), 0);
    }

    #[test]
    fn popup_stays_inside_area() {
        let area = Rect::new(5, 2, 80, 20);
        let r = centered_rect(76, 30, area);
        assert!(r.right() <= area.right() && r.bottom() <= area.bottom());
    }
}
