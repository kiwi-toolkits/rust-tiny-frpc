//! Version string.

use crate::cli::BinaryKind;

/// The single source of truth is `Cargo.toml`; there is deliberately no copy of
/// the number in the source tree.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Version reported by `-v`/`--v`/`--version`.
///
/// The `tiny.` prefix and the `-ssh` suffix keep the output shape of the Go
/// implementation, whose release script greps the version out of
/// `tiny-frpc --v`.
pub fn full(kind: BinaryKind) -> String {
    match kind {
        BinaryKind::Gateway => format!("tiny.{VERSION}"),
        BinaryKind::Native => format!("tiny.{VERSION}-ssh"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_the_native_variant() {
        assert!(full(BinaryKind::Gateway).starts_with("tiny."));
        assert!(full(BinaryKind::Native).ends_with("-ssh"));
    }
}
