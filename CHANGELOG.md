# Changelog

## 0.1.0

First release. A from-scratch Rust client for the frps **SSH tunnel gateway**,
feature-compatible with the Go `tiny-frpc` and with four deliberate behaviour
changes.

### Compatibility

* Proxy types `tcp`, `http`, `https`, `tcpmux` and `stcp` — the set frps'
  gateway accepts. `udp`, `xtcp` and `sudp` are rejected with an explanation
  rather than silently dropped.
* The generated `exec` payload matches the Go client flag for flag, including
  the fixed flag order and the omission of empty values, because frps splits the
  payload on single spaces.
* Configuration in TOML or the legacy `[common]` INI format. The format is
  detected from the content, so the extension does not matter.
* `includes` with `filepath.Match`-style wildcards, and `{{ .Envs.NAME }}`
  templating, both applied before parsing.
* Two binaries: `tiny-frpc` speaks SSH itself, `tiny-frpc-ssh` drives the system
  `ssh` command.
* `-c/--config`, `-v/--v/--version`, and the Go client's default config path.

### Behaviour changes from the Go client

* **The proxy name is no longer prefixed with `{user}.` on the client side.**
  frps already builds the wire-level name from `--user`, so the old behaviour
  registered `{user}.{user}.{proxy}`. Set `userDoublePrefix = true` to restore
  it.
* **Renaming a conflicting proxy is off by default.** `--rename` or
  `rename = true` enables the deterministic `ex_1_` → `ex_2_` → `ex_3_` cycle.
  With it off, a conflict is logged and retried under the same name, exactly
  like the Go client.
* **Proxy metadatas are off by default.** `--metadatas` or
  `metadatasEnabled = true` sends them. They require frps >= 0.61.2: an older
  gateway answers `unknown flag: --metadatas` and rejects the whole
  registration.
* **Fetching the frps address from a remote service is gone.** The
  `serverAddrFromRemote` and `serverAddrRemoteURL` keys are a load-time error
  naming the replacement, rather than being silently ignored. Set `serverAddr`
  and `serverPort` directly.

### Fixes over the previous Rust port

* A missing private key no longer prevents startup. Any of
  `--ssh-private-key`, `ssh_private_key`/`sshPrivateKey`, `$HOME/.ssh/id_rsa` can
  supply one; when none is found the client connects without client
  authentication, which the gateway accepts unless it sets `authorizedKeysFile`.
* Log output goes to stderr and ignores write failures, so a closed pipe or a
  vanished journal cannot take the process down.
* Both binaries handle `SIGTERM` as well as `SIGINT`.
* Forwarded connections are capped (`maxForwardConnections`, default 512) and
  the local dial has a timeout, so a down backend cannot exhaust the process.
* Optional host key verification via `sshKnownHosts`; without it the client
  keeps the historical accept-any-key behaviour and says so.
* The `tiny-frpc-ssh` command is built the way `ssh` expects
  (`v0@host -p port`), which the previous port got wrong for every address.
