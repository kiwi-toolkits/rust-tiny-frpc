//! The embedded runner: one SSH connection per proxy, with reconnect.
//!
//! frps registers exactly one proxy per SSH session, so a config with *n*
//! proxies keeps *n* connections open. Each connection survives independently:
//! one flapping proxy must not disturb the others.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::command::{self, ProxyCommand};
use crate::config::Config;
use crate::logging;
use crate::rename;
use crate::runner::RetryOptions;
use crate::ssh::gateway::{CloseState, TunnelClient, TunnelError, TunnelOptions, TunnelOutcome};

/// Runs every configured proxy until [`GatewayRunner::close`] is called.
pub struct GatewayRunner {
    commands: Vec<ProxyCommand>,
    options: TunnelOptionsTemplate,
    retry: RetryOptions,
    rename_enabled: bool,
    state: CloseState,
    /// Live tunnels, so `close()` can disconnect them instead of waiting for
    /// the next backoff tick.
    active: Mutex<HashMap<usize, Arc<TunnelClient>>>,
}

struct TunnelOptionsTemplate {
    key_path: Option<std::path::PathBuf>,
    known_hosts: Option<std::path::PathBuf>,
    max_forward_connections: usize,
}

impl GatewayRunner {
    pub fn new(config: &Config) -> Self {
        let commands = command::parse_to_gateway(config);
        for proxy in &config.proxies {
            command::warn_unrepresentable(proxy, config, "startup");
        }
        Self {
            commands,
            options: TunnelOptionsTemplate {
                key_path: config.ssh_private_key.clone(),
                known_hosts: config.ssh_known_hosts.clone(),
                max_forward_connections: config.max_forward_connections,
            },
            retry: RetryOptions::default(),
            rename_enabled: config.rename,
            state: CloseState::new(),
            active: Mutex::new(HashMap::new()),
        }
    }

    /// Overrides the retry pacing. Exists for tests, which cannot wait 30
    /// seconds to observe a backoff step.
    pub fn with_retry_options(mut self, retry: RetryOptions) -> Self {
        self.retry = retry;
        self
    }

    pub fn close(&self) {
        self.state.close();
        if let Ok(active) = self.active.lock() {
            for tunnel in active.values() {
                tunnel.close();
            }
        }
    }

    pub fn is_closed(&self) -> bool {
        self.state.is_closed()
    }

