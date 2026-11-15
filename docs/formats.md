# File formats

## Display file (`layerview.json`)

Produced by `layerview-export`, read by `layerview`. Registered at
`$XDG_STATE_HOME/layerview/display.json` (default `~/.local/state/layerview/display.json`).

```jsonc
{
  "format": 1,
  "keyboard": "Glove80",
  "commit": "3f2c1a9…",          // build source, optional
  "built_unix": 1794779082,
  "layers": ["Base", "Lower", "Magic"],
  "keys": [                      // ordered by ZMK key position
    {
      "x": 0, "y": 0.5, "w": 1, "h": 1,   // top-left + size, key units
      "r": 0,                    // clockwise degrees around the key center
      "pos": 0,                  // ZMK key position
      "legends": [{"t": "F1"}, {"t": "Bri−"}, {"t": "BT CLR"}],  // per layer: t=tap, h=hold, s=shifted
      "trans": [false, false, false],     // per layer: transparent (falls through)
      "shift": [false, false, false]      // per layer: binding is Shift
    }
  ]
}
```

## Flash manifest (`flash.json`)

Produced by `make_manifest.py` (in `zmk-release.yml`), read by `zmk-uf2-flash`.

```jsonc
{
  "format": 1,
  "keyboard": "Glove80",
  "commit": "3f2c1a9…",
  "message": "Swap brackets to symbol layer",
  "built_unix": 1794779082,
  "targets": [
    {
      "name": "left",
      "file": "left.uf2",
      "sha256": "1a0e…",
      "volume_label": "GLV80LHBOOT",   // label of the UF2 bootloader drive
      "hint": "hold Lower + Magic, tap Tab",
      "central": true                  // holds the keymap; display file registered after it
    }
  ],
  "display": "layerview.json"          // optional
}
```

## Live reports (KeyPeek layer notifier, Raw HID usage page 0xFF60, 32-byte reports)

| First byte | Layout | Meaning |
|---|---|---|
| `0xFF` | `FF len default_layers:u32le active_layers:u32le` | layer state changed |
| `0xF1` | `F1 0 position pressed` | key at ZMK position pressed (1) / released (0) |
