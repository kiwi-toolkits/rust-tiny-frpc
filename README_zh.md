# rust-tiny-frpc

[English](README.md)

一个超轻量的 frp 客户端，驱动 frps 的 **SSH Tunnel Gateway**。

它不是 `frpc` 的替代品：它通过 SSH 连到 frps，请求反向转发，然后把一条代理命令字符串交给 frps，剩下的都由 frps 在服务端完成。因此不需要实现 frp 私有协议，也不需要额外附带一个二进制。

```
$ tiny-frpc -c frpc.toml
[2026-09-23T15:28:00+08:00] [INFO] proxy total len: 1
[2026-09-23T15:28:00+08:00] [INFO] start to run: tcp --proxy-name ssh --remote-port 6001 --token ***
[2026-09-23T15:28:00+08:00] [INFO] session start cmd [tcp --proxy-name ssh --remote-port 6001 --token ***] success
[2026-09-23T15:28:00+08:00] [INFO] frps: frp (via SSH) (Ctrl+C to quit)
```

## 环境要求

* **frps >= 0.54.0**。该版本起网关会把代理 flag 里的 `_` 归一化成 `-`，而本客户端发的正是连字符形式。
* 若启用代理级 metadatas，需要 **frps >= 0.61.2**（`--metadatas` 自该版本加入）。
* 服务端需开启 `sshTunnelGateway.bindPort`：

  ```toml
  # frps.toml
  bindPort = 7000
  sshTunnelGateway.bindPort = 2200
  ```

  **客户端配置里的 `serverPort` 是这个网关端口，不是 `bindPort`。** 填错会导致 SSH 握手失败，客户端会明确提示：

  ```
  ssh handshake to 1.2.3.4:7000 failed (hint: when the remote server is frps, serverPort
  must be the ssh tunnel gateway port sshTunnelGateway.bindPort, NOT the native protocol
  port bindPort)
  ```

## 构建

```bash
cargo build --release
```

release profile 面向体积（`opt-level = "z"`、fat LTO、`strip`、`panic = "abort"`）。本地快速构建可用 `--profile release-fast`。

同一个 crate 产出两个二进制：

```bash
target/release/tiny-frpc       # 内嵌 SSH 客户端，自带重连
target/release/tiny-frpc-ssh   # 调用系统 ssh 命令
```

`tiny-frpc-ssh` 需要 `PATH` 上有 `ssh`，并放弃重连与改名能力——因为无法从子进程退出状态判断原因。除非设备本身就带 OpenSSH 且你想省掉一份 SSH 实现，否则优先用 `tiny-frpc`。

## 使用

```
Usage: tiny-frpc [options]

  -c, --config <path>       配置文件路径（默认 frpc.toml）
      --rename[=bool]       frps 报告代理名冲突时，用 ex_N_ 前缀重试
      --metadatas[=bool]    向 frps 发送代理级 metadatas
      --ssh-private-key <path>
  -v, --v, --version        打印版本后退出
  -h, --help                打印帮助后退出
```

三个开关会覆盖配置文件里的值，且两个方向都可覆盖（`--no-rename`、`--no-metadatas`）。

## 配置

同时支持 TOML 与旧的 `[common]` INI 格式，且**按内容判断格式而非扩展名**：能解析出段、且存在 `[common]` 段的文件按 INI 处理。最小示例与完整示例见 `conf/`。

```toml
serverAddr = "1.2.3.4"
serverPort = 2200          # ssh tunnel gateway 端口

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

支持的代理类型及其键：

| 类型 | 键 |
| :--- | :--- |
| `tcp` | `remotePort` |
| `http` | `customDomains`、`subdomain`、`locations`、`httpUser`、`httpPassword`、`hostHeaderRewrite` |
| `https` | `customDomains`、`subdomain` |
| `tcpmux` | `customDomains`、`subdomain`、`multiplexer`（必须是 `httpconnect`）、`httpUser`、`httpPassword` |
| `stcp` | `secretKey`、`allowUsers` |

`udp`、`xtcp`、`sudp` 无法通过该网关表达，加载时会直接报错并说明原因。`visitors` 会被解析与校验，但网关无法注册 visitor，所以目前不产生任何效果，属于预留。

INI 细节：分隔符可用 `=` 或 `:`；单独一个词视为布尔真；行内 `#` 属于值的一部分（frp 的 INI 解析器关闭了行内注释）；代理段里的 `meta_*` 键会成为该代理的 metadatas。

### 客户端选项

