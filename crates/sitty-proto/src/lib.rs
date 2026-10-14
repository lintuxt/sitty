//! sitty wire protocol: the only contract shared between the daemon and its clients.

pub mod client;
pub mod paths;
mod types;

pub use types::*;
