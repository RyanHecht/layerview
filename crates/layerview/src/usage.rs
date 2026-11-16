//! Revealed-preference counters: which keys, layers and modifier sources you
//! actually use. Stores daily totals only (no keystroke order, no text) in
//! `$XDG_STATE_HOME/layerview/usage.json`.

use layerview_format::{state_dir, write_atomic, DisplayFile};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

pub fn usage_path() -> PathBuf {
    state_dir().join("layerview").join("usage.json")
}

#[derive(Default, Serialize, Deserialize)]
pub struct Usage {
    pub days: BTreeMap<String, Day>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct Day {
    /// Keymap commit(s) in use that day.
    pub keymaps: Vec<String>,
    /// Presses per key ("Space/Cursor [L69]").
    pub keys: BTreeMap<String, u64>,
    /// Dual-role keys: released without their hold action firing.
    pub taps: BTreeMap<String, u64>,
    /// Dual-role keys: their hold action (layer or modifier) fired.
    pub holds: BTreeMap<String, u64>,
    pub layers: BTreeMap<String, LayerUse>,
    /// modifier ("shift") -> source key -> activations
    pub mods: BTreeMap<String, BTreeMap<String, u64>>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct LayerUse {
    pub count: u64,
    pub ms: u64,
    /// Key that turned the layer on -> times.
    pub via: BTreeMap<String, u64>,
}

#[derive(Default)]
pub struct Tracker {
    usage: Usage,
    loaded: bool,
    down: HashSet<usize>,
    hold_fired: HashSet<usize>,
    layer_since: HashMap<usize, Instant>,
    pub dirty: bool,
}

const MODS: [(u8, &str, &str); 4] = [(0x22, "shift", "Shift"), (0x11, "ctrl", "Ctrl"), (0x44, "alt", "Alt"), (0x88, "super", "Super")];

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn key_name(d: &DisplayFile, pos: usize) -> String {
    let Some(k) = d.keys.iter().find(|k| k.pos == pos) else { return format!("[{pos}]") };
    let l = k.legends.first().cloned().unwrap_or_default();
    let xs: Vec<f32> = d.keys.iter().map(|k| k.x + k.w / 2.0).collect();
    let mid = (xs.iter().cloned().fold(f32::MAX, f32::min) + xs.iter().cloned().fold(f32::MIN, f32::max)) / 2.0;
    let side = if k.x + k.w / 2.0 < mid { "L" } else { "R" };
    let label = if l.h.is_empty() { l.t.clone() } else { format!("{}/{}", l.t, l.h) };
    format!("{} [{side}{pos}]", if label.is_empty() { "?".into() } else { label })
}

fn is_dual_role(d: &DisplayFile, pos: usize) -> bool {
    d.keys.iter().find(|k| k.pos == pos).and_then(|k| k.legends.first()).map(|l| !l.h.is_empty()).unwrap_or(false)
}

impl Tracker {
    fn day(&mut self, d: &DisplayFile) -> &mut Day {
        if !self.loaded {
            self.loaded = true;
            if let Some(u) = std::fs::read(usage_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()) {
                self.usage = u;
            }
        }
        let day = self.usage.days.entry(today()).or_default();
        if !d.commit.is_empty() && !day.keymaps.contains(&d.commit) {
            day.keymaps.push(d.commit.clone());
        }
        day
    }

    /// Record the edges between two live snapshots.
    pub fn observe(&mut self, d: &DisplayFile, old: (u32, &[usize], u8), new: (u32, &[usize], u8)) {
        let (ol, op, om) = old;
        let (nl, np, nm) = new;
        let now = Instant::now();
        let mut any = false;

        for &p in np.iter().filter(|p| !op.contains(p)) {
            let name = key_name(d, p);
            *self.day(d).keys.entry(name).or_default() += 1;
            self.down.insert(p);
            any = true;
        }
        // Layers turning on: count, and credit the held key that activates them.
        for l in 1..32usize {
            let (was, is) = (ol >> l & 1 == 1, nl >> l & 1 == 1);
            if is && !was {
                let lname = d.layers.get(l).cloned().unwrap_or_else(|| format!("layer {l}"));
                let via = self.down.iter().copied().find(|&p| {
                    d.keys.iter().find(|k| k.pos == p).map_or(false, |k| {
                        k.legends.iter().any(|lg| lg.h == lname || lg.t == lname)
                    })
                });
                if let Some(p) = via {
                    if is_dual_role(d, p) && self.hold_fired.insert(p) {
                        *self.day(d).holds.entry(key_name(d, p)).or_default() += 1;
                    }
                }
                let via_name = via.map(|p| key_name(d, p)).unwrap_or_else(|| "toggle/other".into());
                let lu = self.day(d).layers.entry(lname).or_default();
                lu.count += 1;
                *lu.via.entry(via_name).or_default() += 1;
                self.layer_since.insert(l, now);
                any = true;
            } else if was && !is {
                if let Some(t) = self.layer_since.remove(&l) {
                    let lname = d.layers.get(l).cloned().unwrap_or_else(|| format!("layer {l}"));
                    self.day(d).layers.entry(lname).or_default().ms += t.elapsed().as_millis() as u64;
                    any = true;
                }
            }
        }
        // Modifiers turning on: credit a held plain modifier key, else a held home-row mod.
        for (bit, kind, label) in MODS {
            if nm & bit != 0 && om & bit == 0 {
                let find = |want_t: bool| {
                    self.down.iter().copied().find(|&p| {
                        d.keys.iter().find(|k| k.pos == p).and_then(|k| k.legends.first()).map_or(false, |lg| {
                            if want_t { lg.t == label } else { lg.h == label }
                        })
                    })
                };
                let (src, hrm) = match find(true) { Some(p) => (Some(p), false), None => (find(false), true) };
                if let (Some(p), true) = (src, hrm) {
                    if self.hold_fired.insert(p) {
                        *self.day(d).holds.entry(key_name(d, p)).or_default() += 1;
                    }
                }
                let src_name = src.map(|p| key_name(d, p)).unwrap_or_else(|| "other".into());
                *self.day(d).mods.entry(kind.into()).or_default().entry(src_name).or_default() += 1;
                any = true;
            }
        }
        for &p in op.iter().filter(|p| !np.contains(p)) {
            if is_dual_role(d, p) && !self.hold_fired.contains(&p) {
                *self.day(d).taps.entry(key_name(d, p)).or_default() += 1;
                any = true;
            }
            self.down.remove(&p);
            self.hold_fired.remove(&p);
        }
        if any {
            self.dirty = true;
        }
    }

    pub fn save(&mut self) {
        if self.dirty {
            if let Ok(b) = serde_json::to_vec_pretty(&self.usage) {
                let _ = write_atomic(&usage_path(), &b);
            }
            self.dirty = false;
        }
    }
}
