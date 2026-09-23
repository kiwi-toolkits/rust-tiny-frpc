//! TOML loader.
//!
//! Decoding is done by hand against the parsed [`toml::Value`] so that unknown
//! keys can be rejected with a message naming the key and the file, which is
//! what the Go client's `DisallowUnknownFields` did. Pulling in `serde` for
//! this would cost more binary size than the whole SSH stack.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use toml::Value;

use super::model::{AuthConfig, Config, Proxy, ProxyType, Visitor};
use crate::logging;

const ROOT_KEYS: &[&str] = &[
    "auth",
    "user",
    "serverAddr",
    "serverPort",
    "metadatasEnabled",
    "metadatas",
    "rename",
    "userDoublePrefix",
    "sshPrivateKey",
    "sshKnownHosts",
    "maxForwardConnections",
    "includes",
    "proxies",
    "visitors",
];

/// Keys that used to fetch the frps address from a remote service. They are
/// gone; failing loudly beats silently connecting to the wrong server.
const REMOVED_ROOT_KEYS: &[&str] = &["serverAddrFromRemote", "serverAddrRemoteURL"];

// Field-by-field assignment reads better here than one giant literal, since
// each line pairs a config key with its parser.
#[allow(clippy::field_reassign_with_default)]
pub fn parse(content: &str, path: &Path) -> Result<Config, String> {
    let value: Value = toml::from_str(content)
        .map_err(|error| format!("{}: invalid toml: {error}", path.display()))?;
    let table = value
        .as_table()
        .ok_or_else(|| format!("{}: configuration must be a table", path.display()))?;

    for key in REMOVED_ROOT_KEYS {
        if table.contains_key(*key) {
            return Err(format!(
                "{}: {key} was removed; set serverAddr and serverPort directly instead",
                path.display()
            ));
        }
    }
    reject_unknown_keys(table.keys(), ROOT_KEYS, path)?;

    let mut config = Config::default();
    config.auth = parse_auth(table.get("auth"), path)?;
    config.user = parse_string(table.get("user"), "user", path)?;
    config.server_addr = parse_string(table.get("serverAddr"), "serverAddr", path)?;
    config.server_port = parse_port(table.get("serverPort"), "serverPort", path)?;
    config.metadatas_enabled = parse_bool(table.get("metadatasEnabled"), "metadatasEnabled", path)?;
    config.rename = parse_bool(table.get("rename"), "rename", path)?;
    config.user_double_prefix =
        parse_bool(table.get("userDoublePrefix"), "userDoublePrefix", path)?;
    config.ssh_private_key = parse_path(table.get("sshPrivateKey"), "sshPrivateKey", path)?;
    config.ssh_known_hosts = parse_path(table.get("sshKnownHosts"), "sshKnownHosts", path)?;
    if let Some(value) = table.get("maxForwardConnections") {
        config.max_forward_connections = parse_usize(Some(value), "maxForwardConnections", path)?;
    }
    config.includes = parse_string_vec(table.get("includes"), "includes", path)?;

    if let Some(value) = table.get("metadatas") {
        // Kept parseable for compatibility with frpc.toml files that set
        // client-level metadatas, but frps' ssh gateway registers no
        // client-level metas flag, so this can never reach the server.
        parse_map(Some(value), "metadatas", path)?;
        logging::warn(format!(
            "{}: client-level metadatas are accepted but never sent; use the per-proxy \
             `metadatas` table plus `metadatasEnabled = true`",
            path.display()
        ));
    }

    for (index, value) in parse_array(table.get("proxies"), "proxies", path)? {
        let key = format!("proxies[{index}]");
        config.proxies.push(parse_proxy(value, &key, path)?);
    }
    for (index, value) in parse_array(table.get("visitors"), "visitors", path)? {
        let key = format!("visitors[{index}]");
        config.visitors.push(parse_visitor(value, &key, path)?);
    }

    load_includes(&mut config, path)?;
    config.complete();
    Ok(config)
}

