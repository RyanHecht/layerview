//! File formats shared across layerview components. See `docs/formats.md`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A keyboard's physical layout and every layer's legends, as produced by
/// `layerview-export`. Keys are ordered by ZMK key position.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DisplayFile {
    pub format: u32,
    pub keyboard: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub built_unix: u64,
    pub layers: Vec<String>,
    pub keys: Vec<DisplayKey>,
    /// Key combos (several keys pressed together).
    #[serde(default)]
    pub combos: Vec<Combo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Combo {
    /// ZMK key positions pressed together.
    pub keys: Vec<usize>,
    pub legend: Legend,
    /// Layer indices where the combo is active.
    #[serde(default)]
    pub layers: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DisplayKey {
    /// Top-left corner and size in key units; `r` = clockwise degrees around the key center.
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    #[serde(default)]
    pub r: f32,
    /// ZMK key position (matches KeyPeek 0xF1 key reports).
    pub pos: usize,
    /// One legend per layer.
    pub legends: Vec<Legend>,
    /// Per layer: binding falls through to lower layers.
    pub trans: Vec<bool>,
    /// Per layer: binding is a Shift key.
    #[serde(default)]
    pub shift: Vec<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Legend {
    /// Tap label.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub t: String,
    /// Hold label (hold-taps, layer-taps).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub h: String,
    /// Shifted label.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub s: String,
}

/// Published next to firmware files (`flash.json` in a release): what to copy
/// to which UF2 bootloader drive, and which display file belongs to the build.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlashManifest {
    pub format: u32,
    #[serde(default)]
    pub keyboard: String,
    #[serde(default)]
    pub commit: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub built_unix: u64,
    pub targets: Vec<FlashTarget>,
    /// File name of the layerview display file in the same release, if any.
    #[serde(default)]
    pub display: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlashTarget {
    /// Human name, e.g. "left".
    pub name: String,
    /// UF2 file name in the release.
    pub file: String,
    pub sha256: String,
    /// Filesystem label of the bootloader drive, e.g. "GLV80LHBOOT".
    pub volume_label: String,
    /// How to enter the bootloader, shown to the user.
    #[serde(default)]
    pub hint: String,
    /// The half that holds the keymap (ZMK central). The display file is
    /// registered once this target is flashed.
    #[serde(default)]
    pub central: bool,
}

/// Where layerview reads, and zmk-uf2-flash registers, the current display file:
/// `$XDG_STATE_HOME/layerview/display.json` (default `~/.local/state/...`).
pub fn default_display_path() -> PathBuf {
    state_dir().join("layerview").join("display.json")
}

/// Optional alert shown on the display (e.g. by the phone battery watch):
/// `{"message": "..."}`; no file = no alert.
pub fn default_alerts_path() -> PathBuf {
    state_dir().join("layerview").join("alerts.json")
}

pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
}

pub fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".cache"))
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// Writes `bytes` to `path` atomically (temp file + rename), creating parent dirs.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}
