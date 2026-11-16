//! layerview: live keymap display for ZMK keyboards.
//!
//! - **Live**: reads the keyboard's Raw HID interface (usage page 0xFF60) and
//!   decodes the KeyPeek layer notifier's reports: active layers (0xFF) and key
//!   presses by position (0xF1). Read-only, so it also works through KVMs that
//!   only forward device-to-host traffic.
//! - **Keymap**: a display file (see docs/formats.md), normally registered by
//!   `zmk-uf2-flash` when it flashes the matching firmware. Reloaded on change.
//! - **Serve**: one page at `/` plus a Server-Sent Events stream at `/events`.
//!
//! Nothing about your typing is logged or stored. Binds 127.0.0.1 by default
//! because the stream reveals every key you press.

mod usage;

use layerview_format::{default_alerts_path, default_display_path, DisplayFile};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

const PAGE: &str = include_str!("../../../web/index.html");
const RAW_HID_USAGE_PAGE: u16 = 0xff60;
const LAYER_PACKET: u8 = 0xFF;
const KEY_PACKET: u8 = 0xF1;

#[derive(Default, Clone)]
struct Live {
    connected: bool,
    layers: u32,
    pressed: Vec<usize>, // ZMK positions
    mods: u8,            // HID modifier byte as the computer sees it
}

#[derive(Default)]
struct Hub {
    clients: Mutex<Vec<Sender<String>>>,
    status: Mutex<Value>,
    display: Mutex<Option<DisplayFile>>,
    layout_json: Mutex<Option<Value>>,
    live: Mutex<Live>,
    last_state: Mutex<Option<Value>>,
    alert: Mutex<Value>,
    shortcuts: Mutex<Value>,
    usage: Mutex<usage::Tracker>,
    transition: Mutex<Value>,
    esc_pos: Mutex<Option<usize>>,
    track_usage: std::sync::atomic::AtomicBool,
}

