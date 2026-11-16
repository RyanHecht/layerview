//! zmk-uf2-flash: flash a ZMK build from a GitHub Release onto UF2 bootloader
//! drives, one target (e.g. keyboard half) at a time.
//!
//! The release must contain `flash.json` (see docs/formats.md), which names
//! each UF2 file, its sha256, the bootloader drive's volume label, and a hint
//! for entering the bootloader. Targets already flashed with the same file are
//! skipped. If the release includes a layerview display file, it is registered
//! once the central (keymap-holding) target is flashed.
//!
//! Linux only for now: waits for /dev/disk/by-label/<LABEL>, mounts via
//! udisksctl when the desktop didn't automount it, copies, waits for reboot.

use layerview_format::{cache_dir, default_display_path, keycaps_path, state_dir, write_atomic, FlashManifest};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

struct Args {
    repo: Option<String>,
    tag: Option<String>,
    dir: Option<PathBuf>,
    only: Option<String>,
    force: bool,
    display_dest: PathBuf,
    no_display: bool,
}

fn usage() -> ! {
    println!(
        "zmk-uf2-flash [--repo OWNER/NAME] [--tag TAG] [--dir LOCAL_DIR] [--only TARGET]\n\
         \x20             [--force] [--display-dest FILE] [--no-display]\n\n\
         Downloads the latest (or --tag) GitHub Release of a ZMK config repo, then waits for\n\
         each target's UF2 bootloader drive and copies its firmware. --repo is remembered.\n\
         --dir uses a local folder containing flash.json instead of GitHub.\n\
         The display file is registered at {} for layerview.",
        default_display_path().display()
    );
    std::process::exit(0)
}

fn parse_args() -> Args {
    let mut a = Args { repo: None, tag: None, dir: None, only: None, force: false, display_dest: default_display_path(), no_display: false };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        let mut val = |n: &str| it.next().unwrap_or_else(|| panic!("{n} needs a value"));
        match x.as_str() {
            "--repo" => a.repo = Some(val("--repo")),
            "--tag" => a.tag = Some(val("--tag")),
            "--dir" => a.dir = Some(PathBuf::from(val("--dir"))),
            "--only" => a.only = Some(val("--only")),
            "--display-dest" => a.display_dest = PathBuf::from(val("--display-dest")),
            "--force" => a.force = true,
            "--no-display" => a.no_display = true,
            "-h" | "--help" => usage(),
            other => {
                eprintln!("unknown argument: {other}");
                std::process::exit(2);
            }
        }
    }
    a
}

#[derive(Default, Serialize, Deserialize)]
struct State {
    repo: Option<String>,
    /// target name -> sha256 of the last firmware flashed to it
    flashed: HashMap<String, String>,
}

fn state_path() -> PathBuf {
    state_dir().join("zmk-uf2-flash").join("state.json")
}

fn load_state() -> State {
    std::fs::read(state_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_state(s: &State) {
    let _ = write_atomic(&state_path(), &serde_json::to_vec_pretty(s).unwrap());
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------- fetching

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

fn http_get(url: &str) -> Res<Vec<u8>> {
    let resp = ureq::get(url)
        .set("User-Agent", "zmk-uf2-flash")
        .set("Accept", "application/octet-stream, application/json")
        .call()?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf)?;
    Ok(buf)
}

/// Returns (directory holding the release files, manifest).
fn fetch(args: &Args, state: &mut State) -> Res<(PathBuf, FlashManifest)> {
    if let Some(dir) = &args.dir {
        let m: FlashManifest = serde_json::from_slice(&std::fs::read(dir.join("flash.json"))?)?;
        return Ok((dir.clone(), m));
    }
    let repo = args.repo.clone().or_else(|| state.repo.clone()).ok_or(
        "no repo given: pass --repo OWNER/NAME once (it is remembered)",
    )?;
    state.repo = Some(repo.clone());
    let api = match &args.tag {
        Some(t) => format!("https://api.github.com/repos/{repo}/releases/tags/{t}"),
        None => format!("https://api.github.com/repos/{repo}/releases/latest"),
    };
    let rel: Release = serde_json::from_slice(&http_get(&api)?)?;
    let dir = cache_dir().join("zmk-uf2-flash").join(repo.replace('/', "_")).join(&rel.tag_name);
    std::fs::create_dir_all(&dir)?;
    let url = |name: &str| rel.assets.iter().find(|a| a.name == name).map(|a| a.browser_download_url.clone());

    let manifest_bytes = http_get(&url("flash.json").ok_or(format!("release {} has no flash.json", rel.tag_name))?)?;
    std::fs::write(dir.join("flash.json"), &manifest_bytes)?;
    let m: FlashManifest = serde_json::from_slice(&manifest_bytes)?;

    let mut wanted: Vec<String> = m.targets.iter().map(|t| t.file.clone()).collect();
    wanted.extend(m.display.clone());
    for name in wanted {
        let path = dir.join(&name);
        if !path.exists() {
            let bytes = http_get(&url(&name).ok_or(format!("release is missing {name}"))?)?;
            std::fs::write(&path, bytes)?;
        }
    }
    Ok((dir, m))
}

// ---------------------------------------------------------------- drives

fn by_label(label: &str) -> PathBuf {
    // udev escapes unsafe characters in /dev/disk/by-label names as \xNN.
    let esc: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c.to_string() } else { format!("\\x{:02x}", c as u32) })
        .collect();
    PathBuf::from("/dev/disk/by-label").join(esc)
}

