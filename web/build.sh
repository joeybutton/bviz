#!/usr/bin/env bash
# Builds the wasm package the example page loads into web/pkg.
# Needs rustup's wasm32-unknown-unknown target and wasm-pack
# (`rustup target add wasm32-unknown-unknown`, `cargo install wasm-pack`).
set -euo pipefail
cd "$(dirname "$0")/.."
wasm-pack build wasm --target web --release --out-dir ../web/pkg --no-typescript
rm -f web/pkg/.gitignore web/pkg/package.json
echo "Built web/pkg. Serve it with: python3 -m http.server -d web 8080"
