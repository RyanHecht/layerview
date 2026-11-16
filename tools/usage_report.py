#!/usr/bin/env python3
"""Summarize layerview's revealed-preference counters.

    usage_report.py [--days N] [FILE]      (default: last 7 days of ~/.local/state/layerview/usage.json)

Shows, for the period: which keys produce each modifier, layer use (entries,
time, which key), tap vs hold for every dual-role key, and how mirrored keys
(left/right Space, Enter, Shift, Ctrl, Super...) split.
"""
import argparse, json, os, re
from collections import Counter, defaultdict
from datetime import date, timedelta

ap = argparse.ArgumentParser()
ap.add_argument("file", nargs="?", default=os.path.join(os.environ.get("XDG_STATE_HOME", os.path.expanduser("~/.local/state")), "layerview", "usage.json"))
ap.add_argument("--days", type=int, default=7)
a = ap.parse_args()
u = json.load(open(a.file))
since = (date.today() - timedelta(days=a.days - 1)).isoformat()
days = {d: v for d, v in u["days"].items() if d >= since}
keys, taps, holds, mods = Counter(), Counter(), Counter(), defaultdict(Counter)
layers = defaultdict(lambda: {"count": 0, "ms": 0, "via": Counter()})
for d in days.values():
    keys.update(d["keys"]); taps.update(d["taps"]); holds.update(d["holds"])
    for m, src in d["mods"].items(): mods[m].update(src)
    for l, v in d["layers"].items():
        layers[l]["count"] += v["count"]; layers[l]["ms"] += v["ms"]; layers[l]["via"].update(v["via"])
pct = lambda n, t: f"{100 * n / t:5.1f}%" if t else "   – "
print(f"{len(days)} day(s) since {since}: {sum(keys.values()):,} key presses")

print("\nModifiers: where they come from")
for m in ("shift", "ctrl", "alt", "super"):
    src = mods.get(m, Counter()); t = sum(src.values())
    if not t: continue
    print(f"  {m:6} {t:6,}  " + "  ".join(f"{k} {pct(n, t)}" for k, n in src.most_common(5)))

print("\nLayers")
for l, v in sorted(layers.items(), key=lambda x: -x[1]["count"]):
    via = ", ".join(f"{k} ×{n}" for k, n in v["via"].most_common(3))
    print(f"  {l:14} entered {v['count']:5,}×   {v['ms'] / 60000:6.1f} min   via {via}")

print("\nDual-role keys: tap vs hold")
for k in sorted(set(taps) | set(holds), key=lambda k: -(taps[k] + holds[k])):
    t, h = taps[k], holds[k]
    print(f"  {k:28} tap {t:6,}   hold {h:5,}   ({pct(h, t + h)} held)")

print("\nMirrored keys: left vs right")
pairs = defaultdict(dict)
for k, n in keys.items():
    m = re.match(r"(.*) \[([LR])\d+\]$", k)
    if m:
        base = m.group(1).split("/")[0]          # "Space/Cursor" and "Space/Symbol" are both Space
        pairs[base][m.group(2)] = pairs[base].get(m.group(2), 0) + n
for label, sides in sorted(pairs.items(), key=lambda x: -sum(x[1].values())):
    if len(sides) == 2:
        l, r = sides["L"], sides["R"]
        print(f"  {label:20} left {l:6,} ({pct(l, l + r)})   right {r:6,} ({pct(r, l + r)})")