fn mountpoint_of(dev: &Path) -> Option<PathBuf> {
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    mounts.lines().find_map(|l| {
        let mut f = l.split(' ');
        let src = f.next()?;
        let target = f.next()?;
        (Path::new(src) == dev).then(|| PathBuf::from(target.replace("\\040", " ")))
    })
}

fn wait_for_drive(label: &str) -> PathBuf {
    let link = by_label(label);
    let mut dots = Instant::now();
    loop {
        if let Ok(dev) = std::fs::canonicalize(&link) {
            return dev;
        }
        if dots.elapsed() > Duration::from_secs(10) {
            print!(".");
            let _ = std::io::stdout().flush();
            dots = Instant::now();
        }
        thread::sleep(Duration::from_millis(300));
    }
}

fn ensure_mounted(dev: &Path) -> Res<PathBuf> {
    // Give a desktop automounter a moment first.
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        if let Some(m) = mountpoint_of(dev) {
            return Ok(m);
        }
        thread::sleep(Duration::from_millis(250));
    }
    let out = Command::new("udisksctl")
        .args(["mount", "--no-user-interaction", "-b"])
        .arg(dev)
        .output()?;
    if let Some(m) = mountpoint_of(dev) {
        return Ok(m);
    }
    Err(format!(
        "could not mount {}: {}",
        dev.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    )
    .into())
}

fn copy_uf2(src: &[u8], dest: &Path, link: &Path) -> Res<()> {
    let result = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(dest)?;
        f.write_all(src)?;
        f.sync_all()
    })();
    match result {
        Ok(()) => Ok(()),
        // The bootloader often reboots the moment the last block lands, which
        // can fail the final sync. If the drive vanished, the copy finished.
        Err(e) => {
            thread::sleep(Duration::from_millis(1500));
            if link.exists() { Err(e.into()) } else { Ok(()) }
        }
    }
}

fn wait_gone(link: &Path, timeout: Duration) -> bool {
    let until = Instant::now() + timeout;
    while Instant::now() < until {
        if !link.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(300));
    }
    false
}

// ---------------------------------------------------------------- main

fn main() {
    let result = run(parse_args());
    // Our keycap view ends with us (unless it was handed off to the display, see run()).
    let mine = format!("\"pid\": {}", std::process::id());
    if std::fs::read_to_string(pending_path()).is_ok_and(|t| t.contains(&mine)) {
        let _ = std::fs::remove_file(pending_path());
    }
    if let Err(e) = result {
        eprintln!("\nerror: {e}");
        std::process::exit(1);
    }
}

/// While this file exists (and the pid in it is alive), layerview shows which
/// keycaps move between the old and new base layers.
fn pending_path() -> PathBuf {
    state_dir().join("layerview").join("pending.json")
}

/// Base-layer labels per key position, from a display file.
fn base_labels(bytes: &[u8]) -> Option<Vec<String>> {
    let d: layerview_format::DisplayFile = serde_json::from_slice(bytes).ok()?;
    let mut keys = d.keys;
    keys.sort_by_key(|k| k.pos);
    Some(keys.into_iter().map(|k| k.legends.into_iter().next().map(|l| l.t).unwrap_or_default()).collect())
}

