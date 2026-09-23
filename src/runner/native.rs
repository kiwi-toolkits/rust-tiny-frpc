//! The native runner: spawn the system `ssh` once per proxy and be done.
//!
//! Deliberately featureless. The Go client's `nssh` build does exactly this:
//! no reconnect, no backoff, no rename, because the only feedback channel is
//! the child's exit status and there is no way to tell a name conflict from a
//! dropped network.

use std::process::ExitCode;
use std::sync::Arc;

use crate::command;
use crate::config::Config;
use crate::logging;
use crate::runner::gateway::wait_for_shutdown;
use crate::ssh::native::{NativeCancel, NativeChild};

pub async fn run(config: Config) -> ExitCode {
    let commands = command::parse_to_native_ssh(&config);
    for proxy in &config.proxies {
        command::warn_unrepresentable(proxy, &config, "startup");
    }
    logging::info(format!("proxy total len: {}", commands.len()));

    let cancel = NativeCancel::new();
    let mut tasks = tokio::task::JoinSet::new();
    for command_line in commands {
        let cancel = cancel.clone();
        tasks.spawn(async move {
            logging::info(format!(
                "start to run: {}",
                command::redact_command(&command_line)
            ));
            match NativeChild::spawn(&command_line) {
                Ok(child) => {
                    if let Err(error) = child.wait(&cancel).await {
                        logging::error(error);
                    }
                }
                Err(error) => logging::error(format!("failed to start ssh: {error}")),
            }
        });
    }

    let cancel = Arc::new(cancel);
    tokio::select! {
        _ = wait_for_shutdown() => {
            logging::info("shutting down");
            cancel.close();
        }
        _ = tasks.join_next() => {}
    }
    while tasks.join_next().await.is_some() {}
    logging::info("stopping native ssh tunnel to frps");
    ExitCode::SUCCESS
}
