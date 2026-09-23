//! A tiny frp client that drives the frps **SSH tunnel gateway**.
//!
//! This crate is not a reimplementation of `frpc`. It connects to frps over
//! SSH (the `sshTunnelGateway.bindPort` listener), asks for a reverse forward
//! with `tcpip-forward`, and then `exec`s a proxy command string that frps
//! parses server-side to register the proxy. Everything below the SSH layer is
//! frps' job.
//!
//! Two front ends share this library:
//!
//! * `tiny-frpc` speaks the SSH protocol itself (via `russh`) and can therefore
//!   reconnect and, optionally, rename a conflicting proxy.
//! * `tiny-frpc-ssh` shells out to the system `ssh` binary instead, so the
//!   device does not pay for a bundled SSH stack.

pub mod cli;
pub mod command;
pub mod config;
pub mod keys;
pub mod logging;
pub mod rename;
pub mod runner;
pub mod ssh;
pub mod util;
pub mod version;

use std::process::ExitCode;

use cli::{Args, BinaryKind, Parsed};

/// Entry point shared by both binaries.
pub async fn run(kind: BinaryKind) -> ExitCode {
    let args = std::env::args().skip(1);
    match cli::parse(args, kind) {
        Ok(Parsed::Version) => {
            println!("{}", version::full(kind));
            ExitCode::SUCCESS
        }
        Ok(Parsed::Help) => {
            println!("{}", cli::usage(kind));
            ExitCode::SUCCESS
        }
        Ok(Parsed::Run(args)) => run_client(args, kind).await,
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            eprintln!("{}", cli::usage(kind));
            ExitCode::from(2)
        }
    }
}

async fn run_client(args: Args, kind: BinaryKind) -> ExitCode {
    let mut config = match config::load(&args.config_path) {
        Ok(config) => config,
        Err(message) => {
            logging::error(format!(
                "load config [{}]: {message}",
                args.config_path.display()
            ));
            return ExitCode::from(2);
        }
    };
    args.apply_overrides(&mut config);

    if let Err(message) = config.validate() {
        logging::error(format!("invalid config: {message}"));
        return ExitCode::from(2);
    }

    match kind {
        BinaryKind::Gateway => runner::gateway::run(config).await,
        BinaryKind::Native => runner::native::run(config).await,
    }
}
