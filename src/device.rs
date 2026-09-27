//! The only code that talks to the hardware. Everything else sees the `Deck` / `Opener` traits,
//! so the daemon loop is tested against a fake.

use std::time::Duration;

use elgato_streamdeck::info::Kind;
use elgato_streamdeck::{StreamDeck, StreamDeckInput};
use hidapi::HidApi;

use crate::config::KEYS;

pub trait Deck {
    /// One key-state report, `Ok(None)` when nothing arrived before `timeout`.
    fn read(&mut self, timeout: Duration) -> Result<Option<Vec<bool>>, String>;
    /// Brightness plus all keys; `None` draws the key blank.
    fn apply(&mut self, brightness: u8, images: &[Option<Vec<u8>>; KEYS]) -> Result<(), String>;
}

pub trait Opener {
    fn open(&mut self) -> Result<Box<dyn Deck>, String>;
    fn encode(&self, img: image::DynamicImage) -> Result<Vec<u8>, String>;
}

const KIND: Kind = Kind::Mini;

#[derive(Default)]
pub struct HidOpener {
    api: Option<HidApi>,
    /// Enumerate like the real thing but never connect (tests on a machine with a live deck).
    pub probe_only: bool,
}

impl Opener for HidOpener {
    fn open(&mut self) -> Result<Box<dyn Deck>, String> {
        if self.api.is_none() {
            self.api = Some(elgato_streamdeck::new_hidapi().map_err(|e| format!("hidapi: {e}"))?);
        }
        let api = self.api.as_mut().expect("set above");
        elgato_streamdeck::refresh_device_list(api).map_err(|e| format!("hidapi refresh: {e}"))?;
        let serial = elgato_streamdeck::list_devices(api)
            .into_iter()
            .find(|(k, _)| *k == KIND)
            .map(|(_, s)| s)
            .ok_or("no Stream Deck Mini found")?;
        if self.probe_only {
            return Err("probe-only mode, not connecting".into());
        }
        let deck = StreamDeck::connect(api, KIND, &serial).map_err(|e| format!("connect {serial}: {e}"))?;
        Ok(Box::new(HidDeck { deck }))
    }

    fn encode(&self, img: image::DynamicImage) -> Result<Vec<u8>, String> {
        elgato_streamdeck::images::convert_image(KIND, img).map_err(|e| format!("encode: {e}"))
    }
}

struct HidDeck {
    deck: StreamDeck,
}

impl Deck for HidDeck {
    fn read(&mut self, timeout: Duration) -> Result<Option<Vec<bool>>, String> {
        match self.deck.read_input(Some(timeout)).map_err(|e| e.to_string())? {
            StreamDeckInput::ButtonStateChange(states) => Ok(Some(states)),
            _ => Ok(None),
        }
    }

    fn apply(&mut self, brightness: u8, images: &[Option<Vec<u8>>; KEYS]) -> Result<(), String> {
        let e = |e: elgato_streamdeck::StreamDeckError| e.to_string();
        self.deck.set_brightness(brightness).map_err(e)?;
        let blank = KIND.blank_image();
        for (i, img) in images.iter().enumerate() {
            self.deck
                .write_image(i as u8, img.as_deref().unwrap_or(&blank))
                .map_err(e)?;
        }
        self.deck.flush().map_err(e)
    }
}
