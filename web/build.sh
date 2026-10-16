#!/usr/bin/env bash
# Builds bviz for the browser with Bazel: the same //bviz:bviz target as the
# desktop app, compiled for wasm32-unknown-unknown by the repository's pinned
# Rust toolchain and post-processed by the wasm-bindgen CLI multitool pins, so
# nothing beyond Bazel(isk) is needed on the host.
#
#   web/build.sh                          # build //web:site and print its path
#   bazel run --config=wasm //web:serve   # build and serve on :8080
set -euo pipefail
cd "$(dirname "$0")/.."
bazel build --config=wasm //web:site
echo "Built $(bazel cquery --config=wasm --output=files //web:site 2>/dev/null)"
echo "Serve it with: bazel run --config=wasm //web:serve"
