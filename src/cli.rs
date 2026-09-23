//! The one and only argument parser, shared by both binaries.
//!
//! The accepted forms mirror the Go original, which used the standard `flag`
//! package: `-c v`, `-c=v`, `--c v`, `--c=v` and both `-v` and `--v`. The
//! upstream release script calls `tiny-frpc --v`, so accepting the two-dash
//! spelling matters.

use std::path::PathBuf;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryKind {
    /// `tiny-frpc`: embedded SSH client.
    Gateway,
    /// `tiny-frpc-ssh`: drives the system `ssh` binary.
    Native,
}

/// Command-line overrides applied on top of the configuration file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    pub config_path: PathBuf,
    /// `Some(true)`/`Some(false)` when given on the command line, `None` to
    /// keep whatever the file said.
    pub rename: Option<bool>,
    pub metadatas: Option<bool>,
    /// Overrides `ssh_private_key` from the file.
    pub ssh_private_key: Option<PathBuf>,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            config_path: PathBuf::from("frpc.toml"),
            rename: None,
            metadatas: None,
            ssh_private_key: None,
        }
    }
}

impl Args {
    pub fn apply_overrides(&self, config: &mut Config) {
        if let Some(rename) = self.rename {
            config.rename = rename;
        }
        if let Some(metadatas) = self.metadatas {
            config.metadatas_enabled = metadatas;
        }
        if let Some(path) = &self.ssh_private_key {
            config.ssh_private_key = Some(path.clone());
        }
    }
}

pub enum Parsed {
    Run(Args),
    Version,
    Help,
}

