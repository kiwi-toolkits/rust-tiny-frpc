//! Legacy `frpc.ini` loader.
//!
//! The flat key/value style means there is no way to tell a string from a
//! number up front, so every scalar is kept as text and converted at the point
//! of use. Conversion failures are reported and treated as unset, matching the
//! Go client's `MustInt(0)`.

use std::collections::BTreeMap;
use std::path::Path;

use super::model::{Config, Proxy, ProxyType};
use crate::logging;

/// A file is legacy ini when it parses into sections and one of them is
/// `[common]`. Same heuristic as the Go client, so a `.toml` file that happens
/// to be written in ini syntax still loads.
pub fn looks_like_ini(content: &str) -> bool {
    sections(content)
        .map(|sections| sections.iter().any(|section| section.name == "common"))
        .unwrap_or(false)
}

struct Section {
    name: String,
    entries: Vec<(String, String)>,
}

impl Section {
    fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

/// Splits ini text into sections. Deliberately ignores `#`/`;` only at the
/// start of a line: frp's ini parser runs with `IgnoreInlineComment`, so a `#`
/// inside a value stays part of that value.
fn sections(content: &str) -> Result<Vec<Section>, String> {
    let mut result: Vec<Section> = Vec::new();
    for (index, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            result.push(Section {
                name: name.trim().to_string(),
                entries: Vec::new(),
            });
            continue;
        }
        let Some(section) = result.last_mut() else {
            return Err(format!(
                "line {}: {line:?} appears before any [section]",
                index + 1
            ));
        };
        let (key, value) = line
            .split_once('=')
            .or_else(|| line.split_once(':'))
            .map(|(key, value)| (key.trim(), value.trim()))
            .unwrap_or((line, "true"));
        section.entries.push((key.to_string(), unquote(value)));
    }
    Ok(result)
}

fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        return value[1..value.len() - 1].to_string();
    }
    value.to_string()
}

/// Keys that used to fetch the frps address from a remote service.
const REMOVED_COMMON_KEYS: &[&str] = &["server_addr_from_remote", "server_addr_remote_url"];

// Field-by-field assignment reads better here than one giant literal, since
// each line pairs an ini key with its parser.
#[allow(clippy::field_reassign_with_default)]
pub fn parse(content: &str, path: &Path) -> Result<Config, String> {
    let sections = sections(content).map_err(|error| format!("{}: {error}", path.display()))?;
    let common = sections
        .iter()
        .find(|section| section.name == "common")
        .ok_or_else(|| {
            format!(
                "{}: invalid configuration file, not found [common] section",
                path.display()
            )
        })?;

    for key in REMOVED_COMMON_KEYS {
        if common.get(key).is_some() {
            return Err(format!(
                "{}: {key} was removed; set server_addr and server_port directly instead",
                path.display()
            ));
        }
    }

    let mut config = Config::default();
    config.user = string(common.get("user"));
    config.server_addr = string(common.get("server_addr"));
    config.server_port = port(common.get("server_port"), "server_port", path);
    config.metadatas_enabled = boolish(common.get("metadatas_enabled"));
    config.rename = boolish(common.get("rename"));
    config.user_double_prefix = boolish(common.get("user_double_prefix"));
    config.ssh_private_key = path_option(common.get("ssh_private_key"));
    config.ssh_known_hosts = path_option(common.get("ssh_known_hosts"));
    if let Some(value) = common.get("max_forward_connections") {
        config.max_forward_connections = count(value, "max_forward_connections", path);
    }
    config.auth.token = string(common.get("token"));
    if let Some(method) = common.get("authentication_method") {
        if method != "token" {
            logging::warn(format!(
                "{}: authentication_method {method:?} is ignored, frps only accepts \"token\"",
                path.display()
            ));
        }
    }

    for section in &sections {
        if section.name == "common" {
            continue;
        }
        config.proxies.push(parse_proxy(section, path)?);
    }

    config.complete();
    Ok(config)
}

