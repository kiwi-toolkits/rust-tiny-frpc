#![allow(clippy::field_reassign_with_default)]

//! A fake frps ssh tunnel gateway.
//!
//! Enough of frp's `pkg/ssh` to exercise the client without a real frps: accept
//! the `tcpip-forward` request, read the `exec` payload, answer with a scripted
//! response, and record what it saw.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use russh::keys::key::safe_rng;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Session};
use russh::{Channel, ChannelId};
use tokio::sync::{Mutex, Notify};

/// The message frps answers a duplicate proxy name with
/// (`server/control.go`).
pub const CONFLICT_MESSAGE: &str = "proxy [demo] already exists";

/// The banner frp's `createSuccessInfo` writes on a successful registration.
pub const SUCCESS_BANNER: &str =
    "\nfrp (via SSH) (Ctrl+C to quit)\n\nUser: \nProxyName: x\nType: tcp\nRemoteAddress: :0\n";

/// How the gateway answers a session.
#[derive(Clone)]
pub enum Response {
    /// Write the success banner and keep the channel open, like a registered
    /// proxy does.
    Accept,
    /// Write this text and close the channel — frps' rejection path.
    Reject(String),
}

/// What the gateway does with a session, from the test's point of view.
pub struct GatewayOptions {
    /// The steady-state response.
    pub response: Response,
    /// The first N sessions answer with a proxy conflict regardless of
    /// `response`, so a test can drive the rename path and then let it succeed.
    pub conflicts_first: usize,
    /// Open a `forwarded-tcpip` channel back to the client after a successful
    /// registration, the way frps does when a public connection arrives.
    pub forward_after_accept: bool,
}

impl GatewayOptions {
    pub fn accept() -> Self {
        Self {
            response: Response::Accept,
            conflicts_first: 0,
            forward_after_accept: false,
        }
    }

    pub fn reject(message: impl Into<String>) -> Self {
        Self {
            response: Response::Reject(message.into()),
            conflicts_first: 0,
            forward_after_accept: false,
        }
    }

    /// Always a conflict, which is what a permanently taken proxy name looks
    /// like.
    pub fn always_conflict() -> Self {
        Self::reject(CONFLICT_MESSAGE)
    }

    /// A conflict for the first `count` sessions, then a successful
    /// registration.
    pub fn conflict_then_accept(count: usize) -> Self {
        Self {
            response: Response::Accept,
            conflicts_first: count,
            forward_after_accept: false,
        }
    }

    /// Registers successfully, then immediately opens one forwarded channel so
    /// the client's bridging path runs.
    pub fn accept_and_forward() -> Self {
        Self {
            forward_after_accept: true,
            ..Self::accept()
        }
    }
}

/// One session the gateway handled.
#[derive(Debug, Clone)]
pub struct Connection {
    pub command: String,
    pub forwarded_address: String,
    pub forwarded_port: u32,
}

/// A running fake gateway. Dropping it stops the accept loop.
pub struct FakeGateway {
    pub port: u16,
    connections: Arc<Mutex<Vec<Connection>>>,
    forwards: Arc<AtomicUsize>,
    notify: Arc<Notify>,
    _running: tokio::task::JoinHandle<()>,
}

impl FakeGateway {
    pub async fn start(options: GatewayOptions) -> Self {
        let key =
            PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519).expect("generate a host key");
        let config = Arc::new(russh::server::Config {
            keys: vec![key],
            auth_rejection_time: Duration::from_millis(1),
            auth_rejection_time_initial: Some(Duration::ZERO),
            // Keep a session alive long enough for a test to observe it.
            inactivity_timeout: Some(Duration::from_secs(60)),
            ..Default::default()
        });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind fake gateway");
        let port = listener.local_addr().expect("fake gateway address").port();

        let connections = Arc::new(Mutex::new(Vec::new()));
        let forwards = Arc::new(AtomicUsize::new(0));
        let notify = Arc::new(Notify::new());
        let state = Arc::new(GatewayState {
            connections: Arc::clone(&connections),
            forwards: Arc::clone(&forwards),
            notify: Arc::clone(&notify),
            seen: AtomicUsize::new(0),
            conflicts_first: options.conflicts_first,
            response: options.response.clone(),
            forward_after_accept: options.forward_after_accept,
        });

