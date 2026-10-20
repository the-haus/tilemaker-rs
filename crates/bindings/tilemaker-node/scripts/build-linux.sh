#!/bin/sh
# Build and test the Linux binary for one napi target inside a Node image of
# the runner's own architecture, so LuaJIT builds natively (no cross
# compiler) and the binary runs where it was built:
#
#   docker run --rm -v "$PWD:/w" -w /w node:22-bullseye \
#     sh crates/bindings/tilemaker-node/scripts/build-linux.sh aarch64-unknown-linux-gnu
#
# Debian bullseye sets the glibc floor at 2.31; node:22-alpine builds musl.
set -eu
TARGET="$1"

if [ -f /etc/alpine-release ]; then
  apk add --no-cache build-base curl bash perl
fi
curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
# shellcheck source=/dev/null
. "$HOME/.cargo/env"

corepack enable
COREPACK_ENABLE_DOWNLOAD_PROMPT=0 yarn install --immutable
cd crates/bindings/tilemaker-node
yarn exec napi build --platform --release --strip --js false --target "$TARGET"
yarn run build:ts
yarn run test
