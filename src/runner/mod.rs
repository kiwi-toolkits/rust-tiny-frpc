//! Process supervision: one runner per front end.
//!
//! Both runners expose the same tiny surface — `run()` until told to stop,
//! `close()` from a signal handler — so the shared entry point in `lib.rs` does
//! not care which one it built.

pub mod gateway;
pub mod native;

use std::time::Duration;

/// How the retry loop paces itself. Defaults follow the Go client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryOptions {
    /// Delay between two attempts that used the same proxy name.
    pub conflict_retry_delay: Duration,
    /// First delay of the reconnect backoff.
    pub reconnect_base_delay: Duration,
    /// Cap for the reconnect backoff.
    pub reconnect_max_delay: Duration,
    /// Same-name attempts before falling back to a rename. Only the first
    /// attempt of a run uses these.
    pub max_same_name_retries: u32,
    /// Renamed attempts before giving up and backing off.
    pub max_rename_retries: u32,
}

impl Default for RetryOptions {
    fn default() -> Self {
        Self {
            conflict_retry_delay: Duration::from_secs(5),
            reconnect_base_delay: Duration::from_secs(3),
            reconnect_max_delay: Duration::from_secs(30),
            max_same_name_retries: 2,
            max_rename_retries: 3,
        }
    }
}
