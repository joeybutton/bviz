#!/bin/sh
# Bazel workspace status command: names the commit a release is built from.
commit=${GITHUB_SHA:-$(git rev-parse HEAD 2>/dev/null)}
if [ -n "$commit" ]; then
    echo "STABLE_SOURCE_COMMIT $commit"
fi
