#!/usr/bin/env bash
# Drive a gpui app with pointer and keyboard input, screenshotting it as it goes.
#
# cage (screenshot.sh) delivers no injected input. Headless sway does: it takes
# a wlr-virtual-pointer client (virtual_pointer.py, fed commands through a FIFO)
# and wtype's virtual keyboard, and grim captures its output. sway, grim, wtype,
# a Python with pywayland and the protocol definition are fetched on demand
# from nixpkgs.
#
# Usage: screenshot-input.sh <steps-file> <app-binary> [args...]
#
# The steps file is sourced as the app starts, so it begins by sleeping through
# the app's load. It may call:
#   pointer abs X Y                  move to output pixel (X, Y)
#   pointer press|release|click|dclick left|right
#   pointer wheel N | hwheel N       turn a wheel N notches (vertical, horizontal)
#   pointer swipe DX DY              scroll as two fingers on a trackpad do
#   shot <out.png>                   capture the output
#   wtype ...                        type keys (see the gpui skill before using it)
#
# Env: SCREENSHOT_SIZE  output size as WIDTHxHEIGHT (default 1280x720)
set -euo pipefail

usage='usage: screenshot-input.sh <steps-file> <app-binary> [args...]'
steps=${1:?$usage}
: "${2:?$usage}"
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

if [ -z "${SCREENSHOT_INPUT_SHELL:-}" ]; then
    tools='let pkgs = (builtins.getFlake "nixpkgs").legacyPackages.${builtins.currentSystem}; in {
        inherit (pkgs) sway grim wtype wlr-protocols;
        python = pkgs.python3.withPackages (p: [ p.pywayland ]);
    }'
    nix=(nix --extra-experimental-features 'nix-command flakes')
    protocols=$("${nix[@]}" build --no-link --print-out-paths --impure --expr "$tools" wlr-protocols)
    VIRTUAL_POINTER_XML=$protocols/share/wlr-protocols/unstable/wlr-virtual-pointer-unstable-v1.xml
    export VIRTUAL_POINTER_XML SCREENSHOT_INPUT_SHELL=1
    exec "${nix[@]}" shell --impure --expr "$tools" sway grim wtype python \
        --command "$here/screenshot-input.sh" "$@"
fi
shift
size=${SCREENSHOT_SIZE:-1280x720}
# Longer than any run, so the held keyboard outlasts the steps.
KEYBOARD_HOLD_MS=3600000

export XDG_RUNTIME_DIR
XDG_RUNTIME_DIR=$(mktemp -d)
trap 'rm -rf "$XDG_RUNTIME_DIR"' EXIT

cat >"$XDG_RUNTIME_DIR/config" <<CONFIG
output HEADLESS-1 resolution $size
default_border none
seat seat0 hide_cursor 0
CONFIG
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
    sway -c "$XDG_RUNTIME_DIR/config" >"$XDG_RUNTIME_DIR/sway.log" 2>&1 &
sway=$!
SWAYSOCK=
for _ in $(seq 50); do
    SWAYSOCK=$(ls "$XDG_RUNTIME_DIR"/sway-ipc.*.sock 2>/dev/null || true)
    [ -n "$SWAYSOCK" ] && break
    sleep 0.2
done
export SWAYSOCK
WAYLAND_DISPLAY=$(ls "$XDG_RUNTIME_DIR" | grep -m1 '^wayland-[0-9]*$')
export WAYLAND_DISPLAY

fifo=$XDG_RUNTIME_DIR/pointer
mkfifo "$fifo"
python3 "$here/virtual_pointer.py" "$fifo" "${size%x*}" "${size#*x}" \
    "$VIRTUAL_POINTER_XML" >"$XDG_RUNTIME_DIR/pointer.log" 2>&1 &
pointer_pid=$!
sleep 1
if ! kill -0 "$pointer_pid" 2>/dev/null; then
    cat "$XDG_RUNTIME_DIR/pointer.log" >&2
    kill "$sway"
    exit 1
fi

# The seat has a keyboard only while a virtual one is attached, and the keys of
# a wtype run that attaches the only one never reach the app. One wtype held for
# the whole run keeps a keyboard on the seat, so each later run's keys arrive.
wtype -s "$KEYBOARD_HOLD_MS" &
keyboard=$!

"$@" >"$XDG_RUNTIME_DIR/app.log" 2>&1 &
app=$!

pointer() { echo "$*" >"$fifo"; }
shot() { grim "$1"; echo "wrote $1"; }
# shellcheck source=/dev/null
source "$steps"

pointer quit
kill "$app" "$keyboard" "$sway" 2>/dev/null || true
wait 2>/dev/null || true
