#!/usr/bin/env bash
set -euo pipefail

gen_rust_project=$(rlocation "$1")

cd "$BUILD_WORKSPACE_DIRECTORY"

targets=()
while IFS= read -r target; do
  targets+=("$target")
done < <(bazel query 'kind(".*rust.*", deps(//... except //tools/rust_analyzer:all))')

"$gen_rust_project" "${targets[@]}"

# Bazel plants an external repository's symlink under the execution root only
# while a locally executed action needs it, and prunes it once none does, so the
# crate source paths gen_rust_project writes resolve to nothing under remote
# execution. Name the repository checkouts those symlinks stand in for, which
# no build removes.
sed "s|$(bazel info execution_root)/external/|$(bazel info output_base)/external/|g" \
  rust-project.json >rust-project.json.tmp
mv rust-project.json.tmp rust-project.json
