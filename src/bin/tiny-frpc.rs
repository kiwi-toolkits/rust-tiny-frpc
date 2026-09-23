//! `tiny-frpc`: the embedded SSH client.

use std::process::ExitCode;

use tiny_frpc::cli::BinaryKind;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    tiny_frpc::run(BinaryKind::Gateway).await
}
