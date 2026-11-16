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

use layerview_format::{default_display_path, DisplayFile};
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
}

#[derive(Default)]
struct Hub {
    clients: Mutex<Vec<Sender<String>>>,
    status: Mutex<Value>,
    display: Mutex<Option<DisplayFile>>,
    layout_json: Mutex<Option<Value>>,
    live: Mutex<Live>,
    last_state: Mutex<Option<Value>>,
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
        f(&mut self.live.lock().unwrap());
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
        let v = json!({ "layers": live.layers, "pressed": pressed, "effective": effective, "shift": shift });
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
}

fn parse_args() -> Args {
    let mut args = Args { listen: "127.0.0.1:8740".into(), device_filter: String::new(), display: default_display_path() };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--listen" => args.listen = it.next().expect("--listen needs ADDR:PORT"),
            "--device" => args.device_filter = it.next().expect("--device needs a product-name substring"),
            "--display" => args.display = PathBuf::from(it.next().expect("--display needs a file")),
            "-h" | "--help" => {
                println!(
                    "layerview [--listen 127.0.0.1:8740] [--device NAME] [--display FILE]\n\n\
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
    hub.refresh_status();

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

fn find_device(filter: &str) -> Option<(std::ffi::CString, String)> {
    let api = hidapi::HidApi::new().ok()?;
    let found = api
        .device_list()
        .filter(|d| d.usage_page() == RAW_HID_USAGE_PAGE)
        .find(|d| filter.is_empty() || d.product_string().unwrap_or("").to_lowercase().contains(filter))
        .map(|d| (d.path().to_owned(), d.product_string().unwrap_or("Keyboard").to_string()));
    found
}

fn live_loop(hub: &Hub, filter: &str) {
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
        hub.update_live(|l| *l = Live { connected: true, layers: 1, pressed: vec![] });
        hub.refresh_status();

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
