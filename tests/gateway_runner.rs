//! End-to-end tests of the reconnect and rename state machine against a fake
//! frps ssh tunnel gateway.
//!
//! The gateway is a real SSH server speaking the same subset of the protocol
//! that frp's `pkg/ssh` does, so these tests exercise the actual russh client
//! rather than a stub.

#![allow(clippy::field_reassign_with_default)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use support::{FakeGateway, GatewayOptions, CONFLICT_MESSAGE};
use tiny_frpc::config::{Config, Proxy, ProxyType};
use tiny_frpc::runner::gateway::GatewayRunner;
use tiny_frpc::runner::RetryOptions;

fn fast_retries() -> RetryOptions {
    RetryOptions {
        conflict_retry_delay: Duration::from_millis(20),
        reconnect_base_delay: Duration::from_millis(20),
        reconnect_max_delay: Duration::from_millis(80),
        ..RetryOptions::default()
    }
}

fn config_for(port: u16) -> Config {
    let mut config = Config::default();
    config.server_addr = "127.0.0.1".to_string();
    config.server_port = port;
    config.ssh_known_hosts = None;
    let mut proxy = Proxy::default();
    proxy.name = "demo".to_string();
    proxy.proxy_type = ProxyType::Tcp;
    proxy.local_ip = "127.0.0.1".to_string();
    proxy.local_port = 1; // never dialled in these tests
    proxy.remote_port = 8080;
    config.proxies.push(proxy);
    config.complete();
    config
}

/// Spawns the runner's reconnect loop in the background.
fn spawn_runner(runner: &Arc<GatewayRunner>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(Arc::clone(runner).run())
}

