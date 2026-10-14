use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, test::TermSize};
use alacritty_terminal::vte::ansi::Processor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
}

/// sitty's emulation interface. Swapping the core (e.g. to libghostty-vt) means a new impl of this trait.
pub trait Terminal: Send {
    /// Feed PTY output. Returns bytes the terminal wants written back to the PTY
    /// (answers to queries such as cursor-position reports). Callers MUST write them.
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8>;
    fn resize(&mut self, size: Size);
    fn size(&self) -> Size;
    /// Visible lines, top to bottom, trailing spaces trimmed.
    fn screen_lines(&self) -> Vec<String>;
    /// History + screen, oldest first, at most `max` most-recent lines.
    fn scrollback_lines(&self, max: usize) -> Vec<String>;
    fn cursor(&self) -> Cursor;
}

/// Collects `PtyWrite` events (query replies) emitted while parsing.
#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<u8>>>);

impl EventListener for Replies {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(s) = event {
            self.0.lock().unwrap().extend_from_slice(s.as_bytes());
        }
    }
}

pub struct AlacrittyTerminal {
    term: Term<Replies>,
    parser: Processor,
    replies: Replies,
    size: Size,
}

impl AlacrittyTerminal {
    pub fn new(size: Size, scrollback: usize) -> Self {
        let replies = Replies::default();
        let config = Config { scrolling_history: scrollback, ..Config::default() };
        let dims = TermSize::new(size.cols as usize, size.rows as usize);
        let term = Term::new(config, &dims, replies.clone());
        Self { term, parser: Processor::new(), replies, size }
    }

    fn line_text(&self, line: i32) -> String {
        let grid = self.term.grid();
        let row = &grid[Line(line)];
        let mut s = String::with_capacity(grid.columns());
        for col in 0..grid.columns() {
            let cell = &row[Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            s.push(cell.c);
            if let Some(extra) = cell.zerowidth() {
                s.extend(extra.iter());
            }
        }
        s.trim_end().to_string()
    }
}

impl Terminal for AlacrittyTerminal {
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.parser.advance(&mut self.term, bytes);
        std::mem::take(&mut *self.replies.0.lock().unwrap())
    }

    fn resize(&mut self, size: Size) {
        self.term.resize(TermSize::new(size.cols as usize, size.rows as usize));
        self.size = size;
    }

    fn size(&self) -> Size {
        self.size
    }

    fn screen_lines(&self) -> Vec<String> {
        (0..self.term.grid().screen_lines() as i32).map(|l| self.line_text(l)).collect()
    }

    fn scrollback_lines(&self, max: usize) -> Vec<String> {
        let grid = self.term.grid();
        let top = grid.topmost_line().0;
        let bottom = grid.screen_lines() as i32 - 1;
        let all: Vec<String> = (top..=bottom).map(|l| self.line_text(l)).collect();
        let skip = all.len().saturating_sub(max);
        all.into_iter().skip(skip).collect()
    }

    fn cursor(&self) -> Cursor {
        let p = self.term.grid().cursor.point;
        Cursor { row: p.line.0 as u16, col: p.column.0 as u16 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(cols: u16, rows: u16) -> AlacrittyTerminal {
        AlacrittyTerminal::new(Size { cols, rows }, 100)
    }

    #[test]
    fn plain_text_lands_on_screen() {
        let mut t = term(20, 3);
        t.feed(b"hello\r\nworld");
        assert_eq!(t.screen_lines(), vec!["hello", "world", ""]);
        assert_eq!(t.cursor(), Cursor { row: 1, col: 5 });
    }

    #[test]
    fn colors_are_not_text() {
        let mut t = term(20, 2);
        t.feed(b"\x1b[31mred\x1b[0m ok");
        assert_eq!(t.screen_lines()[0], "red ok");
    }

    #[test]
    fn scrollback_keeps_lines_that_left_the_screen() {
        let mut t = term(20, 3);
        t.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        assert_eq!(t.screen_lines(), vec!["three", "four", "five"]);
        assert_eq!(t.scrollback_lines(100), vec!["one", "two", "three", "four", "five"]);
        assert_eq!(t.scrollback_lines(2), vec!["four", "five"]);
    }

    #[test]
    fn replies_to_cursor_position_query() {
        let mut t = term(20, 3);
        let reply = t.feed(b"ab\x1b[6n");
        assert_eq!(reply, b"\x1b[1;3R");
    }

    #[test]
    fn wide_chars_have_no_spacer_gaps() {
        let mut t = term(20, 2);
        t.feed("你好 🙂!".as_bytes());
        assert_eq!(t.screen_lines()[0], "你好 🙂!");
    }

    #[test]
    fn resize_changes_size() {
        let mut t = term(20, 3);
        t.resize(Size { cols: 40, rows: 5 });
        assert_eq!(t.size(), Size { cols: 40, rows: 5 });
        assert_eq!(t.screen_lines().len(), 5);
    }
}
