//! End-to-end test against a real frps.
//!
//! Ignored by default, because it needs a frps binary and a config. Run it
//! with:
//!
//! ```text
//! RUN_REAL_FRPS_TESTS=1 \
//! FRPS_BIN=/path/to/frps \
//! FRPS_CONFIG=tests/fixtures/frps-integration.toml \
//! cargo test --test real_frps -- --ignored --nocapture
//! ```
//!
//! `#[ignore]` rather than an early return on purpose: an early return reports
//! as a pass, so a CI job that forgets the environment variables would look
//! green while testing nothing.

#![allow(clippy::field_reassign_with_default)]

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tiny_frpc::config::{Config, Proxy, ProxyType};
use tiny_frpc::runner::gateway::GatewayRunner;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};

/// Must match tests/fixtures/frps-integration.toml.
const SSH_PORT: u16 = 22_220;
const REMOTE_PORT: u16 = 26_000;
const PROXY_NAME: &str = "rust-real-metadatas";
const TOKEN: &str = "rust-tiny-frpc-integration";

fn frps() -> Option<(String, String)> {
    std::env::var_os("RUN_REAL_FRPS_TESTS")?;
    let bin = std::env::var("FRPS_BIN").expect("FRPS_BIN is required");
    let config = std::env::var("FRPS_CONFIG")
        .expect("FRPS_CONFIG is required; use tests/fixtures/frps-integration.toml");
    Some((bin, config))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real frps: set RUN_REAL_FRPS_TESTS=1, FRPS_BIN and FRPS_CONFIG"]
async fn real_frps_gateway_round_trip_with_metadatas() {
    let Some((bin, config)) = frps() else {
        return;
    };

    let mut frps = Command::new(bin)
        .arg("-c")
        .arg(config)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start frps");
    wait_for_port(SSH_PORT, Duration::from_secs(10)).await;

    // A local echo service stands in for a real backend.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind echo service");
    let local_port = listener.local_addr().expect("echo address").port();
    let echo = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept echo client");
        let mut buffer = [0u8; 4096];
        loop {
            let count = stream.read(&mut buffer).await.expect("read echo client");
            if count == 0 {
                break;
            }
            stream
                .write_all(&buffer[..count])
                .await
                .expect("write echo client");
        }
    });

    let config = real_config(local_port);
    let runner = Arc::new(GatewayRunner::new(&config));
    let runner_task = tokio::spawn(Arc::clone(&runner).run());

    let mut public = wait_for_port_and_connect(REMOTE_PORT, Duration::from_secs(15)).await;
    let payload = b"real-frps-rust-round-trip";
    public.write_all(payload).await.expect("write public");
    let mut received = vec![0u8; payload.len()];
    public.read_exact(&mut received).await.expect("read public");
    assert_eq!(received, payload);

    runner.close();
    let _ = tokio::time::timeout(Duration::from_secs(10), runner_task).await;
    let _ = echo.await;
    stop(&mut frps).await;
}

/// A proxy that carries metadatas, which is only accepted by frps >= 0.61.2.
/// A successful round trip therefore also proves the `--metadatas` encoding is
/// the one this frps release expects.
fn real_config(local_port: u16) -> Config {
    let mut config = Config::default();
    config.server_addr = "127.0.0.1".to_string();
    config.server_port = SSH_PORT;
    config.metadatas_enabled = true;
    config.auth.token = TOKEN.to_string();
    let mut proxy = Proxy::default();
    proxy.name = PROXY_NAME.to_string();
    proxy.proxy_type = ProxyType::Tcp;
    proxy.local_ip = "127.0.0.1".to_string();
    proxy.local_port = local_port;
    proxy.remote_port = REMOTE_PORT;
    proxy
        .metadatas
        .insert("round".to_string(), "trip".to_string());
    config.proxies.push(proxy);
    config.complete();
    config
}

/// The `--user` footgun, against a real frps: the client sends the bare name and
/// frps applies the prefix, so the registered proxy is `{user}.{name}` and not
/// `{user}.{user}.{name}`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a real frps: set RUN_REAL_FRPS_TESTS=1, FRPS_BIN and FRPS_CONFIG"]
async fn real_frps_applies_the_user_prefix_exactly_once() {
    let Some((bin, config)) = frps() else {
        return;
    };
    let mut frps = Command::new(bin)
        .arg("-c")
        .arg(config)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start frps");
    wait_for_port(SSH_PORT, Duration::from_secs(10)).await;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind echo");
    let local_port = listener.local_addr().expect("echo address").port();

    let mut config = real_config(local_port);
    config.user = "hqu".to_string();
    config.proxies[0].remote_port = REMOTE_PORT + 1;
    config.proxies[0].name = "double-prefix".to_string();
    config.complete();

    // The command frps will parse. Pinning it here is what makes the assertion
    // below about frps' log meaningful.
    assert_eq!(
        tiny_frpc::command::gen_extra(&config.proxies[0], &config),
        format!(
            "tcp --proxy-name double-prefix --remote-port {} --metadatas round=trip --user hqu \
             --token {TOKEN}",
            REMOTE_PORT + 1
        ),
        "the client must send the bare name and let frps prefix it"
    );

    let runner = Arc::new(GatewayRunner::new(&config));
    let task = tokio::spawn(Arc::clone(&runner).run());
    // Give frps time to register the proxy and flush its log.
    tokio::time::sleep(Duration::from_secs(2)).await;
    runner.close();
    let _ = tokio::time::timeout(Duration::from_secs(10), task).await;

    let output = read_and_stop(&mut frps).await;
    assert!(
        output.contains("hqu.double-prefix"),
        "frps should have registered hqu.double-prefix, log was:\n{output}"
    );
    assert!(
        !output.contains("hqu.hqu.double-prefix"),
        "the client must not double the user prefix, log was:\n{output}"
    );
}

async fn wait_for_port(port: u16, budget: Duration) {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("port {port} did not become ready");
}

async fn wait_for_port_and_connect(port: u16, budget: Duration) -> TcpStream {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)).await {
            return stream;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "proxy port {port} did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn stop(child: &mut Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
}

/// Kills frps and returns everything it wrote to stdout and stderr, so a test
/// can assert on what frps thought it registered.
async fn read_and_stop(child: &mut Child) -> String {
    use tokio::io::AsyncReadExt;
    stop(child).await;
    let mut output = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut output).await;
    }
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut output).await;
    }
    output
}
