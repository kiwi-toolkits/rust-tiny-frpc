# Deployment

## Files

| path | mode | purpose |
| :--- | :--- | :--- |
| `/usr/local/bin/tiny-frpc` | 0755 | the client |
| `/etc/tiny-frpc/frpc.toml` | 0600 | configuration (holds the auth token) |
| `/etc/tiny-frpc/` | 0755 | working directory, so the default `frpc.toml` resolves |

The config is read from the **working directory** when `-c` is not given, so the
service unit sets `WorkingDirectory=/etc/tiny-frpc` rather than relying on `-c`.
Setting `-c /etc/tiny-frpc/frpc.toml` explicitly is fine too and does not need
the working directory.

## SSH key

The client authenticates to the frps ssh tunnel gateway with a private key. In
order:

1. `--ssh-private-key` on the command line,
2. `sshPrivateKey` in the configuration,
3. `$HOME/.ssh/id_rsa` (on Windows `USERPROFILE` is consulted as well).

If none of these yields a usable key the client logs a warning and connects
**without client authentication**. That works when frps leaves
`sshTunnelGateway.authorizedKeysFile` empty; when frps requires keys, the
registration fails with `ssh authentication failed` and the client keeps
retrying with a growing backoff.

So a service account without a home directory is fine, and a key that is present
but unreadable is reported rather than fatal:

```
[WARN] failed to load private key [/root/.ssh/id_rsa]: Permission denied (os error 13),
       connecting without client authentication
```

When frps does require a key, generate one for the service account and register
its public half in the server's `authorizedKeysFile`:

```bash
sudo -u tiny-frpc ssh-keygen -t ed25519 -N '' -f /var/lib/tiny-frpc/.ssh/id_rsa
cat /var/lib/tiny-frpc/.ssh/id_rsa.pub | ssh root@frps-server \
  'cat >> /etc/frp/authorized_keys'
```

## systemd

```ini
[Unit]
Description=rust-tiny-frpc tunnel client
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=tiny-frpc
Group=tiny-frpc
WorkingDirectory=/etc/tiny-frpc
ExecStart=/usr/local/bin/tiny-frpc -c /etc/tiny-frpc/frpc.toml
Restart=always
RestartSec=3

# The client handles SIGTERM itself, so no KillSignal override is needed.
TimeoutStopSec=10

# Hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/var/log/tiny-frpc

StandardOutput=journal
StandardError=journal
SyslogIdentifier=tiny-frpc

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now tiny-frpc
journalctl -u tiny-frpc -f
```

Notes:

* `Restart=always` plus the client's own reconnect loop is deliberate: the unit
  covers a crash, the client covers a dropped connection or an frps restart.
* `ProtectHome=read-only` still allows reading `~/.ssh/id_rsa`; use
  `ProtectHome=no` if the key lives somewhere the sandbox hides.
* `tiny-frpc-ssh` needs `ssh` on `PATH`; it has no reconnect logic of its own, so
  prefer the embedded `tiny-frpc` for a supervised service.

## OpenRC

```sh
#!/sbin/openrc-run

name="tiny-frpc"
description="rust-tiny-frpc tunnel client"
command="/usr/local/bin/tiny-frpc"
command_args="-c /etc/tiny-frpc/frpc.toml"
command_user="tiny-frpc:tiny-frpc"
command_background="yes"
pidfile="/run/${RC_SVCNAME}.pid"
directory="/etc/tiny-frpc"

depend() {
    need net
    after firewall
}

start_pre() {
    checkpath --directory --owner "${command_user}" --mode 0755 /run
}
```

## Upgrade

```bash
systemctl stop tiny-frpc
install -m 0755 tiny-frpc /usr/local/bin/tiny-frpc
/usr/local/bin/tiny-frpc --v      # prints tiny.<version>
systemctl start tiny-frpc
```

The version flag accepts `-v`, `--v` and `--version`.

## Troubleshooting

| symptom | cause |
| :--- | :--- |
| `ssh handshake ... failed`, log mentions `sshTunnelGateway.bindPort` | `serverPort` points at frps' `bindPort` instead of the gateway port |
| `frps: unknown flag: --metadatas` | `metadatasEnabled` is on but frps is older than 0.61.2 |
| `frps: proxy [x] already exists` | the name is taken; enable `--rename` or change the name |
| `fatal: unknown field serverAddrFromRemote` | that key was removed; set `serverAddr`/`serverPort` directly |
| `Warning: Permanently added '[host]:port' ... to the list of known hosts` | expected from `tiny-frpc-ssh`; recorded in the null device, not your `known_hosts` |
| everything connects but the public port refuses | the local service is not listening, or `localPort` is wrong; the client logs `ssh tunnel client dial ... error` |
| `dropping a forwarded connection ... maximum number of connections` | more than `maxForwardConnections` (default 512) are bridged at once |
