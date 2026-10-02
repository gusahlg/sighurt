"""A virtual pointer and a virtual keyboard that last the whole run, through a minimal Wayland
client in the standard library (wlr-virtual-pointer-unstable-v1 and
virtual-keyboard-unstable-v1, which headless sway offers).

Why not `wlrctl` and `wtype` per action: every call creates a new device and removes it when it
exits. The seat has no other pointer or keyboard, so it loses that capability between calls;
the browser drops its wl_pointer or wl_keyboard and binds a new one (loading the keymap again)
when the next device appears, and events sent before it has done so are lost. `wayland-info`
shows the seat's capabilities going from none to "keyboard" and back to none around one `wtype`
call, and the first baseline set lost ctrl+q in 7 of 84 runs that way. One device of each kind
for the whole run keeps both capabilities, and needs no process per event.

The keyboard's keymap is `us.xkb` (US layout, evdev rules, pc105; `xkbcli compile-keymap
--output-format 1 --rules evdev --model pc105 --layout us`, libxkbcommon 1.13.2), checked in so
every run and build gets the same one.
"""

import os
import pathlib
import socket
import struct
import time

BTN_LEFT = 0x110
_WHEEL, _VERTICAL = 0, 0
# One mouse wheel notch, as libinput reports it: 15 degrees.
NOTCH = 15.0

KEYMAP = pathlib.Path(__file__).resolve().parent / "us.xkb"
_XKB_V1 = 1
_PRESSED, _RELEASED = 1, 0

# Linux evdev key codes (linux/input-event-codes.h) of the keys the scripts press.
KEYS = {
    "escape": 1, "backspace": 14, "tab": 15, "return": 28, "space": 57,
    "ctrl": 29, "shift": 42, "alt": 56, "super": 125,
    "minus": 12, "equal": 13, "comma": 51, "period": 52, "slash": 53,
    **{str(d): 2 + (d - 1) % 10 for d in range(10)},
    **{c: code for row, first in (("qwertyuiop", 16), ("asdfghjkl", 30), ("zxcvbnm", 44))
       for code, c in enumerate(row, first)},
}
# The characters type() can type without a modifier, and their keys.
CHARACTERS = {" ": "space", "-": "minus", "=": "equal", ",": "comma", ".": "period", "/": "slash",
              **{c: c for c in "abcdefghijklmnopqrstuvwxyz0123456789"}}
# Bits of wl_keyboard.modifiers' masks: xkbcommon numbers the real modifiers Shift, Lock,
# Control, Mod1 (Alt), Mod2, Mod3, Mod4 (Super), Mod5 first, in that order.
MODIFIERS = {"shift": 1 << 0, "ctrl": 1 << 2, "alt": 1 << 3, "super": 1 << 6}


class Seat:
    """Connects to the compositor in $XDG_RUNTIME_DIR/$WAYLAND_DISPLAY and adds a virtual pointer
    and a virtual keyboard to its seat. Every action is sent at once; it has reached the
    compositor's socket when the method returns."""

    def __init__(self, keymap=KEYMAP):
        path = os.path.join(os.environ["XDG_RUNTIME_DIR"], os.environ["WAYLAND_DISPLAY"])
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self._next_id = 2  # 1 is wl_display
        registry = self._new_id()
        self._send(1, 1, _uint(registry))  # wl_display.get_registry
        globals_ = self._roundtrip(registry)

        def bind(interface, version):
            name = next((n for n, i in globals_.items() if i == interface), None)
            if name is None:
                raise RuntimeError(f"the compositor has no {interface}")
            new = self._new_id()
            # wl_registry.bind(name, interface, version, id)
            self._send(registry, 0, _uint(name) + _string(interface) + _uint(version) + _uint(new))
            return new

        pointers = bind("zwlr_virtual_pointer_manager_v1", 1)
        keyboards = bind("zwp_virtual_keyboard_manager_v1", 1)
        seat = bind("wl_seat", 1)
        self.pointer = self._new_id()
        self._send(pointers, 0, _uint(0) + _uint(self.pointer))  # create_virtual_pointer(default seat, id)
        self.keyboard = self._new_id()
        self._send(keyboards, 0, _uint(seat) + _uint(self.keyboard))  # create_virtual_keyboard(seat, id)
        # keymap(format, fd, size): the fd travels beside the message, as SCM_RIGHTS; the size
        # counts the terminating NUL.
        text = pathlib.Path(keymap).read_bytes() + b"\0"
        fd = os.memfd_create("keymap", os.MFD_CLOEXEC)
        try:
            os.write(fd, text)
            self._send(self.keyboard, 0, _uint(_XKB_V1) + _uint(len(text)), fds=[fd])
        finally:
            os.close(fd)
        self._roundtrip(registry)

    # ── Pointer ──

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

    # ── Keyboard ──

    def press(self, combo):
        """Presses and releases a key with modifiers held, like a person would: "ctrl+t",
        "escape", "ctrl+shift+tab". Names are those of KEYS."""
        *mods, key = combo.lower().split("+")
        mask = 0
        for mod in mods:
            self._key(KEYS[mod], _PRESSED)
            mask |= MODIFIERS[mod]
            self._modifiers(mask)
        self._key(KEYS[key], _PRESSED)
        self._key(KEYS[key], _RELEASED)
        for mod in reversed(mods):
            self._key(KEYS[mod], _RELEASED)
            mask &= ~MODIFIERS[mod]
            self._modifiers(mask)

    def type(self, text, interval):
        """Types `text` (lower-case letters, digits, space and - = , . /), one character every
        `interval` seconds."""
        start = time.monotonic()
        for i, c in enumerate(text):
            time.sleep(max(0.0, start + i * interval - time.monotonic()))
            self.press(CHARACTERS[c])

    def close(self):
        self._send(self.pointer, 8, b"")  # zwlr_virtual_pointer_v1.destroy
        self._send(self.keyboard, 3, b"")  # zwp_virtual_keyboard_v1.destroy
        self.sock.close()

    def _key(self, code, state):
        self._send(self.keyboard, 1, _uint(_now()) + _uint(code) + _uint(state))

    def _modifiers(self, depressed):
        # The compositor does not derive modifiers from a virtual keyboard's keys: it takes them
        # from this request (wlroots passes update_state = false for virtual keyboards).
        self._send(self.keyboard, 2, _uint(depressed) + _uint(0) + _uint(0) + _uint(0))

    # ── Wire format ──

    def _frame(self):
        self._send(self.pointer, 4, b"")

    def _new_id(self):
        self._next_id += 1
        return self._next_id - 1

    def _send(self, obj, opcode, args, fds=()):
        message = struct.pack("<II", obj, (8 + len(args)) << 16 | opcode) + args
        if fds:
            socket.send_fds(self.sock, [message], list(fds))
        else:
            self.sock.sendall(message)

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
