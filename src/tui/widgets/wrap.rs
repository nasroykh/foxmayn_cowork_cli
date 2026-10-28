//! Word-wrapping of styled lines. The chat panel wraps text itself (instead of letting
//! `Paragraph` do it) so the number of rendered rows is known exactly for scrolling and
//! per-entry caching.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

#[derive(Clone, Copy)]
struct Cell {
    ch: char,
    style: Style,
    width: usize,
}

fn char_width(c: char) -> usize {
    if c.is_ascii() {
        1
    } else {
        Span::raw(c.to_string()).width()
    }
}

/// Flatten a line into styled cells. Tabs become four spaces; other control characters
/// (ESC, CR, ...) are dropped so file names / tool output can never drive the terminal.
fn cells(line: &Line) -> Vec<Cell> {
    let mut out = Vec::new();
    for span in &line.spans {
        let style = line.style.patch(span.style);
        for ch in span.content.chars() {
            match ch {
                '\t' => out.extend((0..4).map(|_| Cell {
                    ch: ' ',
                    style,
                    width: 1,
                })),
                c if c.is_control() => {}
                c => out.push(Cell {
                    ch: c,
                    style,
                    width: char_width(c),
                }),
            }
        }
    }
    out
}

/// Greedy word wrap into rows of at most `width` columns (words longer than a row are split).
/// Whitespace at a wrap point is dropped. Always returns at least one row.
fn wrap_cells(cells: &[Cell], width: usize) -> Vec<Vec<Cell>> {
    let width = width.max(1);
    let mut rows: Vec<Vec<Cell>> = vec![Vec::new()];
    let mut cur = 0usize;
    let mut i = 0;
    while i < cells.len() {
        if cells[i].ch.is_whitespace() {
            let c = cells[i];
            if cur + c.width > width {
                rows.push(Vec::new());
                cur = 0;
            } else {
                rows.last_mut().expect("rows is never empty").push(c);
                cur += c.width;
            }
            i += 1;
            continue;
        }
        let start = i;
        let mut word_w = 0;
        while i < cells.len() && !cells[i].ch.is_whitespace() {
            word_w += cells[i].width;
            i += 1;
        }
        let word = &cells[start..i];
        if cur + word_w <= width {
            rows.last_mut().expect("rows is never empty").extend(word);
            cur += word_w;
        } else if word_w <= width {
            trim_trailing_space(rows.last_mut().expect("rows is never empty"));
            rows.push(word.to_vec());
            cur = word_w;
        } else {
            for c in word {
                if cur + c.width > width && cur > 0 {
                    rows.push(Vec::new());
                    cur = 0;
                }
                rows.last_mut().expect("rows is never empty").push(*c);
                cur += c.width;
            }
        }
    }
    rows
}

fn trim_trailing_space(row: &mut Vec<Cell>) {
    while row.last().is_some_and(|c| c.ch.is_whitespace()) {
        row.pop();
    }
}

/// Re-assemble cells into spans, merging neighbours that share a style.
fn row_spans(row: &[Cell]) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut style = Style::default();
    for c in row {
        if !buf.is_empty() && c.style != style {
            spans.push(Span::styled(std::mem::take(&mut buf), style));
        }
        style = c.style;
        buf.push(c.ch);
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    spans
}

/// Wrap one styled line into owned span rows of at most `width` columns.
pub fn wrap_line(line: &Line, width: usize) -> Vec<Vec<Span<'static>>> {
    wrap_cells(&cells(line), width)
        .iter()
        .map(|r| row_spans(r))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(text: &str, width: usize) -> Vec<String> {
        wrap_line(&Line::from(text.to_string()), width)
            .into_iter()
            .map(|r| r.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn wraps_on_word_boundaries() {
        // Plain div_ceil(width) row counting disagrees with word wrapping on inputs like this.
        assert_eq!(
            rows("aaaaaa bbbbbb cccccc", 10),
            vec!["aaaaaa", "bbbbbb", "cccccc"]
        );
        assert_eq!(rows("aaa bbb ccc", 8), vec!["aaa bbb", "ccc"]);
    }

    #[test]
    fn splits_overlong_words() {
        assert_eq!(rows("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn empty_line_is_one_row() {
        assert_eq!(rows("", 10), vec![""]);
    }

    #[test]
    fn wide_chars_respect_columns() {
        // Each CJK char is 2 columns: 3 per row at width 6.
        assert_eq!(rows("你好世界你好", 6), vec!["你好世", "界你好"]);
    }

    #[test]
    fn control_chars_are_stripped() {
        assert_eq!(rows("a\x1b[31mb\rc\td", 20), vec!["a[31mbc    d"]);
    }

    #[test]
    fn no_row_exceeds_width() {
        for w in 1..30 {
            for r in rows("the quick brown fox 你好 jumps over the lazy dog", w) {
                assert!(Span::raw(r.clone()).width() <= w.max(2), "{w}: {r:?}");
            }
        }
    }
}
