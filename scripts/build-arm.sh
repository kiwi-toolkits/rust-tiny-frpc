#!/usr/bin/env sh
#
# Cross-build for 32-bit ARM Linux (armv7), the usual target for embedded
# boards. Needs zig and cargo-zigbuild on PATH; zig supplies the C toolchain
# that aws-lc (pulled in by russh's default backend) otherwise expects to find
# in a cross-gcc package.
#
#   cargo install cargo-zigbuild
#   rustup target add armv7-unknown-linux-musleabihf
#   rustup target add armv7-unknown-linux-gnueabihf
#
# If zig and cargo-zigbuild live in a local .tools/ directory, put their
# directories first on PATH before running this script.
set -eu

TARGETS="armv7-unknown-linux-musleabihf armv7-unknown-linux-gnueabihf"

# Full LTO is fine natively but can stall an ARM cross-link for a very long
# time; release size optimisation stays on.
export CARGO_PROFILE_RELEASE_LTO=off
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16

PROJECT_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ -x "$PROJECT_ROOT/.tools/zig/zig" ]; then
    PATH="$PROJECT_ROOT/.tools/zig:$PROJECT_ROOT/.tools/cargo-zigbuild/bin:$PATH"
    export PATH
fi

command -v cargo-zigbuild >/dev/null || {
    echo "cargo-zigbuild is required: cargo install cargo-zigbuild" >&2
    exit 1
}
command -v zig >/dev/null || {
    echo "zig is required and must be on PATH" >&2
    exit 1
}

for target in $TARGETS; do
    rustup target add "$target"
done

cd "$PROJECT_ROOT"
for target in $TARGETS; do
    echo "==> $target"
    cargo zigbuild --release --target "$target" --bin tiny-frpc --bin tiny-frpc-ssh
done

echo
echo "artifacts:"
for target in $TARGETS; do
    ls -l "target/$target/release/tiny-frpc" "target/$target/release/tiny-frpc-ssh"
done