fn load_includes(config: &mut Config, path: &Path) -> Result<(), String> {
    let includes = std::mem::take(&mut config.includes);
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    for pattern in includes {
        let matches = expand_pattern(base, &pattern)?;
        if matches.is_empty() {
            return Err(format!(
                "{}: include {pattern:?} matched no files",
                path.display()
            ));
        }
        for included in matches {
            let content = std::fs::read_to_string(&included)
                .map_err(|error| format!("read {}: {error}", included.display()))?;
            let rendered = super::template::render(&content)?;
            if super::ini_load::looks_like_ini(&rendered) {
                return Err(format!(
                    "included config {} is legacy ini; mixed formats are unsupported",
                    included.display()
                ));
            }
            let mut extra = parse(&rendered, &included)?;
            config.proxies.append(&mut extra.proxies);
            config.visitors.append(&mut extra.visitors);
        }
    }
    Ok(())
}

/// Expands a glob relative to `base`, sorted for a deterministic result.
fn expand_pattern(base: &Path, pattern: &str) -> Result<Vec<PathBuf>, String> {
    let joined = base.join(pattern);
    let parent = joined
        .parent()
        .ok_or_else(|| format!("invalid include pattern {pattern:?}"))?;
    let name = joined
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("invalid include pattern {pattern:?}"))?;

    let entries =
        std::fs::read_dir(parent).map_err(|error| format!("read {}: {error}", parent.display()))?;
    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| format!("read {}: {error}", parent.display()))?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if wildcard_match(name, file_name) && entry.path().is_file() {
            matches.push(entry.path());
        }
    }
    matches.sort();
    Ok(matches)
}

/// `filepath.Match`-style matching: `*` and `?` never cross a path separator.
fn wildcard_match(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    let mut memo = std::collections::HashMap::new();
    matches_from(&pattern, &name, 0, 0, &mut memo)
}

fn matches_from(
    pattern: &[char],
    name: &[char],
    mut p: usize,
    mut n: usize,
    memo: &mut std::collections::HashMap<(usize, usize), bool>,
) -> bool {
    loop {
        if let Some(cached) = memo.get(&(p, n)) {
            return *cached;
        }
        let result = match pattern.get(p) {
            None => n == name.len(),
            Some('*') => {
                let mut next = p + 1;
                while pattern.get(next) == Some(&'*') {
                    next += 1;
                }
                let mut candidate = n;
                loop {
                    if matches_from(pattern, name, next, candidate, memo) {
                        break true;
                    }
                    match name.get(candidate) {
                        Some('/') | Some('\\') | None => break false,
                        Some(_) => candidate += 1,
                    }
                }
            }
            Some('?') => match name.get(n) {
                Some('/') | Some('\\') | None => false,
                Some(_) => {
                    p += 1;
                    n += 1;
                    continue;
                }
            },
            Some('[') => {
                let Some((ok, next)) = match_class(pattern, p, name.get(n).copied()) else {
                    // Not a valid class; treat '[' literally, like filepath.Match.
                    let literal = name.get(n) == Some(&'[');
                    memo.insert((p, n), literal);
                    return literal;
                };
                if !ok {
                    memo.insert((p, n), false);
                    return false;
                }
                p = next;
                n += 1;
                continue;
            }
            Some(&expected) => {
                if name.get(n) == Some(&expected) {
                    p += 1;
                    n += 1;
                    continue;
                }
                false
            }
        };
        memo.insert((p, n), result);
        return result;
    }
}

/// Parses a `[...]` character class starting at `pattern[start]`. Returns the
/// match result and the index just past the class.
fn match_class(pattern: &[char], start: usize, candidate: Option<char>) -> Option<(bool, usize)> {
    let mut index = start + 1;
    let negated = matches!(pattern.get(index), Some('^') | Some('!'));
    if negated {
        index += 1;
    }
    let mut matched = false;
    let mut first = true;
    loop {
        let ch = *pattern.get(index)?;
        if ch == ']' && !first {
            if candidate.is_none() {
                return Some((false, index + 1));
            }
            return Some((matched != negated, index + 1));
        }
        first = false;
        if pattern.get(index + 1) == Some(&'-') && pattern.get(index + 2).is_some_and(|c| *c != ']')
        {
            let low = ch;
            let high = *pattern.get(index + 2)?;
            if let Some(candidate) = candidate {
                if low <= candidate && candidate <= high {
                    matched = true;
                }
            }
            index += 3;
        } else {
            if candidate == Some(ch) {
                matched = true;
            }
            index += 1;
        }
    }
}

