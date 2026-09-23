//! The embedded SSH client that speaks to frps' ssh tunnel gateway.
//!
//! The protocol is short: authenticate as `v0`, ask for a reverse forward with
//! `tcpip-forward`, open a session channel, `exec` the proxy command, then
//! serve the forwarded connections that arrive on it. frps parses the command
//! server-side and does the actual proxying.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, Handle, Handler};
use russh::keys::{self, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{Channel, ChannelMsg, Disconnect};
use tokio::net::TcpStream;
use tokio::sync::{Notify, Semaphore};

use crate::logging;

/// Timeout for the ssh handshake, authentication, and the
/// `tcpip-forward`/`exec` sequence.
///
/// frps only waits 3s (`waitForwardAddrAndExtraPayload`) for the forward
/// request and the exec payload, so a setup slower than this can never succeed:
/// the tunnel must be torn down and rebuilt rather than waited on.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(15);

/// Idle keepalives. Without them a silently dropped connection (mobile network,
/// NAT rebinding) leaves a tunnel looking healthy forever.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_MAX: usize = 3;

/// Upper bound on the frps session output we buffer. frps writes a banner or a
/// single error line; a peer that keeps streaming must not be able to grow this
/// process without limit.
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// How long to wait for the local service to accept a forwarded connection.
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// The gateway's success banner (`createSuccessInfo` in frp's
/// `pkg/ssh/terminal.go`).
const SUCCESS_BANNER: &str = "frp (via SSH)";

const GATEWAY_USER: &str = "v0";

/// Hardcoded by the protocol: frps ignores the requested address and port and
/// always replies on the channel it opened for the session.
const FORWARD_ADDR: &str = "0.0.0.0";
const FORWARD_PORT: u32 = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelError {
    /// frps refused the proxy name because it is taken.
    ProxyConflict,
    /// The runner asked the tunnel to stop.
    Canceled,
    Other(String),
}

impl std::fmt::Display for TunnelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProxyConflict => f.write_str("proxy conflict on frps"),
            Self::Canceled => f.write_str("tunnel client canceled"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

/// How an attempt ended successfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelOutcome {
    /// frps closed a tunnel that had been registered.
    Closed,
}

impl std::fmt::Display for TunnelOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("closed")
    }
}

/// Cancellation shared between a tunnel and its runner. A tunnel that starts
/// after `close()` ran still observes it.
#[derive(Clone)]
pub struct CloseState {
    closed: Arc<std::sync::atomic::AtomicBool>,
    notify: Arc<Notify>,
}

impl Default for CloseState {
    fn default() -> Self {
        Self::new()
    }
}

impl CloseState {
    pub fn new() -> Self {
        Self {
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            notify: Arc::new(Notify::new()),
        }
    }

    pub fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Resolves when the tunnel has been closed, including when `close()`
    /// already ran.
    pub async fn cancelled(&self) {
        self.closed().await
    }

    async fn closed(&self) {
        // Register before re-checking so a `close()` racing this call cannot be
        // missed.
        let notified = self.notify.notified();
        if self.is_closed() {
            return;
        }
        notified.await
    }
}

/// Everything a tunnel needs to run one attempt.
#[derive(Clone)]
pub struct TunnelOptions {
    pub local_addr: String,
    pub server_addr: String,
    pub command: String,
    pub key_path: Option<PathBuf>,
    pub known_hosts: Option<PathBuf>,
    pub max_forward_connections: usize,
}

pub struct TunnelClient {
    options: TunnelOptions,
    state: CloseState,
    /// Live session, so `close()` can tear the tunnel down instead of merely
    /// asking it to stop.
    session: Arc<std::sync::Mutex<Option<Arc<Handle<TunnelHandler>>>>>,
}