/// Stops the runner and waits for its task to finish.
async fn shutdown(runner: &Arc<GatewayRunner>, task: tokio::task::JoinHandle<()>) {
    runner.close();
    let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generates_a_payload_without_the_user_prefix() {
    let gateway = FakeGateway::start(GatewayOptions::always_conflict()).await;
    let mut config = config_for(gateway.port);
    config.user = "hqu".to_string();
    config.auth.token = "secret".to_string();
    config.complete();

    let runner = Arc::new(GatewayRunner::new(&config).with_retry_options(fast_retries()));
    let task = spawn_runner(&runner);
    let seen = gateway.wait_for(1, Duration::from_secs(5)).await;
    shutdown(&runner, task).await;

    assert_eq!(seen.len(), 1, "rename is off, so exactly one attempt");
    assert_eq!(
        seen[0].command, "tcp --proxy-name demo --remote-port 8080 --user hqu --token secret",
        "the client must not add the user prefix; frps does that"
    );
    assert_eq!(seen[0].forwarded_address, "0.0.0.0");
    assert_eq!(seen[0].forwarded_port, 80);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rename_off_retries_the_same_name_and_never_renames() {
    let gateway = FakeGateway::start(GatewayOptions::always_conflict()).await;
    let mut config = config_for(gateway.port);
    config.rename = false;
    config.complete();

    let runner = Arc::new(GatewayRunner::new(&config).with_retry_options(fast_retries()));
    let task = spawn_runner(&runner);
    // Wait long enough for several backoff cycles. `wait_for` polls rather than
    // sleeping a fixed amount, so a loaded CI machine does not turn this into a
    // flake.
    let seen = gateway.wait_for(3, Duration::from_secs(5)).await;
    shutdown(&runner, task).await;

    assert!(
        seen.len() >= 3,
        "the reconnect loop keeps trying: {}",
        seen.len()
    );
    for connection in &seen {
        assert_eq!(
            connection.command, "tcp --proxy-name demo --remote-port 8080",
            "with renaming off the name never changes"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rename_on_walks_the_ex_sequence_from_the_original_command() {
    // Three same-name attempts (the first plus `max_same_name_retries`), then
    // ex_1_, ex_2_, ex_3_ — six conflicts before the seventh succeeds.
    let gateway = FakeGateway::start(GatewayOptions::conflict_then_accept(6)).await;
    let mut config = config_for(gateway.port);
    config.rename = true;
    config.complete();

    let runner = Arc::new(GatewayRunner::new(&config).with_retry_options(fast_retries()));
    let task = spawn_runner(&runner);
    let seen = gateway.wait_for(7, Duration::from_secs(10)).await;
    shutdown(&runner, task).await;

    let commands: Vec<&str> = seen.iter().map(|c| c.command.as_str()).collect();
    assert_eq!(
        commands,
        vec![
            "tcp --proxy-name demo --remote-port 8080",
            "tcp --proxy-name demo --remote-port 8080",
            "tcp --proxy-name demo --remote-port 8080",
            "tcp --proxy-name ex_1_demo --remote-port 8080",
            "tcp --proxy-name ex_2_demo --remote-port 8080",
            "tcp --proxy-name ex_3_demo --remote-port 8080",
            // The sequence is exhausted, so the cycle restarts from the name we
            // actually want: this is what lets a conflict that has since
            // cleared restore the original name.
            "tcp --proxy-name demo --remote-port 8080",
        ],
        "same-name retries come first, then the sequence, each rebuilt from the original command"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rename_wraps_back_to_ex_1_after_ex_3() {
    // Nine conflicts: 1 same-name + ex_1_..ex_3_ (cycle 1) + ex_1_..ex_3_ (cycle
    // 2 is entered again because the sequence is sticky and restarts at whatever
    // the next value is). Assert the wrap rather than an exact count.
    let gateway = FakeGateway::start(GatewayOptions::always_conflict()).await;
    let mut config = config_for(gateway.port);
    config.rename = true;
    config.complete();

    let runner = Arc::new(GatewayRunner::new(&config).with_retry_options(fast_retries()));
    let task = spawn_runner(&runner);
    let seen = gateway.wait_for(9, Duration::from_secs(10)).await;
    shutdown(&runner, task).await;

    let names: Vec<String> = seen
        .iter()
        .map(|connection| {
            connection
                .command
                .split_whitespace()
                .skip_while(|token| *token != "--proxy-name")
                .nth(1)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert!(names.contains(&"ex_1_demo".to_string()), "{names:?}");
    assert!(names.contains(&"ex_2_demo".to_string()), "{names:?}");
    assert!(names.contains(&"ex_3_demo".to_string()), "{names:?}");
    // ex_1_ appears twice: once in the first cycle, once after wrapping.
    assert!(
        names.iter().filter(|name| *name == "ex_1_demo").count() >= 2,
        "the sequence must wrap back to ex_1_: {names:?}"
    );
    assert!(
        !names.iter().any(|name| name.contains("ex_1_ex_")),
        "prefixes must never stack: {names:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_registration_failure_that_is_not_a_conflict_is_reported_as_an_error() {
    // `unknown flag` is what an older frps answers when it is handed
    // --metadatas; it must not be mistaken for a conflict.
    let gateway = FakeGateway::start(GatewayOptions::reject("unknown flag: --metadatas")).await;
    let mut config = config_for(gateway.port);
    config.rename = true;
    config.complete();

    let runner = Arc::new(GatewayRunner::new(&config).with_retry_options(fast_retries()));
    let task = spawn_runner(&runner);
    // Poll rather than sleeping a fixed amount: the point is that the loop
    // keeps retrying, not how fast it does so on a given machine.
    let seen = gateway.wait_for(3, Duration::from_secs(5)).await;
    shutdown(&runner, task).await;

    assert!(seen.len() >= 3, "a non-conflict failure still retries");
    for connection in &seen {
        assert_eq!(
            connection.command, "tcp --proxy-name demo --remote-port 8080",
            "a flag rejection must not trigger a rename"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridges_a_forwarded_connection_to_the_local_service() {
    // A real echo service, so the bridged bytes can be observed end to end.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind echo service");
    let local_port = listener.local_addr().expect("echo address").port();
    let echo = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let (mut read, mut write) = stream.split();
        let _ = tokio::io::copy(&mut read, &mut write).await;
    });

    let gateway = FakeGateway::start(GatewayOptions::accept_and_forward()).await;
    let mut config = config_for(gateway.port);
    config.proxies[0].local_port = local_port;
    config.complete();

    let runner = Arc::new(GatewayRunner::new(&config).with_retry_options(fast_retries()));
    let task = spawn_runner(&runner);
    // Wait for the gateway to open the forwarded channel, then let the bridge
    // settle before tearing down.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while gateway.forwarded_channels() == 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    shutdown(&runner, task).await;

    assert_eq!(
        gateway.forwarded_channels(),
        1,
        "the client should have accepted the forwarded channel"
    );
    let _ = echo.await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dial_failure_does_not_take_the_tunnel_down() {
    // Nothing is listening on the configured local port, so the bridge fails.
    // The tunnel itself must stay registered.
    let gateway = FakeGateway::start(GatewayOptions::accept_and_forward()).await;
    let mut config = config_for(gateway.port);
    // Port 1 is reserved and nothing listens there.
    config.proxies[0].local_port = 1;
    config.complete();

    let runner = Arc::new(GatewayRunner::new(&config).with_retry_options(fast_retries()));
    let task = spawn_runner(&runner);
    tokio::time::sleep(Duration::from_millis(500)).await;
    let seen = gateway.connections().await;
    shutdown(&runner, task).await;

    assert_eq!(
        seen.len(),
        1,
        "a bridge failure must not make the client reconnect"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_default_response_is_the_message_frps_uses_for_a_taken_name() {
    // Guards the classification keywords against a typo in the fake.
    assert!(CONFLICT_MESSAGE.contains("already exists"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn metadatas_are_sent_only_when_enabled() {
    let gateway = FakeGateway::start(GatewayOptions::always_conflict()).await;
    let mut config = config_for(gateway.port);
    config.metadatas_enabled = false;
    config.proxies[0]
        .metadatas
        .insert("round".to_string(), "trip".to_string());
    config.complete();

    let runner = Arc::new(GatewayRunner::new(&config).with_retry_options(fast_retries()));
    let task = spawn_runner(&runner);
    let off = gateway.wait_for(1, Duration::from_secs(5)).await;
    shutdown(&runner, task).await;
    assert!(
        !off[0].command.contains("--metadatas"),
        "{}",
        off[0].command
    );

    let gateway = FakeGateway::start(GatewayOptions::always_conflict()).await;
    let mut config = config_for(gateway.port);
    config.metadatas_enabled = true;
    config.proxies[0]
        .metadatas
        .insert("round".to_string(), "trip".to_string());
    config.complete();

    let runner = Arc::new(GatewayRunner::new(&config).with_retry_options(fast_retries()));
    let task = spawn_runner(&runner);
    let on = gateway.wait_for(1, Duration::from_secs(5)).await;
    shutdown(&runner, task).await;
    assert!(
        on[0].command.contains("--metadatas round=trip"),
        "{}",
        on[0].command
    );
}