fn parse_auth(value: Option<&Value>, path: &Path) -> Result<AuthConfig, String> {
    let mut auth = AuthConfig::default();
    let Some(value) = value else {
        return Ok(auth);
    };
    let table = value
        .as_table()
        .ok_or_else(|| format!("{}: auth must be a table", path.display()))?;
    reject_unknown_keys(table.keys(), &["method", "token"], path)?;
    if let Some(method) = table.get("method") {
        let method = as_str(method, "auth.method", path)?;
        if method != "token" {
            logging::warn(format!(
                "{}: auth.method {method:?} is ignored, frps only accepts \"token\"",
                path.display()
            ));
        }
    }
    auth.token = parse_string(table.get("token"), "auth.token", path)?;
    Ok(auth)
}

fn parse_proxy(value: &Value, key: &str, path: &Path) -> Result<Proxy, String> {
    let table = value
        .as_table()
        .ok_or_else(|| format!("{}: {key} must be a table", path.display()))?;
    let type_name = table
        .get("type")
        .ok_or_else(|| format!("{}: {key}.type is required", path.display()))?;
    let proxy_type = ProxyType::parse(as_str(type_name, &format!("{key}.type"), path)?)
        .map_err(|error| format!("{}: {key}.{error}", path.display()))?;

    // frps registers a different flag set per proxy type, and an unregistered
    // flag fails the whole registration, so type-inappropriate keys are
    // rejected here rather than forwarded.
    let mut allowed = vec!["name", "type", "localIP", "localPort", "metadatas"];
    match proxy_type {
        ProxyType::Tcp => allowed.push("remotePort"),
        ProxyType::Http => allowed.extend([
            "customDomains",
            "subdomain",
            "locations",
            "httpUser",
            "httpPassword",
            "hostHeaderRewrite",
            "routeByHTTPUser",
            "requestHeaders",
        ]),
        ProxyType::Https => allowed.extend(["customDomains", "subdomain"]),
        ProxyType::TcpMux => allowed.extend([
            "customDomains",
            "subdomain",
            "multiplexer",
            "httpUser",
            "httpPassword",
            "routeByHTTPUser",
        ]),
        ProxyType::Stcp => allowed.extend(["secretKey", "allowUsers"]),
    }
    reject_unknown_keys(table.keys(), &allowed, path)?;

    let mut proxy = Proxy {
        name: parse_string(table.get("name"), &format!("{key}.name"), path)?,
        proxy_type,
        local_ip: parse_string(table.get("localIP"), &format!("{key}.localIP"), path)?,
        local_port: parse_port(table.get("localPort"), &format!("{key}.localPort"), path)?,
        remote_port: parse_port(table.get("remotePort"), &format!("{key}.remotePort"), path)?,
        custom_domains: parse_string_vec(
            table.get("customDomains"),
            &format!("{key}.customDomains"),
            path,
        )?,
        subdomain: parse_string(table.get("subdomain"), &format!("{key}.subdomain"), path)?,
        locations: parse_string_vec(table.get("locations"), &format!("{key}.locations"), path)?,
        http_user: parse_string(table.get("httpUser"), &format!("{key}.httpUser"), path)?,
        http_password: parse_string(
            table.get("httpPassword"),
            &format!("{key}.httpPassword"),
            path,
        )?,
        host_header_rewrite: parse_string(
            table.get("hostHeaderRewrite"),
            &format!("{key}.hostHeaderRewrite"),
            path,
        )?,
        route_by_http_user: parse_string(
            table.get("routeByHTTPUser"),
            &format!("{key}.routeByHTTPUser"),
            path,
        )?,
        multiplexer: parse_string(
            table.get("multiplexer"),
            &format!("{key}.multiplexer"),
            path,
        )?,
        secret_key: parse_string(table.get("secretKey"), &format!("{key}.secretKey"), path)?,
        allow_users: parse_string_vec(table.get("allowUsers"), &format!("{key}.allowUsers"), path)?,
        metadatas: parse_map(table.get("metadatas"), &format!("{key}.metadatas"), path)?,
    };

    if table.contains_key("requestHeaders") {
        logging::warn(format!(
            "{}: {key}.requestHeaders is accepted but not forwarded, \
             frp has no --headers flag",
            path.display()
        ));
    }
    if !proxy.route_by_http_user.is_empty() {
        logging::warn(format!(
            "{}: {key}.routeByHTTPUser is accepted but not forwarded, \
             the ssh gateway registers no such flag",
            path.display()
        ));
        proxy.route_by_http_user.clear();
    }
    Ok(proxy)
}

