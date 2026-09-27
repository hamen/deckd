# deckd

A tiny daemon for the Elgato **Stream Deck Mini** (USB `0fd9:0063`, 6 keys). Each key shows an
icon and runs a shell command when pressed. That is all it does.

It replaces StreamController, which grew to 2.9 GB of RAM + swap after 44 days to drive four
buttons. deckd is one Rust binary with one thread and no GUI, and it uses about **3.5 MB RSS**.
A test fails the build if it goes over 10 MB, or if it grows while reloading.

## Install

**Step 0 — find the ghost key first** (see below). Nothing is mapped, so nothing can run:

```sh
bin/install                                   # build, install binary + user unit, default config
flatpak kill com.core447.StreamController     # only one program can drive the deck
: > /tmp/deckd-empty.toml                     # empty config = all 6 keys unmapped
DECKD_CONFIG=/tmp/deckd-empty.toml DECKD_DEBUG=1 ~/.local/bin/deckd
# press each of 2x0, 0x1, 1x1, 2x1 about 10 times, then Ctrl-C
```

Every line is `t=<ms>ms key <col>x<row> DOWN|UP`. A key that goes DOWN without being pressed is
the ghost. **If the ghost is one of the four mapped keys, stop here**: restart StreamController
(`flatpak run com.core447.StreamController -b`) — deckd has no delay filter for a mapped ghost.
If it is `0x0` or `1x0`, go on:

```sh
mv ~/.config/autostart/StreamController.desktop{,.disabled}
systemctl --user enable --now deckd
journalctl --user -u deckd -f                 # logs
```

`bin/install` never overwrites `~/.config/deckd/config.toml` or existing icons. To pick up new
icons from the repo, delete the old files first.

Rollback: `systemctl --user disable --now deckd`, rename the autostart file back, then
`flatpak run com.core447.StreamController -b`.

## Config

`~/.config/deckd/config.toml` (or `$DECKD_CONFIG`). See [`config.example.toml`](config.example.toml).
deckd reloads it when you save. A config with any error (bad TOML, unknown field, brightness
outside 0-100, empty command, a missing icon) is rejected as a whole, and the previous one stays
active. With no config at all, deckd runs with blank keys and waits for the file.

Keys are named `<col>x<row>` (StreamController's naming): `0x0 1x0 2x0` on top, `0x1 1x1 2x1`
below. `~` is expanded in `icon` only; `command` goes to `sh -c` as written.

Behaviour worth knowing:

- A key fires on press, immediately. A key with no command does nothing.
- A second press of the same key within `debounce_ms` (default 150) is dropped.
- A press while that key's previous command is still running is dropped. This is on purpose, so
  a slow `adb` cannot pile up. Only the `sh` process is tracked, not what it leaves behind.
- The unit uses `KillMode=process`, so a screenshot in progress survives a deckd restart. The new
  deckd does not know about it, so pressing that key again right after a restart can start a
  second run.
- Unplug and replug: deckd reconnects within about 2 s and redraws.

## The ghost key

This Mini has a faulty membrane under one key: it sends a DOWN by itself when a neighbouring key
is pressed or released (documented in the StreamController fork, `BetterDeck.py`). deckd's answer
is to keep that key **unmapped**: a key with no command cannot run anything, so the ghost is
harmless and the other keys need no delay. It is believed to be one of the unused keys (`0x0` /
`1x0`), but that is **not verified yet**: step 0 of the install checks it. Record the result here.

## Development

```sh
bin/setup-dev     # once per clone: enables the pre-push gate
bin/ci            # fmt --check, clippy -D warnings, tests (incl. the RSS guard)
bin/make-icons    # regenerates icons/*.png (ImageMagick)
```

Environment switches: `DECKD_DEBUG=1` (raw key reports), `DECKD_PROBE_ONLY=1` (enumerate the
deck but never connect; used by the RSS test), `DECKD_TEST_SPAWN=N` (test hook).
