//! Turns a [`Config`] into the two command shapes frps understands.
//!
//! The output is byte-for-byte compatible with the Go client, because frps
//! splits the payload on single spaces (`strings.Split(payload, " ")`). Flag
//! order is fixed and matches the Go `flag` struct order, and any value that is
//! empty or zero is omitted entirely.

use crate::config::{Config, Proxy, ProxyType};

/// The `exec` payload for the embedded SSH client: `"{type} {flags}"`.
pub fn gen_extra(proxy: &Proxy, config: &Config) -> String {
    let mut flags: Vec<String> = Vec::new();
    push(&mut flags, "--proxy-name", &proxy.name);
    if proxy.proxy_type == ProxyType::Tcp {
        push_num(&mut flags, "--remote-port", proxy.remote_port);
    }
    push_list(&mut flags, "--custom-domain", &proxy.custom_domains);
    push_list(&mut flags, "--locations", &proxy.locations);
    push(&mut flags, "--sd", &proxy.subdomain);
    push(&mut flags, "--http-user", &proxy.http_user);
    push(&mut flags, "--http-pwd", &proxy.http_password);
    push(
        &mut flags,
        "--host-header-rewrite",
        &proxy.host_header_rewrite,
    );
    push(&mut flags, "--sk", &proxy.secret_key);
    push_list(&mut flags, "--allow-users", &proxy.allow_users);
    push(&mut flags, "--mux", &proxy.multiplexer);
    if config.metadatas_enabled && !proxy.metadatas.is_empty() {
        // BTreeMap iteration is sorted, which is what the Go client's
        // EncodeMetadatas does too.
        let joined = proxy
            .metadatas
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(",");
        push(&mut flags, "--metadatas", &joined);
    }
    push(&mut flags, "--user", &config.user);
    if !config.auth.method.is_empty() {
        push(&mut flags, "--token", &config.auth.token);
    }

    if flags.is_empty() {
        proxy.proxy_type.as_str().to_string()
    } else {
        format!("{} {}", proxy.proxy_type.as_str(), flags.join(" "))
    }
}

/// The full shell command for the native variant, which drives system `ssh`.
///
/// The destination is `v0@host` with the port passed separately as `-p`, which
/// is how the Go client builds it: `ssh` parses the `@` part as a host name, so
/// `v0@host:port` would be read as an unresolvable hostname.
///
/// Host key policy matches the embedded client: when no `ssh_known_hosts` is
/// configured, verification is disabled (frps regenerates its
/// `.autogen_ssh_key` on startup, so a recorded key goes stale immediately);
/// when one is configured, `ssh` is pointed at it and keeps its strict
/// default.
pub fn parse_to_native_ssh(config: &Config) -> Vec<String> {
    let host_key_options = match &config.ssh_known_hosts {
        Some(path) => format!("-o UserKnownHostsFile={}", path.display()),
        None => format!(
            "-o StrictHostKeyChecking=no -o UserKnownHostsFile={}",
            null_device()
        ),
    };
    config
        .proxies
        .iter()
        .map(|proxy| {
            format!(
                "ssh v0@{} -p {} -R :80:{} {host_key_options} {}",
                crate::util::bracket_host(&config.server_addr),
                config.server_port,
                proxy.local_addr(),
                gen_extra(proxy, config),
            )
        })
        .collect()
}

/// A path `ssh` can write to and that is discarded.
///
/// `NUL` is the Windows null device, but `ssh` is not a native Windows program
/// and writes it as an ordinary file literally named `NUL` in the working
/// directory, which on Linux is the file of that name in the current folder.
/// `/dev/null` is understood everywhere this client runs.
fn null_device() -> &'static str {
    "/dev/null"
}

/// One entry per proxy for the embedded client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyCommand {
    pub local_addr: String,
    pub server_addr: String,
    pub command: String,
}

pub fn parse_to_gateway(config: &Config) -> Vec<ProxyCommand> {
    let server_addr = crate::util::join_host_port(&config.server_addr, config.server_port);
    config
        .proxies
        .iter()
        .map(|proxy| ProxyCommand {
            local_addr: proxy.local_addr(),
            server_addr: server_addr.clone(),
            command: gen_extra(proxy, config),
        })
        .collect()
}

