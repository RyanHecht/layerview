#!/usr/bin/env python3
"""Export a ZMK keymap as a layerview display file.

Uses keymap-drawer to parse the keymap (layers, legends, transparency) and to
generate the keyboard's physical layout, then writes the JSON the layerview
bridge renders on the phone. Key order = ZMK key positions, which is what the
KeyPeek layer notifier reports for key presses.

    layerview-export config/glove80.keymap layerview.json --zmk-keyboard glove80
"""
import argparse
import json
import re
import sys
import time

from keymap_drawer.config import Config
from keymap_drawer.parse import ZmkKeymapParser
from keymap_drawer.physical_layout import PhysicalLayoutGenerator

# Friendlier legends for raw ZMK names that keymap-drawer passes through.
PRETTY = {
    "LSHFT": "Shift", "RSHFT": "Shift", "LCTRL": "Ctrl", "RCTRL": "Ctrl", "LALT": "Alt", "RALT": "AltGr",
    "LGUI": "Super", "RGUI": "Super", "BSPC": "Bksp", "RET": "Enter", "ENTER": "Enter", "SPACE": "Space",
    "ESC": "Esc", "TAB": "Tab", "DEL": "Del", "INS": "Ins", "PG UP": "PgUp", "PG DN": "PgDn",
    "PSCRN": "PrtSc", "PRINTSCREEN": "PrtSc", "LEFT": "←", "RIGHT": "→", "UP": "↑", "DOWN": "↓",
    "LEFT ARROW": "←", "RIGHT ARROW": "→", "UP ARROW": "↑", "DOWN ARROW": "↓", "HOME": "Home", "END": "End",
    "KP NUM": "NumLk", "KP EQUAL": "=", "KP SLASH": "/", "KP MULTIPLY": "*", "KP MINUS": "-", "KP PLUS": "+",
    "KP DOT": ".", "KP ENTER": "Enter", "SCROLLLOCK": "ScrLk", "PAUSE BREAK": "Pause", "CAPS": "Caps",
    "K APP": "Menu", "BRI DN": "Bri−", "BRI UP": "Bri+", "PP": "Play", "PREV": "Prev", "NEXT": "Next",
    "MUTE": "Mute", "VOL DN": "Vol−", "VOL UP": "Vol+", "&lower": "Lower",
    "&rgb_ug_status_macro": "RGB", "&bootloader": "Boot", "&sys_reset": "Reset",
}
SHIFT_LEGENDS = {"Shift"}
SHIFTED = dict(zip(r"`1234567890-=[]\;',./", "~!@#$%^&*()_+{}|:\"<>?"))


def pretty(s: str) -> str:
    s = s.strip()
    if s in PRETTY:
        return PRETTY[s]
    if m := re.fullmatch(r"KP (\d)", s):
        return m.group(1)
    if m := re.fullmatch(r"Sft\+(.)", s):
        return SHIFTED.get(m.group(1), s)
    return s


def norm(binding) -> dict:
    if isinstance(binding, str):
        binding = {"t": binding}
    t = pretty(str(binding.get("t", "")))
    h = pretty(str(binding.get("h", "")))
    s = str(binding.get("s", ""))
    return {"t": t, "h": h, "s": s, "type": binding.get("type", "")}


def flatten(rows):
    out = []
    for r in rows:
        out.extend(r if isinstance(r, list) else [r])
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("keymap")
    ap.add_argument("out")
    ap.add_argument("--zmk-keyboard", default="glove80")
    ap.add_argument("--name", default="Glove80")
    ap.add_argument("--commit", default="")
    a = ap.parse_args()

    cfg = Config()
    with open(a.keymap, encoding="utf-8") as f:
        parsed = ZmkKeymapParser(cfg.parse_config, 0).parse(f)
    layer_names = list(parsed["layers"].keys())
    layers = [[norm(b) for b in flatten(rows)] for rows in parsed["layers"].values()]

    phys = PhysicalLayoutGenerator(config=cfg, zmk_keyboard=a.zmk_keyboard).generate()
    unit = cfg.draw_config.key_h
    if any(len(l) != len(phys.keys) for l in layers):
        print(f"layer sizes {[len(l) for l in layers]} != physical keys {len(phys.keys)}", file=sys.stderr)
        return 1

    keys = []
    for pos, pk in enumerate(phys.keys):
        w, h = pk.width / unit, pk.height / unit
        legends = [layer[pos] for layer in layers]
        keys.append({
            "x": round(pk.pos.x / unit - w / 2, 4), "y": round(pk.pos.y / unit - h / 2, 4),
            "w": round(w, 4), "h": round(h, 4), "r": pk.rotation, "pos": pos,
            "legends": [{k: v for k, v in l.items() if k in ("t", "h", "s") and v} for l in legends],
            "trans": [l["type"] == "trans" for l in legends],
            "shift": [l["t"] in SHIFT_LEGENDS for l in legends],
        })

    out = {"format": 1, "keyboard": a.name, "commit": a.commit, "built_unix": int(time.time()),
           "layers": layer_names, "keys": keys}
    with open(a.out, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, separators=(",", ":"))
    print(f"wrote {a.out}: {len(layer_names)} layers, {len(keys)} keys")
    return 0


if __name__ == "__main__":
    sys.exit(main())
