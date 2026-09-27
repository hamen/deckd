//! The single-threaded loop: reconnect, read keys, run commands, reap them, hot-reload config.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::config::{self, KEYS, Loaded};
use crate::device::{Deck, Opener};
use crate::engine::{Engine, Event};

pub const TICK: Duration = Duration::from_millis(250);
pub const RETRY: Duration = Duration::from_secs(2);

pub fn log(msg: &str) {
    eprintln!("deckd: {msg}");
}

pub struct Daemon<O: Opener> {
    opener: O,
    cfg_path: PathBuf,
    home: PathBuf,
    debug: bool,
    loaded: Loaded,
    stamp: Option<(SystemTime, u64)>,
    last_cfg_err: Option<String>,
    deck: Option<Box<dyn Deck>>,
    next_open: Instant,
    last_open_err: Option<String>,
    engine: Engine,
    children: [Option<Child>; KEYS],
    /// Test hook (DECKD_TEST_SPAWN): after each good config load, spawn and reap `true` this many
    /// times through the real spawn path, so the RSS test exercises spawning without a device.
    pub test_spawn: usize,
}

impl<O: Opener> Daemon<O> {
    pub fn new(opener: O, cfg_path: PathBuf, home: PathBuf, debug: bool, now: Instant) -> Self {
        Daemon {
            opener,
            cfg_path,
            home,
            debug,
            loaded: Loaded {
                config: config::Config::default(),
                images: Default::default(),
            },
            stamp: None,
            last_cfg_err: None,
            deck: None,
            next_open: now,
            last_open_err: None,
            engine: Engine::default(),
            children: Default::default(),
            test_spawn: 0,
        }
    }

    /// One loop iteration. Blocks at most `TICK` (in the device read).
    /// Returns false when there is no device, so the caller can sleep instead.
    pub fn tick(&mut self, now: Instant) -> bool {
        self.reap();
        if self.maybe_reload() {
            self.redraw();
        }
        if self.deck.is_none() && now >= self.next_open {
            self.try_open(now);
        }
        let Some(deck) = self.deck.as_mut() else {
            return false;
        };
        match deck.read(TICK) {
            Ok(Some(states)) => {
                if self.debug {
                    log(&format!(
                        "raw {:?}",
                        states.iter().map(|&b| b as u8).collect::<Vec<_>>()
                    ));
                }
                let running = self.running();
                for ev in self
                    .engine
                    .on_report(&states, Instant::now(), &self.loaded.config, &running)
                {
                    self.handle(ev);
                }
            }
            Ok(None) => {}
            Err(e) => self.drop_deck(now, &format!("device error: {e}")),
        }
        true
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Fire(i) => {
                let cmd = self.loaded.config.keys[i].command.clone().unwrap_or_default();
                match spawn(&cmd) {
                    Ok(child) => {
                        log(&format!("key {i}: run {cmd}"));
                        self.children[i] = Some(child);
                    }
                    Err(e) => log(&format!("key {i}: spawn failed: {e}")),
                }
            }
            Event::Busy(i) => log(&format!("key {i}: still running, press ignored")),
            Event::Debounced(i) if self.debug => log(&format!("key {i}: debounced")),
            Event::Unmapped(i) if self.debug => log(&format!("key {i}: unmapped DOWN (ghost?)")),
            _ => {}
        }
    }

    fn running(&self) -> [bool; KEYS] {
        std::array::from_fn(|i| self.children[i].is_some())
    }

    fn reap(&mut self) {
        for (i, slot) in self.children.iter_mut().enumerate() {
            if let Some(child) = slot {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if !status.success() {
                            log(&format!("key {i}: command exited with {status}"));
                        }
                        *slot = None;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        log(&format!("key {i}: wait failed: {e}"));
                        *slot = None;
                    }
                }
            }
        }
    }

    /// Reloads when (mtime, size) changed. A rejected file is re-read every tick until it parses;
    /// the error is logged only when its text changes. Returns true when a new config is active.
    fn maybe_reload(&mut self) -> bool {
        let stamp = std::fs::metadata(&self.cfg_path)
            .ok()
            .map(|m| (m.modified().unwrap_or(SystemTime::UNIX_EPOCH), m.len()));
        if stamp.is_some() && stamp == self.stamp {
            return false;
        }
        let opener = &self.opener;
        match config::load(&self.cfg_path, &self.home, &|img| opener.encode(img)) {
            Ok(loaded) => {
                self.loaded = loaded;
                self.stamp = stamp;
                self.last_cfg_err = None;
                log(&format!("config loaded from {}", self.cfg_path.display()));
                for _ in 0..self.test_spawn {
                    if let Ok(mut c) = spawn("true") {
                        let _ = c.wait();
                    }
                }
                true
            }
            Err(e) => {
                if self.last_cfg_err.as_deref() != Some(&e) {
                    log(&format!("config rejected, keeping the previous one: {e}"));
                    self.last_cfg_err = Some(e);
                }
                false
            }
        }
    }

    fn redraw(&mut self) {
        if let Some(deck) = self.deck.as_mut()
            && let Err(e) = deck.apply(self.loaded.config.brightness, &self.loaded.images)
        {
            self.drop_deck(Instant::now(), &format!("draw failed: {e}"));
        }
    }

    fn try_open(&mut self, now: Instant) {
        match self.opener.open() {
            Ok(mut deck) => match deck.apply(self.loaded.config.brightness, &self.loaded.images) {
                Ok(()) => {
                    log("Stream Deck connected");
                    self.last_open_err = None;
                    self.engine.reset();
                    self.deck = Some(deck);
                }
                Err(e) => self.drop_deck(now, &format!("draw failed: {e}")),
            },
            Err(e) => {
                if self.last_open_err.as_deref() != Some(&e) {
                    log(&format!("waiting for device: {e}"));
                    self.last_open_err = Some(e);
                }
                self.next_open = now + RETRY;
            }
        }
    }

    fn drop_deck(&mut self, now: Instant, why: &str) {
        log(why);
        self.deck = None;
        self.engine.reset();
        self.next_open = now + RETRY;
    }

    #[cfg(test)]
    fn connected(&self) -> bool {
        self.deck.is_some()
    }
}

