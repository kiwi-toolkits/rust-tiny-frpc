//! Private key lookup for the SSH tunnel gateway.
//!
//! The obvious failure mode — "cannot start without `/root/.ssh/id_rsa`" — comes
//! from treating a missing key as fatal. It never is: frps only demands client
//! authentication when it configures `authorizedKeysFile`, and even then the
//! correct outcome is a logged authentication failure and a retry, not a
//! process that refuses to boot. Everything in this module therefore returns
//! `Option` and explains itself in the log.

use std::path::{Path, PathBuf};

use crate::logging;

/// Where the default key lives, plus the environment variable it came from so
/// that a warning can name the variable rather than just the path.
fn default_key_path() -> (Option<PathBuf>, String) {
    // Windows sets USERPROFILE, Linux and macOS set HOME. Check both so a
    // scrubbed service environment does not silently lose the key.
    for variable in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(variable) {
            if !home.is_empty() {
                return (
                    Some(PathBuf::from(home).join(".ssh").join("id_rsa")),
                    variable.to_string(),
                );
            }
        }
    }
    (None, "HOME".to_string())
}

/// Resolves the private key to use.
///
/// `configured` wins when set. Otherwise the default location is tried, and a
/// miss is reported as a warning. `None` means "connect without client
/// authentication", which the gateway accepts unless it requires keys.
pub fn resolve_key_path(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        if path.exists() {
            return Some(path.to_path_buf());
        }
        logging::warn(format!(
            "configured private key [{}] does not exist, connecting without client authentication",
            path.display()
        ));
        return None;
    }

    let (path, variable) = default_key_path();
    let Some(path) = path else {
        logging::warn(
            "neither HOME nor USERPROFILE is set, so no default ssh private key could be \
             located; connecting without client authentication. Set ssh-private-key to \
             authenticate with a key",
        );
        return None;
    };
    if path.exists() {
        return Some(path);
    }
    logging::warn(format!(
        "private key [{}] not found (from ${variable}), connecting without client \
         authentication. Set ssh-private-key to use a different key",
        path.display()
    ));
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_key_wins_when_it_exists() {
        let file = std::env::temp_dir().join("rust-tiny-frpc-key-test");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(resolve_key_path(Some(&file)), Some(file.clone()));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn a_missing_configured_key_degrades_instead_of_failing() {
        let missing = std::env::temp_dir().join("rust-tiny-frpc-key-missing");
        let _ = std::fs::remove_file(&missing);
        assert_eq!(resolve_key_path(Some(&missing)), None);
    }
}