impl Hub {
    fn sse(event: &str, data: &Value) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }

    fn broadcast(&self, event: &str, data: &Value) {
        let msg = Self::sse(event, data);
        self.clients.lock().unwrap().retain(|tx| tx.send(msg.clone()).is_ok());
    }

    fn set_status(&self, kind: &str, message: &str) {
        let v = json!({ "kind": kind, "message": message });
        let mut cur = self.status.lock().unwrap();
        if *cur != v {
            eprintln!("[layerview] {kind}: {message}");
            *cur = v.clone();
            drop(cur);
            self.broadcast("status", &v);
        }
    }

    fn refresh_status(&self) {
        let live = self.live.lock().unwrap().connected;
        let display = self.display.lock().unwrap().as_ref().map(|d| d.keyboard.clone());
        match (live, display) {
            (true, Some(name)) => self.set_status("live", &name),
            (true, None) => self.set_status(
                "nokeymap",
                "Keyboard connected, but no keymap registered yet. Flash with zmk-uf2-flash (or layerview --display FILE).",
            ),
            (false, Some(_)) => self.set_status("offline", "Keyboard not connected"),
            (false, None) => self.set_status("searching", "Looking for keyboard"),
        }
    }

    fn set_display(&self, d: DisplayFile) {
        let layout = json!({
            "name": d.keyboard,
            "commit": d.commit,
            "layers": d.layers,
            "combos": d.combos,
            "keys": d.keys.iter().map(|k| json!({
                "x": k.x, "y": k.y, "w": k.w, "h": k.h, "r": k.r, "pos": k.pos, "legends": k.legends
            })).collect::<Vec<_>>(),
        });
        *self.display.lock().unwrap() = Some(d);
        *self.layout_json.lock().unwrap() = Some(layout.clone());
        self.broadcast("layout", &layout);
        self.publish_state();
        self.refresh_status();
    }

    fn update_live(&self, f: impl FnOnce(&mut Live)) {
        let (old, new) = {
            let mut live = self.live.lock().unwrap();
            let old = live.clone();
            f(&mut live);
            (old, live.clone())
        };
        // Esc dismisses a handed-off keycap view.
        if let Some(esc) = *self.esc_pos.lock().unwrap() {
            if new.pressed.contains(&esc) && !old.pressed.contains(&esc) {
                finish_keycap_view();
            }
        }
        if self.track_usage.load(std::sync::atomic::Ordering::Relaxed) && new.connected {
            if let Some(d) = self.display.lock().unwrap().as_ref() {
                self.usage.lock().unwrap().observe(
                    d,
                    (old.layers, &old.pressed, old.mods),
                    (new.layers, &new.pressed, new.mods),
                );
            }
        }
        self.publish_state();
    }

    /// Works out what each key shows from the active layers and the keymap.
    fn publish_state(&self) {
        let live = self.live.lock().unwrap().clone();
        let guard = self.display.lock().unwrap();
        let Some(d) = guard.as_ref() else { return };
        let effective: Vec<usize> = d
            .keys
            .iter()
            .map(|k| {
                (0..d.layers.len())
                    .rev()
                    .find(|&l| live.layers >> l & 1 == 1 && !k.trans.get(l).copied().unwrap_or(false))
                    .unwrap_or(0)
            })
            .collect();
        let by_pos: HashMap<usize, usize> = d.keys.iter().enumerate().map(|(i, k)| (k.pos, i)).collect();
        let pressed: Vec<usize> = live.pressed.iter().filter_map(|p| by_pos.get(p).copied()).collect();
        let shift = pressed
            .iter()
            .any(|&i| d.keys[i].shift.get(effective[i]).copied().unwrap_or(false));
        let v = json!({ "layers": live.layers, "pressed": pressed, "effective": effective,
                        "shift": shift || live.mods & 0x22 != 0, "mods": live.mods });
        drop(guard);
        let mut last = self.last_state.lock().unwrap();
        if last.as_ref() != Some(&v) {
            *last = Some(v.clone());
            drop(last);
            self.broadcast("state", &v);
        }
    }

    fn subscribe(&self) -> mpsc::Receiver<String> {
        let (tx, rx) = mpsc::channel();
        // Lets open pages reload themselves when a new bridge serves a new page.
        let _ = tx.send(Self::sse("hello", &json!({ "page": page_fingerprint() })));
        let _ = tx.send(Self::sse("status", &self.status.lock().unwrap()));
        if let Some(l) = self.layout_json.lock().unwrap().as_ref() {
            let _ = tx.send(Self::sse("layout", l));
        }
        if let Some(s) = self.last_state.lock().unwrap().as_ref() {
            let _ = tx.send(Self::sse("state", s));
        }
        let _ = tx.send(Self::sse("alert", &self.alert.lock().unwrap()));
        let _ = tx.send(Self::sse("shortcuts", &self.shortcuts.lock().unwrap()));
        let _ = tx.send(Self::sse("transition", &self.transition.lock().unwrap()));
        self.clients.lock().unwrap().push(tx);
        rx
    }
}

fn page_fingerprint() -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    PAGE.hash(&mut h);
    format!("{:016x}", h.finish())
}

struct Args {
    listen: String,
    device_filter: String,
    display: PathBuf,
    niri: Option<PathBuf>,
    usage: bool,
}

