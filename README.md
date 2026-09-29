# presence

**Make your mic sound better in every app on your Mac.**

presence is a small terminal app that EQs your microphone system-wide. Pick a preset (or shape the curve yourself), and every call app — Zoom, Meet, FaceTime, Slack, Discord — hears the processed voice. No per-app plugins, no DAW.

```
your mic ──► presence (EQ) ──► virtual device ──► call app picks it as its mic
```

macOS doesn't let apps modify a microphone in place, so presence plays its output into a virtual loopback device (BlackHole or VB-Cable), and your apps use that device as their mic. See [docs/RESEARCH.md](docs/RESEARCH.md) for why.

> Status: v0.1 — early, but tested end to end. EQ only for now; noise suppression and dynamics are next ([roadmap](docs/ROADMAP.md)).

## Setup

1. **Install a virtual loopback device** (once):
   ```sh
   brew install blackhole-2ch    # or install VB-Cable
   ```
2. **Install presence** (needs [Rust](https://rustup.rs)):
   ```sh
   git clone https://github.com/ilias-t/presence
   cd presence
   cargo install --path crates/presence
   ```
   This puts `presence` in `~/.cargo/bin`, so you can run it from anywhere.
3. **Run it:**
   ```sh
   presence
   ```
   The first time, macOS asks for microphone access for your terminal — click Allow.
4. **Choose the virtual device as your mic** — either per app (e.g. Zoom → Settings → Audio → Microphone → "BlackHole 2ch"), or system-wide in **System Settings → Sound → Input**, which apps like FaceTime and browsers follow.

presence always captures a real microphone: if your default input is the virtual device, it uses your built-in mic instead. To use a different mic, press `d` in the TUI or pass `--input "<name>"` (see `presence devices`).

## Usage

```sh
presence                          # the TUI
presence run --preset broadcast   # headless; Ctrl-C to stop
presence presets                  # list presets
presence devices                  # list audio devices
```

Flags: `--input <name>`, `--output <name>`, `--config <path>`. Device names match case-insensitively, and any unique part of a name works (`--input macbook`).

In the TUI, press `?` for all keys. The ones you'll use most:

| Key | Action |
|---|---|
| `↑↓` / `Enter` | Pick a preset |
| `b` | Bypass — A/B the processed and raw mic |
| `Tab` | Switch between presets and the band editor |
| `←→` `=` `-` | Select a field and adjust it (`+` `_` for bigger steps) |
| `t` / `space` | Change filter type / toggle a band |
| `s` | Save your settings as a preset |
| `d` | Choose input and output devices |
| `q` | Quit |

Settings and your presets are saved in `~/Library/Application Support/presence/config.toml`.

## Presets

**Voice** — subtle, loudness-matched to the raw mic so bypass A/B is fair:

| Preset | What it does |
|---|---|
| `clarity` (default) | Crisp calls: less boxiness, more presence and air |
| `natural` | Subtle cleanup: less rumble and mud, your voice as is |
| `broadcast` | Radio/podcast warmth: fuller chest, smooth top |
| `laptop` | Built-in mics: less fan rumble and boxiness, more body |
| `headset` | Headset/AirPods mics: less nasal, clearer words |
| `flat` | No EQ, safety limiter only |

**Fun** — exaggerated on purpose, to hear that presence is working: `telephone`, `megaphone`, `next-room`, `late-night`.

Details, exact settings, and measurements: [docs/PRESETS.md](docs/PRESETS.md).

## Tips

- **Turn off your call app's noise suppression**, or set it to low, so your voice isn't processed twice (Zoom: "Original sound"; Meet: noise cancellation off).
- **Use headphones.** Speakers can leak into the mic, and the call app's echo cancellation runs after presence.
- **Cutting highs doesn't remove background noise.** Rumble and hum are low — every voice preset already cuts them. Keyboard clicks and other voices overlap speech; that needs noise suppression, which is on the roadmap.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

| Crate | What it does |
|---|---|
| `crates/presence-dsp` | Real-time EQ (state-variable filters matching the RBJ cookbook), smoothing, limiter, presets |
| `crates/presence-engine` | CoreAudio devices, mic → DSP → virtual device, drift-compensating resampler, meters |
| `crates/presence` | The CLI and TUI |

Hear a preset offline: `cargo run -p presence-dsp --example render -- --preset telephone in.wav out.wav`.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
