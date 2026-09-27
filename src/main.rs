use std::path::PathBuf;
use std::time::Instant;

use deckd::daemon::{Daemon, TICK};
use deckd::device::HidOpener;

fn env_is(name: &str, value: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == value)
}

fn main() {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| "/".into()));
    let cfg_path = std::env::var_os("DECKD_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config/deckd/config.toml"));
    let mut opener = HidOpener::default();
    opener.probe_only = env_is("DECKD_PROBE_ONLY", "1");
    let mut d = Daemon::new(opener, cfg_path, home, env_is("DECKD_DEBUG", "1"), Instant::now());
    d.test_spawn = std::env::var("DECKD_TEST_SPAWN")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    loop {
        if !d.tick(Instant::now()) {
            std::thread::sleep(TICK);
        }
    }
}