impl TunnelClient {
    pub fn new(options: TunnelOptions) -> Self {
        Self {
            options,
            state: CloseState::new(),
            session: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    pub fn close(&self) {
        self.state.close();
        let session = self.session.lock().ok().and_then(|mut slot| slot.take());
        if let Some(session) = session {
            // Dropping the handle stops the ssh event loop and closes the
            // socket, so an idle tunnel cannot hold shutdown open.
            tokio::spawn(async move {
                let _ = session.disconnect(Disconnect::ByApplication, "", "").await;
            });
        }
    }

    pub async fn start(&self) -> Result<TunnelOutcome, TunnelError> {
        if self.state.is_closed() {
            return Err(TunnelError::Canceled);
        }

        let handler = TunnelHandler {
            local_addr: self.options.local_addr.clone(),
            known_hosts: self.options.known_hosts.clone(),
            server_addr: self.options.server_addr.clone(),
            permits: Arc::new(Semaphore::new(self.options.max_forward_connections.max(1))),
            state: self.state.clone(),
        };
        let config = Arc::new(client::Config {
            // A long-lived reverse tunnel must not be dropped for being idle;
            // reachability is proven by the keepalives instead.
            inactivity_timeout: None,
            keepalive_interval: Some(KEEPALIVE_INTERVAL),
            keepalive_max: KEEPALIVE_MAX,
            ..Default::default()
        });

        let mut session = tokio::time::timeout(
            SETUP_TIMEOUT,
            russh::client::connect(config, self.options.server_addr.as_str(), handler),
        )
        .await
        .map_err(|_| {
            add_port_hint(format!(
                "ssh handshake to {} timed out after {SETUP_TIMEOUT:?}",
                self.options.server_addr
            ))
        })?
        .map_err(|error| add_port_hint(error.to_string()))?;

        let auth = self.authenticate(&mut session).await?;
        if !auth.success() {
            return Err(TunnelError::Other("ssh authentication failed".to_string()));
        }
        if self.state.is_closed() {
            return Err(TunnelError::Canceled);
        }

        let session = Arc::new(session);
        if let Ok(mut slot) = self.session.lock() {
            *slot = Some(Arc::clone(&session));
        }

        let channel = tokio::time::timeout(SETUP_TIMEOUT, async {
            // Order matters. frps reads the forward request from the global
            // request channel and the exec payload from the session channel in
            // parallel, but it only lets 3s pass overall, and it drops an exec
            // payload that arrives before it starts listening.
            session.tcpip_forward(FORWARD_ADDR, FORWARD_PORT).await?;
            let channel = session.channel_open_session().await?;
            channel.exec(true, self.options.command.clone()).await?;
            Ok::<_, russh::Error>(channel)
        })
        .await
        .map_err(|_| {
            TunnelError::Other(format!(
                "ssh tunnel setup timed out after {SETUP_TIMEOUT:?}"
            ))
        })?
        .map_err(|error| TunnelError::Other(error.to_string()))?;

        logging::info(format!(
            "session start cmd [{}] success",
            crate::command::redact_command(&self.options.command)
        ));

        self.pump(channel, Arc::clone(&session)).await
    }

    /// Collects the session output until frps closes the channel.
    async fn pump(
        &self,
        mut channel: Channel<client::Msg>,
        session: Arc<Handle<TunnelHandler>>,
    ) -> Result<TunnelOutcome, TunnelError> {
        let mut output = Vec::new();
        let mut exit_status = None;
        loop {
            tokio::select! {
                _ = self.state.closed() => return Err(TunnelError::Canceled),
                message = channel.wait() => match message {
                    Some(ChannelMsg::Data { data }) => push_output(&mut output, &data),
                    Some(ChannelMsg::ExtendedData { data, .. }) => push_output(&mut output, &data),
                    Some(ChannelMsg::ExitStatus { exit_status: status }) => exit_status = Some(status),
                    Some(ChannelMsg::Close) | None => break,
                    // Everything else is a channel open/request reply with no
                    // session payload.
                    _ => {}
                }
            }
        }
        drop(channel);
        let _ = session.disconnect(Disconnect::ByApplication, "", "").await;

        let output = String::from_utf8_lossy(&output).into_owned();
        if output.len() >= MAX_OUTPUT_BYTES {
            logging::warn(format!(
                "frps session output exceeded {MAX_OUTPUT_BYTES} bytes and was truncated"
            ));
        }
        report_output(&output);
        if is_proxy_conflict(&output) {
            return Err(TunnelError::ProxyConflict);
        }
        // frps reports a rejected registration as plain session output and then
        // closes the channel, so anything that is not the success banner means
        // the proxy was never registered. Reporting that as a failure (rather
        // than a clean close) keeps the reconnect backoff growing and puts the
        // reason in the log.
        if !is_success_banner(&output) || exit_status.is_some_and(|status| status != 0) {
            return Err(TunnelError::Other(describe_failure(&output, exit_status)));
        }
        logging::info("ssh tunnel to frps closed");
        Ok(TunnelOutcome::Closed)
    }

    async fn authenticate(
        &self,
        session: &mut Handle<TunnelHandler>,
    ) -> Result<client::AuthResult, TunnelError> {
        let Some(key_path) = crate::keys::resolve_key_path(self.options.key_path.as_deref()) else {
            return self.authenticate_none(session).await;
        };
        let key = match keys::load_secret_key(&key_path, None) {
            Ok(key) => key,
            Err(error) => {
                logging::warn(format!(
                    "failed to load private key [{}]: {error}, connecting without client \
                     authentication",
                    key_path.display()
                ));
                return self.authenticate_none(session).await;
            }
        };
        logging::info(format!(
            "using private key [{}] to authenticate to frps",
            key_path.display()
        ));
        let hash = tokio::time::timeout(SETUP_TIMEOUT, session.best_supported_rsa_hash())
            .await
            .map_err(|_| TunnelError::Other("ssh authentication timed out".to_string()))?
            .map_err(|error| TunnelError::Other(error.to_string()))?
            .flatten();
        tokio::time::timeout(
            SETUP_TIMEOUT,
            session.authenticate_publickey(
                GATEWAY_USER,
                PrivateKeyWithHashAlg::new(Arc::new(key), hash),
            ),
        )
        .await
        .map_err(|_| TunnelError::Other("ssh authentication timed out".to_string()))?
        .map_err(|error| TunnelError::Other(error.to_string()))
    }

    async fn authenticate_none(
        &self,
        session: &mut Handle<TunnelHandler>,
    ) -> Result<client::AuthResult, TunnelError> {
        tokio::time::timeout(SETUP_TIMEOUT, session.authenticate_none(GATEWAY_USER))
            .await
            .map_err(|_| TunnelError::Other("ssh authentication timed out".to_string()))?
            .map_err(|error| TunnelError::Other(error.to_string()))
    }
}

fn push_output(output: &mut Vec<u8>, data: &[u8]) {
    if output.len() >= MAX_OUTPUT_BYTES {
        return;
    }
    let room = MAX_OUTPUT_BYTES - output.len();
    output.extend_from_slice(&data[..data.len().min(room)]);
}

fn is_success_banner(output: &str) -> bool {
    output.contains(SUCCESS_BANNER)
}

pub fn is_proxy_conflict(output: &str) -> bool {
    output.contains("already exists") || output.contains("router config conflict")
}

fn describe_failure(output: &str, exit_status: Option<u32>) -> String {
    let message = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("<no output>");
    match exit_status {
        Some(status) if status != 0 => {
            format!("proxy register failed (exit status {status}): {message}")
        }
        _ => format!("proxy register failed: {message}"),
    }
}

fn report_output(output: &str) {
    let info_level = is_success_banner(output);
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if info_level {
            logging::info(format!("frps: {line}"));
        } else {
            logging::warn(format!("frps: {line}"));
        }
    }
}

/// A failed handshake is nearly always the wrong port: frps has two listeners
/// and the gateway is not the one `bindPort` opens.
fn add_port_hint(error: String) -> TunnelError {
    if error.contains("handshake")
        || error.contains("Kex")
        || error.contains("EOF")
        || error.contains("invalid")
    {
        TunnelError::Other(format!(
            "{error} (hint: when the remote server is frps, serverPort must be the ssh tunnel \
             gateway port sshTunnelGateway.bindPort, NOT the native protocol port bindPort)"
        ))
    } else {
        TunnelError::Other(error)
    }
}

struct TunnelHandler {
    local_addr: String,
    server_addr: String,
    known_hosts: Option<PathBuf>,
    permits: Arc<Semaphore>,
    /// Used only to tell a real bridge failure from the expected teardown
    /// noise when the tunnel is closed.
    state: CloseState,
}

impl Handler for TunnelHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let Some(known_hosts) = &self.known_hosts else {
            return Ok(true);
        };
        // A certificate carries its own signing key; the recorded entry is the
        // plain host key, so verifying a certificate against known_hosts would
        // always fail. Say so instead.
        let PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            logging::error(
                "the server presented a certificate; sshKnownHosts can only verify plain host \
                 keys, refusing to connect",
            );
            return Ok(false);
        };
        let (host, port) = split_server_addr(&self.server_addr);
        match keys::check_known_hosts_path(&host, port, key, known_hosts) {
            Ok(true) => Ok(true),
            Ok(false) => {
                logging::error(format!(
                    "{host}:{port} is not present in {}, refusing to connect. Add the host key \
                     there to trust it",
                    known_hosts.display()
                ));
                Ok(false)
            }
            Err(error) => {
                logging::error(format!(
                    "failed to verify {host}:{port} against {}: {error}",
                    known_hosts.display()
                ));
                Ok(false)
            }
        }
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<client::Msg>,
        _: &str,
        _: u32,
        _: &str,
        _: u32,
        reply: client::ChannelOpenHandle,
        _: &mut client::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;

        // Cap how many connections are bridged at once. Without this a busy
        // frps can spawn forwardings faster than the local service retires them
        // and the device runs out of file descriptors.
        let Ok(permit) = Arc::clone(&self.permits).try_acquire_owned() else {
            logging::warn(format!(
                "dropping a forwarded connection to {}: already bridging the maximum number of \
                 connections",
                self.local_addr
            ));
            return Ok(());
        };

        let local_addr = self.local_addr.clone();
        let state = self.state.clone();
        tokio::spawn(async move {
            let _permit = permit;
            logging::info(format!("accept a new connection. local: {local_addr}"));
            let mut remote = channel.into_stream();
            // frps has already accepted the connection, so a local service that
            // is down or hanging must not pin this task forever.
            match tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(&local_addr)).await {
                Ok(Ok(mut local)) => {
                    if let Err(error) = tokio::io::copy_bidirectional(&mut remote, &mut local).await
                    {
                        // Tearing the tunnel down closes live channels in
                        // flight; that is not worth an error line.
                        if state.is_closed() {
                            logging::info(format!(
                                "connection to {local_addr} closed with the tunnel"
                            ));
                        } else {
                            logging::error(format!("ssh tunnel client bridge error: {error}"));
                        }
                    }
                    let _ = tokio::io::AsyncWriteExt::shutdown(&mut local).await;
                }
                Ok(Err(error)) => logging::error(format!(
                    "ssh tunnel client dial {local_addr} error: {error}"
                )),
                Err(_) => logging::error(format!(
                    "ssh tunnel client dial {local_addr} timed out after {DIAL_TIMEOUT:?}"
                )),
            }
        });
        Ok(())
    }
}