/// Ini has no visitor syntax; `role = visitor` is an explicit error so that a
/// converted frpc.ini does not silently lose its visitors.
fn parse_proxy(section: &Section, path: &Path) -> Result<Proxy, String> {
    let where_ = format!("{}: [{}]", path.display(), section.name);
    if section.get("role") == Some("visitor") {
        return Err(format!(
            "{where_}: ini cannot express visitors; use toml or drop the section"
        ));
    }

    let type_name = section.get("type").unwrap_or("tcp");
    let proxy_type = ProxyType::parse(type_name).map_err(|error| format!("{where_}: {error}"))?;

    // frps registers a different flag set per proxy type and rejects an
    // unregistered flag, so keys that belong to another type are dropped here
    // rather than forwarded. The toml loader gets the same guard for free from
    // its unknown-key check.
    let mut proxy = Proxy {
        name: section.name.clone(),
        proxy_type,
        local_ip: string(section.get("local_ip")),
        local_port: port(section.get("local_port"), "local_port", path),
        remote_port: 0,
        custom_domains: Vec::new(),
        subdomain: String::new(),
        locations: Vec::new(),
        http_user: String::new(),
        http_password: String::new(),
        host_header_rewrite: String::new(),
        route_by_http_user: String::new(),
        multiplexer: String::new(),
        secret_key: String::new(),
        allow_users: Vec::new(),
        metadatas: prefixed(&section.entries, "meta_"),
    };

    match proxy_type {
        ProxyType::Tcp => {
            proxy.remote_port = port(section.get("remote_port"), "remote_port", path);
        }
        ProxyType::Http => {
            proxy.subdomain = string(section.get("subdomain"));
            proxy.custom_domains = list(section.get("custom_domains"));
            proxy.locations = list(section.get("locations"));
            proxy.http_user = string(section.get("http_user"));
            proxy.http_password = string(section.get("http_pwd"));
            proxy.host_header_rewrite = string(section.get("host_header_rewrite"));
        }
        ProxyType::Https => {
            proxy.subdomain = string(section.get("subdomain"));
            proxy.custom_domains = list(section.get("custom_domains"));
        }
        ProxyType::TcpMux => {
            proxy.subdomain = string(section.get("subdomain"));
            proxy.custom_domains = list(section.get("custom_domains"));
            proxy.multiplexer = string(section.get("multiplexer"));
            proxy.http_user = string(section.get("http_user"));
            proxy.http_password = string(section.get("http_pwd"));
        }
        ProxyType::Stcp => {
            proxy.secret_key = string(section.get("sk"));
            proxy.allow_users = list(section.get("allow_users"));
        }
    }

    if section.get("route_by_http_user").is_some() {
        logging::warn(format!(
            "{where_}: route_by_http_user is accepted but not forwarded, \
             the ssh gateway registers no such flag"
        ));
    }
    Ok(proxy)
}

fn prefixed(entries: &[(String, String)], prefix: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (key, value) in entries {
        if let Some(name) = key.strip_prefix(prefix) {
            map.insert(name.to_string(), value.clone());
        }
    }
    map
}

fn string(value: Option<&str>) -> String {
    value.unwrap_or_default().to_string()
}

fn list(value: Option<&str>) -> Vec<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn path_option(value: Option<&str>) -> Option<std::path::PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}