fn parse_args() -> Args {
    let default_niri = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config/niri/config.kdl")).filter(|p| p.exists());
    let mut args = Args { listen: "127.0.0.1:8740".into(), device_filter: String::new(), display: default_display_path(), niri: default_niri, usage: true };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--listen" => args.listen = it.next().expect("--listen needs ADDR:PORT"),
            "--device" => args.device_filter = it.next().expect("--device needs a product-name substring"),
            "--display" => args.display = PathBuf::from(it.next().expect("--display needs a file")),
            "--niri-config" => args.niri = Some(PathBuf::from(it.next().expect("--niri-config needs a file"))),
            "--no-niri" => args.niri = None,
            "--no-usage" => args.usage = false,
            "-h" | "--help" => {
                println!(
                    "layerview [--listen 127.0.0.1:8740] [--device NAME] [--display FILE] [--niri-config FILE | --no-niri] [--no-usage]\n\n\
                     Live layer/keymap display for a ZMK keyboard running the KeyPeek layer notifier.\n\
                     --display defaults to {} (registered by zmk-uf2-flash).\n\
                     Binds localhost by default: the stream reveals every key you press.",
                    default_display_path().display()
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    args
}

fn main() {
    let args = parse_args();
    let hub = Arc::new(Hub::default());
    hub.track_usage.store(args.usage, std::sync::atomic::Ordering::Relaxed);
    hub.refresh_status();
    if args.usage {
        eprintln!("[layerview] counting key/layer/modifier usage (daily totals) in {}", usage::usage_path().display());
        let hub = Arc::clone(&hub);
        thread::spawn(move || loop {
            thread::sleep(Duration::from_secs(60));
            hub.usage.lock().unwrap().save();
        });
    }

    let listener = TcpListener::bind(&args.listen).unwrap_or_else(|e| panic!("cannot bind {}: {e}", args.listen));
    eprintln!("[layerview] serving http://{}  (display file: {})", args.listen, args.display.display());
    {
        let hub = Arc::clone(&hub);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let hub = Arc::clone(&hub);
                thread::spawn(move || {
                    let _ = handle_http(stream, &hub);
                });
            }
        });
    }
    {
        let hub = Arc::clone(&hub);
        let path = args.display.clone();
        thread::spawn(move || watch_display(&hub, path));
    }
    {
        let hub = Arc::clone(&hub);
        thread::spawn(move || watch_alerts(&hub, default_alerts_path()));
    }
    {
        let hub = Arc::clone(&hub);
        thread::spawn(move || watch_pending(&hub));
    }
    if let Some(path) = args.niri.clone() {
        let hub = Arc::clone(&hub);
        thread::spawn(move || watch_niri(&hub, path));
    }
    live_loop(&hub, &args.device_filter.to_lowercase());
}