/// Hides the auth token before a command reaches the log. The command carries
/// `--token <secret>` and is logged on every (re)connect.
pub fn redact_command(command: &str) -> String {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut result: Vec<&str> = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        result.push(token);
        if token == "--token" && index + 1 < tokens.len() {
            index += 1;
            result.push("***");
        }
        index += 1;
    }
    result.join(" ")
}

/// Warns about values that cannot survive frps' space splitting. A space inside
/// any of these silently corrupts the registration, so the user needs to know
/// before the proxy fails to appear.
pub fn warn_unrepresentable(proxy: &Proxy, config: &Config, context: &str) {
    let mut offending: Vec<String> = Vec::new();
    let mut check = |label: &str, value: &str| {
        if value.contains(' ') {
            offending.push(format!("{label} ({value:?})"));
        }
    };
    check("name", &proxy.name);
    check("token", &config.auth.token);
    check("user", &config.user);
    check("httpPassword", &proxy.http_password);
    check("secretKey", &proxy.secret_key);
    check("hostHeaderRewrite", &proxy.host_header_rewrite);
    for domain in &proxy.custom_domains {
        check("customDomains", domain);
    }
    for (key, value) in &proxy.metadatas {
        check("metadatas key", key);
        check("metadatas value", value);
    }
    if !offending.is_empty() {
        crate::logging::warn(format!(
            "{context}: proxy [{}] has whitespace in {}, which frps cannot represent — \
             it splits the command on spaces",
            proxy.name,
            offending.join(", ")
        ));
    }
}

fn push(flags: &mut Vec<String>, name: &str, value: &str) {
    if !value.is_empty() {
        flags.push(name.to_string());
        flags.push(value.to_string());
    }
}

fn push_num(flags: &mut Vec<String>, name: &str, value: u16) {
    if value != 0 {
        flags.push(name.to_string());
        flags.push(value.to_string());
    }
}