/// `sh -c <command>`: stdin from /dev/null, output to our stderr/stdout (the journal), and every
/// inherited fd >= 3 closed, so a child can never hold the hidraw device open.
pub fn spawn(command: &str) -> std::io::Result<Child> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(command).stdin(Stdio::null());
    // SAFETY: close_range is async-signal-safe; nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32);
            Ok(())
        });
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    type Images = [Option<Vec<u8>>; KEYS];

    #[derive(Default)]
    struct Shared {
        reports: VecDeque<Result<Option<Vec<bool>>, String>>,
        applies: Vec<(u8, Images)>,
        opens: usize,
        present: bool,
    }

    struct FakeOpener(Rc<RefCell<Shared>>);
    struct FakeDeck(Rc<RefCell<Shared>>);

    impl Opener for FakeOpener {
        fn open(&mut self) -> Result<Box<dyn Deck>, String> {
            let mut s = self.0.borrow_mut();
            if !s.present {
                return Err("absent".into());
            }
            s.opens += 1;
            Ok(Box::new(FakeDeck(self.0.clone())))
        }
        fn encode(&self, img: image::DynamicImage) -> Result<Vec<u8>, String> {
            // the first pixel's red byte identifies which icon was drawn
            Ok(vec![img.to_rgb8().get_pixel(0, 0)[0]])
        }
    }

    impl Deck for FakeDeck {
        fn read(&mut self, _t: Duration) -> Result<Option<Vec<bool>>, String> {
            self.0.borrow_mut().reports.pop_front().unwrap_or(Ok(None))
        }
        fn apply(&mut self, b: u8, images: &Images) -> Result<(), String> {
            self.0.borrow_mut().applies.push((b, images.clone()));
            Ok(())
        }
    }

    struct Env {
        dir: PathBuf,
        shared: Rc<RefCell<Shared>>,
        d: Daemon<FakeOpener>,
        t: Instant,
    }

    fn icon(dir: &std::path::Path, name: &str, red: u8) -> String {
        let p = dir.join(name);
        image::RgbImage::from_pixel(4, 4, image::Rgb([red, 0, 0]))
            .save(&p)
            .unwrap();
        p.display().to_string()
    }

    fn setup(tag: &str, cfg: &str) -> Env {
        let dir = std::env::temp_dir().join(format!("deckd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), cfg).unwrap();
        let shared = Rc::new(RefCell::new(Shared {
            present: true,
            ..Default::default()
        }));
        let t = Instant::now();
        let d = Daemon::new(
            FakeOpener(shared.clone()),
            dir.join("config.toml"),
            dir.clone(),
            false,
            t,
        );
        Env { dir, shared, d, t }
    }

    fn marker(dir: &std::path::Path) -> String {
        format!("echo x >> {}", dir.join("ran").display())
    }

    fn runs(dir: &std::path::Path) -> usize {
        std::fs::read_to_string(dir.join("ran"))
            .map(|s| s.lines().count())
            .unwrap_or(0)
    }

    fn wait_children(d: &mut Daemon<FakeOpener>) {
        for _ in 0..200 {
            d.reap();
            if !d.running().iter().any(|&r| r) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("children did not finish");
    }

    fn st(down: &[usize]) -> Vec<bool> {
        (0..KEYS).map(|i| down.contains(&i)).collect()
    }

    #[test]
    fn press_runs_command_once_and_all_six_keys_are_drawn() {
        let mut e = setup("press", "");
        let cmd = marker(&e.dir);
        let ic = icon(&e.dir, "a.png", 7);
        std::fs::write(
            e.dir.join("config.toml"),
            format!("[keys.\"2x0\"]\nicon = \"{ic}\"\ncommand = \"{cmd}\"\n"),
        )
        .unwrap();
        e.shared
            .borrow_mut()
            .reports
            .extend([Ok(Some(st(&[2]))), Ok(Some(st(&[])))]);
        e.d.tick(e.t);
        e.d.tick(e.t);
        wait_children(&mut e.d);
        assert_eq!(runs(&e.dir), 1);
        let s = e.shared.borrow();
        let (b, imgs) = s.applies.last().unwrap();
        assert_eq!(*b, 75);
        assert_eq!(imgs[2], Some(vec![7]));
        assert!(
            imgs.iter().enumerate().all(|(i, im)| i == 2 || im.is_none()),
            "other keys blank"
        );
    }

    #[test]
    fn ghost_on_unmapped_key_runs_nothing() {
        let mut e = setup("ghost", "");
        std::fs::write(
            e.dir.join("config.toml"),
            format!("[keys.\"0x1\"]\ncommand = \"{}\"\n", marker(&e.dir)),
        )
        .unwrap();
        // pattern B then pattern A, ghost = key 0 (0x0)
        e.shared.borrow_mut().reports.extend([
            Ok(Some(st(&[0]))),
            Ok(Some(st(&[0, 3]))),
            Ok(Some(st(&[3]))),
            Ok(Some(st(&[3, 0]))),
            Ok(Some(st(&[]))),
        ]);
        for _ in 0..5 {
            e.d.tick(e.t);
        }
        wait_children(&mut e.d);
        assert_eq!(runs(&e.dir), 1, "only the real press of 0x1");
    }

    #[test]
    fn press_while_running_is_ignored() {
        let mut e = setup("busy", "");
        std::fs::write(
            e.dir.join("config.toml"),
            format!(
                "debounce_ms = 0\n[keys.\"0x0\"]\ncommand = \"sleep 0.3; {}\"\n",
                marker(&e.dir)
            ),
        )
        .unwrap();
        e.shared
            .borrow_mut()
            .reports
            .extend([Ok(Some(st(&[0]))), Ok(Some(st(&[]))), Ok(Some(st(&[0])))]);
        for _ in 0..3 {
            e.d.tick(e.t);
        }
        wait_children(&mut e.d);
        assert_eq!(runs(&e.dir), 1);
    }

    #[test]
    fn rejected_file_is_reread_even_if_the_fix_keeps_size_and_mtime() {
        let mut e = setup("reread", "");
        let cfg = e.dir.join("config.toml");
        let set_mtime = |secs: i64| {
            let ts = [libc::timespec {
                tv_sec: secs,
                tv_nsec: 0,
            }; 2];
            let c = std::ffi::CString::new(cfg.to_str().unwrap()).unwrap();
            assert_eq!(
                unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), ts.as_ptr(), 0) },
                0
            );
        };
        std::fs::write(&cfg, "brightness = 101").unwrap(); // rejected: out of range
        set_mtime(1_000_000);
        e.d.tick(e.t);
        assert_eq!(e.shared.borrow().applies.last().unwrap().0, 75, "default kept");
        std::fs::write(&cfg, "brightness = 99 ").unwrap(); // same length
        set_mtime(1_000_000); // same mtime: a stamp-only check would miss this
        e.d.tick(e.t);
        assert_eq!(e.shared.borrow().applies.last().unwrap().0, 99);
    }

    #[test]
    fn device_error_reconnects_after_retry_and_redraws() {
        let mut e = setup("reconnect", "");
        e.shared.borrow_mut().reports.push_back(Err("unplugged".into()));
        e.d.tick(e.t);
        assert!(!e.d.connected());
        e.d.tick(e.t + Duration::from_millis(500));
        assert_eq!(e.shared.borrow().opens, 1, "no reopen before the retry deadline");
        e.d.tick(e.t + RETRY);
        assert!(e.d.connected());
        assert_eq!(e.shared.borrow().opens, 2);
        assert_eq!(e.shared.borrow().applies.len(), 2, "drawn on each open");
    }

    #[test]
    fn absent_device_keeps_the_loop_alive() {
        let mut e = setup("absent", "");
        e.shared.borrow_mut().present = false;
        assert!(!e.d.tick(e.t));
        e.shared.borrow_mut().present = true;
        assert!(e.d.tick(e.t + RETRY));
        assert!(e.d.connected());
    }

    #[test]
    fn hot_reload_redraws_and_bad_config_keeps_the_old_one() {
        let mut e = setup("reload", "");
        let a = icon(&e.dir, "a.png", 1);
        let b = icon(&e.dir, "b.png", 2);
        let cfg = e.dir.join("config.toml");
        std::fs::write(&cfg, format!("[keys.\"0x0\"]\nicon = \"{a}\"\n")).unwrap();
        e.d.tick(e.t);
        assert_eq!(e.shared.borrow().applies.last().unwrap().1[0], Some(vec![1]));
        std::fs::write(&cfg, format!("[keys.\"0x0\"]\nicon = \"{b}\"\n# changed size\n")).unwrap();
        e.d.tick(e.t);
        assert_eq!(e.shared.borrow().applies.last().unwrap().1[0], Some(vec![2]));
        let n = e.shared.borrow().applies.len();
        std::fs::write(&cfg, "[keys.\"0x0\"]\nicon = \"/missing.png\"\n").unwrap();
        e.d.tick(e.t);
        std::fs::write(&cfg, "brightness = = 3").unwrap();
        e.d.tick(e.t);
        assert_eq!(e.shared.borrow().applies.len(), n, "rejected configs do not redraw");
        assert_eq!(e.d.loaded.images[0], Some(vec![2]), "old images kept");
        std::fs::write(&cfg, format!("brightness = 30\n[keys.\"0x0\"]\nicon = \"{a}\"\n")).unwrap();
        e.d.tick(e.t);
        assert_eq!(e.shared.borrow().applies.last().unwrap().0, 30);
    }

    #[test]
    fn missing_config_at_start_draws_blank_and_stays_up() {
        let mut e = setup("nocfg", "");
        std::fs::remove_file(e.dir.join("config.toml")).unwrap();
        assert!(e.d.tick(e.t));
        let s = e.shared.borrow();
        assert!(s.applies.last().unwrap().1.iter().all(|i| i.is_none()));
    }

    #[test]
    fn spawned_child_does_not_inherit_extra_fds() {
        let dir = std::env::temp_dir().join(format!("deckd-fds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let held = std::fs::File::open("/dev/null").unwrap();
        // a high fd without FD_CLOEXEC (dup2 clears it), so only close_range can stop the leak
        use std::os::fd::AsRawFd;
        const FD: i32 = 200;
        assert_eq!(unsafe { libc::dup2(held.as_raw_fd(), FD) }, FD);
        let out = dir.join("fds");
        let mut c = spawn(&format!("ls /proc/self/fd > {}", out.display())).unwrap();
        c.wait().unwrap();
        unsafe { libc::close(FD) };
        let fds: Vec<i32> = std::fs::read_to_string(&out)
            .unwrap()
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        assert!(!fds.is_empty());
        assert!(!fds.contains(&FD), "fd {FD} leaked into child: {fds:?}");
    }
}
