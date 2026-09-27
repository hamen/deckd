//! Startup guard: from exec to "config loaded" (config parsed, icons decoded and encoded) must
//! take well under 100 ms. The full requirement — exec to all keys drawn on a real deck, which
//! adds the USB image transfer — is measured on the device (71-76 ms, see README).
//! Probe-only, so it never touches a deck that is plugged in.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LIMIT: Duration = Duration::from_millis(100);

fn time_to_config_loaded(cfg: &Path) -> Duration {
    let t0 = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_deckd"))
        .env("DECKD_CONFIG", cfg)
        .env("DECKD_PROBE_ONLY", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let found = lines.any(|l| l.map(|l| l.contains("config loaded")).unwrap_or(false));
    let elapsed = t0.elapsed();
    let _ = child.kill();
    let _ = child.wait();
    assert!(found, "deckd never logged 'config loaded'");
    elapsed
}

#[test]
fn starts_in_under_100ms() {
    let icons = Path::new(env!("CARGO_MANIFEST_DIR")).join("icons");
    let dir = std::env::temp_dir().join(format!("deckd-start-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("config.toml");
    let mut text = String::new();
    for (k, n) in [("2x0", "iphone"), ("0x1", "s9"), ("1x1", "8a"), ("2x1", "fold")] {
        text += &format!(
            "[keys.\"{k}\"]\nicon = \"{}/{n}.png\"\ncommand = \"true\"\n",
            icons.display()
        );
    }
    std::fs::write(&cfg, text).unwrap();

    // median of 5, so one scheduling hiccup on a busy machine does not fail the build
    let mut runs: Vec<Duration> = (0..5).map(|_| time_to_config_loaded(&cfg)).collect();
    runs.sort();
    eprintln!("startup to config loaded: {runs:?}");
    assert!(runs[2] < LIMIT, "median startup {:?}, limit {LIMIT:?}", runs[2]);
}