fn parse_visitor(value: &Value, key: &str, path: &Path) -> Result<Visitor, String> {
    let table = value
        .as_table()
        .ok_or_else(|| format!("{}: {key} must be a table", path.display()))?;
    reject_unknown_keys(
        table.keys(),
        &[
            "name",
            "type",
            "secretKey",
            "serverUser",
            "serverName",
            "bindAddr",
            "bindPort",
        ],
        path,
    )?;
    Ok(Visitor {
        name: parse_string(table.get("name"), &format!("{key}.name"), path)?,
        visitor_type: parse_string(table.get("type"), &format!("{key}.type"), path)?,
        secret_key: parse_string(table.get("secretKey"), &format!("{key}.secretKey"), path)?,
        server_user: parse_string(table.get("serverUser"), &format!("{key}.serverUser"), path)?,
        server_name: parse_string(table.get("serverName"), &format!("{key}.serverName"), path)?,
        bind_addr: parse_string(table.get("bindAddr"), &format!("{key}.bindAddr"), path)?,
        bind_port: parse_port(table.get("bindPort"), &format!("{key}.bindPort"), path)?,
    })
}

fn reject_unknown_keys<'a, I>(keys: I, allowed: &[&str], path: &Path) -> Result<(), String>
where
    I: Iterator<Item = &'a String>,
{
    for key in keys {
        if !allowed.contains(&key.as_str()) {
            return Err(format!(
                "{}: unknown field {key}, expected one of: {}",
                path.display(),
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

fn as_str<'a>(value: &'a Value, key: &str, path: &Path) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("{}: {key} must be a string", path.display()))
}

fn parse_string(value: Option<&Value>, key: &str, path: &Path) -> Result<String, String> {
    match value {
        None => Ok(String::new()),
        Some(value) => Ok(as_str(value, key, path)?.to_string()),
    }
}

fn parse_path(value: Option<&Value>, key: &str, path: &Path) -> Result<Option<PathBuf>, String> {
    match value {
        None => Ok(None),
        Some(value) => {
            let raw = as_str(value, key, path)?;
            if raw.is_empty() {
                Ok(None)
            } else {
                Ok(Some(PathBuf::from(raw)))
            }
        }
    }
}

/// Ports are parsed leniently: a value that will not fit is reported and
/// treated as unset, so one typo cannot take the whole client down. This
/// matches the Go client's `MustInt(0)` behaviour, only louder.
fn parse_port(value: Option<&Value>, key: &str, path: &Path) -> Result<u16, String> {
    let Some(value) = value else {
        return Ok(0);
    };
    match value.as_integer() {
        Some(number) if (0..=u16::MAX as i64).contains(&number) => Ok(number as u16),
        _ => {
            logging::warn(format!(
                "{}: {key} must be an integer between 0 and 65535, got {value}; ignoring it",
                path.display()
            ));
            Ok(0)
        }
    }
}

fn parse_usize(value: Option<&Value>, key: &str, path: &Path) -> Result<usize, String> {
    let Some(value) = value else {
        return Ok(0);
    };
    match value.as_integer() {
        Some(number) if number > 0 => Ok(number as usize),
        _ => Err(format!(
            "{}: {key} must be a positive integer, got {value}",
            path.display()
        )),
    }
}

fn parse_bool(value: Option<&Value>, key: &str, path: &Path) -> Result<bool, String> {
    match value {
        None => Ok(false),
        Some(Value::Boolean(flag)) => Ok(*flag),
        Some(Value::Integer(number)) => Ok(*number != 0),
        Some(other) => Err(format!(
            "{}: {key} must be a boolean, got {other}",
            path.display()
        )),
    }
}

fn parse_string_vec(value: Option<&Value>, key: &str, path: &Path) -> Result<Vec<String>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{}: {key} must be an array of strings", path.display()))?;
    array
        .iter()
        .map(|item| Ok(as_str(item, key, path)?.to_string()))
        .collect()
}

fn parse_map(
    value: Option<&Value>,
    key: &str,
    path: &Path,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let table = value
        .as_table()
        .ok_or_else(|| format!("{}: {key} must be a table of strings", path.display()))?;
    let mut map = BTreeMap::new();
    for (name, item) in table {
        map.insert(name.clone(), as_str(item, key, path)?.to_string());
    }
    Ok(map)
}

fn parse_array<'a>(
    value: Option<&'a Value>,
    key: &str,
    path: &Path,
) -> Result<Vec<(usize, &'a Value)>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{}: {key} must be an array", path.display()))?;
    Ok(array.iter().enumerate().collect())
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn load(text: &str) -> Result<Config, String> {
        parse(text, Path::new("test.toml"))
    }

    #[test]
    fn loads_a_minimal_file() {
        let config = load(
            r#"
serverAddr = "1.2.3.4"
serverPort = 2222
auth.token = "secret"

[[proxies]]
name = "demo"
type = "tcp"
localPort = 22
remotePort = 6000
"#,
        )
        .unwrap();
        assert_eq!(config.server_addr, "1.2.3.4");
        assert_eq!(config.server_port, 2222);
        assert_eq!(config.auth.token, "secret");
        assert_eq!(config.proxies.len(), 1);
        assert_eq!(config.proxies[0].local_ip, "127.0.0.1");
        assert!(config.proxies[0].metadatas.is_empty());
    }

    #[test]
    fn parses_dotted_metadata_keys() {
        let config = load(
            r#"
[[proxies]]
name = "demo"
type = "tcp"
remotePort = 1
metadatas.var1 = "abc"
metadatas.var2 = "123"
"#,
        )
        .unwrap();
        assert_eq!(config.proxies[0].metadatas["var1"], "abc");
        assert_eq!(config.proxies[0].metadatas["var2"], "123");
    }

    #[test]
    fn rejects_the_removed_remote_address_keys() {
        let error =
            load("serverAddrFromRemote = 1\n[[proxies]]\nname=\"a\"\ntype=\"tcp\"\n").unwrap_err();
        assert!(error.contains("was removed"), "{error}");
        let error =
            load("serverAddrRemoteURL = \"http://x\"\n[[proxies]]\nname=\"a\"\ntype=\"tcp\"\n")
                .unwrap_err();
        assert!(error.contains("was removed"), "{error}");
    }

    #[test]
    fn rejects_unknown_root_and_proxy_keys() {
        assert!(load("nope = 1\n").unwrap_err().contains("unknown field"));
        let error =
            load("[[proxies]]\nname = \"a\"\ntype = \"https\"\nlocations = [\"/\"]\n").unwrap_err();
        assert!(error.contains("unknown field locations"), "{error}");
    }

    #[test]
    fn rejects_proxy_types_the_gateway_cannot_express() {
        let error = load("[[proxies]]\nname = \"a\"\ntype = \"udp\"\n").unwrap_err();
        assert!(error.contains("not supported"), "{error}");
    }

    #[test]
    fn ports_are_lenient_and_booleans_are_not() {
        let config = load(
            "serverPort = \"nope\"\n[[proxies]]\nname=\"a\"\ntype=\"tcp\"\nremotePort = 70000\n",
        )
        .unwrap();
        assert_eq!(config.server_port, 2200, "unset, then defaulted");
        assert_eq!(config.proxies[0].remote_port, 0);
        assert!(load("rename = \"yes\"\n")
            .unwrap_err()
            .contains("must be a boolean"));
    }

    #[test]
    fn accepts_booleans_written_as_integers() {
        let config = load("metadatasEnabled = 1\nuserDoublePrefix = 0\n").unwrap();
        assert!(config.metadatas_enabled);
        assert!(!config.user_double_prefix);
    }

    #[test]
    fn wildcard_matching_follows_filepath_match() {
        assert!(wildcard_match("*.toml", "a.toml"));
        assert!(!wildcard_match("*.toml", "a.ini"));
        assert!(wildcard_match("conf?.toml", "conf1.toml"));
        assert!(wildcard_match("[ab]*.toml", "a.toml"));
        assert!(!wildcard_match("[!ab]*.toml", "a.toml"));
        assert!(wildcard_match("a*", "abc"));
        assert!(
            !wildcard_match("*", "a/b"),
            "wildcards do not cross separators"
        );
    }
}