/// Loads the display file and reloads it whenever it changes on disk.
fn watch_display(hub: &Hub, path: PathBuf) {
    let mut seen: Option<SystemTime> = None;
    loop {
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime.is_some() && mtime != seen {
            seen = mtime;
            match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|b| {
                serde_json::from_slice::<DisplayFile>(&b).map_err(|e| e.to_string())
            }) {
                Ok(d) => {
                    eprintln!("[layerview] loaded display file: {} ({} layers, {} keys)", d.keyboard, d.layers.len(), d.keys.len());
                    hub.set_display(d);
                }
                Err(e) => eprintln!("[layerview] cannot read {}: {e}", path.display()),
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

/// Shows `{"message": ...}` from the alerts file on every display; no file = no alert.
fn watch_alerts(hub: &Hub, path: PathBuf) {
    loop {
        let v = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(|m| json!({ "message": m })))
            .unwrap_or_else(|| json!({ "message": null }));
        let mut cur = hub.alert.lock().unwrap();
        if *cur != v {
            eprintln!("[layerview] alert: {}", v["message"]);
            *cur = v.clone();
            drop(cur);
            hub.broadcast("alert", &v);
        }
        thread::sleep(Duration::from_secs(3));
    }
}

/// Reads the keyboard's own HID report (the one the computer types from) on a
/// second handle and publishes its modifier byte, so the display knows exactly
/// which modifiers are active, however they were produced (thumb key, home row mod...).
fn spawn_modifier_reader(hub: Arc<Hub>, raw_path: &std::ffi::CStr) {
    let Ok(api) = hidapi::HidApi::new() else { return };
    let Some(raw) = api.device_list().find(|d| d.path() == raw_path) else { return };
    let (vid, pid, serial) = (raw.vendor_id(), raw.product_id(), raw.serial_number().map(str::to_owned));
    let Some(kbd) = api.device_list().find(|d| {
        d.vendor_id() == vid && d.product_id() == pid && d.usage_page() == 0x01 && d.usage() == 0x06
            && d.serial_number().map(str::to_owned) == serial
    }) else { return };
    let Ok(dev) = api.open_path(kbd.path()) else { return };
    eprintln!("[layerview] reading modifiers from {}", kbd.path().to_string_lossy());
    thread::spawn(move || {
        let mut buf = [0u8; 64];
        loop {
            match dev.read_timeout(&mut buf, 1000) {
                Ok(0) => continue,
                Ok(n) if n >= 2 && buf[0] == 0x01 => {
                    let m = buf[1];
                    if hub.live.lock().unwrap().mods != m {
                        hub.update_live(|l| l.mods = m);
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        hub.update_live(|l| l.mods = 0);
    });
}

/// Keycaps now match the new layout: record it as the keycap baseline, end the view.
fn finish_keycap_view() {
    if let Some(v) = std::fs::read(pending_path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        if let Ok(b) = serde_json::to_vec(&v["new"]) {
            let _ = layerview_format::write_atomic(&layerview_format::keycaps_path(), &b);
        }
    }
    let _ = std::fs::remove_file(pending_path());
}

fn pending_path() -> PathBuf {
    layerview_format::state_dir().join("layerview").join("pending.json")
}

/// Keycap-move view: shown while the flasher that wrote pending.json runs
/// (pid alive), or until its handoff deadline; stale files are removed.
fn watch_pending(hub: &Hub) {
    loop {
        let v = std::fs::read(pending_path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let active = v.as_ref().is_some_and(|v| {
            let pid = v["pid"].as_u64().unwrap_or(0);
            let until = v["until"].as_u64().unwrap_or(0);
            let now = SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            (pid != 0 && std::path::Path::new(&format!("/proc/{pid}")).exists()) || until > now
        });
        if v.as_ref().is_some_and(|v| v["pid"].as_u64() == Some(0)) && !active {
            finish_keycap_view(); // handoff deadline passed: assume the caps were rearranged
        } else if v.is_some() && !active {
            let _ = std::fs::remove_file(pending_path());
        }
        let out = if active { let v = v.unwrap(); json!({ "old": v["old"], "new": v["new"] }) } else { Value::Null };
        let esc = if active {
            out["new"]["keys"].as_array().and_then(|ks| {
                ks.iter().find(|k| k["legends"][0]["t"] == "Esc").and_then(|k| k["pos"].as_u64()).map(|p| p as usize)
            })
        } else {
            None
        };
        *hub.esc_pos.lock().unwrap() = esc;
        let mut cur = hub.transition.lock().unwrap();
        if *cur != out {
            eprintln!("[layerview] keycap view {}", if active { "on" } else { "off" });
            *cur = out.clone();
            drop(cur);
            hub.broadcast("transition", &out);
        }
        thread::sleep(Duration::from_secs(1));
    }
}

/// Parses window-manager shortcuts from a niri config (following `include`s)
/// and publishes them; re-reads when the main file changes.
fn watch_niri(hub: &Hub, path: PathBuf) {
    let mut seen: Option<SystemTime> = None;
    loop {
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if mtime.is_some() && mtime != seen {
            seen = mtime;
            let binds = niri_binds(&path);
            eprintln!("[layerview] niri shortcuts: {} from {}", binds.len(), path.display());
            let v = Value::Array(binds);
            *hub.shortcuts.lock().unwrap() = v.clone();
            hub.broadcast("shortcuts", &v);
        }
        thread::sleep(Duration::from_secs(5));
    }
}

fn niri_binds(path: &std::path::Path) -> Vec<Value> {
    let mut text = String::new();
    let mut files = vec![path.to_path_buf()];
    let mut i = 0;
    let inc = regex::Regex::new(r#"(?m)^\s*include\s+"([^"]+)""#).unwrap();
    while i < files.len() && i < 32 {
        if let Ok(t) = std::fs::read_to_string(&files[i]) {
            let dir = files[i].parent().map(|d| d.to_path_buf()).unwrap_or_default();
            for c in inc.captures_iter(&t) {
                files.push(dir.join(&c[1]));
            }
            text.push_str(&t);
            text.push('\n');
        }
        i += 1;
    }
    // KEY_COMBO [props] { action...; }   (combo starts uppercase: niri config node names are lowercase)
    let bind = regex::Regex::new(
        r#"(?m)^[ \t]*((?:[A-Za-z_0-9]+\+)*[A-Z][A-Za-z0-9_]*)[ \t]+((?:[a-z-]+=(?:"[^"]*"|[^\s{]+)[ \t]*)*)\{([^{}]*)\}"#,
    )
    .unwrap();
    let title = regex::Regex::new(r#"hotkey-overlay-title="([^"]*)""#).unwrap();
    let mut out = Vec::new();
    for c in bind.captures_iter(&text) {
        let combo = &c[1];
        let parts: Vec<&str> = combo.split('+').collect();
        let (key, mod_names) = parts.split_last().unwrap();
        let mut mods = Vec::new();
        for m in mod_names {
            match m.to_ascii_lowercase().as_str() {
                "mod" | "super" | "win" => mods.push("super"),
                "ctrl" | "control" => mods.push("ctrl"),
                "alt" => mods.push("alt"),
                "shift" => mods.push("shift"),
                _ => {}
            }
        }
        let body = c[3].split("//").next().unwrap_or("").trim().trim_end_matches(';').trim().to_string();
        let label = title
            .captures(&c[2])
            .map(|t| t[1].to_string())
            .unwrap_or_else(|| niri_action_label(&body));
        out.push(json!({ "mods": mods, "key": key.to_ascii_lowercase(), "label": label, "action": body }));
    }
    out
}

fn niri_action_label(body: &str) -> String {
    let mut words = body.split_whitespace();
    let action = words.next().unwrap_or("");
    let args: Vec<String> = words.map(|w| w.trim_matches('"').to_string()).collect();
    let dir = |s: &str| {
        if s.ends_with("-left") { "←" } else if s.ends_with("-right") { "→" } else if s.ends_with("-up") { "↑" } else if s.ends_with("-down") { "↓" } else { "" }
    };
    let a = action;
    // `spawn "niri" "msg" "action" X` is just niri action X.
    if a == "spawn" && args.len() >= 4 && args[0] == "niri" && args[1] == "msg" && args[2] == "action" {
        return niri_action_label(&args[3..].join(" "));
    }
    let end = |s: &str| if s.ends_with("-first") { "first" } else if s.ends_with("-last") { "last" } else { "" };
    let label = match a {
        _ if a.starts_with("focus-column-first") || a.starts_with("focus-column-last") => format!("Focus {}", end(a)),
        _ if a.starts_with("move-column-to-first") || a.starts_with("move-column-to-last") => format!("Move {}", end(a)),
        _ if a.starts_with("focus-column-") || a.starts_with("focus-window-") => format!("Focus {}", dir(a)),
        _ if a.starts_with("move-column-to-workspace") => format!("Col→WS {}", if args.is_empty() { dir(a).to_string() } else { args.join(" ") }),
        _ if a.starts_with("move-column-to-monitor") || a.starts_with("move-window-to-monitor") => format!("→Mon {}", dir(a)),
        _ if a.starts_with("move-column-") || a.starts_with("move-window-") => format!("Move {}", dir(a)),
        _ if a.starts_with("focus-workspace") => format!("WS {}", if args.is_empty() { dir(a).to_string() } else { args.join(" ") }),
        _ if a.starts_with("move-workspace") => format!("Move WS {}", dir(a)),
        _ if a.starts_with("focus-monitor") => format!("Mon {}", dir(a)),
        _ if a.starts_with("consume-or-expel-window") => format!("Eat {}", dir(a)),
        "close-window" => "Close".into(),
        "maximize-column" => "Maximize".into(),
        "fullscreen-window" => "Fullscreen".into(),
        "toggle-window-floating" => "Float".into(),
        "toggle-overview" => "Overview".into(),
        "center-column" => "Center".into(),
        "switch-preset-column-width" => "Width".into(),
        "switch-preset-window-height" => "Height".into(),
        "set-column-width" => format!("Width {}", args.join(" ")),
        "set-window-height" => format!("Height {}", args.join(" ")),
        "screenshot" => "Screenshot".into(),
        "expand-column-to-available-width" => "Fill width".into(),
        "center-visible-columns" => "Center all".into(),
        "reset-window-height" => "Reset height".into(),
        "toggle-column-tabbed-display" => "Tabbed".into(),
        "switch-focus-between-floating-and-tiling" => "Float↔Tile".into(),
        "show-hotkey-overlay" => "Hotkeys".into(),
        "toggle-keyboard-shortcuts-inhibit" => "Inhibit keys".into(),
        "power-off-monitors" => "Monitors off".into(),
        "quit" => "Quit niri".into(),
        "screenshot-screen" => "Shot screen".into(),
        "screenshot-window" => "Shot window".into(),
        "spawn" => args.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).unwrap_or_default(),
        _ => {
            let t = a.replace('-', " ");
            let mut c = t.chars();
            c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
        }
    };
    label.trim().to_string()
}

fn find_device(filter: &str) -> Option<(std::ffi::CString, String)> {
    let api = hidapi::HidApi::new().ok()?;
    let found = api
        .device_list()
        .filter(|d| d.usage_page() == RAW_HID_USAGE_PAGE)
        .find(|d| filter.is_empty() || d.product_string().unwrap_or("").to_lowercase().contains(filter))
        .map(|d| (d.path().to_owned(), d.product_string().unwrap_or("Keyboard").to_string()));
    found
}

fn live_loop(hub: &Arc<Hub>, filter: &str) {
    loop {
        let Some((path, _name)) = find_device(filter) else {
            hub.update_live(|l| *l = Live::default());
            hub.refresh_status();
            thread::sleep(Duration::from_secs(2));
            continue;
        };
        let device = hidapi::HidApi::new().ok().and_then(|api| api.open_path(&path).ok());
        let Some(device) = device else {
            thread::sleep(Duration::from_secs(2));
            continue;
        };
        hub.update_live(|l| *l = Live { connected: true, layers: 1, pressed: vec![], mods: 0 });
        hub.refresh_status();
        spawn_modifier_reader(Arc::clone(hub), &path);

        let mut buf = [0u8; 64];
        loop {
            match device.read_timeout(&mut buf, 1000) {
                Ok(0) => {
                    // Quiet: check the device is still there (KVM switch, unplug).
                    if find_device(filter).map(|(p, _)| p != path).unwrap_or(true) {
                        break;
                    }
                }
                Ok(n) => {
                    let mut p = &buf[..n];
                    if p.len() > 1 && p[0] == 0 && (p[1] == LAYER_PACKET || p[1] == KEY_PACKET) {
                        p = &p[1..];
                    }
                    if p.len() >= 10 && p[0] == LAYER_PACKET {
                        let default = u32::from_le_bytes([p[2], p[3], p[4], p[5]]);
                        let state = u32::from_le_bytes([p[6], p[7], p[8], p[9]]);
                        hub.update_live(|l| l.layers = state | default);
                    } else if p.len() >= 4 && p[0] == KEY_PACKET {
                        let (pos, down) = (p[2] as usize, p[3] != 0);
                        hub.update_live(|l| {
                            l.pressed.retain(|&x| x != pos);
                            if down {
                                l.pressed.push(pos);
                            }
                        });
                    }
                }
                Err(_) => break,
            }
        }
        hub.update_live(|l| *l = Live::default());
        hub.refresh_status();
    }
}

fn handle_http(stream: TcpStream, hub: &Hub) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("/");
    let path = path.split('?').next().unwrap_or("/");
    let mut out = stream;
    match path {
        "/" | "/index.html" => write!(
            out,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            PAGE.len(),
            PAGE
        )?,
        "/events" => {
            out.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\n\
                  Connection: keep-alive\r\n\r\nretry: 1500\n\n",
            )?;
            let rx = hub.subscribe();
            loop {
                match rx.recv_timeout(Duration::from_secs(15)) {
                    Ok(msg) => out.write_all(msg.as_bytes())?,
                    Err(RecvTimeoutError::Timeout) => out.write_all(b": ping\n\n")?,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                }
                out.flush()?;
            }
        }
        "/healthz" => {
            let body = hub.status.lock().unwrap().to_string();
            write!(
                out,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{}",
                body.len(),
                body
            )?
        }
        _ => out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?,
    }
    Ok(())
}