pub fn parse<I>(mut argv: I, kind: BinaryKind) -> Result<Parsed, String>
where
    I: Iterator<Item = String>,
{
    let mut args = Args::default();

    while let Some(token) = argv.next() {
        let (name, inline) = split_inline(&token);
        match name {
            "-v" | "--v" | "--version" | "-V" => return Ok(Parsed::Version),
            "-h" | "--h" | "--help" => return Ok(Parsed::Help),
            "-c" | "--c" | "--config" => {
                let value = match inline {
                    Some(value) => value.to_string(),
                    None => argv
                        .next()
                        .ok_or_else(|| format!("missing value for {name}"))?,
                };
                if value.is_empty() {
                    return Err(format!("missing value for {name}"));
                }
                args.config_path = PathBuf::from(value);
            }
            "--rename" => args.rename = Some(flag_value(name, inline)?),
            "--no-rename" => args.rename = Some(false),
            "--metadatas" | "--meta" => args.metadatas = Some(flag_value(name, inline)?),
            "--no-metadatas" | "--no-meta" => args.metadatas = Some(false),
            "--ssh-private-key" => {
                let value = match inline {
                    Some(value) => value.to_string(),
                    None => argv
                        .next()
                        .ok_or_else(|| format!("missing value for {name}"))?,
                };
                args.ssh_private_key = Some(PathBuf::from(value));
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let _ = kind;
    Ok(Parsed::Run(args))
}

/// Splits `--key=value` into `("--key", Some("value"))`.
fn split_inline(token: &str) -> (&str, Option<&str>) {
    match token.split_once('=') {
        Some((name, value)) if name.starts_with('-') => (name, Some(value)),
        _ => (token, None),
    }
}

/// A boolean flag accepts `--flag`, `--flag=true` and `--flag=false`.
fn flag_value(name: &str, inline: Option<&str>) -> Result<bool, String> {
    match inline {
        None => Ok(true),
        Some("true") | Some("1") | Some("yes") => Ok(true),
        Some("false") | Some("0") | Some("no") => Ok(false),
        Some(other) => Err(format!("invalid boolean value {other:?} for {name}")),
    }
}

pub fn usage(kind: BinaryKind) -> String {
    let name = match kind {
        BinaryKind::Gateway => "tiny-frpc",
        BinaryKind::Native => "tiny-frpc-ssh",
    };
    let mut out = String::new();
    out.push_str(&format!("Usage: {name} [options]\n\n"));
    out.push_str("Options:\n");
    out.push_str(
        "  -c, --config <path>       path to the configuration file (default: frpc.toml)\n",
    );
    out.push_str(
        "      --rename[=bool]       retry a conflicting proxy under an ex_N_ name instead\n",
    );
    out.push_str(
        "                            of failing (default: false; the config file wins unless\n",
    );
    out.push_str("                            this flag is given)\n");
    out.push_str(
        "      --metadatas[=bool]    send proxy metadatas to frps (default: false; needs\n",
    );
    out.push_str(
        "                            frps >= 0.61.2, older gateways reject --metadatas)\n",
    );
    out.push_str("      --ssh-private-key <path>\n");
    out.push_str(
        "                            private key used to authenticate to the SSH tunnel\n",
    );
    out.push_str("                            gateway (default: $HOME/.ssh/id_rsa)\n");
    out.push_str("  -v, --v, --version        print the version and exit\n");
    out.push_str("  -h, --help                print this help and exit\n");
    out.push('\n');
    out.push_str("When no private key is available the client connects without client\n");
    out.push_str("authentication, which the gateway accepts unless it sets authorizedKeysFile.\n");
    out
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn parse_ok(tokens: &[&str]) -> Args {
        match parse(tokens.iter().map(|t| t.to_string()), BinaryKind::Gateway) {
            Ok(Parsed::Run(args)) => args,
            Ok(_) => panic!("expected a run"),
            Err(error) => panic!("unexpected error: {error}"),
        }
    }

    #[test]
    fn defaults_to_frpc_toml_in_the_working_directory() {
        assert_eq!(parse_ok(&[]).config_path, PathBuf::from("frpc.toml"));
    }

    #[test]
    fn accepts_every_config_spelling_the_go_flag_package_did() {
        assert_eq!(
            parse_ok(&["-c", "a.toml"]).config_path,
            PathBuf::from("a.toml")
        );
        assert_eq!(
            parse_ok(&["-c=a.toml"]).config_path,
            PathBuf::from("a.toml")
        );
        assert_eq!(
            parse_ok(&["--c", "a.toml"]).config_path,
            PathBuf::from("a.toml")
        );
        assert_eq!(
            parse_ok(&["--c=a.toml"]).config_path,
            PathBuf::from("a.toml")
        );
    }

    #[test]
    fn accepts_double_dash_v_used_by_the_release_script() {
        for token in ["-v", "--v", "--version"] {
            assert!(matches!(
                parse([token.to_string()].into_iter(), BinaryKind::Gateway),
                Ok(Parsed::Version)
            ));
        }
    }

    #[test]
    fn parses_boolean_switches_with_and_without_values() {
        assert_eq!(parse_ok(&["--rename"]).rename, Some(true));
        assert_eq!(parse_ok(&["--rename=false"]).rename, Some(false));
        assert_eq!(parse_ok(&["--no-rename"]).rename, Some(false));
        assert_eq!(parse_ok(&["--metadatas"]).metadatas, Some(true));
        assert_eq!(parse_ok(&["--no-meta"]).metadatas, Some(false));
        assert_eq!(parse_ok(&[]).rename, None);
        assert_eq!(parse_ok(&[]).metadatas, None);
    }

    #[test]
    fn rejects_unknown_arguments_and_missing_values() {
        assert!(parse(["--nope".to_string()].into_iter(), BinaryKind::Gateway).is_err());
        assert!(parse(["-c".to_string()].into_iter(), BinaryKind::Gateway).is_err());
        assert!(parse(
            ["--rename=maybe".to_string()].into_iter(),
            BinaryKind::Gateway
        )
        .is_err());
    }

    #[test]
    fn overrides_only_what_was_given() {
        let mut config = Config::default();
        config.rename = true;
        config.metadatas_enabled = true;
        parse_ok(&["--no-rename"]).apply_overrides(&mut config);
        assert!(!config.rename);
        assert!(
            config.metadatas_enabled,
            "metadatas was not on the command line"
        );
    }
}