fn run(args: Args) -> Res<()> {
    let mut state = load_state();
    let (dir, m) = fetch(&args, &mut state)?;
    save_state(&state);

    let msg = m.message.lines().next().unwrap_or("");
    let short = m.commit.get(..7).unwrap_or(&m.commit);
    println!("Build: {} {short} \"{msg}\"", m.keyboard);

    // Keycap moves: show them on the display while flashing, if the printed labels change.
    let mut keycaps_change = false;
    if let (false, Some(name)) = (args.no_display, &m.display) {
        let new = std::fs::read(dir.join(name))?;
        if let Ok(old) = std::fs::read(keycaps_path()).or_else(|_| std::fs::read(&args.display_dest)) {
            if let (Some(a), Some(b)) = (base_labels(&old), base_labels(&new)) {
                if a != b {
                    keycaps_change = true;
                    let pending = format!(
                        "{{\"pid\": {}, \"old\": {}, \"new\": {}}}",
                        std::process::id(),
                        String::from_utf8_lossy(&old),
                        String::from_utf8_lossy(&new)
                    );
                    write_atomic(&pending_path(), pending.as_bytes())?;
                    let moved = a.iter().zip(&b).filter(|(x, y)| x != y).count();
                    println!("Keycaps: {moved} key labels change. The layer display now shows which caps to move.");
                }
            }
        }
    }

    let mut flashed_any = false;
    for t in &m.targets {
        if args.only.as_ref().is_some_and(|o| o != &t.name) {
            continue;
        }
        let bytes = std::fs::read(dir.join(&t.file))?;
        let sum = sha256_hex(&bytes);
        if sum != t.sha256 {
            return Err(format!("{}: checksum mismatch (expected {}, got {sum})", t.file, t.sha256).into());
        }
        if !args.force && state.flashed.get(&t.name) == Some(&sum) {
            println!("{}: unchanged since last flash, skipping (use --force to reflash)", t.name);
            continue;
        }

        let hint = if t.hint.is_empty() { String::new() } else { format!(" ({})", t.hint) };
        print!("{}: connect it directly and enter the bootloader{hint}", t.name);
        let _ = std::io::stdout().flush();
        let link = by_label(&t.volume_label);
        let dev = wait_for_drive(&t.volume_label);
        print!("\n  found {} … ", t.volume_label);
        let _ = std::io::stdout().flush();
        let mnt = ensure_mounted(&dev)?;
        copy_uf2(&bytes, &mnt.join(&t.file), &link)?;
        print!("copied ✓ … ");
        let _ = std::io::stdout().flush();
        if wait_gone(&link, Duration::from_secs(30)) {
            println!("restarted ✓");
        } else {
            println!("copied, but the drive is still present. Check the keyboard.");
        }
        state.flashed.insert(t.name.clone(), sum);
        save_state(&state);
        flashed_any = true;

        if t.central && !args.no_display {
            register_display(&dir, &m, &args.display_dest)?;
        }
    }

    // If the keyboard's central half runs this build, make sure its display
    // file is the one registered (covers runs where everything was skipped).
    if let (false, Some(name)) = (args.no_display, &m.display) {
        let current = m.targets.iter().filter(|t| t.central).all(|t| state.flashed.get(&t.name) == Some(&t.sha256));
        let want = std::fs::read(dir.join(name))?;
        if current && std::fs::read(&args.display_dest).ok().as_deref() != Some(&want[..]) {
            register_display(&dir, &m, &args.display_dest)?;
        }
    }
    let _ = flashed_any;
    if keycaps_change {
        if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            print!("Rearrange your keycaps (see the layer display), then press Enter… ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            if let Some(name) = &m.display {
                write_atomic(&keycaps_path(), &std::fs::read(dir.join(name))?)?; // caps now match this layout
            }
        } else if let Ok(t) = std::fs::read_to_string(pending_path()) {
            // No terminal to wait on: hand the view to the display for 20 minutes (Esc dismisses it).
            let until = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) + 20 * 60;
            let handed = t.replacen(&format!("\"pid\": {}", std::process::id()), &format!("\"pid\": 0, \"until\": {until}"), 1);
            write_atomic(&pending_path(), handed.as_bytes())?;
            println!("Keycap moves stay on the layer display for 20 minutes; press Esc on the keyboard when done.");
        }
    }
    println!("Done.");
    Ok(())
}

fn register_display(dir: &Path, m: &FlashManifest, dest: &Path) -> Res<()> {
    if let Some(name) = &m.display {
        write_atomic(dest, &std::fs::read(dir.join(name))?)?;
        println!("  keymap registered for layerview ✓ ({})", dest.display());
    }
    Ok(())
}
