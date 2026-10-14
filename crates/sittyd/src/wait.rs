//! `pane.wait`: block until a pane exits, new output matches a regex, or it goes quiet.

use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use sitty_proto::{ErrorKind, RpcError, Until, WaitResult};
use tokio::time::Instant;

use crate::pane::PaneRuntime;

pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;
pub const MAX_TIMEOUT_MS: u64 = 600_000;

fn err(kind: ErrorKind, msg: impl Into<String>) -> RpcError {
    RpcError::new(kind, msg.into())
}

/// Never holds the pane io lock across an `.await`.
pub async fn wait(rt: Arc<PaneRuntime>, until: Until, timeout_ms: Option<u64>) -> Result<WaitResult, RpcError> {
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS));
    let deadline = Instant::now() + timeout;
    let re = match &until {
        Until::Match(p) => Some(Regex::new(p).map_err(|e| err(ErrorKind::InvalidParams, e.to_string()))?),
        _ => None,
    };
    let mut rx = rt.subscribe();
    // Only output produced after this point may satisfy a match.
    let start = rt.io().transcript.end();

    loop {
        let mut wake = deadline;
        {
            let io = rt.io();
            match &until {
                Until::Exit => {
                    if let Some(code) = io.exit_code {
                        return Ok(WaitResult { reason: "exit".into(), exit_code: Some(code), matched: None });
                    }
                }
                Until::Match(_) => {
                    if let Some(m) = re.as_ref().and_then(|re| re.find(io.transcript.since(start))) {
                        return Ok(WaitResult { reason: "match".into(), exit_code: None, matched: Some(m.as_str().into()) });
                    }
                    if io.exit_code.is_some() {
                        return Err(err(ErrorKind::PaneExited, "exited before match"));
                    }
                }
                Until::Quiet(ms) => {
                    let quiet_at = Instant::from_std(io.last_output) + Duration::from_millis(*ms);
                    if Instant::now() >= quiet_at {
                        return Ok(WaitResult { reason: "quiet".into(), exit_code: None, matched: None });
                    }
                    wake = wake.min(quiet_at);
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(err(ErrorKind::Timeout, format!("after {} ms", timeout.as_millis())));
        }
        tokio::select! {
            _ = rx.changed() => {}
            _ = tokio::time::sleep_until(wake) => {}
        }
    }
}
