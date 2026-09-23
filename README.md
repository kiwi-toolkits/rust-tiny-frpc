# rust-tiny-frpc

[中文说明](README_zh.md)

A tiny frp client that drives the frps **SSH tunnel gateway**.

It is not a replacement for `frpc`. It connects to frps over SSH, asks for a
reverse forward, and hands frps a proxy command string; frps does the rest
server-side. That means no frp protocol implementation and no second binary to
ship — just an SSH client and a config parser.

```
$ tiny-frpc -c frpc.toml
[2026-09-23T15:28:00+08:00] [INFO] proxy total len: 1
[2026-09-23T15:28:00+08:00] [INFO] start to run: tcp --proxy-name ssh --remote-port 6001 --token ***
[2026-09-23T15:28:00+08:00] [INFO] session start cmd [tcp --proxy-name ssh --remote-port 6001 --token ***] success
[2026-09-23T15:28:00+08:00] [INFO] frps: frp (via SSH) (Ctrl+C to quit)
```

## Requirements

* **frps >= 0.54.0.** The gateway normalises `_` to `-` in proxy flags from that
  release, and this client emits the hyphenated spelling.
* **frps >= 0.61.2** if you enable proxy metadatas (`--metadatas` was added
  there).
* A `sshTunnelGateway.bindPort` on the server:

  ```toml
  # frps.toml
  bindPort = 7000
  sshTunnelGateway.bindPort = 2200
  ```

  **`serverPort` in the client config is that gateway port, not `bindPort`.**
  Pointing it at `bindPort` fails the SSH handshake, and the client says so:

  ```
  ssh handshake to 1.2.3.4:7000 failed (hint: when the remote server is frps, serverPort
  must be the ssh tunnel gateway port sshTunnelGateway.bindPort, NOT the native protocol
  port bindPort)
  ```

## Build

```bash
cargo build --release
```

The release profile is size-oriented (`opt-level = "z"`, fat LTO, `strip`,
`panic = "abort"`). For a quick local build use `--profile release-fast`.

Both binaries are built from the same crate:

```bash
target/release/tiny-frpc       # embedded SSH client, reconnects on its own
target/release/tiny-frpc-ssh   # drives the system ssh command
```

`tiny-frpc-ssh` needs `ssh` on `PATH` and trades away reconnect and rename
support — there is no way to tell why the child exited. Prefer `tiny-frpc`
unless the device already ships OpenSSH and you want to avoid a second SSH
stack.

## Cross-compiling

For ARMv7 boards there are build scripts and a static musl artifact:

```bash
./scripts/build-arm.sh          # Linux / macOS / WSL
./scripts/build-arm.ps1         # Windows (PowerShell)
```

See [doc/build-linux-arm.md](doc/build-linux-arm.md) for the target triples and
their requirements.

## Usage

```
Usage: tiny-frpc [options]

  -c, --config <path>       path to the configuration file (default: frpc.toml)
      --rename[=bool]       retry a conflicting proxy under an ex_N_ name
      --metadatas[=bool]    send proxy metadatas to frps
      --ssh-private-key <path>
  -v, --v, --version        print the version and exit
  -h, --help                print this help and exit
```

The three switches override whatever the configuration file said, in both
directions (`--no-rename`, `--no-metadatas`).

## Configuration

TOML and the legacy `[common]` INI format are both accepted, and the format is
detected from the content rather than the file name: a file that parses into
sections and has a `[common]` one is INI. See `conf/` for a minimal pair and a
full example of each.

```toml
serverAddr = "1.2.3.4"
serverPort = 2200          # the ssh tunnel gateway port

auth.method = "token"
auth.token = "12345678"

[[proxies]]
name = "ssh"
type = "tcp"
localIP = "127.0.0.1"
localPort = 22
remotePort = 6001
```

```ini
[common]
server_addr = 1.2.3.4
server_port = 2200
token = 12345678

[ssh]
type = tcp
local_ip = 127.0.0.1
local_port = 22
remote_port = 6001
```

Supported proxy types and their keys:

| type | keys |
| :--- | :--- |
| `tcp` | `remotePort` |
| `http` | `customDomains`, `subdomain`, `locations`, `httpUser`, `httpPassword`, `hostHeaderRewrite` |
| `https` | `customDomains`, `subdomain` |
| `tcpmux` | `customDomains`, `subdomain`, `multiplexer` (must be `httpconnect`), `httpUser`, `httpPassword` |
| `stcp` | `secretKey`, `allowUsers` |

`udp`, `xtcp` and `sudp` cannot be expressed through the gateway, so they are
rejected at load time with an explanation. `visitors` are parsed and validated
but have no effect — the gateway cannot register one — so they are reserved for
a future release.