/// Splits `host:port`/`[host]:port` for the known-hosts lookup.
fn split_server_addr(server_addr: &str) -> (String, u16) {
    crate::util::parse_host_port(server_addr).unwrap_or_else(|_| (server_addr.to_string(), 22))
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn classifies_registration_failures() {
        assert!(is_success_banner(
            "\nfrp (via SSH) (Ctrl+C to quit)\n\nProxyName: d\n"
        ));
        assert!(!is_success_banner("unknown flag: --metadatas"));
        assert!(!is_success_banner(""));
    }

    #[test]
    fn classifies_conflicts() {
        assert!(is_proxy_conflict("proxy [u.demo] already exists"));
        assert!(is_proxy_conflict("router config conflict"));
        assert!(!is_proxy_conflict("unknown flag: --metadatas"));
    }

    #[test]
    fn describes_rejections_with_and_without_exit_status() {
        assert_eq!(
            describe_failure("unknown flag: --metadatas\n", None),
            "proxy register failed: unknown flag: --metadatas"
        );
        assert_eq!(
            describe_failure("port already used", Some(1)),
            "proxy register failed (exit status 1): port already used"
        );
        assert_eq!(
            describe_failure("", None),
            "proxy register failed: <no output>"
        );
    }

    #[test]
    fn caps_session_output() {
        let mut output = Vec::new();
        push_output(&mut output, &vec![b'a'; MAX_OUTPUT_BYTES]);
        assert_eq!(output.len(), MAX_OUTPUT_BYTES);
        push_output(&mut output, b"more");
        assert_eq!(output.len(), MAX_OUTPUT_BYTES);
    }

    #[test]
    fn adds_the_port_hint_only_for_handshake_failures() {
        let hinted = add_port_hint("ssh handshake failed".to_string()).to_string();
        assert!(hinted.contains("sshTunnelGateway.bindPort"));
        let plain = add_port_hint("connection reset".to_string()).to_string();
        assert!(!plain.contains("sshTunnelGateway.bindPort"));
    }

    #[test]
    fn splits_the_server_address_for_known_hosts() {
        assert_eq!(
            split_server_addr("1.2.3.4:2200"),
            ("1.2.3.4".to_string(), 2200)
        );
        assert_eq!(split_server_addr("[::1]:2200"), ("::1".to_string(), 2200));
    }

    #[test]
    fn close_state_is_sticky() {
        let state = CloseState::new();
        assert!(!state.is_closed());
        state.close();
        assert!(state.is_closed());
        // A fresh waiter must observe the already-closed state.
        let cloned = state.clone();
        let handle = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        handle.block_on(async move {
            tokio::time::timeout(Duration::from_millis(50), cloned.closed())
                .await
                .expect("closed() returned immediately");
        });
    }

    #[test]
    fn joins_ipv6_server_addresses() {
        assert_eq!(crate::util::join_host_port("::1", 2200), "[::1]:2200");
    }
}
