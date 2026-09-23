//! The configuration model, shared by both file formats.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// Default SSH tunnel gateway port. Not frp's own default (`bindPort` is 7000,
/// `sshTunnelGateway.bindPort` has no default), but the value the Go client
/// falls back to.
pub const DEFAULT_SERVER_PORT: u16 = 2200;
pub const DEFAULT_SERVER_ADDR: &str = "0.0.0.0";
pub const DEFAULT_LOCAL_IP: &str = "127.0.0.1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    pub method: String,
    pub token: String,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            // frps only ever accepts token auth here, and the Go client forces
            // this value regardless of what the file said.
            method: "token".to_string(),
            token: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub auth: AuthConfig,
    pub user: String,
    pub server_addr: String,
    pub server_port: u16,

    /// Proxy-level metadatas are only sent when this is on. Off by default
    /// because `--metadatas` is rejected by frps <= 0.61.1 with
    /// `unknown flag: --metadatas`, which fails the whole registration.
    pub metadatas_enabled: bool,

    /// Retry a conflicting proxy under an `ex_N_` name instead of backing off
    /// and failing. Off by default: it changes the public proxy name.
    pub rename: bool,

    /// Reproduce the pre-0.1.0 behaviour where the client also prefixes the
    /// proxy name with `{user}.`, on top of the prefix frps applies itself.
    /// Only useful for matching an older deployment's proxy names.
    pub user_double_prefix: bool,

    /// Private key used against the SSH tunnel gateway. `None` means "try the
    /// default location, and fall back to no client authentication".
    pub ssh_private_key: Option<PathBuf>,

    /// `known_hosts` file enabling server host key verification. `None` keeps
    /// the historical behaviour of accepting any host key (with a warning).
    pub ssh_known_hosts: Option<PathBuf>,

    /// Upper bound on concurrently bridged forwarded connections.
    pub max_forward_connections: usize,

    /// Extra configuration files, resolved relative to the including file.
    pub includes: Vec<String>,

    pub proxies: Vec<Proxy>,
    pub visitors: Vec<Visitor>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auth: AuthConfig::default(),
            user: String::new(),
            server_addr: String::new(),
            server_port: 0,
            metadatas_enabled: false,
            rename: false,
            user_double_prefix: false,
            ssh_private_key: None,
            ssh_known_hosts: None,
            max_forward_connections: 512,
            includes: Vec::new(),
            proxies: Vec::new(),
            visitors: Vec::new(),
        }
    }
}

impl Config {
    /// Applies defaults and pushes the client `user` down into the proxies.
    /// Runs once, at the end of loading.
    pub fn complete(&mut self) {
        if self.server_addr.is_empty() {
            self.server_addr = DEFAULT_SERVER_ADDR.to_string();
        }
        if self.server_port == 0 {
            self.server_port = DEFAULT_SERVER_PORT;
        }
        if self.auth.method.is_empty() {
            self.auth.method = "token".to_string();
        }
        for proxy in &mut self.proxies {
            // The `{user}.` prefix is frps' job: it builds the wire name from
            // `--user`. Only the legacy compatibility switch makes the client
            // add it too, which is what produced `{user}.{user}.{proxy}`.
            proxy.complete(if self.user_double_prefix && !self.user.is_empty() {
                Some(self.user.as_str())
            } else {
                None
            });
        }
        for visitor in &mut self.visitors {
            visitor.complete(&self.user);
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.proxies.is_empty() && self.visitors.is_empty() {
            return Err("no proxies or visitors are configured".to_string());
        }
        for proxy in &self.proxies {
            proxy.validate()?;
        }
        for visitor in &self.visitors {
            visitor.validate()?;
        }
        Ok(())
    }
}

use std::fmt;

/// The proxy types frps' SSH tunnel gateway accepts. It rejects anything else
/// before it even looks at the flags, so `udp`, `xtcp` and `sudp` cannot be
/// expressed through this client at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyType {
    Tcp,
    Http,
    Https,
    TcpMux,
    Stcp,
}

impl ProxyType {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "tcp" => Ok(Self::Tcp),
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            "tcpmux" => Ok(Self::TcpMux),
            "stcp" => Ok(Self::Stcp),
            "udp" | "xtcp" | "sudp" => Err(format!(
                "proxy type {value:?} is not supported by the frps ssh tunnel gateway, \
                 which only accepts tcp, http, https, tcpmux and stcp"
            )),
            other => Err(format!(
                "unknown proxy type: {other}, support types: [tcp http https tcpmux stcp]"
            )),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Http => "http",
            Self::Https => "https",
            Self::TcpMux => "tcpmux",
            Self::Stcp => "stcp",
        }
    }
}

