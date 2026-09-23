//! Small shared helpers.

/// Joins a host and a port, bracketing the host when it is a bare IPv6 literal.
///
/// `("::1", 80)` becomes `[::1]:80`, which is what a `host:port` field expects.
/// A host that is already bracketed is left alone.
pub fn join_host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Brackets a bare IPv6 literal for use on its own, with no port attached.
///
/// `ssh v0@[::1] -p 2200` is correct; `ssh v0@::1 -p 2200` is not, because the
/// remote part of an ssh destination is parsed as a host, not a `host:port`.
pub fn bracket_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

/// Splits `host:port`, accepting a bracketed IPv6 literal.
pub fn parse_host_port(value: &str) -> Result<(String, u16), String> {
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, rest) = rest
            .split_once(']')
            .ok_or_else(|| format!("invalid host:port {value:?}: missing ']'"))?;
        let port = rest
            .strip_prefix(':')
            .ok_or_else(|| format!("invalid host:port {value:?}: missing port"))?;
        (host.to_string(), port)
    } else {
        let (host, port) = value
            .rsplit_once(':')
            .ok_or_else(|| format!("invalid host:port {value:?}: missing port"))?;
        if host.contains(':') {
            // A bare IPv6 literal is ambiguous; require brackets.
            return Err(format!(
                "invalid host:port {value:?}: IPv6 addresses must be enclosed in square brackets"
            ));
        }
        (host.to_string(), port)
    };
    let port: u16 = port
        .parse()
        .map_err(|error| format!("invalid port in {value:?}: {error}"))?;
    Ok((host, port))
}

/// Whether a string looks like an IPv4/IPv6 literal rather than a host name.
pub fn is_ip_literal(host: &str) -> bool {
    host.parse::<std::net::IpAddr>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brackets_ipv6_only_when_needed() {
        assert_eq!(join_host_port("127.0.0.1", 2200), "127.0.0.1:2200");
        assert_eq!(join_host_port("example.com", 2200), "example.com:2200");
        assert_eq!(join_host_port("::1", 2200), "[::1]:2200");
        assert_eq!(join_host_port("[::1]", 2200), "[::1]:2200");
    }

    #[test]
    fn brackets_a_bare_host_without_a_port() {
        assert_eq!(bracket_host("127.0.0.1"), "127.0.0.1");
        assert_eq!(bracket_host("example.com"), "example.com");
        assert_eq!(bracket_host("::1"), "[::1]");
        assert_eq!(bracket_host("[::1]"), "[::1]");
    }

    #[test]
    fn parses_host_and_port() {
        assert_eq!(
            parse_host_port("example.com:2200").unwrap(),
            ("example.com".to_string(), 2200)
        );
        assert_eq!(
            parse_host_port("[::1]:2200").unwrap(),
            ("::1".to_string(), 2200)
        );
    }

    #[test]
    fn rejects_malformed_host_ports() {
        assert!(parse_host_port("example.com").is_err());
        assert!(parse_host_port("::1:2200").is_err());
        assert!(parse_host_port("example.com:notaport").is_err());
        assert!(parse_host_port("[::1:2200").is_err());
    }
}