fn boolish(value: Option<&str>) -> bool {
    matches!(
        value.map(str::trim),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

fn port(value: Option<&str>, key: &str, path: &Path) -> u16 {
    let Some(value) = value else {
        return 0;
    };
    match value.trim().parse::<u16>() {
        Ok(port) => port,
        Err(error) => {
            logging::warn(format!(
                "{}: {key} must be an integer between 0 and 65535, got {value:?} ({error}); \
                 ignoring it",
                path.display()
            ));
            0
        }
    }
}

fn count(value: &str, key: &str, path: &Path) -> usize {
    match value.trim().parse::<usize>() {
        Ok(number) if number > 0 => number,
        _ => {
            logging::warn(format!(
                "{}: {key} must be a positive integer, got {value:?}; using the default",
                path.display()
            ));
            Config::default().max_forward_connections
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ini_by_the_common_section() {
        assert!(looks_like_ini("[common]\nserver_addr = 1.2.3.4\n"));
        assert!(!looks_like_ini("serverAddr = \"1.2.3.4\"\n"));
        assert!(!looks_like_ini("[ssh]\ntype = tcp\n"));
    }

    #[test]
    fn loads_a_common_section_and_proxies() {
        let config = parse(
            "[common]\n\
             server_addr = 1.2.3.4\n\
             server_port = 2222\n\
             token = secret\n\
             user = hqu\n\
             \n\
             [ssh]\n\
             type = tcp\n\
             local_port = 22\n\
             remote_port = 6000\n",
            Path::new("test.ini"),
        )
        .unwrap();
        assert_eq!(config.server_addr, "1.2.3.4");
        assert_eq!(config.server_port, 2222);
        assert_eq!(config.auth.token, "secret");
        assert_eq!(config.proxies.len(), 1);
        assert_eq!(
            config.proxies[0].name, "ssh",
            "the client no longer prefixes"
        );
        assert_eq!(config.proxies[0].local_ip, "127.0.0.1");
    }

    #[test]
    fn keeps_inline_comments_as_part_of_the_value() {
        let config = parse(
            "[common]\nserver_addr = host # retained\n\n[ssh]\ntype = tcp\n",
            Path::new("test.ini"),
        )
        .unwrap();
        assert_eq!(config.server_addr, "host # retained");
    }

    #[test]
    fn accepts_colon_separators_and_bare_keys() {
        let config = parse(
            "[common]\nserver_addr: 1.2.3.4\n\n[ssh]\ntype: tcp\n",
            Path::new("test.ini"),
        )
        .unwrap();
        assert_eq!(config.server_addr, "1.2.3.4");
        assert_eq!(config.proxies[0].proxy_type, ProxyType::Tcp);
    }

    #[test]
    fn strips_the_meta_prefix() {
        let config = parse(
            "[common]\n\n[ssh]\ntype = tcp\nmeta_var1 = abc\nmeta_var2 = 123\n",
            Path::new("test.ini"),
        )
        .unwrap();
        assert_eq!(config.proxies[0].metadatas["var1"], "abc");
        assert_eq!(config.proxies[0].metadatas["var2"], "123");
    }

    #[test]
    fn does_not_leak_flags_across_proxy_types() {
        // An https section carrying http-only keys must not produce --locations.
        let config = parse(
            "[common]\n\n[web]\ntype = https\nlocations = /,/pic\nmultiplexer = httpconnect\n\
             http_user = admin\n",
            Path::new("test.ini"),
        )
        .unwrap();
        let proxy = &config.proxies[0];
        assert!(proxy.locations.is_empty());
        assert!(proxy.multiplexer.is_empty());
        assert!(proxy.http_user.is_empty());
    }

    #[test]
    fn rejects_removed_keys_and_visitor_roles() {
        let error = parse(
            "[common]\nserver_addr_from_remote = 1\n",
            Path::new("test.ini"),
        )
        .unwrap_err();
        assert!(error.contains("was removed"), "{error}");

        let error = parse(
            "[common]\n\n[vis]\nrole = visitor\ntype = stcp\n",
            Path::new("test.ini"),
        )
        .unwrap_err();
        assert!(error.contains("cannot express visitors"), "{error}");
    }

    #[test]
    fn requires_a_common_section() {
        let error = parse("[ssh]\ntype = tcp\n", Path::new("test.ini")).unwrap_err();
        assert!(error.contains("not found [common] section"), "{error}");
    }

    #[test]
    fn a_bad_port_is_ignored_rather_than_fatal() {
        let config = parse(
            "[common]\nserver_port = notaport\n\n[ssh]\ntype = tcp\nlocal_port = 22\n",
            Path::new("test.ini"),
        )
        .unwrap();
        assert_eq!(config.server_port, 2200);
        assert_eq!(config.proxies[0].local_port, 22);
    }

    #[test]
    fn unquotes_matching_quotes_only() {
        assert_eq!(unquote("\"abc\""), "abc");
        assert_eq!(unquote("'abc'"), "abc");
        assert_eq!(unquote("\"abc"), "\"abc");
    }
}
