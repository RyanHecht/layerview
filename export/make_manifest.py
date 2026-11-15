#!/usr/bin/env python3
"""Assemble a release folder: copy each target's UF2 under a stable name and
write flash.json (see docs/formats.md). Stdlib only.

    make_manifest.py FIRMWARE_DIR RELEASE_DIR --targets JSON [--display layerview.json]
                     [--keyboard NAME] [--commit SHA] [--message TEXT]

--targets is a JSON list of objects:
  {"name": "left", "file_glob": "*glove80_lh*.uf2", "volume_label": "GLV80LHBOOT",
   "hint": "how to enter the bootloader", "central": true}
"""
import argparse, glob, hashlib, json, os, shutil, sys, time


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("firmware_dir")
    ap.add_argument("release_dir")
    ap.add_argument("--targets", required=True)
    ap.add_argument("--display", default="")
    ap.add_argument("--keyboard", default="")
    ap.add_argument("--commit", default="")
    ap.add_argument("--message", default="")
    a = ap.parse_args()

    os.makedirs(a.release_dir, exist_ok=True)
    targets = []
    for t in json.loads(a.targets):
        matches = sorted(glob.glob(os.path.join(a.firmware_dir, "**", t["file_glob"]), recursive=True))
        if len(matches) != 1:
            print(f"target {t['name']}: expected 1 file for {t['file_glob']}, found {matches}", file=sys.stderr)
            return 1
        out_name = f"{t['name']}.uf2"
        shutil.copyfile(matches[0], os.path.join(a.release_dir, out_name))
        with open(matches[0], "rb") as f:
            digest = hashlib.sha256(f.read()).hexdigest()
        targets.append({
            "name": t["name"], "file": out_name, "sha256": digest,
            "volume_label": t["volume_label"], "hint": t.get("hint", ""), "central": bool(t.get("central")),
        })

    if a.display and not os.path.exists(os.path.join(a.release_dir, a.display)):
        print(f"display file {a.display} not found in {a.release_dir}", file=sys.stderr)
        return 1
    manifest = {"format": 1, "keyboard": a.keyboard, "commit": a.commit, "message": a.message,
                "built_unix": int(time.time()), "targets": targets, "display": a.display or None}
    with open(os.path.join(a.release_dir, "flash.json"), "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)
    print(json.dumps(manifest, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
