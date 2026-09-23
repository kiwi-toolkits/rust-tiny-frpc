//! Logging.
//!
//! Two rules drive the design:
//!
//! 1. Logs go to **stderr**. stdout belongs to the command's real output
//!    (`--version`, `--help`), so a caller that pipes stdout still gets a clean
//!    stream.
//! 2. Writing a log line must never be able to kill the process. With
//!    `panic = "abort"` in the release profile a panic here would take every
//!    tunnel down, and `println!` panics when the pipe it writes to is closed
//!    (`tiny-frpc | head -1`, or journald going away). Every write below is
//!    therefore a `let _ = ...`.

use std::io::Write;

use chrono::Local;

fn log(level: &str, message: impl AsRef<str>) {
    let line = format!(
        "[{}] [{}] {}",
        Local::now().to_rfc3339(),
        level,
        message.as_ref()
    );
    let stderr = std::io::stderr();
    let mut locked = stderr.lock();
    let _ = writeln!(locked, "{line}");
    let _ = locked.flush();
}

pub fn info(message: impl AsRef<str>) {
    log("INFO", message);
}

pub fn warn(message: impl AsRef<str>) {
    log("WARN", message);
}

pub fn error(message: impl AsRef<str>) {
    log("ERROR", message);
}
