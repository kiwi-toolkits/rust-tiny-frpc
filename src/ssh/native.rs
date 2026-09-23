//! The `tiny-frpc-ssh` variant: drive the system `ssh` binary.
//!
//! This exists so a device that already ships OpenSSH does not have to pay for
//! a bundled SSH stack. It has no reconnect, no backoff and no rename support —
//! exactly like the Go client's `nssh` build, because there is no way to tell
//! *why* the child exited.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Notify;

use crate::logging;

/// Cancellation for the running `ssh` children.
#[derive(Clone, Default)]
pub struct NativeCancel {
    closed: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl NativeCancel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    async fn closed(&self) {
        let notified = self.notify.notified();
        if self.is_closed() {
            return;
        }
        notified.await
    }
}

/// A single `ssh` child process.
pub struct NativeChild {
    command: String,
    child: Child,
}

impl NativeChild {
    /// Spawns `command`, which must start with the program to run.
    pub fn spawn(command: &str) -> Result<Self, String> {
        let mut words = shell_words(command).into_iter();
        let program = words
            .next()
            .ok_or_else(|| "empty ssh command".to_string())?;
        let args: Vec<String> = words.collect();
        let child = Command::new(&program)
            .args(&args)
            // Without this the child inherits our stdin and can steal input
            // from whatever started us (a terminal, a supervisor, a pipe).
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false)
            .spawn()
            .map_err(|error| format!("spawn {program}: {error}"))?;
        Ok(Self {
            command: command.to_string(),
            child,
        })
    }

    /// Waits for the child, forwarding its output, until it exits or `cancel`
    /// fires.
    pub async fn wait(mut self, cancel: &NativeCancel) -> Result<(), String> {
        let stdout = self.child.stdout.take();
        let stderr = self.child.stderr.take();
        let out_task = stdout.map(|out| tokio::spawn(forward(out, false)));
        let err_task = stderr.map(|err| tokio::spawn(forward(err, true)));

        let status = tokio::select! {
            _ = cancel.closed() => {
                let _ = self.child.start_kill();
                let _ = self.child.wait().await;
                None
            }
            status = self.child.wait() => Some(status),
        };

        if let Some(task) = out_task {
            let _ = task.await;
        }
        if let Some(task) = err_task {
            let _ = task.await;
        }

        match status {
            None => Ok(()),
            Some(Ok(status)) if status.success() => Ok(()),
            Some(Ok(status)) => Err(format!(
                "ssh exited with {status}: {}",
                crate::command::redact_command(&self.command)
            )),
            Some(Err(error)) => Err(format!("ssh wait failed: {error}")),
        }
    }
}

async fn forward<R>(reader: R, is_stderr: bool)
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if is_stderr {
                    logging::warn(format!("ssh: {line}"));
                } else {
                    logging::info(format!("ssh: {line}"));
                }
            }
            Ok(None) => return,
            Err(error) => {
                logging::error(format!("ssh output error: {error}"));
                return;
            }
        }
    }
}

/// Splits a command line into words, honouring single and double quotes.
///
/// [`crate::command`] never emits quotes, so in practice this is equivalent to
/// splitting on whitespace; it exists so a hand-written command inside a config
/// file still behaves the way a shell would.
pub fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut has_word = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();

    while let Some(ch) = chars.next() {
        match quote {
            Some(active) => {
                if ch == active {
                    quote = None;
                } else if ch == '\\' && active == '"' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else {
                    current.push(ch);
                }
            }
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    has_word = true;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                ch if ch.is_whitespace() => {
                    if has_word || !current.is_empty() {
                        words.push(std::mem::take(&mut current));
                        has_word = false;
                    }
                }
                ch => {
                    current.push(ch);
                    has_word = true;
                }
            },
        }
    }
    if has_word || !current.is_empty() {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_plain_command() {
        assert_eq!(
            shell_words("ssh v0@1.2.3.4 -p 2200 -R :80:127.0.0.1:22 tcp"),
            vec![
                "ssh",
                "v0@1.2.3.4",
                "-p",
                "2200",
                "-R",
                ":80:127.0.0.1:22",
                "tcp"
            ]
        );
    }

    #[test]
    fn honours_quotes_and_escapes() {
        assert_eq!(shell_words("ssh \"a b\" 'c d'"), vec!["ssh", "a b", "c d"]);
        assert_eq!(shell_words(r#"ssh a\ b"#), vec!["ssh", "a b"]);
    }

    #[test]
    fn cancel_state_is_sticky() {
        let cancel = NativeCancel::new();
        assert!(!cancel.is_closed());
        cancel.close();
        assert!(cancel.is_closed());
    }
}