INI details: keys may use `=` or `:`; a bare word is a boolean; an inline `#` is
part of the value (frp runs its INI parser with inline comments disabled);
`meta_*` keys in a proxy section become that proxy's metadatas.

### Client options

| TOML | INI | Default | Meaning |
| :--- | :--- | :--- | :--- |
| `user` | `user` | empty | proxy owner; frps prefixes the wire name with it |
| `userDoublePrefix` | `user_double_prefix` | `false` | also prefix on the client, for older deployments |
| `rename` | `rename` | `false` | retry a conflicting proxy under `ex_N_` |
| `metadatasEnabled` | `metadatas_enabled` | `false` | send proxy metadatas (needs frps >= 0.61.2) |
| `sshPrivateKey` | `ssh_private_key` | `$HOME/.ssh/id_rsa` | key for the gateway |
| `sshKnownHosts` | `ssh_known_hosts` | none | verify the gateway's host key |
| `maxForwardConnections` | `max_forward_connections` | `512` | concurrent bridged connections |
| `includes` | — | none | extra TOML files, glob relative to this one |

### Templating

A configuration file is rendered before it is parsed, and `.Envs` is the only
thing exposed:

```toml
auth.token = "{{ .Envs.FRP_TOKEN }}"
```

A missing variable renders as the empty string, matching Go's `text/template`.
Any other action (`{{ .Name }}`, conditionals) is a load error rather than being
passed through literally, so a config that relies on frpc's richer templating
fails loudly instead of connecting with the wrong values.

## Behaviour notes

### The user prefix

frps builds the registered name from `--user` itself. The client therefore sends
the bare name, and a proxy called `ssh` with `user = "alice"` is registered as
`alice.ssh`. Older builds of this client (and the Go original) also prefixed the
name locally, which produced `alice.alice.ssh`; set `userDoublePrefix = true` if
you depend on those names.

### Rename retry

With `--rename`, a proxy frps rejects as a duplicate is retried under
`ex_1_<name>`, then `ex_2_`, then `ex_3_`, then `ex_1_` again. The sequence is
deterministic so a monitoring service that aggregates proxy names does not
accumulate random identities, and each candidate is rebuilt from the original
command so prefixes never stack. The chosen name sticks across reconnects, but
every cycle probes the original name first, so the original identity is restored
as soon as the conflict clears:

```
[WARN] proxy already exists, retrying with the same name in 5s
[WARN] proxy already exists, retrying with the same name in 5s
[WARN] renamed proxy ex_1_ still conflicts, retrying with ex_2_ in 5s
[INFO] session start cmd [tcp --proxy-name ex_2_ssh --remote-port 6001] success
```

### SSH authentication

The client tries, in order: `--ssh-private-key`/`sshPrivateKey`, then
`$HOME/.ssh/id_rsa` (`USERPROFILE` is consulted too on Windows). If neither
yields a usable key it logs a warning and connects without client
authentication, which the gateway accepts unless it sets `authorizedKeysFile`.
A missing key never prevents startup.

### Host key verification

Off by default, matching the Go implementation: the client accepts any host key.
Set `sshKnownHosts` to a `known_hosts` file to enable verification. Note that
frps regenerates `.autogen_ssh_key` when its working directory changes, so a
recorded key can go stale on its own — which is why this is opt-in.

`tiny-frpc-ssh` follows the same policy: without `sshKnownHosts` it passes
`StrictHostKeyChecking=no` to `ssh` and points `UserKnownHostsFile` at the null
device; with one configured, `ssh` keeps its strict default.

### Logging and shutdown

Logs go to **stderr** so stdout stays clean for `--version` and `--help`, and a
log write that fails (a closed pipe, a journal that went away) is ignored rather
than fatal. Both binaries treat `SIGINT` and `SIGTERM` as shutdown, so a service
manager's `stop` is handled without a `KillSignal=` override.

## Tests

```bash
cargo test
```

The reconnect and rename state machine is covered by `tests/gateway_runner.rs`,
which runs against a fake gateway (`tests/support/`) built from the same `russh`
crate — a real SSH server speaking the subset of the protocol frp's `pkg/ssh`
uses.

The end-to-end test against a real frps is ignored by default:

```bash
RUN_REAL_FRPS_TESTS=1 \
FRPS_BIN=/path/to/frps \
FRPS_CONFIG=tests/fixtures/frps-integration.toml \
cargo test --test real_frps -- --ignored --nocapture
```

It is `#[ignore]`d rather than returning early on purpose: an early return
reports as a pass, so a CI job missing the environment variables would look green
while testing nothing.

## License

Apache-2.0, matching the frp ecosystem.
