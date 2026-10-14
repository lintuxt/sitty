use alacritty_terminal::vte::{Parser, Perform};

/// ANSI-stripped plain-text log of a pane's output, capped in bytes, with absolute offsets.
pub struct Transcript {
    parser: Parser,
    text: String,
    base: usize,
    cap: usize,
}

struct Sink<'a>(&'a mut String);

impl Perform for Sink<'_> {
    fn print(&mut self, c: char) {
        self.0.push(c);
    }

    fn execute(&mut self, byte: u8) {
        if byte == b'\n' {
            self.0.push('\n');
        }
    }
}

impl Transcript {
    pub fn new(cap: usize) -> Self {
        Self { parser: Parser::new(), text: String::new(), base: 0, cap }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut sink = Sink(&mut self.text);
        self.parser.advance(&mut sink, bytes);
        self.trim();
    }

    pub fn end(&self) -> usize {
        self.base + self.text.len()
    }

    pub fn since(&self, offset: usize) -> &str {
        let mut start = offset.saturating_sub(self.base).min(self.text.len());
        while !self.text.is_char_boundary(start) {
            start += 1;
        }
        &self.text[start..]
    }

    fn trim(&mut self) {
        if self.text.len() <= self.cap {
            return;
        }
        let mut cut = self.text.len() - self.cap;
        while !self.text.is_char_boundary(cut) {
            cut += 1;
        }
        self.text.drain(..cut);
        self.base += cut;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escape_sequences_and_carriage_returns() {
        let mut t = Transcript::new(1024);
        t.feed(b"\x1b[1;32mok\x1b[0m\r\nnext\x1b]0;title\x07 line");
        assert_eq!(t.since(0), "ok\nnext line");
    }

    #[test]
    fn since_returns_only_new_text() {
        let mut t = Transcript::new(1024);
        t.feed(b"$ ");
        let mark = t.end();
        t.feed(b"make\r\nbuilt\r\n$ ");
        assert_eq!(t.since(mark), "make\nbuilt\n$ ");
    }

    #[test]
    fn cap_drops_oldest_text_but_offsets_stay_absolute() {
        let mut t = Transcript::new(8);
        t.feed(b"0123456789");
        assert_eq!(t.end(), 10);
        assert_eq!(t.since(0), "23456789");
        assert_eq!(t.since(9), "9");
    }

    #[test]
    fn cap_never_splits_a_utf8_char() {
        let mut t = Transcript::new(4);
        t.feed("aé你好".as_bytes()); // 1 + 2 + 3 + 3 bytes
        assert!(t.since(0).len() <= 4);
        assert_eq!(t.since(0), "好");
    }

    #[test]
    fn escape_sequence_split_across_feeds() {
        let mut t = Transcript::new(1024);
        t.feed(b"a\x1b[3");
        t.feed(b"1mb");
        assert_eq!(t.since(0), "ab");
    }
}