impl fmt::Display for ProxyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proxy {
    pub name: String,
    pub proxy_type: ProxyType,
    pub local_ip: String,
    pub local_port: u16,
    pub remote_port: u16,
    pub custom_domains: Vec<String>,
    pub subdomain: String,
    pub locations: Vec<String>,
    pub http_user: String,
    pub http_password: String,
    pub host_header_rewrite: String,
    pub route_by_http_user: String,
    pub multiplexer: String,
    pub secret_key: String,
    pub allow_users: Vec<String>,
    pub metadatas: BTreeMap<String, String>,
}

impl Default for Proxy {
    fn default() -> Self {
        Self {
            name: String::new(),
            proxy_type: ProxyType::Tcp,
            local_ip: String::new(),
            local_port: 0,
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
            metadatas: BTreeMap::new(),
        }
    }
}

impl Proxy {
    /// `user_prefix` is `Some(user)` only when the legacy double-prefix
    /// behaviour is enabled; normally the prefix is left to frps.
    pub fn complete(&mut self, user_prefix: Option<&str>) {
        if let Some(user) = user_prefix {
            if !user.is_empty() {
                self.name = format!("{user}.{}", self.name);
            }
        }
        if self.local_ip.is_empty() {
            self.local_ip = DEFAULT_LOCAL_IP.to_string();
        }
    }

    pub fn local_addr(&self) -> String {
        crate::util::join_host_port(&self.local_ip, self.local_port)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("proxy name is required".to_string());
        }
        let needs_domain = matches!(
            self.proxy_type,
            ProxyType::Http | ProxyType::Https | ProxyType::TcpMux
        );
        if needs_domain && self.subdomain.is_empty() && self.custom_domains.is_empty() {
            return Err(format!(
                "proxy {}: http/https/tcpmux proxies require subdomain or customDomains",
                self.name
            ));
        }
        if self.proxy_type == ProxyType::TcpMux && self.multiplexer != "httpconnect" {
            return Err(format!(
                "proxy {}: tcpmux multiplexer must be httpconnect",
                self.name
            ));
        }
        Ok(())
    }
}

/// Parsed and validated, but never used to open anything — same as the Go
/// original, which reserves visitors for a future release.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Visitor {
    pub name: String,
    pub visitor_type: String,
    pub secret_key: String,
    pub server_user: String,
    pub server_name: String,
    pub bind_addr: String,
    pub bind_port: u16,
}

impl Visitor {
    pub fn complete(&mut self, user: &str) {
        let prefix = |value: &str| {
            if user.is_empty() {
                value.to_string()
            } else {
                format!("{user}.{value}")
            }
        };
        if !self.name.is_empty() {
            self.name = prefix(&self.name);
        }
        if self.server_user.is_empty() && !self.server_name.is_empty() {
            self.server_name = prefix(&self.server_name);
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.visitor_type != "stcp" {
            return Err(format!(
                "visitor {}: only stcp visitors are supported",
                self.name
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn complete_fills_gateway_defaults() {
        let mut config = Config::default();
        config.complete();
        assert_eq!(config.server_addr, "0.0.0.0");
        assert_eq!(config.server_port, 2200);
        assert_eq!(config.auth.method, "token");
    }

    #[test]
    fn does_not_prefix_the_proxy_name_by_default() {
        let mut proxy = Proxy::default();
        proxy.name = "demo".to_string();
        proxy.complete(None);
        assert_eq!(proxy.name, "demo", "frps adds the user prefix itself");
        assert_eq!(proxy.local_ip, "127.0.0.1");
    }

    #[test]
    fn prefixes_only_when_the_legacy_switch_is_on() {
        let mut proxy = Proxy::default();
        proxy.name = "demo".to_string();
        proxy.complete(Some("hqu"));
        assert_eq!(proxy.name, "hqu.demo");
    }

    #[test]
    fn rejects_types_the_gateway_cannot_express() {
        assert!(ProxyType::parse("udp")
            .unwrap_err()
            .contains("not supported"));
        assert!(ProxyType::parse("nope")
            .unwrap_err()
            .contains("unknown proxy type"));
        assert_eq!(ProxyType::parse("tcpmux").unwrap(), ProxyType::TcpMux);
    }

    #[test]
    fn validates_domain_proxies() {
        let mut proxy = Proxy::default();
        proxy.name = "web".to_string();
        proxy.proxy_type = ProxyType::Http;
        assert!(proxy.validate().is_err());
        proxy.subdomain = "web".to_string();
        assert!(proxy.validate().is_ok());

        proxy.proxy_type = ProxyType::TcpMux;
        proxy.subdomain.clear();
        proxy.custom_domains = vec!["a.com".to_string()];
        assert!(
            proxy.validate().is_err(),
            "tcpmux still needs a multiplexer"
        );
        proxy.multiplexer = "httpconnect".to_string();
        assert!(proxy.validate().is_ok());
    }

    #[test]
    fn tcp_proxies_need_nothing_but_a_name() {
        let mut proxy = Proxy::default();
        proxy.name = "ssh".to_string();
        assert!(proxy.validate().is_ok());
    }
}