fn push_list(flags: &mut Vec<String>, name: &str, values: &[String]) {
    if !values.is_empty() {
        flags.push(name.to_string());
        flags.push(values.join(","));
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    fn tcp_config() -> Config {
        let mut config = Config::default();
        config.user = "hqu".to_string();
        config.auth.token = "secret".to_string();
        let mut proxy = Proxy::default();
        proxy.name = "demo".to_string();
        proxy.local_port = 22;
        proxy.remote_port = 8080;
        proxy.metadatas.insert("a".to_string(), "1".to_string());
        proxy.metadatas.insert("z".to_string(), "2".to_string());
        config.proxies.push(proxy);
        config.complete();
        config
    }

    #[test]
    fn generates_the_same_payload_as_the_go_client() {
        let mut config = tcp_config();
        config.metadatas_enabled = true;
        assert_eq!(
            gen_extra(&config.proxies[0], &config),
            "tcp --proxy-name demo --remote-port 8080 --metadatas a=1,z=2 --user hqu --token secret"
        );
    }

    #[test]
    fn keeps_the_proxy_name_free_of_the_user_prefix() {
        let config = tcp_config();
        assert!(
            gen_extra(&config.proxies[0], &config).starts_with("tcp --proxy-name demo "),
            "frps applies the user prefix, the client must not"
        );
    }

    #[test]
    fn omits_metadatas_unless_enabled() {
        let config = tcp_config();
        assert!(!gen_extra(&config.proxies[0], &config).contains("--metadatas"));
    }

    #[test]
    fn omits_empty_and_zero_values() {
        let mut config = Config::default();
        let mut proxy = Proxy::default();
        proxy.name = "demo".to_string();
        config.proxies.push(proxy);
        config.complete();
        assert_eq!(
            gen_extra(&config.proxies[0], &config),
            "tcp --proxy-name demo"
        );
    }

    #[test]
    fn emits_domain_and_stcp_flags_for_their_types() {
        let mut config = Config::default();
        let mut proxy = Proxy::default();
        proxy.name = "web".to_string();
        proxy.proxy_type = ProxyType::Http;
        proxy.custom_domains = vec!["a.com".to_string(), "b.com".to_string()];
        proxy.subdomain = "web".to_string();
        proxy.locations = vec!["/".to_string(), "/pic".to_string()];
        proxy.http_user = "admin".to_string();
        proxy.http_password = "admin".to_string();
        proxy.host_header_rewrite = "example.com".to_string();
        config.proxies.push(proxy);
        config.complete();
        assert_eq!(
            gen_extra(&config.proxies[0], &config),
            "http --proxy-name web --custom-domain a.com,b.com --locations /,/pic --sd web \
             --http-user admin --http-pwd admin --host-header-rewrite example.com"
        );
    }

    #[test]
    fn emits_remote_port_for_tcp_only() {
        let mut config = Config::default();
        let mut proxy = Proxy::default();
        proxy.name = "web".to_string();
        proxy.proxy_type = ProxyType::Stcp;
        proxy.remote_port = 9000;
        proxy.secret_key = "abcdefg".to_string();
        proxy.allow_users = vec!["*".to_string()];
        config.proxies.push(proxy);
        config.complete();
        let command = gen_extra(&config.proxies[0], &config);
        assert!(!command.contains("--remote-port"));
        assert!(command.contains("--sk abcdefg"));
        assert!(command.contains("--allow-users *"));
    }

    #[test]
    fn brackets_ipv6_in_the_native_command() {
        let mut config = Config::default();
        config.server_addr = "::1".to_string();
        config.server_port = 2200;
        let mut proxy = Proxy::default();
        proxy.name = "demo".to_string();
        proxy.local_port = 22;
        config.proxies.push(proxy);
        config.complete();
        let commands = parse_to_native_ssh(&config);
        // `ssh` parses the part after `@` as a host name, so the address is
        // bracketed but the port travels separately in -p.
        assert!(
            commands[0].starts_with("ssh v0@[::1] -p 2200 -R :80:127.0.0.1:22 "),
            "{}",
            commands[0]
        );
    }

    #[test]
    fn native_commands_do_not_leave_a_stray_known_hosts_file() {
        // `NUL` is not the null device for a non-Windows `ssh`: it creates a
        // real file of that name in the working directory.
        let mut config = Config::default();
        config.server_addr = "1.2.3.4".to_string();
        config.server_port = 2200;
        let mut proxy = Proxy::default();
        proxy.name = "demo".to_string();
        proxy.local_port = 22;
        config.proxies.push(proxy);
        config.complete();
        let commands = parse_to_native_ssh(&config);
        assert!(
            commands[0].contains("UserKnownHostsFile=/dev/null"),
            "{}",
            commands[0]
        );
        assert!(!commands[0].contains("=NUL"), "{}", commands[0]);
    }

    #[test]
    fn native_commands_use_a_bare_host_and_a_separate_port() {
        let mut config = Config::default();
        config.server_addr = "1.2.3.4".to_string();
        config.server_port = 2200;
        let mut proxy = Proxy::default();
        proxy.name = "demo".to_string();
        proxy.local_port = 22;
        config.proxies.push(proxy);
        config.complete();
        let commands = parse_to_native_ssh(&config);
        assert!(
            commands[0].starts_with("ssh v0@1.2.3.4 -p 2200 -R :80:127.0.0.1:22 "),
            "{}",
            commands[0]
        );
        assert!(
            commands[0].contains("StrictHostKeyChecking=no"),
            "no known_hosts configured, so verification must be off: {}",
            commands[0]
        );
    }

    #[test]
    fn redacts_the_token_only() {
        assert_eq!(
            redact_command("tcp --proxy-name demo --user u --token s3cr3t"),
            "tcp --proxy-name demo --user u --token ***"
        );
        assert_eq!(
            redact_command("tcp --proxy-name demo"),
            "tcp --proxy-name demo"
        );
        assert_eq!(redact_command("tcp --token"), "tcp --token");
    }
}
