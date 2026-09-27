//! Config file: parsing and validation are pure; loading also decodes the icons, so a config is
//! either fully usable or rejected as a whole.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The Stream Deck Mini: 3 columns x 2 rows.
pub const COLS: usize = 3;
pub const ROWS: usize = 2;
pub const KEYS: usize = COLS * ROWS;

pub const DEFAULT_BRIGHTNESS: u8 = 75;
pub const DEFAULT_DEBOUNCE_MS: u64 = 150;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    brightness: Option<u8>,
    debounce_ms: Option<u64>,
    #[serde(default)]
    keys: std::collections::BTreeMap<String, RawKey>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawKey {
    icon: Option<String>,
    command: Option<String>,
    debounce_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Key {
    pub icon: Option<PathBuf>,
    pub command: Option<String>,
    pub debounce_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub brightness: u8,
    pub keys: [Key; KEYS],
}

impl Default for Config {
    /// What deckd runs with before any config was loaded: every key blank and inert.
    fn default() -> Self {
        Config {
            brightness: DEFAULT_BRIGHTNESS,
            keys: std::array::from_fn(|_| Key {
                debounce_ms: DEFAULT_DEBOUNCE_MS,
                ..Key::default()
            }),
        }
    }
}

/// `"CxR"` (StreamController's naming, column x row) to the device key index.
pub fn key_index(name: &str) -> Result<usize, String> {
    let bad = || {
        format!(
            "bad key name {name:?}: expected \"<col>x<row>\" with col 0-{} and row 0-{}",
            COLS - 1,
            ROWS - 1
        )
    };
    let (c, r) = name.split_once('x').ok_or_else(bad)?;
    let (c, r): (usize, usize) = (c.parse().map_err(|_| bad())?, r.parse().map_err(|_| bad())?);
    if c >= COLS || r >= ROWS {
        return Err(bad());
    }
    Ok(r * COLS + c)
}

fn expand_home(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

pub fn parse(text: &str, home: &Path) -> Result<Config, String> {
    let raw: RawConfig = toml::from_str(text).map_err(|e| e.to_string())?;
    let brightness = raw.brightness.unwrap_or(DEFAULT_BRIGHTNESS);
    if brightness > 100 {
        return Err(format!("brightness {brightness} is outside 0-100"));
    }
    let default_debounce = raw.debounce_ms.unwrap_or(DEFAULT_DEBOUNCE_MS);
    let mut cfg = Config {
        brightness,
        ..Config::default()
    };
    for k in cfg.keys.iter_mut() {
        k.debounce_ms = default_debounce;
    }
    for (name, rk) in raw.keys {
        let i = key_index(&name)?;
        if let Some(c) = &rk.command
            && c.trim().is_empty()
        {
            return Err(format!("key {name}: command is empty"));
        }
        cfg.keys[i] = Key {
            icon: rk.icon.map(|p| expand_home(&p, home)),
            command: rk.command,
            debounce_ms: rk.debounce_ms.unwrap_or(default_debounce),
        };
    }
    Ok(cfg)
}

/// A config plus its key images, already converted to the device's wire format.
pub struct Loaded {
    pub config: Config,
    pub images: [Option<Vec<u8>>; KEYS],
}

/// Reads, validates and decodes everything. Any failure rejects the whole config.
pub fn load(
    path: &Path,
    home: &Path,
    encode: &dyn Fn(image::DynamicImage) -> Result<Vec<u8>, String>,
) -> Result<Loaded, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let config = parse(&text, home)?;
    let mut images: [Option<Vec<u8>>; KEYS] = Default::default();
    for (i, key) in config.keys.iter().enumerate() {
        if let Some(icon) = &key.icon {
            let img = image::open(icon).map_err(|e| format!("icon {}: {e}", icon.display()))?;
            images[i] = Some(encode(img)?);
        }
    }
    Ok(Loaded { config, images })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/test";

    fn p(text: &str) -> Result<Config, String> {
        parse(text, Path::new(HOME))
    }

    #[test]
    fn key_names_map_col_x_row_to_row_major_index() {
        assert_eq!(key_index("0x0"), Ok(0));
        assert_eq!(key_index("2x0"), Ok(2));
        assert_eq!(key_index("0x1"), Ok(3));
        assert_eq!(key_index("1x1"), Ok(4));
        assert_eq!(key_index("2x1"), Ok(5));
    }

    #[test]
    fn bad_key_names_are_rejected() {
        for bad in ["3x0", "0x2", "1-1", "x1", "ax0", ""] {
            assert!(key_index(bad).is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../config.example.toml");
        let cfg = p(text).unwrap();
        assert_eq!(cfg.brightness, 75);
        assert_eq!(
            cfg.keys[2].command.as_deref(),
            Some("/home/ivan/.local/bin/iphone-screenshot")
        );
        assert_eq!(
            cfg.keys[5].command.as_deref(),
            Some("/home/ivan/.local/bin/phone-screenshot 35191FDHS0003Q")
        );
        assert_eq!(
            cfg.keys[2].icon.as_deref(),
            Some(Path::new("/home/test/.config/deckd/icons/iphone.png"))
        );
        assert!(cfg.keys[0].command.is_none() && cfg.keys[1].command.is_none());
    }

    #[test]
    fn defaults_and_per_key_debounce() {
        let cfg = p("debounce_ms = 200\n[keys.\"1x1\"]\ncommand = \"true\"\ndebounce_ms = 500\n").unwrap();
        assert_eq!(cfg.brightness, DEFAULT_BRIGHTNESS);
        assert_eq!(cfg.keys[4].debounce_ms, 500);
        assert_eq!(cfg.keys[0].debounce_ms, 200);
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(p("brightness = 101").is_err());
        assert!(p("[keys.\"0x0\"]\ncommand = \"  \"").is_err());
        assert!(
            p("[keys.\"0x0\"]\ncomand = \"true\"").is_err(),
            "unknown field accepted"
        );
        assert!(p("colour = 1").is_err(), "unknown top-level field accepted");
        assert!(p("[keys.\"9x9\"]\ncommand = \"true\"").is_err());
        assert!(p("brightness = ").is_err());
    }

    #[test]
    fn tilde_expands_in_icon_only() {
        let cfg = p("[keys.\"0x0\"]\nicon = \"~/i.png\"\ncommand = \"~/bin/x\"").unwrap();
        assert_eq!(cfg.keys[0].icon.as_deref(), Some(Path::new("/home/test/i.png")));
        assert_eq!(cfg.keys[0].command.as_deref(), Some("~/bin/x"));
    }

    #[test]
    fn missing_icon_rejects_whole_config() {
        let dir = std::env::temp_dir().join(format!("deckd-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("c.toml");
        std::fs::write(&f, "[keys.\"0x0\"]\nicon = \"/nonexistent/x.png\"\ncommand = \"true\"").unwrap();
        let enc = |_: image::DynamicImage| Ok(vec![]);
        assert!(load(&f, Path::new(HOME), &enc).is_err());
    }
}
