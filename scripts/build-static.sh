#!/usr/bin/env bash
# Builds stomp as a static x86_64 Linux binary (musl). It has no shared
# library dependencies, so it runs on any x86_64 cluster whatever its glibc
# version. Copy the result to ~/bin on the cluster.
#
#   scripts/build-static.sh          build dist/stomp-<version>-x86_64-linux
#   scripts/build-static.sh --test   run the test suite against musl first
#
# Builds natively when musl-gcc and the rustup musl target are installed
# (Ubuntu: `sudo apt install musl-tools` and
# `rustup target add x86_64-unknown-linux-musl`); otherwise builds in Docker
# with the official Rust Alpine image (override with STOMP_BUILD_IMAGE).
set -euo pipefail

target=x86_64-unknown-linux-musl
image=${STOMP_BUILD_IMAGE:-rust:1.97-alpine}

test=false
case ${1:-} in
    "") ;;
    --test) test=true ;;
    *) echo "usage: $0 [--test]" >&2; exit 2 ;;
esac

cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)

build="cargo build --release --locked --target $target"
if $test; then
    build="cargo test --locked --target $target && $build"
fi

if command -v musl-gcc >/dev/null &&
    rustup target list --installed 2>/dev/null | grep -qx "$target"; then
    echo "Building with the local musl toolchain."
    sh -c "$build"
    bin=target/$target/release/stomp
elif command -v docker >/dev/null; then
    echo "Building in Docker ($image)."
    # Run as the calling user so the build output isn't owned by root. The
    # crate cache lives under target/ so later builds don't download again.
    docker run --rm --user "$(id -u):$(id -g)" \
        -v "$PWD:/src" -w /src \
        -e HOME=/tmp \
        -e CARGO_HOME=/src/target/alpine/cargo-home \
        -e CARGO_TARGET_DIR=/src/target/alpine \
        "$image" sh -c "$build"
    bin=target/alpine/$target/release/stomp
else
    echo "error: needs musl-gcc and the $target rustup target, or Docker" >&2
    exit 1
fi

# A dynamically linked result would defeat the point; refuse to ship it.
if command -v file >/dev/null && file "$bin" | grep -q "dynamically linked"; then
    echo "error: $bin is dynamically linked" >&2
    exit 1
fi

mkdir -p dist
out=dist/stomp-$version-x86_64-linux
cp "$bin" "$out"
(cd dist && sha256sum "${out#dist/}" > "${out#dist/}.sha256")

echo
echo "Built $out ($(du -h "$out" | cut -f1), $("$out" --version))"
echo "SHA-256 in $out.sha256. To install it on the cluster:"
echo
echo "  ssh <cluster> 'mkdir -p ~/bin'"
echo "  scp $out <cluster>:bin/stomp"
echo "  ssh <cluster> 'chmod +x ~/bin/stomp && ~/bin/stomp --version'"
