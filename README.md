# layerview

Tools for ZMK keyboards, built as separate pieces so you can take only what you need:

| Piece | What it does | Works with |
|---|---|---|
| [`zmk-uf2-flash`](crates/zmk-uf2-flash) | Downloads your latest GitHub build and flashes it: waits for each UF2 bootloader drive, copies the right file, skips halves that didn't change | Any UF2-flashed ZMK keyboard, split or not (Linux) |
| [`zmk-release.yml`](.github/workflows/zmk-release.yml) | Reusable GitHub workflow: builds firmware, exports a display file, publishes a Release with a `flash.json` manifest | Any ZMK config repo |
| [`layerview-export`](export) | Turns a `.keymap` into a display file (physical layout + every layer's legends) using [keymap-drawer](https://github.com/caksoylar/keymap-drawer) | Any keyboard keymap-drawer knows |
| [`layerview`](crates/layerview) | Live display: shows the active layer and pressed keys in any browser | ZMK keyboards running the [KeyPeek layer notifier](https://github.com/srwi/zmk-keypeek-layer-notifier) |
| [`android/`](android) | Turns a spare Android phone, tethered by USB, into a permanent layer display | Any Android with USB debugging |

```
 zmk-config repo ── zmk-release.yml ──► GitHub Release
                     (build + export)     ├─ left.uf2, right.uf2
                                          ├─ layerview.json  (display file)
                                          └─ flash.json      (manifest)
                                                │
                              zmk-uf2-flash ◄───┘  copies UF2s, registers display file
                                                │
 keyboard ── Raw HID (layers, key presses) ──► layerview ──SSE──► browser / phone
```

The pieces only meet through two documented files ([docs/formats.md](docs/formats.md)): the **display file** and the **flash manifest**. Use the flasher without any display, use the display with your own build pipeline, or use the exporter on its own.

## Firmware requirements for the live display

`layerview` reads reports that the KeyPeek layer notifier sends over Raw HID: active layers and key presses by position. It never sends anything to the keyboard, so it also works through KVMs that only pass keyboard-to-computer traffic. Add to your zmk-config:

- `west.yml`: projects `zmk-raw-hid` (zzeneg) and `zmk-keypeek-layer-notifier` (srwi)
- `build.yaml`: `shield: raw_hid_adapter` on the central half
- `.conf`: `CONFIG_RAW_HID=y`

ZMK Studio is **not** needed: the keymap comes from your build, via the display file.

## Quick start

```sh
cargo install --path crates/zmk-uf2-flash
cargo install --path crates/layerview

zmk-uf2-flash --repo YOU/zmk-config     # flash the latest build, register its keymap
layerview                               # serve http://127.0.0.1:8740
```

`layerview` binds to localhost by default because its stream reveals every key you press. To use a phone, tether it over USB with `adb reverse` (see [android/](android)) rather than exposing it on your network.

## License

MIT. The firmware modules and keymap-drawer are separate projects under their own licenses.
