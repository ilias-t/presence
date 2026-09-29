# Roadmap

Background and sources for the decisions below are in [RESEARCH.md](RESEARCH.md).

## Phase 1 — v0.1: EQ into an existing virtual device (shipped)

- `presence-dsp`: high-pass + 8 parametric bands (state-variable filters matching the RBJ cookbook), input/output gain, −1 dBFS safety limiter, parameter smoothing, bypass crossfade, built-in voice presets.
- `presence-engine` (cpal 0.16): mic capture → DSP → playback into BlackHole / VB-Cable, adaptive-ratio resampler that absorbs clock drift, adaptive ring headroom after underruns, meters, spectrum tap. Device listing reads CoreAudio HAL properties directly, so it never touches the microphone. The mic is opened at its current nominal rate.
- Virtual-input safety: automatic input selection never captures a virtual loopback device, even when it's the macOS default input; it uses the built-in mic (or another real input) and says so. An explicitly chosen virtual input works but warns, and capturing the output device itself is refused.
- Microphone permission preflight and periodic re-check: presence refuses to start, or stops, while access is denied instead of running with a silent mic.
- Single-instance lock, so two copies can't both play into the virtual device.
- `presence` TUI: presets, band editor, EQ curve + live spectrum, meters, bypass A/B, device picker (virtual devices marked, confirmation before an audible output), user presets, config persistence with corrupt-file backup. Headless `presence run`, `presence devices`, `presence presets`.

Limitation: users need a third-party loopback device and must select it (or make it the system default input). Presets are EQ-only.

## Phase 2 — Voice processing chain

Complete the chain described in RESEARCH.md §4, in signal order:

1. **Noise suppression**: RNNoise via [`nnnoiseless`](https://crates.io/crates/nnnoiseless), a pure-Rust port (48 kHz, 10 ms frames, negligible CPU). Cap attenuation at 12–20 dB to avoid the "underwater" sound. Runs before any boost or compression.
2. **Downward expander**: 1:2 ratio, 10 dB range. Its trigger can be RNNoise's voice-activity output.
3. **De-esser**: split band, 5–8 kHz, at most 6 dB of reduction.
4. **Compressor**: 2–3:1, soft knee. Presets gain their intended dynamics (see PRESETS.md).
5. **Hum notches**: 50/60 Hz plus harmonics, enabled only when hum is detected.
6. Keep dynamics gentle by default. Call apps run echo cancellation *after* us, and nonlinear processing hurts it.
7. Opt-in **DeepFilterNet3** "HQ" noise suppression (20–40 ms added latency).

## Phase 3 — Our own virtual microphone (HAL driver)

Removes the dependency on VB-Cable / BlackHole and makes presence feel native.

- `driver/`: AudioServerPlugIn in C++ on [libASPL](https://github.com/gavv/libASPL) (MIT). It exposes one visible input-only device, named `<Real Mic> (presence)`, and one hidden output-only mirror joined by a loopback ring buffer. The driver does no DSP.
- Engine outputs to the hidden device (`KNOWN_VIRTUAL_DEVICES` already lists "presence" first).
- Auto-set it as the system default input on start. Restore the previous default on clean exit. Follow the user's chosen real mic, and don't fight macOS auto-switching when a USB mic is plugged in.
- Capture the real mic only while the virtual device has clients, so the orange mic indicator isn't always on.
- Heartbeat: if the agent dies, remove the device so the HAL falls back to the real mic instead of delivering silence.
- Install script: copy to `/Library/Audio/Plug-Ins/HAL`, then `launchctl kickstart -k system/com.apple.audio.coreaudiod`.
- **Spike first**: does coreaudiod load an ad-hoc-signed driver on Apple Silicon? The answer decides the open-source distribution story.

## Phase 4 — Background agent + clients

- Split the engine into a daemon (`presenced`) inside a `.app` bundle. The bundle is needed so macOS grants the Microphone permission to presence rather than to the terminal. Register it as a LaunchAgent via SMAppService, with KeepAlive.
- Local IPC: a unix socket with a JSON protocol for settings, meters and spectrum.
- The TUI becomes a thin client, so closing it no longer stops the audio.
- CLI verbs over the socket: `presence preset clarity`, `presence bypass`, `presence status`.

## Phase 5 — Menu-bar app & distribution

- SwiftUI menu-bar client: preset picker, bypass, level meter.
- Developer ID signing + notarized `.pkg` (driver + app), Homebrew cask.

## Backlog / ideas

- Input channel selection. v0.1 always captures channel 0, which gives silence on multi-input interfaces where the mic is on input 2.
- Follow nominal-rate changes on the mic made by other apps mid-session. Today they'd push the rate beyond the resampler's ±0.5% correction and cause constant xruns.
- A "no signal" warning when the input stays at digital silence (disconnected phone mic, wrong channel).
- Echo cancellation reference: macOS 27 `kAudioDevicePropertySuggestedReferenceDevice` on the virtual device.
- Own AEC: WebRTC AEC3 fed by a Core Audio process tap of system output.
- Auto-EQ: analyze the voice's long-term spectrum and suggest corrections toward a target curve.
- Per-mic profiles: remember settings per input device and switch them automatically.
- A/B loudness matching, so bypass comparisons aren't fooled by level differences.
- Latency measurement tool (loopback impulse).

## Open questions to verify

1. Ad-hoc-signed HAL driver loading on Apple Silicon (Phase 3 blocker).
2. Default-input races with macOS device auto-switching.
3. Device-removal fallback when the agent dies.
4. Resampler quality under long-running drift.
5. Whether macOS Voice Isolation applies to a virtual device.
6. Per-device "running somewhere" detection in the mirrored-device setup.
