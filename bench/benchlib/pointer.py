"""One virtual pointer that lasts the whole run, through a minimal Wayland client in the
standard library (the wlr-virtual-pointer-unstable-v1 protocol, which headless sway offers).

Why not `wlrctl pointer ...` per action: every call creates a new pointer device and removes it
on exit. With no other pointer on the seat, the seat loses its pointer capability in between, so
the browser drops its wl_pointer each time and rebinds it when the next device appears; buttons
and wheel events sent before it has rebound are lost (only the entry position survives). One
device for the whole run keeps the capability, and needs no process per event.
"""

import os
import socket
import struct
import time

BTN_LEFT = 0x110
_WHEEL, _VERTICAL = 0, 0
# One mouse wheel notch, as libinput reports it: 15 degrees.
NOTCH = 15.0


class VirtualPointer:
    """Connects to the compositor in $XDG_RUNTIME_DIR/$WAYLAND_DISPLAY and creates a virtual
    pointer on its default seat. Every action is sent at once and ends with a frame."""

    def __init__(self):
        path = os.path.join(os.environ["XDG_RUNTIME_DIR"], os.environ["WAYLAND_DISPLAY"])
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self._next_id = 2  # 1 is wl_display
        registry = self._new_id()
        self._send(1, 1, _uint(registry))  # wl_display.get_registry
        globals_ = self._roundtrip(registry)
        name = next((n for n, interface in globals_.items() if interface == "zwlr_virtual_pointer_manager_v1"), None)
        if name is None:
            raise RuntimeError("the compositor has no zwlr_virtual_pointer_manager_v1")
        manager = self._new_id()
        # wl_registry.bind(name, interface, version, id)
        self._send(registry, 0, _uint(name) + _string("zwlr_virtual_pointer_manager_v1") + _uint(1) + _uint(manager))
        self.pointer = self._new_id()
        self._send(manager, 0, _uint(0) + _uint(self.pointer))  # create_virtual_pointer(default seat, id)
        self._roundtrip(registry)

    def move_to(self, x, y, width, height):
        """Puts the pointer at (x, y) of an output layout `width` × `height` big."""
        self._send(self.pointer, 1, b"".join(_uint(v) for v in (_now(), round(x), round(y), width, height)))
        self._frame()

    def click(self, button=BTN_LEFT):
        for state in (1, 0):
            self._send(self.pointer, 2, _uint(_now()) + _uint(button) + _uint(state))
            self._frame()

    def scroll(self, notches):
        """Turns the wheel by `notches` (positive: down) in one event."""
        self._send(self.pointer, 5, _uint(_WHEEL))  # axis_source
        # axis_discrete(time, axis, value, discrete)
        self._send(self.pointer, 7, _uint(_now()) + _uint(_VERTICAL) + _fixed(NOTCH * notches) + _int(notches))
        self._frame()

    def close(self):
        self._send(self.pointer, 8, b"")  # destroy
        self.sock.close()

    def _frame(self):
        self._send(self.pointer, 4, b"")

    def _new_id(self):
        self._next_id += 1
        return self._next_id - 1

    def _send(self, obj, opcode, args):
        self.sock.sendall(struct.pack("<II", obj, (8 + len(args)) << 16 | opcode) + args)

    def _roundtrip(self, registry):
        """wl_display.sync, reading events until it is done. Returns the registry's globals as
        {name: interface}; raises on a protocol error."""
        callback = self._new_id()
        self._send(1, 0, _uint(callback))
        globals_, buffer = {}, b""
        while True:
            while len(buffer) < 8 or len(buffer) < struct.unpack_from("<I", buffer, 4)[0] >> 16:
                chunk = self.sock.recv(65536)
                if not chunk:
                    raise RuntimeError("the compositor closed the connection")
                buffer += chunk
            obj, size_opcode = struct.unpack_from("<II", buffer)
            size, opcode = size_opcode >> 16, size_opcode & 0xFFFF
            body, buffer = buffer[8:size], buffer[size:]
            if obj == 1 and opcode == 0:  # wl_display.error(object, code, message)
                raise RuntimeError(f"Wayland protocol error: {_read_string(body, 8)!r}")
            if obj == registry and opcode == 0:  # wl_registry.global(name, interface, version)
                globals_[struct.unpack_from("<I", body)[0]] = _read_string(body, 4)
            if obj == callback and opcode == 0:  # wl_callback.done
                return globals_


def _now():
    return int(time.monotonic() * 1000) & 0xFFFFFFFF


def _uint(v):
    return struct.pack("<I", v)


def _int(v):
    return struct.pack("<i", v)


def _fixed(v):
    return struct.pack("<i", round(v * 256))


def _string(s):
    data = s.encode() + b"\0"
    return _uint(len(data)) + data + b"\0" * (-len(data) % 4)


def _read_string(body, offset):
    length = struct.unpack_from("<I", body, offset)[0]
    return body[offset + 4 : offset + 4 + length - 1].decode()
