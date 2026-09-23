# Cross-compiling for ARMv7 Linux

The supported artefact for embedded boards is
`armv7-unknown-linux-musleabihf`: a statically linked hard-float ARM binary
with no dependency on the target's libc.

## Prerequisites

```bash
cargo install cargo-zigbuild
rustup target add armv7-unknown-linux-musleabihf
rustup target add armv7-unknown-linux-gnueabihf
```

Zig is needed as well and must be on `PATH`. It supplies the ARM C toolchain
that `aws-lc-sys` (pulled in by `russh`'s default rustls backend) otherwise
expects to find in a `gcc-arm-linux-gnueabihf` package — so no cross-gcc
install is required.

If `zig` and `cargo-zigbuild` live in the repository's `.tools/` directory (a
common local setup, and what `.gitignore` excludes), the build scripts put them
on `PATH` themselves.

## Build

```bash
./scripts/build-arm.sh          # Linux, macOS, WSL
.\scripts\build-arm.ps1         # Windows
```

The scripts disable full LTO for the cross-link (`CARGO_PROFILE_RELEASE_LTO=off`,
`codegen-units=16`): fat LTO over an ARM link can take an unbounded amount of
time, and release size optimisation stays on either way.

Artifacts land in `target/<triple>/release/`:

| triple | libc | linking | use when |
| :--- | :--- | :--- | :--- |
| `armv7-unknown-linux-musleabihf` | musl | static | default choice; works on any ARMv7 rootfs |
| `armv7-unknown-linux-gnueabihf` | glibc | dynamic (`/lib/ld-linux-armhf.so.3`) | the target already ships a compatible glibc and you want the smaller shared libc |

Both are hard-float (`EF_ARM_ABI_FLOAT_HARD` set in the ELF flags) and use the
ARMv7 VFP instruction set, so both require a board with VFP — that is, an
ARMv7-A/R core such as Cortex-A7/A9/A53, or a Cortex-M with the FPU enabled.
A soft-float-only or ARMv5/ARMv6 board needs a different target triple
(`arm-unknown-linux-musleabi`) and will not run either of these.

## Verifying an artifact

```bash
file target/armv7-unknown-linux-musleabihf/release/tiny-frpc
# ELF 32-bit LSB executable, ARM, EABI5 version 1 (SYSV), statically linked, stripped
```

On the board:

```bash
./tiny-frpc --v                 # tiny.0.1.0
./tiny-frpc -c /etc/tiny-frpc/frpc.toml
```

A missing private key is not fatal: the client logs a warning and connects
without client authentication, which the gateway accepts unless it configures
`authorizedKeysFile`. See doc/deployment.md.

## Notes for embedded targets

* The release profile is size-oriented (`opt-level = "z"`, fat LTO, `strip`),
  giving a ~2.6 MB static binary.
* `tiny-frpc` needs no external programs. `tiny-frpc-ssh` needs an `ssh` binary
  on the board, which most embedded rootfs images do not ship — prefer
  `tiny-frpc`.
* A single proxy costs roughly 8 MB of resident memory on 32-bit ARM; each open
  forwarded connection adds a little more. Cap them with
  `maxForwardConnections` if the board is tight.
* For an internal package proxy, set `HTTP_PROXY`/`HTTPS_PROXY` before
  installing targets, Zig, or crates.