    /// Spawns one task per proxy and waits for them all.
    pub async fn run(self: Arc<Self>) {
        logging::info(format!("proxy total len: {}", self.commands.len()));
        let mut tasks = tokio::task::JoinSet::new();
        for (index, param) in self.commands.iter().cloned().enumerate() {
            let runner = Arc::clone(&self);
            tasks.spawn(async move {
                runner.run_proxy(param, index).await;
            });
        }
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                // A proxy task only ends on cancellation, so a join error means
                // it panicked: stop the others rather than keep a half-working
                // client alive.
                logging::error(format!("a proxy task failed: {error}"));
                self.close();
            }
        }
        logging::info("stopping ssh tunnel to frps");
    }

    /// Reconnect loop for a single proxy.
    async fn run_proxy(&self, param: ProxyCommand, index: usize) {
        // The un-renamed command. Every renamed candidate is rebuilt from this,
        // so prefixes never accumulate.
        let original = param.command.clone();
        // Sticky across reconnects: once an `ex_N_` name works we keep it, so
        // the public address stays stable.
        let mut ex_number = 0u32;
        let mut backoff = self.retry.reconnect_base_delay;

        while !self.state.is_closed() {
            let outcome = self
                .try_proxy(&param, &original, index, &mut ex_number)
                .await;
            let failed = outcome.is_err();
            match outcome {
                Ok(outcome) => {
                    logging::warn(format!(
                        "ssh tunnel to frps {outcome}, reconnecting in {backoff:?}"
                    ));
                    // A registered tunnel that frps then closed is normal (an
                    // frps restart, for instance), so recover quickly.
                    backoff = self.retry.reconnect_base_delay;
                }
                Err(TunnelError::Canceled) => return,
                Err(error) => {
                    logging::error(format!(
                        "proxy run error: {error}, reconnecting in {backoff:?}"
                    ));
                }
            }
            if !self.wait(backoff).await {
                return;
            }
            if failed {
                backoff = (backoff * 2).min(self.retry.reconnect_max_delay);
            }
        }
    }

    /// Runs one attempt, applying the same-name and rename retry phases.
    async fn try_proxy(
        &self,
        param: &ProxyCommand,
        original: &str,
        index: usize,
        ex_number: &mut u32,
    ) -> Result<TunnelOutcome, TunnelError> {
        // Phase 1: the name we actually want.
        //
        // With renaming off there is a single attempt per reconnect cycle,
        // which is exactly the Go client's behaviour: a conflict is a plain
        // failure and the backoff grows.
        //
        // With renaming on, the same name gets `max_same_name_retries` extra
        // attempts the *first* time only. Afterwards a single probe is enough,
        // which is what lets a vanished conflict restore the original name.
        let extra_same_name_attempts = if self.rename_enabled && *ex_number == 0 {
            self.retry.max_same_name_retries
        } else {
            0
        };
        for attempt in 0..=extra_same_name_attempts {
            match self.start(param, original, index).await {
                Ok(outcome) => return Ok(outcome),
                Err(TunnelError::Canceled) => return Err(TunnelError::Canceled),
                Err(TunnelError::ProxyConflict) if !self.rename_enabled => {
                    return Err(TunnelError::ProxyConflict)
                }
                Err(TunnelError::ProxyConflict) => {
                    if attempt < extra_same_name_attempts {
                        logging::warn(format!(
                            "proxy already exists, retrying with the same name in {:?}",
                            self.retry.conflict_retry_delay
                        ));
                        if !self.wait(self.retry.conflict_retry_delay).await {
                            return Err(TunnelError::Canceled);
                        }
                    }
                }
                Err(other) => return Err(other),
            }
        }

        // Phase 2: walk the deterministic ex_N_ sequence, rebuilding each
        // candidate from the original command so prefixes never stack.
        if *ex_number == 0 {
            *ex_number = 1;
        }
        for attempt in 0..self.retry.max_rename_retries {
            let conflicted = *ex_number;
            let candidate = rename::apply_rename_prefix(original, &rename::ex_prefix(conflicted));
            match self.start(param, &candidate, index).await {
                Ok(outcome) => return Ok(outcome),
                Err(TunnelError::Canceled) => return Err(TunnelError::Canceled),
                Err(TunnelError::ProxyConflict) => {
                    *ex_number = rename::next_ex(conflicted);
                    if attempt + 1 < self.retry.max_rename_retries {
                        logging::warn(format!(
                            "renamed proxy ex_{conflicted}_ still conflicts, retrying with \
                             ex_{}_ in {:?}",
                            *ex_number, self.retry.conflict_retry_delay
                        ));
                        if !self.wait(self.retry.conflict_retry_delay).await {
                            return Err(TunnelError::Canceled);
                        }
                    }
                }
                Err(other) => return Err(other),
            }
        }
        Err(TunnelError::ProxyConflict)
    }

    async fn start(
        &self,
        param: &ProxyCommand,
        command: &str,
        index: usize,
    ) -> Result<TunnelOutcome, TunnelError> {
        if self.state.is_closed() {
            return Err(TunnelError::Canceled);
        }
        logging::info(format!(
            "start to run: {}",
            command::redact_command(command)
        ));
        let tunnel = Arc::new(TunnelClient::new(TunnelOptions {
            local_addr: param.local_addr.clone(),
            server_addr: param.server_addr.clone(),
            command: command.to_string(),
            key_path: self.options.key_path.clone(),
            known_hosts: self.options.known_hosts.clone(),
            max_forward_connections: self.options.max_forward_connections,
        }));
        if let Ok(mut active) = self.active.lock() {
            active.insert(index, Arc::clone(&tunnel));
        }
        let result = tunnel.start().await;
        if let Ok(mut active) = self.active.lock() {
            active.remove(&index);
        }
        result
    }

    /// Sleeps, returning `false` if the runner was closed meanwhile.
    async fn wait(&self, delay: Duration) -> bool {
        tokio::select! {
            _ = self.state.cancelled() => false,
            _ = tokio::time::sleep(delay) => !self.state.is_closed(),
        }
    }
}

/// Runs the gateway client until a shutdown signal arrives.
pub async fn run(config: Config) -> ExitCode {
    let runner = Arc::new(GatewayRunner::new(&config));
    let runner_task = tokio::spawn(Arc::clone(&runner).run());
    wait_for_shutdown().await;
    logging::info("shutting down");
    runner.close();
    let _ = runner_task.await;
    ExitCode::SUCCESS
}

/// Resolves when the process should stop: SIGINT or SIGTERM on unix, Ctrl+C
/// elsewhere. Both binaries share this so neither can forget SIGTERM, which
/// matters because a service manager typically stops a process with it.
pub async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(signal) => signal,
            Err(error) => {
                logging::error(format!("cannot listen for SIGTERM: {error}"));
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(signal) => signal,
            Err(error) => {
                logging::error(format!("cannot listen for SIGINT: {error}"));
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = sigint.recv() => logging::info("received SIGINT"),
            _ = sigterm.recv() => logging::info("received SIGTERM"),
            _ = tokio::signal::ctrl_c() => logging::info("received ctrl-c"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        logging::info("received ctrl-c");
    }
}