        // Drive the accept loop directly rather than through
        // `Server::run_on_socket` so the listener can move into the task.
        let running = {
            let state = Arc::clone(&state);
            let config = Arc::clone(&config);
            tokio::spawn(async move {
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        return;
                    };
                    let handler = GatewayHandler {
                        state: Arc::clone(&state),
                        forwarded: None,
                    };
                    let config = Arc::clone(&config);
                    tokio::spawn(async move {
                        let _ = russh::server::run_stream(config, socket, handler).await;
                    });
                }
            })
        };

        Self {
            port,
            connections,
            forwards,
            notify,
            _running: running,
        }
    }

    /// How many `forwarded-tcpip` channels the gateway has opened.
    pub fn forwarded_channels(&self) -> usize {
        self.forwards.load(Ordering::SeqCst)
    }

    pub async fn connections(&self) -> Vec<Connection> {
        self.connections.lock().await.clone()
    }

    /// Waits until at least `count` sessions have arrived, or `timeout`
    /// elapses, then returns everything seen.
    pub async fn wait_for(&self, count: usize, timeout: Duration) -> Vec<Connection> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let seen = self.connections().await;
            if seen.len() >= count || tokio::time::Instant::now() >= deadline {
                return seen;
            }
            let notified = self.notify.notified();
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(deadline) => {}
            }
        }
    }
}

/// Shared between every connection the gateway accepts.
struct GatewayState {
    connections: Arc<Mutex<Vec<Connection>>>,
    forwards: Arc<AtomicUsize>,
    notify: Arc<Notify>,
    seen: AtomicUsize,
    conflicts_first: usize,
    response: Response,
    forward_after_accept: bool,
}

struct GatewayHandler {
    state: Arc<GatewayState>,
    forwarded: Option<(String, u32)>,
}

impl russh::server::Handler for GatewayHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _: &str) -> Result<Auth, Self::Error> {
        // frps behaves this way when no authorizedKeysFile is configured.
        Ok(Auth::Accept)
    }

    async fn auth_publickey(
        &mut self,
        _: &str,
        _: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        // The client falls back to no authentication when it has no key, but a
        // developer machine does have one, so accept both.
        Ok(Auth::Accept)
    }

    async fn tcpip_forward(
        &mut self,
        address: &str,
        port: &mut u32,
        _: &mut Session,
    ) -> Result<bool, Self::Error> {
        // frps ignores the requested address and port and always answers on the
        // channel it opened for the session.
        self.forwarded = Some((address.to_string(), *port));
        Ok(true)
    }

    async fn channel_open_session(
        &mut self,
        _: Channel<Msg>,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        let (address, port) = self
            .forwarded
            .clone()
            .unwrap_or_else(|| ("0.0.0.0".to_string(), 80));
        self.state.connections.lock().await.push(Connection {
            command: command.clone(),
            forwarded_address: address,
            forwarded_port: port,
        });
        let index = self.state.seen.fetch_add(1, Ordering::SeqCst);
        self.state.notify.notify_waiters();

        session.channel_success(channel)?;
        let response = if index < self.state.conflicts_first {
            &Response::Reject(CONFLICT_MESSAGE.to_string())
        } else {
            &self.state.response
        };
        match response {
            Response::Accept => {
                session.data(channel, SUCCESS_BANNER.as_bytes().to_vec())?;
                // Deliberately left open: a registered tunnel stays up.
                if self.state.forward_after_accept {
                    open_a_forwarded_channel(session.handle(), &self.state.forwards);
                }
            }
            Response::Reject(message) => {
                // frps writes the reason as session output and closes, without
                // setting an exit status.
                session.data(channel, format!("{message}\n").into_bytes())?;
                session.close(channel)?;
            }
        }
        Ok(())
    }
}

/// Opens the channel frps opens when a public connection arrives, then drains
/// whatever the client bridges through it.
fn open_a_forwarded_channel(handle: russh::server::Handle, forwards: &Arc<AtomicUsize>) {
    let forwards = Arc::clone(forwards);
    tokio::spawn(async move {
        let Ok(mut channel) = handle
            .channel_open_forwarded_tcpip("0.0.0.0", 80, "127.0.0.1", 1)
            .await
        else {
            return;
        };
        forwards.fetch_add(1, Ordering::SeqCst);
        let _ = channel.data(&b"ping"[..]).await;
        loop {
            match channel.wait().await {
                Some(russh::ChannelMsg::Data { .. }) => {}
                Some(russh::ChannelMsg::Close) | None => break,
                Some(_) => {}
            }
        }
    });
}