| TOML | INI | 默认 | 说明 |
| :--- | :--- | :--- | :--- |
| `user` | `user` | 空 | 代理归属；由 frps 给线上名字加前缀 |
| `userDoublePrefix` | `user_double_prefix` | `false` | 客户端也加前缀，用于兼容旧部署 |
| `rename` | `rename` | `false` | 冲突时用 `ex_N_` 重试 |
| `metadatasEnabled` | `metadatas_enabled` | `false` | 发送代理级 metadatas（需 frps >= 0.61.2） |
| `sshPrivateKey` | `ssh_private_key` | `$HOME/.ssh/id_rsa` | 网关认证私钥 |
| `sshKnownHosts` | `ssh_known_hosts` | 无 | 校验网关主机密钥 |
| `maxForwardConnections` | `max_forward_connections` | `512` | 并发桥接连接上限 |
| `includes` | — | 无 | 附加 TOML 文件，按通配符相对本文件解析 |

### 模板

配置文件在解析前会先做一次渲染，只暴露 `.Envs`：

```toml
auth.token = "{{ .Envs.FRP_TOKEN }}"
```

缺失变量渲染为空字符串，与 Go 的 `text/template` 一致。其它动作（`{{ .Name }}`、条件判断等）会直接报错而不是原样保留，这样依赖 frpc 更丰富模板能力的配置会明显失败，而不是带着错误的值连上去。

## 行为说明

### 关于 user 前缀

frps 自己会用 `--user` 拼出注册名。所以客户端只发裸名：`user = "alice"` 且代理名 `ssh`，最终注册为 `alice.ssh`。本客户端的早期版本（以及 Go 原版）会在本地再加一次前缀，得到 `alice.alice.ssh`；若你依赖那些旧名字，把 `userDoublePrefix` 设为 `true`。

### 改名重试

打开 `--rename` 后，被 frps 判为重复的代理会用 `ex_1_<name>` → `ex_2_` → `ex_3_` → 回到 `ex_1_` 依次重试。序列是确定的，这样汇总代理名的监控插件不会累积随机身份；每个候选名都从原始命令重建，前缀不会叠加。选中的名字在重连间保持粘性，但每轮都会先探测原名，因此冲突一旦消失就会恢复原始身份：

```
[WARN] proxy already exists, retrying with the same name in 5s
[WARN] proxy already exists, retrying with the same name in 5s
[WARN] renamed proxy ex_1_ still conflicts, retrying with ex_2_ in 5s
[INFO] session start cmd [tcp --proxy-name ex_2_ssh --remote-port 6001] success
```

### SSH 认证

依次尝试：`--ssh-private-key`/`sshPrivateKey`，然后 `$HOME/.ssh/id_rsa`（Windows 上同时看 `USERPROFILE`）。都没有可用私钥时打一条 WARN，改为无客户端认证连接——只要网关没配 `authorizedKeysFile` 就能通过。**缺少私钥绝不会阻断启动。**

### 主机密钥校验

默认关闭，与 Go 实现一致：接受任意主机密钥。把 `sshKnownHosts` 指向一个 `known_hosts` 文件即可开启校验。注意 frps 的工作目录变化时会重新生成 `.autogen_ssh_key`，记录下来的密钥可能自行失效——这也是它默认为可选的原因。

`tiny-frpc-ssh` 采用同一策略：没有 `sshKnownHosts` 时给 `ssh` 传 `StrictHostKeyChecking=no` 并把 `UserKnownHostsFile` 指向空设备；配置了则保留 `ssh` 的严格默认。

### 日志与退出

日志走 **stderr**，stdout 留给 `--version` / `--help` 的正常输出；写日志失败（管道关闭、journald 消失）只忽略，不会致命。两个二进制都把 `SIGINT` 与 `SIGTERM` 当作退出信号，服务管理器 `stop` 无需额外配置 `KillSignal=`。

## 测试

```bash
cargo test
```

重连与改名状态机由 `tests/gateway_runner.rs` 覆盖，跑在 `tests/support/` 里的假网关上——它是用同一个 `russh` 实现的真实 SSH 服务端，实现了 frp `pkg/ssh` 用到的那部分协议。

对真实 frps 的端到端测试默认忽略：

```bash
RUN_REAL_FRPS_TESTS=1 \
FRPS_BIN=/path/to/frps \
FRPS_CONFIG=tests/fixtures/frps-integration.toml \
cargo test --test real_frps -- --ignored --nocapture
```

刻意用 `#[ignore]` 而不是提前 return：提前 return 会被报成通过，从而让漏设环境变量的 CI 看起来是绿的。

## 许可证

Apache-2.0，与 frp 生态保持一致。
