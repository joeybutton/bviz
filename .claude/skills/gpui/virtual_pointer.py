"""A wlr-virtual-pointer client fed one command per line through a FIFO.

Usage: virtual_pointer.py <fifo> <output-width> <output-height>
                          <wlr-virtual-pointer-unstable-v1.xml>
Commands are those screenshot-input.sh documents for `pointer`, plus `quit`.
"""

import importlib
import os
import sys
import tempfile
import time

import pywayland.protocol.wayland as core
from pywayland.client import Display
from pywayland.protocol.wayland import WlSeat
from pywayland.protocol_core import Interface
from pywayland.scanner import Protocol

# Linux input event codes (linux/input-event-codes.h).
BUTTONS = {"left": 0x110, "right": 0x111}
PRESSED, RELEASED = 1, 0
VERTICAL, HORIZONTAL = 0, 1
WHEEL, FINGER = 0, 1
# Long enough apart for the compositor to see a press and a release, and close
# enough together for the app to count two clicks as a double click.
CLICK_HOLD_S = 0.05
DOUBLE_CLICK_GAP_S = 0.08


def now_ms():
    return int(time.monotonic() * 1000) & 0xFFFFFFFF


def generate_bindings(xml_file):
    """Generates Python bindings for the protocol `xml_file` defines and
    imports them, resolving the core interfaces it names to pywayland's own."""
    package = "protocols"
    root = tempfile.mkdtemp()
    os.mkdir(f"{root}/{package}")
    protocol = Protocol.parse_file(xml_file)
    owners = {
        interface.name: "wayland"
        for interface in vars(core).values()
        if isinstance(interface, type) and issubclass(interface, Interface)
    }
    owners.update((interface.name, protocol.name) for interface in protocol.interface)
    protocol.output(f"{root}/{package}", owners)
    with open(f"{root}/{package}/__init__.py", "w"):
        pass
    with open(f"{root}/{package}/wayland.py", "w") as shim:
        shim.write("from pywayland.protocol.wayland import *\n")
    sys.path.insert(0, root)
    return importlib.import_module(f"{package}.{protocol.name}")


def connect(ZwlrVirtualPointerManagerV1):
    display = Display()
    display.connect()
    found = {}

    def on_global(registry, name, interface, _version):
        if interface == "wl_seat":
            found["seat"] = registry.bind(name, WlSeat, 1)
        elif interface == "zwlr_virtual_pointer_manager_v1":
            found["manager"] = registry.bind(name, ZwlrVirtualPointerManagerV1, 1)

    registry = display.get_registry()
    registry.dispatcher["global"] = on_global
    display.roundtrip()
    pointer = found["manager"].create_virtual_pointer(found["seat"])
    display.roundtrip()
    return display, pointer


def main():
    fifo, width, height = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
    display, pointer = connect(generate_bindings(sys.argv[4]).ZwlrVirtualPointerManagerV1)

    def button(code, state):
        pointer.button(now_ms(), code, state)
        pointer.frame()
        display.flush()

    def click(code):
        button(code, PRESSED)
        time.sleep(CLICK_HOLD_S)
        button(code, RELEASED)

    while True:
        with open(fifo) as commands:
            for line in commands:
                command, *args = line.split() or [""]
                if command == "quit":
                    return
                if command == "abs":
                    x, y = int(args[0]), int(args[1])
                    pointer.motion_absolute(now_ms(), x, y, width, height)
                    pointer.frame()
                elif command in ("wheel", "hwheel"):
                    axis = VERTICAL if command == "wheel" else HORIZONTAL
                    steps = float(args[0])
                    pointer.axis_source(WHEEL)
                    pointer.axis_discrete(now_ms(), axis, steps, 1 if steps > 0 else -1)
                    pointer.frame()
                elif command == "swipe":
                    dx, dy = float(args[0]), float(args[1])
                    pointer.axis_source(FINGER)
                    pointer.axis(now_ms(), VERTICAL, dy)
                    pointer.axis(now_ms(), HORIZONTAL, dx)
                    pointer.frame()
                elif command == "press":
                    button(BUTTONS[args[0]], PRESSED)
                elif command == "release":
                    button(BUTTONS[args[0]], RELEASED)
                elif command == "click":
                    click(BUTTONS[args[0]])
                elif command == "dclick":
                    click(BUTTONS[args[0]])
                    time.sleep(DOUBLE_CLICK_GAP_S)
                    click(BUTTONS[args[0]])
                display.flush()


if __name__ == "__main__":
    main()
