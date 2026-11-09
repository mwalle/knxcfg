#!/usr/bin/env python3
"""Drive the ETS Windows VM over QMP (see docs/sim-plan.md, "Operating ETS").

The VM must be started with
    -qmp unix:$HOME/.win7.qmp,server=on,wait=off
(another socket path can be given in $VM_QMP). Click coordinates are screen
pixels as in the screenshot; the VM needs -usbdevice tablet.

vm.py shot [file]          screenshot (PNG), prints size
vm.py click X Y [right|double]
vm.py move X Y
vm.py type TEXT
vm.py key COMBO...         e.g. ret, ctrl-a, alt-f4, meta_l (Ctrl+Z = ctrl-y)
"""
import json, os, socket, sys, tempfile, time
from PIL import Image

SOCK = os.environ.get("VM_QMP", os.path.expanduser("~/.win7.qmp"))
SHOT = os.path.join(tempfile.gettempdir(), "vm-shot.png")


class Qmp:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(SOCK)
        self.f = self.s.makefile("rw")
        self.f.readline()
        self.cmd("qmp_capabilities")

    def cmd(self, name, **args):
        self.f.write(json.dumps({"execute": name, "arguments": args}) + "\n")
        self.f.flush()
        while True:
            r = json.loads(self.f.readline())
            if "return" in r:
                return r["return"]
            if "error" in r:
                raise SystemExit(r["error"])


def screen_size(q):
    with tempfile.NamedTemporaryFile(suffix=".png") as f:
        q.cmd("screendump", filename=f.name, format="png")
        return Image.open(f.name).size


def move(q, x, y):
    w, h = screen_size(q)
    q.cmd("input-send-event", events=[
        {"type": "abs", "data": {"axis": "x", "value": int(x * 32767 / (w - 1))}},
        {"type": "abs", "data": {"axis": "y", "value": int(y * 32767 / (h - 1))}}])


def button(q, btn, down):
    q.cmd("input-send-event", events=[
        {"type": "btn", "data": {"down": down, "button": btn}}])


# The guest uses the German keyboard layout; qcodes name physical (US) keys.
# Ctrl+Z is therefore "key ctrl-y".
DE = {' ': ['spc'], '\n': ['ret'], '\t': ['tab'], '.': ['dot'], ',': ['comma'],
      '-': ['slash'], '_': ['shift', 'slash'], '/': ['shift', '7'],
      ':': ['shift', 'dot'], ';': ['shift', 'comma'], '=': ['shift', '0'],
      '+': ['bracket_right'], '*': ['shift', 'bracket_right'], '#': ['backslash'],
      "'": ['shift', 'backslash'], '"': ['shift', '2'], '!': ['shift', '1'],
      '$': ['shift', '4'], '%': ['shift', '5'], '&': ['shift', '6'],
      '(': ['shift', '8'], ')': ['shift', '9'], '?': ['shift', 'minus'],
      '<': ['less'], '>': ['shift', 'less'], '|': ['alt_r', 'less'],
      '\\': ['alt_r', 'minus'], '@': ['alt_r', 'q'], '{': ['alt_r', '7'],
      '}': ['alt_r', '0'], '[': ['alt_r', '8'], ']': ['alt_r', '9'],
      '~': ['alt_r', 'bracket_right']}
SWAP = {'y': 'z', 'z': 'y'}


def keys_for(c):
    if c.isascii() and c.isalpha():
        k = SWAP.get(c.lower(), c.lower())
        return ["shift", k] if c.isupper() else [k]
    if c.isdigit():
        return [c]
    if c in DE:
        return DE[c]
    raise SystemExit(f"no key for {c!r}")


def send_keys(q, names):
    q.cmd("send-key", keys=[{"type": "qcode", "data": n} for n in names])
    time.sleep(0.03)


def main():
    a = sys.argv[1:]
    if not a:
        raise SystemExit(__doc__)
    q = Qmp()
    if a[0] == "shot":
        out = os.path.abspath(a[1] if len(a) > 1 else SHOT)
        q.cmd("screendump", filename=out, format="png")
        print(out, Image.open(out).size)
    elif a[0] in ("click", "move"):
        move(q, int(a[1]), int(a[2]))
        if a[0] == "click":
            btn = "right" if "right" in a[3:] else "left"
            for _ in range(2 if "double" in a[3:] else 1):
                button(q, btn, True)
                time.sleep(0.05)
                button(q, btn, False)
                time.sleep(0.08)
    elif a[0] == "type":
        for c in " ".join(a[1:]):
            send_keys(q, keys_for(c))
    elif a[0] == "key":
        for combo in a[1:]:
            send_keys(q, combo.split("-"))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
