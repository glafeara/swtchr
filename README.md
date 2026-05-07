# swtchr

Keyboard layout autoswitcher for Arch Linux + Hyprland. Watches your typing,
notices when a word was entered in the wrong layout (English ↔ Russian only),
deletes it, switches the layout, and retypes it correctly. Detection runs on
each word boundary (space, Tab, Enter, punctuation).

It is a Wayland-native take on Punto Switcher / xneur. Because Wayland blocks
app-level global keyboard hooks by design, swtchr captures and injects input
at the kernel level via `evdev` (`/dev/input/event*`) and `uinput`, and only
talks to Hyprland for layout state.

## Requirements

- Arch Linux + Hyprland 0.40+
- Rust 1.85+ (edition 2024)
- `libxkbcommon`
- Hunspell English dictionary (the `hunspell-en_us` package ships both
  `en_US.dic` and `en_US-large.dic`; we use the large variant by default):
  ```
  sudo pacman -S hunspell-en_us
  ```
- Russian word list — fetched automatically by `install.sh` from the
  [FrequencyWords](https://github.com/hermitdave/FrequencyWords) project
  (top-200k Cyrillic forms from OpenSubtitles-2018, CC-BY-SA-4.0), merged
  with a curated supplement (`dist/ru_supplement.txt`) that patches
  post-2018 vocabulary (covid, AI/LLM, релокация) and IT-jargon inflections.
  To re-fetch later:
  ```
  ./dist/fetch-dicts.sh
  ```
  We use a frequency list rather than `hunspell-ru` because Hunspell `.dic`
  files store only lemmas — and without affix expansion every Russian
  inflection (cases, tenses, gender) misses. The frequency list ships
  pre-inflected forms.

  Without these dictionaries, auto-detection silently falls back to no-op
  (manual mode still works).
- `wl-clipboard` (only for the selection-swap command):
  ```
  sudo pacman -S wl-clipboard
  ```
- User in the `input` group (one-time setup, see install.sh).

## Install

```sh
git clone https://github.com/glafeara/swtchr.git
cd swtchr
./dist/install.sh
```

`install.sh` does:

1. `cargo build --release`
2. installs the binary to `~/.local/bin/swtchr`
3. installs the udev rule for `/dev/uinput` (needs `sudo` once)
4. fetches the Russian word list to `~/.local/share/swtchr/dicts/ru_top200k.txt`
5. drops a default config at `~/.config/swtchr/config.toml` (only on fresh
   installs — existing configs are left alone)
6. installs and starts the systemd user unit `swtchr.service`

If you upgrade an existing install, `install.sh` reuses your current config.
To pick up new defaults (e.g. switched dictionary paths), edit the
`[dictionaries]` section of `~/.config/swtchr/config.toml` by hand and
`systemctl --user restart swtchr`.

If you weren't already in the `input` group, log out and back in after the
install.

## Usage

| Action | What it does |
|---|---|
| Type normally | swtchr inspects each word at the boundary (space/Tab/Enter/`.,;:!?`) and silently corrects it if the swapped form is a real word in the other language |
| `swtchr swap` | swap layout on the current Wayland selection (highlight text, run command) |
| `swtchr ping` | health-check the running daemon |
| `journalctl --user -u swtchr -f` | follow logs |
| `systemctl --user restart swtchr` | reload after a config change |

### Hotkey for selection swap

`swtchr swap` is the user-facing trigger; bind it to a hotkey in
`~/.config/hypr/hyprland.conf`:

```
bind = SUPER, BackSlash, exec, swtchr swap
```

Highlight text in any window, hit the bind, and swtchr backspaces the
selection and retypes it in the other layout. The daemon listens on a
per-user unix socket at `$XDG_RUNTIME_DIR/swtchr.sock`.

## Configuration

Default location: `~/.config/swtchr/config.toml`. See `dist/config.example.toml`
for the full schema. Common knobs:

```toml
[general]
idle_reset_ms = 800        # drop the word buffer after this long with no typing

[dictionaries]
en = "/usr/share/hunspell/en_US-large.dic"
ru = "$HOME/.local/share/swtchr/dicts/ru_top200k.txt"
extra_words = ["systemd", "tmux", "swtchr", "hyprland"]
```

Reload by restarting the unit.

## How it works

```
┌─────────────────┐     ┌─────────────┐
│ /dev/input/...  │──▶  │ EvdevReader │──▶┐
└─────────────────┘     └─────────────┘   │
                                          │   ┌────────────────┐
┌─────────────────┐                       ├──▶│  Service loop  │
│ Hyprland IPC    │──▶  ActiveLayout/Win ─┘   │  (CoreState +  │
└─────────────────┘                           │   XkbState)    │
                                              └──────┬─────────┘
                                                     │
              detect mismatch ──────┐                ▼
                                    │      ┌────────────────────┐
              hyprctl switch ◀──────┴──────│   UinputInjector   │──▶ /dev/uinput
                                           │  BS×N + replay     │
                                           └────────────────────┘
```

1. Each evdev keyboard device is read in its own tokio task.
2. `xkbcommon` decodes evdev keycodes to UTF-8 with the *current* Hyprland
   layout. Modifier state is tracked separately for the word buffer.
3. On a word boundary, the **detector** asks: "is this word in the current
   layout's dictionary? if not, would it be a valid word in the other
   layout's dictionary?" If yes-then-no — switch and retype.
4. **Replay**: backspace the wrong word + boundary char, dispatch
   `hyprctl switchxkblayout current next`, wait briefly for the new layout
   to take effect, replay the *same evdev keycodes* (which now produce the
   correct characters in the new layout), and re-emit the boundary key.
5. The injector device is named `swtchr-virtual`; the readers filter that
   exact name to avoid feedback loops.

## Troubleshooting

**The service starts but typing nothing happens.**
Check that the dictionaries are installed:
- `ls /usr/share/hunspell/en_US-large.dic` (English)
- `ls ~/.local/share/swtchr/dicts/ru_top200k.txt` (Russian, run `./dist/fetch-dicts.sh` if missing)

Without them auto-detection is a no-op.

**`Permission denied` on `/dev/uinput`.**
You're not in the `input` group, or the udev rule didn't apply. Run
`getfacl /dev/uinput`; you should see your user with `rw-`. Otherwise:
`sudo udevadm control --reload && sudo udevadm trigger --name-match=uinput`,
or re-run `dist/install.sh`.

**Replay produces wrong characters.**
The xkb layout list reported by `hyprctl getoption input:kb_layout` must match
what's in `[general] default_layout`. swtchr trusts Hyprland and queries it on
startup; if your Hyprland config changes, restart the unit.

## Development

```
cargo test          # unit tests across config/xkb/dict/detector/state/service
cargo build         # debug build
cargo build --release
RUST_LOG=swtchr=debug ./target/debug/swtchr --decode-only   # decode-only smoke test
```

The crate has both a `lib` and a `bin` target so most logic is testable
without root or a Hyprland session.

## License

MIT.
