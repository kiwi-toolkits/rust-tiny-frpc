//! End-to-end loading of the shipped example configs, both formats.

#![allow(clippy::field_reassign_with_default)]

use std::path::Path;

use tiny_frpc::config::{Config, ProxyType};

fn load(path: &str) -> Config {
    tiny_frpc::config::load(Path::new(path)).unwrap_or_else(|error| panic!("{path}: {error}"))
}

#[test]
fn the_minimal_toml_example_loads() {
    let config = load("conf/frpc.toml");
    assert_eq!(config.server_port, 2200);
    assert_eq!(config.proxies.len(), 1);
    assert_eq!(config.proxies[0].name, "test-tcp");
    assert_eq!(config.proxies[0].proxy_type, ProxyType::Tcp);
    assert_eq!(config.proxies[0].local_ip, "127.0.0.1");
}

#[test]
fn the_minimal_ini_example_loads_to_the_same_shape() {
    let toml = load("conf/frpc.toml");
    let ini = load("conf/frpc.ini");
    assert_eq!(ini.server_addr, toml.server_addr);
    assert_eq!(ini.server_port, toml.server_port);
    assert_eq!(ini.auth.token, toml.auth.token);
    assert_eq!(ini.proxies.len(), toml.proxies.len());
    assert_eq!(ini.proxies[0].name, toml.proxies[0].name);
    assert_eq!(ini.proxies[0].local_port, toml.proxies[0].local_port);
    assert_eq!(ini.proxies[0].remote_port, toml.proxies[0].remote_port);
}

#[test]
fn the_full_toml_example_covers_every_proxy_type() {
    let config = load("conf/frpc_full_example.toml");
    let types: Vec<ProxyType> = config
        .proxies
        .iter()
        .map(|proxy| proxy.proxy_type)
        .collect();
    assert_eq!(
        types,
        vec![
            ProxyType::Tcp,
            ProxyType::Http,
            ProxyType::Https,
            ProxyType::TcpMux,
            ProxyType::Stcp
        ]
    );
    let web01 = &config.proxies[1];
    assert_eq!(web01.subdomain, "web01");
    assert_eq!(web01.locations, vec!["/", "/pic"]);
    assert_eq!(web01.http_user, "admin");
    assert_eq!(web01.host_header_rewrite, "example.com");
    let tcpmux = &config.proxies[3];
    assert_eq!(tcpmux.multiplexer, "httpconnect");
    let stcp = &config.proxies[4];
    assert_eq!(stcp.secret_key, "abcdefg");
    assert_eq!(stcp.allow_users, vec!["*"]);
    config.validate().expect("the example must validate");
}

#[test]
fn the_full_ini_example_matches_the_toml_one() {
    let toml = load("conf/frpc_full_example.toml");
    let ini = load("conf/frpc_full_example.ini");
    assert_eq!(ini.proxies.len(), toml.proxies.len());
    for (from_ini, from_toml) in ini.proxies.iter().zip(&toml.proxies) {
        assert_eq!(from_ini.name, from_toml.name);
        assert_eq!(from_ini.proxy_type, from_toml.proxy_type);
        assert_eq!(from_ini.local_port, from_toml.local_port);
        assert_eq!(from_ini.remote_port, from_toml.remote_port);
        assert_eq!(from_ini.subdomain, from_toml.subdomain);
        assert_eq!(from_ini.custom_domains, from_toml.custom_domains);
        assert_eq!(from_ini.secret_key, from_toml.secret_key);
    }
    ini.validate().expect("the example must validate");
}

#[test]
fn the_full_examples_document_the_new_defaults() {
    let toml = load("conf/frpc_full_example.toml");
    assert!(!toml.rename, "renaming is off unless asked for");
    assert!(
        !toml.metadatas_enabled,
        "metadatas are off unless asked for"
    );
    assert!(!toml.user_double_prefix);
    assert!(toml.ssh_private_key.is_none());
    assert!(toml.ssh_known_hosts.is_none());
}

#[test]
fn renaming_off_keeps_the_proxy_name_untouched() {
    let config = load("conf/frpc_full_example.toml");
    assert_eq!(config.user, "your_name");
    assert_eq!(
        config.proxies[0].name, "ssh",
        "frps adds the user prefix; the client must not"
    );
}

#[test]
fn format_detection_follows_the_content_not_the_extension() {
    // A toml file with no [common] section is toml; the same text with one is
    // ini, whatever it is called.
    let toml_text = "serverAddr = \"1.2.3.4\"\n[[proxies]]\nname = \"a\"\ntype = \"tcp\"\n";
    let config = tiny_frpc::config::parse(toml_text, Path::new("named.ini")).unwrap();
    assert_eq!(config.server_addr, "1.2.3.4");

    let ini_text = "[common]\nserver_addr = 1.2.3.4\n\n[a]\ntype = tcp\n";
    let config = tiny_frpc::config::parse(ini_text, Path::new("named.toml")).unwrap();
    assert_eq!(config.server_addr, "1.2.3.4");
}

#[test]
fn includes_pull_in_extra_proxies() {
    let dir = std::env::temp_dir().join(format!("rust-tiny-frpc-includes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("conf.d")).expect("create include dir");

    let main = dir.join("frpc.toml");
    std::fs::write(
        &main,
        "serverAddr = \"1.2.3.4\"\nincludes = [\"conf.d/*.toml\"]\n\n\
         [[proxies]]\nname = \"main\"\ntype = \"tcp\"\nremotePort = 1\n",
    )
    .expect("write main config");
    std::fs::write(
        dir.join("conf.d").join("extra.toml"),
        "[[proxies]]\nname = \"extra\"\ntype = \"tcp\"\nremotePort = 2\n",
    )
    .expect("write include");

    let config = tiny_frpc::config::load(&main).expect("load with includes");
    assert_eq!(config.proxies.len(), 2);
    let names: Vec<&str> = config.proxies.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["main", "extra"]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_removed_remote_address_key_is_an_error_in_both_formats() {
    let error = tiny_frpc::config::parse(
        "serverAddrFromRemote = 1\n[[proxies]]\nname=\"a\"\ntype=\"tcp\"\n",
        Path::new("a.toml"),
    )
    .unwrap_err();
    assert!(error.contains("was removed"), "{error}");

    let error = tiny_frpc::config::parse(
        "[common]\nserver_addr_from_remote = 1\n\n[a]\ntype = tcp\n",
        Path::new("a.ini"),
    )
    .unwrap_err();
    assert!(error.contains("was removed"), "{error}");
}

#[test]
fn templates_are_rendered_before_parsing() {
    std::env::set_var("RUST_TINY_FRPC_IT_HOST", "10.0.0.1");
    let config = tiny_frpc::config::parse(
        "serverAddr = \"{{ .Envs.RUST_TINY_FRPC_IT_HOST }}\"\n\
         [[proxies]]\nname = \"a\"\ntype = \"tcp\"\n",
        Path::new("a.toml"),
    )
    .unwrap();
    assert_eq!(config.server_addr, "10.0.0.1");
}
