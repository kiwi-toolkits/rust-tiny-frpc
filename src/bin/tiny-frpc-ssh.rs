//! `tiny-frpc-ssh`: the variant that drives the system `ssh` binary.

use std::process::ExitCode;

use tiny_frpc::cli::BinaryKind;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    tiny_frpc::run(BinaryKind::Native).await
}
