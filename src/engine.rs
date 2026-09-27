//! Pure key logic: turns key-state reports into "run this key's command" decisions.
//! Time is passed in, so tests control it.

use std::time::Instant;

use crate::config::{Config, KEYS};

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Run the key's command.
    Fire(usize),
    /// DOWN on a key with no command (this is where the ghost key ends up).
    Unmapped(usize),
    /// Same key pressed again within its debounce window.
    Debounced(usize),
    /// The key's previous command is still running.
    Busy(usize),
}

#[derive(Default)]
pub struct Engine {
    held: [bool; KEYS],
    last_down: [Option<Instant>; KEYS],
}

impl Engine {
    /// Forget the key state, e.g. after a reconnect: the next report is taken as fresh.
    pub fn reset(&mut self) {
        self.held = [false; KEYS];
    }

    /// Feed one full key-state report. `running[i]` says whether key i's command is still alive.
    pub fn on_report(&mut self, states: &[bool], now: Instant, cfg: &Config, running: &[bool; KEYS]) -> Vec<Event> {
        let mut out = Vec::new();
        for (i, held) in self.held.iter_mut().enumerate() {
            let down = states.get(i).copied().unwrap_or(false);
            let was = std::mem::replace(held, down);
            if !down || was {
                continue;
            }
            let key = &cfg.keys[i];
            if key.command.is_none() {
                out.push(Event::Unmapped(i));
                continue;
            }
            let prev = self.last_down[i].replace(now);
            if let Some(prev) = prev
                && now.duration_since(prev).as_millis() < u128::from(key.debounce_ms)
            {
                out.push(Event::Debounced(i));
                continue;
            }
            if running[i] {
                out.push(Event::Busy(i));
                continue;
            }
            out.push(Event::Fire(i));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    // 2x0 = key 2 mapped, 0x0 and 1x0 (keys 0, 1) unmapped, like Ivan's deck.
    fn cfg() -> Config {
        crate::config::parse(
            "debounce_ms = 150\n[keys.\"2x0\"]\ncommand = \"true\"\n[keys.\"0x1\"]\ncommand = \"true\"\n",
            Path::new("/h"),
        )
        .unwrap()
    }

    fn st(down: &[usize]) -> [bool; KEYS] {
        let mut s = [false; KEYS];
        for &i in down {
            s[i] = true;
        }
        s
    }

    const IDLE: [bool; KEYS] = [false; KEYS];

    fn fires(evs: &[Event]) -> Vec<usize> {
        evs.iter()
            .filter_map(|e| if let Event::Fire(i) = e { Some(*i) } else { None })
            .collect()
    }

    #[test]
    fn press_fires_once_and_release_does_nothing() {
        let (c, t0, mut e) = (cfg(), Instant::now(), Engine::default());
        assert_eq!(e.on_report(&st(&[2]), t0, &c, &IDLE), vec![Event::Fire(2)]);
        assert!(
            e.on_report(&st(&[2]), t0 + Duration::from_millis(50), &c, &IDLE)
                .is_empty(),
            "held is not a new press"
        );
        assert!(
            e.on_report(&st(&[]), t0 + Duration::from_millis(100), &c, &IDLE)
                .is_empty()
        );
    }

    #[test]
    fn same_key_bounce_inside_window_is_dropped_and_edge_fires() {
        let (c, t0, mut e) = (cfg(), Instant::now(), Engine::default());
        e.on_report(&st(&[2]), t0, &c, &IDLE);
        e.on_report(&st(&[]), t0 + Duration::from_millis(20), &c, &IDLE);
        assert_eq!(
            e.on_report(&st(&[2]), t0 + Duration::from_millis(149), &c, &IDLE),
            vec![Event::Debounced(2)]
        );
        e.on_report(&st(&[]), t0 + Duration::from_millis(200), &c, &IDLE);
        // exactly debounce_ms after the previous DOWN (at 149) counts as outside the window
        assert_eq!(
            e.on_report(&st(&[2]), t0 + Duration::from_millis(299), &c, &IDLE),
            vec![Event::Fire(2)]
        );
    }

    #[test]
    fn windows_are_per_key() {
        let (c, t0, mut e) = (cfg(), Instant::now(), Engine::default());
        e.on_report(&st(&[2]), t0, &c, &IDLE);
        e.on_report(&st(&[]), t0 + Duration::from_millis(10), &c, &IDLE);
        assert_eq!(
            e.on_report(&st(&[3]), t0 + Duration::from_millis(20), &c, &IDLE),
            vec![Event::Fire(3)]
        );
    }

    #[test]
    fn busy_key_is_dropped() {
        let (c, t0, mut e) = (cfg(), Instant::now(), Engine::default());
        let mut running = IDLE;
        running[2] = true;
        assert_eq!(e.on_report(&st(&[2]), t0, &c, &running), vec![Event::Busy(2)]);
    }

    #[test]
    fn ghost_pattern_a_unmapped_down_while_mapped_key_held() {
        let (c, t0, mut e) = (cfg(), Instant::now(), Engine::default());
        let mut all = e.on_report(&st(&[2]), t0, &c, &IDLE);
        all.extend(e.on_report(&st(&[2, 1]), t0 + Duration::from_millis(80), &c, &IDLE));
        all.extend(e.on_report(&st(&[1]), t0 + Duration::from_millis(120), &c, &IDLE));
        all.extend(e.on_report(&st(&[]), t0 + Duration::from_millis(160), &c, &IDLE));
        assert_eq!(fires(&all), vec![2]);
        assert!(all.contains(&Event::Unmapped(1)));
    }

    #[test]
    fn ghost_pattern_b_unmapped_down_150ms_before_real_press() {
        let (c, t0, mut e) = (cfg(), Instant::now(), Engine::default());
        let mut all = e.on_report(&st(&[0]), t0, &c, &IDLE);
        all.extend(e.on_report(&st(&[0, 3]), t0 + Duration::from_millis(150), &c, &IDLE));
        all.extend(e.on_report(&st(&[]), t0 + Duration::from_millis(300), &c, &IDLE));
        assert_eq!(fires(&all), vec![3]);
    }

    #[test]
    fn reset_makes_a_held_key_fire_again_after_reconnect() {
        let (c, t0, mut e) = (cfg(), Instant::now(), Engine::default());
        e.on_report(&st(&[2]), t0, &c, &IDLE);
        e.reset();
        assert_eq!(
            e.on_report(&st(&[2]), t0 + Duration::from_secs(1), &c, &IDLE),
            vec![Event::Fire(2)]
        );
    }
}
