#!/usr/bin/env bash
# Screenshot a gpui app for visual verification.
#
# gpui windows here are Wayland surfaces under GNOME, whose own screenshot
# interfaces (the org.gnome.Shell.Screenshot D-Bus API and the XDG screenshot
# portal) refuse non-interactive callers. Instead, run the app inside a nested
# wlroots compositor (cage) and capture that compositor's output with grim,
# which speaks wlr-screencopy. cage and grim are fetched on demand from nixpkgs.
#
# cage runs on the wlroots headless backend so it renders to an offscreen output
# (still GPU-accelerated) rather than a nested window. grim captures that output
# regardless of whether the host session is locked, idle, or on another
# workspace — a nested window would not be presented (and grim would hang) while
# the GNOME screen is locked.
#
# Usage: screenshot.sh <out.png> <app-binary> [args...]
# Env:   SCREENSHOT_DELAY  seconds to wait before capturing (default 5); must
#                          cover the app's load and layout, not just its first frame
#        SCREENSHOT_ALLOW_FASTBUILD  capture an unoptimized binary anyway
set -euo pipefail

out=${1:?usage: screenshot.sh <out.png> <app-binary> [args...]}
shift
delay=${SCREENSHOT_DELAY:-5}

# An unoptimized binary loads and lays out roughly an order of magnitude slower,
# so the capture tends to land on a loading state. That reads as a successful
# capture, and because a spinner is drawn wherever its animation had reached, two
# runs of one binary differ — so a comparison reports a change that is only the
# animation.
case ${1:?usage: screenshot.sh <out.png> <app-binary> [args...]} in
*fastbuild*)
    if [ -z "${SCREENSHOT_ALLOW_FASTBUILD:-}" ]; then
        echo "screenshot.sh: $1 is an unoptimized build." >&2
        echo "  Build and resolve it with -c opt:" >&2
        echo "    bazel build -c opt //path:target" >&2
        echo "    bazel cquery -c opt --output=files //path:target" >&2
        echo "  Set SCREENSHOT_ALLOW_FASTBUILD=1 to capture it anyway." >&2
        exit 1
    fi
    ;;
esac

# Inside cage, $@ is the app command; cage exports WAYLAND_DISPLAY for it and for
# grim. Capture after the app has drawn, then stop it so cage (and this run) exit.
inner='"$@" & app=$!; sleep '"$delay"'; grim "'"$out"'"; kill "$app" 2>/dev/null; wait "$app" 2>/dev/null || true'

source "$(dirname "${BASH_SOURCE[0]}")/nixpkgs.sh"
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
nix --extra-experimental-features 'nix-command flakes' \
    shell "$nixpkgs#cage" "$nixpkgs#grim" --command \
    cage -- bash -c "$inner" bash "$@"

echo "wrote $out"
