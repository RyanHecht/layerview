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
    "&rgb_ug_status_macro": "Magic", "Alt+Ctl+LSHFT": "Meh", "&caps_word": "Caps Word", "&bootloader": "Boot", "&sys_reset": "Reset",
}
SHIFT_LEGENDS = {"Shift"}
SHIFTED = dict(zip(r"`1234567890-=[]\;',./", "~!@#$%^&*()_+{}|:\"<>?"))


KEYCODE_CHARS = {
    "EQUAL": "=", "MINUS": "-", "BSLH": "\\", "SEMI": ";", "SQT": "'", "GRAVE": "`", "COMMA": ",",
    "DOT": ".", "FSLH": "/", "LBKT": "[", "RBKT": "]",
}
ARROWS = {"UP": "↑", "DOWN": "↓", "LEFT": "←", "RIGHT": "→"}
MOUSE_BUTTONS = {"LCLK": "L click", "RCLK": "R click", "MCLK": "M click", "MB4": "Back", "MB5": "Fwd"}


def pretty_behavior(s: str) -> str | None:
    """Readable labels for raw behavior bindings keymap-drawer passes through
    (`&name ARGS`), e.g. pointing keys and TailorKey/Glorious Engrammer macros."""
    m = re.fullmatch(r"&(\S+)\s*(.*)", s)
    if not m:
        return None
    name, arg = m.group(1), m.group(2).strip()
    if name == "mkp":
        return MOUSE_BUTTONS.get(arg, arg)
    if name == "mmv":
        return "Mouse " + ARROWS.get(arg.removeprefix("MOVE_"), arg)
    if name == "msc":
        return "Scroll " + ARROWS.get(arg.removeprefix("SCRL_"), arg)
    if m2 := re.fullmatch(r"bt_(\d)", name):
        return f"BT {m2.group(1)}"
    base = re.sub(r"_v\d+[A-Z]?(_[A-Z]+)?$", "", name)          # drop version/author suffixes
    if base == "AS" and arg:                                        # autoshift key: show the key
        if m3 := re.fullmatch(r"N(\d)", arg):
            return m3.group(1)
        return KEYCODE_CHARS.get(arg, arg)
    if base.startswith("cur_"):
        verb, _, what = base[4:].partition("_")
        return f"{'Sel' if verb == 'SELECT' else verb.title()[:3]} {what.lower()}".strip()
    if base == "mod_tab_chord" and arg:
        mod = {"LALT": "Alt", "LCTRL": "Ctrl", "LGUI": "Super", "RALT": "Alt", "RCTRL": "Ctrl", "RGUI": "Super"}
        return mod.get(arg.split()[0], arg.split()[0]) + "+Tab"
    if base == "caps_word":
        return "Caps Word"
    if base == "symb_dotdot":
        return "../"
    words = base.replace("_", " ").strip()
    return (words[:1].upper() + words[1:])[:12] if words else None


def pretty(s: str) -> str:
    s = s.strip()
    if s in PRETTY:
        return PRETTY[s]
    if s.startswith("&") and len(s) > 1 and (b := pretty_behavior(s)) is not None:
        return b
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
    ap.add_argument("--label", action="append", default=[], metavar="BINDING=LABEL",
                    help='name a raw binding, e.g. --label "&kp LG(LA(SPACE))=Dictate" (repeatable)')
    a = ap.parse_args()

    cfg = Config()
    for item in a.label:
        binding, sep, label = item.partition("=")
        if not sep:
            ap.error(f"--label needs BINDING=LABEL, got {item!r}")
        cfg.parse_config.raw_binding_map[binding.strip()] = label.strip()
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

    combos = []
    for c in parsed.get("combos", []):
        legend = {k: v for k, v in norm(c.get("k", "")).items() if k in ("t", "h", "s") and v}
        names = c.get("l") or layer_names
        combos.append({"keys": list(c.get("p", [])), "legend": legend,
                       "layers": [layer_names.index(n) for n in names if n in layer_names]})

    out = {"format": 1, "keyboard": a.name, "commit": a.commit, "built_unix": int(time.time()),
           "layers": layer_names, "keys": keys, "combos": combos}
    with open(a.out, "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, separators=(",", ":"))
    print(f"wrote {a.out}: {len(layer_names)} layers, {len(keys)} keys")
    return 0


if __name__ == "__main__":
    sys.exit(main())
