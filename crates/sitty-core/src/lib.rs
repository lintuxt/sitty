//! sitty's terminal-emulation boundary. Nothing outside this crate touches alacritty_terminal.

mod terminal;
mod transcript;

pub use terminal::{AlacrittyTerminal, Cursor, Size, Terminal};
pub use transcript::Transcript;
