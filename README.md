# quran-tui

A terminal UI Quran audio player written in Rust — a cross-platform port of the
macOS SwiftUI `MultiBluetoothQuran` app.

It runs in two modes from one codebase:

1. **Standalone player (default).** Plays on the system's default audio output.
   No Bluetooth, no device setup — open the app, pick a surah, press play.
2. **Multi-output mode (optional).** Route different surahs/reciters to several
   audio output devices at once (e.g. one headphone per family member).

Mode 1 is just "exactly one output channel"; mode 2 is "N channels". There is no
separate code path.

## Build

Requires a recent stable Rust toolchain.

```sh
cargo build --release      # binary at target/release/quran-tui
cargo run                  # debug run
```

Or with [`just`](https://github.com/casey/just):

```sh
just run                   # run
just check                 # fmt-check + clippy + test + build
```

## Run

```sh
quran-tui                              # default audio output
quran-tui --audio-dir ../TestAssets    # use a specific audio directory
quran-tui --log-level debug            # error | warn | info | debug | trace
quran-tui --version
quran-tui --help
```

The TUI needs a terminal of at least **80×24**; below that it shows a resize
prompt until the window is enlarged.

## Keybindings

Press `?` in the app for the full list. Summary:

| Key | Action |
|---|---|
| `1`–`4` | Jump to tab (Now Playing / Browse / Outputs / Presets) |
| `Tab` / `Shift+Tab` | Cycle tabs (or fields, within Browse / Outputs) |
| `Space` | Play / pause the focused output |
| `s` | Stop the focused output |
| `n` / `p` | Next / previous track |
| `l` | Toggle loop |
| `+` / `-` | Volume ±5% |
| `A` / `P` / `S` | Play all / pause all / stop all (multi-output) |
| `←` / `→` | Change reciter (Browse) |
| `/` | Search surahs (Browse) |
| `?` | Toggle help |
| `q` / `Ctrl+C` | Quit |

## Audio storage

`audio_root` is where local audio lives and where downloads land. It is resolved
in this order:

1. the `--audio-dir` CLI flag
2. the `audio_root` value in `config.json`
3. `<data_dir>/audio` (created on first run)

`config.json` and `presets.json` live in the OS config/data directories
(`directories` crate, `dev.local.quran-tui`). The log file is written to the OS
cache directory as `quran-tui.log` — never to stdout.

Audio files are resolved per-ayah first
(`<audio_root>/<reciter-slug>/<NNN>/<AAA>.mp3`), then fall back to a whole-surah
file (`<audio_root>/<NNN>-<surah-slug>-<reciter-slug>.mp3`). Missing per-ayah
files are downloaded on demand from [everyayah.com](https://everyayah.com) with
visible progress, then cached.

## Platform notes & limitations

- **macOS** — `cpal` → CoreAudio. Multi-output works well. Primary dev target.
- **Linux** — `cpal` → ALSA. Under PulseAudio/PipeWire individual sinks may
  collapse into one "default" entry, weakening multi-output. The standalone
  player always works.
- **Windows** — `cpal` → WASAPI. Both modes work; tested under Windows Terminal.
- **No Bluetooth detection.** `cpal` cannot report a device's transport type, so
  the app cannot reliably distinguish Bluetooth devices from built-in/USB ones.
  Every output device is listed by name; a `BT?` tag is a cosmetic name
  heuristic only. "Multi-Bluetooth" is effectively "multi-output".
- **Manual device refresh.** `cpal` has no portable hotplug callback. The device
  list is enumerated once at startup and re-enumerated when you press `r` on the
  Outputs tab.

## Architecture

- **Main thread** owns all `App` state and the ratatui render loop. It never
  blocks — input is polled with a timeout, background results arrive as messages.
- **One engine thread per output channel.** `rodio`'s `OutputStream`/`Sink` are
  `!Send`, so each output device gets a dedicated long-lived thread that owns its
  stream for its whole life and processes commands from a channel.
- **Download workers** are short-lived threads spawned per download job.
- All concurrency is plain OS threads + `crossbeam-channel`; there is no async
  runtime.

See `RUST_TUI_PLAN.md` in the repository root for the full design.
