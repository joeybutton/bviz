#!/usr/bin/env bash
# Builds the example page with Bazel, using the repository's pinned Rust
# toolchain (with its wasm32-unknown-unknown standard library) and the
# wasm-bindgen CLI multitool pins, so nothing beyond Bazel(isk) is needed on
# the host. Copies the wasm package to web/pkg so the page and the Node tests
# also work straight from the source tree.
#
#   web/build.sh                          # build, then copy to web/pkg
#   bazel run --config=wasm //web:serve   # build and serve on :8080
set -euo pipefail
cd "$(dirname "$0")/.."
bazel build --config=wasm //web:site
site="$(bazel cquery --config=wasm --output=files //web:site 2>/dev/null)"
rm -rf web/pkg
mkdir -p web/pkg
cp -L "$site"/pkg/bviz_wasm.js "$site"/pkg/bviz_wasm_bg.wasm web/pkg/
chmod u+w web/pkg/*
echo "Built $site and copied its package to web/pkg."
echo "Serve it with: bazel run --config=wasm //web:serve"
