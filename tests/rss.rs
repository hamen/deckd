//! Memory guard: the release binary must stay under 10 MB RSS, and must not grow while it
//! reloads configs (decoding icons each time) and spawns/reaps commands.
//! Runs in probe-only mode, so it never touches a Stream Deck that is plugged in.
#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

const LIMIT_KB: u64 = 10 * 1024;
const MAX_GROWTH_KB: u64 = 1024;
const RELOADS: usize = 50;
const SPAWNS_PER_RELOAD: usize = 4; // 50 x 4 = 200 commands

fn rss_kb(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("process alive");
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
        .expect("VmRSS")
}

fn config(icons: &Path, flip: bool) -> String {
    let names = ["iphone", "s9", "8a", "fold"];
    let keys = ["2x0", "0x1", "1x1", "2x1"];
    let mut s = String::from("brightness = 75\n");
    for (i, k) in keys.iter().enumerate() {
        let n = names[if flip { 3 - i } else { i }];
        s += &format!(
            "[keys.\"{k}\"]\nicon = \"{}/{n}.png\"\ncommand = \"true\"\n",
            icons.display()
        );
    }
    s
}

#[test]
fn rss_stays_under_10mb_and_flat() {
    let icons = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons");
    let dir = std::env::temp_dir().join(format!("deckd-rss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("config.toml");
    let log = dir.join("stderr.log");
    std::fs::write(&cfg, config(&icons, false)).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_deckd"))
        .env("DECKD_CONFIG", &cfg)
        .env("DECKD_PROBE_ONLY", "1")
        .env("DECKD_TEST_SPAWN", SPAWNS_PER_RELOAD.to_string())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .unwrap();
    let pid = child.id();

    std::thread::sleep(Duration::from_secs(5));
    let first = rss_kb(pid);

    // Each rewrite changes the size, so each one must be a real reload with 4 icon decodes.
    for i in 0..RELOADS {
        std::fs::write(&cfg, config(&icons, i % 2 == 0) + &"#".repeat(i + 1)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    std::thread::sleep(Duration::from_secs(1));
    let last = rss_kb(pid);

    let _ = child.kill();
    let _ = child.wait();
    let loads = std::fs::read_to_string(&log).unwrap().matches("config loaded").count();
    eprintln!("VmRSS first={first} kB last={last} kB, config loads={loads}");
    assert_eq!(loads, RELOADS + 1, "every rewrite must have been loaded");
    assert!(first < LIMIT_KB, "RSS {first} kB at start, limit {LIMIT_KB} kB");
    assert!(last < LIMIT_KB, "RSS {last} kB after reloads, limit {LIMIT_KB} kB");
    assert!(last <= first + MAX_GROWTH_KB, "RSS grew from {first} to {last} kB");
}
