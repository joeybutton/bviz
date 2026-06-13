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
# Env:   SCREENSHOT_DELAY  seconds to wait for the first frame (default 5)
set -euo pipefail

out=${1:?usage: screenshot.sh <out.png> <app-binary> [args...]}
shift
delay=${SCREENSHOT_DELAY:-5}

# Inside cage, $@ is the app command; cage exports WAYLAND_DISPLAY for it and for
# grim. Capture after the app has drawn, then stop it so cage (and this run) exit.
inner='"$@" & app=$!; sleep '"$delay"'; grim "'"$out"'"; kill "$app" 2>/dev/null; wait "$app" 2>/dev/null || true'

WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 \
nix --extra-experimental-features 'nix-command flakes' \
    shell nixpkgs#cage nixpkgs#grim --command \
    cage -- bash -c "$inner" bash "$@"

echo "wrote $out"
