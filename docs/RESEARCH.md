# Design rationale

Why presence is built the way it is: the macOS constraints that shape the architecture, the existing alternatives, and the reasoning behind the DSP chain. Written in 2026 against macOS 27. Items marked *unverified* have not been tested yet.

## 1. Existing alternatives

At the time of writing there is no open-source, app-agnostic virtual microphone that combines an EQ with voice presets, and none with a terminal UI.

| Option | License | Price | Notes |
|---|---|---|---|
| [Curve](https://github.com/tomderham/curve) + BlackHole | AGPL-3 / GPL-3 | Free | Closest open-source match. A menu-bar AU/VST3 host: mic → plugin chain (e.g. Apple's AUNBandEQ) → BlackHole. Presets are DIY, two components to install, GUI only. |
| Element / Carla + BlackHole | GPL | Free | Same idea with heavier, DAW-style plugin hosts. |
| [OpenConnct](https://github.com/trsdn/OpenConnct) | MIT | Free | Ships its own virtual mic with high-pass, gate, compressor and exciter; no EQ or presets. |
| [MicLine](https://github.com/thesammykins/micline) | Apache-2.0 | Free | Requires BlackHole; macOS 27 and Apple Silicon only. |
| Audio Hijack + Loopback | Proprietary | ~$134 bundle | The most polished option: 10-band and parametric EQ, presets, speech denoise. |
| Elgato Wave Link 3 | Proprietary | Free | Runs AU effects on any mic; a mix device is selected per app. |
| Hardware DSP (e.g. Shure MV7+ / MOTIV Mix, Elgato Wave MK.2 VST insert) | Proprietary | Hardware | The only fully transparent option: processing happens on the mic itself. |
| Krisp and similar | Mixed | Varies | Noise suppression only, no EQ. |
| eqMac, SoundSource, Background Music | Mixed | Varies | Output processing only. Mic support in eqMac has been an open request for years. |
| macOS itself | — | — | No input EQ. Mic Modes (Voice Isolation, Wide Spectrum) are only available in supporting apps during calls. |

One trick applies to any virtual mic: make it the system default input, and every app set to "Same as System" / "Default" follows it without per-app configuration.

## 2. Constraint: a microphone can't be modified in place

Checked against the macOS 27 SDK headers and Apple's documentation:

- **Core Audio process taps** (macOS 14.2+) capture output only.
- **AudioServerPlugIn** (HAL plug-in) drivers can publish new devices but cannot wrap or intercept another device. They run sandboxed in their own process and cannot call the client-side HAL API, so a driver can't open the real mic itself.
- **AudioDriverKit**: Apple does not grant the entitlement for virtual audio drivers (WWDC21) and points to AudioServerPlugIn instead.
- **Kernel extensions** require Reduced Security on Apple Silicon, which rules them out for a consumer tool.
- Rogue Amoeba's own mic-effects setup is Audio Hijack feeding a Loopback virtual device.

So every approach comes down to a **virtual input device fed by a user-space process** that captures the real mic. "Transparent" can only mean setting that device as the default input and naming it well.

## 3. Architecture

### v0.1: an existing loopback device

presence captures the mic, processes it, and plays the result into a third-party loopback device (BlackHole or VB-Cable), whose input side call apps select as their microphone.

Because users often make that loopback device the macOS default input, presence never picks a virtual device as its input automatically: it uses the built-in microphone (or another real input) instead and says so. Capturing the device it plays into would feed its output straight back into itself.

### Target: our own virtual device

```
Real mic --AUHAL in--> [agent: resample -> DSP chain] --AUHAL out--> hidden output-only device
                                                                        | (same driver, loopback ring)
Apps <------------------------------------------------ visible input-only "<Real Mic> (presence)" device
```

- **Driver** (C++ on [libASPL](https://github.com/gavv/libASPL), MIT): one visible input-only device and one hidden output-only mirror, joined by a loopback ring buffer. The driver does no DSP. BlackHole's hidden-device pattern is the model, but its code is GPL-3, so it isn't reused. A later option is shared memory or a mach service instead of the output device, saving one IO cycle.
- **Agent** (per-user LaunchAgent registered via SMAppService; needs microphone permission): captures the real mic with a raw AUHAL (not the voice-processing unit), runs the DSP and writes to the hidden device. It should capture only while the visible device has clients; otherwise the orange microphone indicator stays on permanently. An adaptive resampler absorbs clock drift between the two devices, a known gap in plain loopback drivers.
- **Transparency**: set the virtual device as the default input and restore the previous default on clean exit; name it after the real mic; follow the user's chosen source; don't fight macOS auto-switching when a USB mic is plugged in.
- **Failure mode**: if the agent dies, the virtual device delivers silence, because the driver can't reach the mic. Mitigations: launchd KeepAlive, plus a heartbeat that removes the device so the HAL falls back to the real mic (*unverified*).
- **Install**: copy the driver to `/Library/Audio/Plug-Ins/HAL` and restart coreaudiod (`sudo launchctl kickstart -k system/com.apple.audio.coreaudiod`). Distribution needs a signed, notarized package.
- **Frontends**: a SwiftUI menu-bar app and the TUI, both thin clients of the agent over local IPC (unix socket, JSON).
- **Echo cancellation**: on macOS 27, implement `kAudioDevicePropertySuggestedReferenceDevice` on the virtual device so call apps point their echo cancellation at the real output.

## 4. DSP chain

The full chain, at 48 kHz mono, in signal order. v0.1 implements steps 1, 4 and 7 (high-pass, parametric EQ, limiter); the rest is planned (see [ROADMAP.md](ROADMAP.md)).

1. **Input trim and high-pass**: around 80 Hz at 12 dB/oct for most voices, 18–24 dB/oct for noisy rooms and laptop mics. Optional hum notches at 50/60 Hz and harmonics (Q ≈ 30), enabled only when hum is detected.
2. **Noise suppression**, before any boost or compression, with attenuation capped at 12–20 dB to avoid the "underwater" sound. [RNNoise](https://github.com/xiph/rnnoise) by default (about 10 ms of latency and ~1% CPU; BSD code, although the license of its weights has been questioned: xiph/rnnoise#284). [DeepFilterNet3](https://github.com/Rikorose/DeepFilterNet) as an opt-in high-quality mode (MIT/Apache, 20–40 ms).
3. **Downward expander** rather than a hard gate: 1:2 ratio, 10 dB range, 80 ms hold, 200 ms release. RNNoise's voice-activity output can drive it.
4. **EQ**: subtractive cuts, then additive boosts. RBJ cookbook responses, implemented as state-variable filters (Simper/Cytomic), which stay smooth while parameters are modulated.
5. **De-esser**: 5–8 kHz, at most 6 dB of reduction.
6. **Compressor**: 2–3:1, 3–6 dB of gain reduction, 5–10 ms attack, 100 ms release, soft knee.
7. **Limiter** at −1 dBFS.

Preset values and their rationale are in [PRESETS.md](PRESETS.md).

### Noise is mostly not an EQ problem

Cutting the highs removes little background noise. Fan and HVAC rumble and mains hum are low-frequency, which is what the high-pass filter is for. Keyboard clicks and other voices overlap the speech band and need ML noise suppression. Reverb isn't a frequency band at all. Only hiss lives in the highs, and cutting it costs crispness.

### Interaction with call apps

presence runs before the call app's own processing, including its echo cancellation. Linear EQ doesn't disturb echo cancellation; noise suppression, gating and compression are nonlinear and degrade it when the far end plays on speakers. So dynamics stay gentle by default, headphones are recommended, and our own echo cancellation (the voice-processing unit, or WebRTC AEC3 fed by an output tap) is a later option. Running two noise suppressors makes things worse: suggest Zoom's "Original Sound" or noise suppression set to Low, and turning off Meet's noise cancellation.

### Real-time rules

No allocation, locks, Objective-C messaging or Swift ARC in the audio callback. Parameters cross to the audio thread through lock-free structures, and changes are smoothed over 20–50 ms. The spectrum analyzer receives samples through an SPSC ring and runs its FFT on the UI thread.

### Latency

Estimated added latency: about 15–22 ms with RNNoise, 25–50 ms with DeepFilterNet3. These are estimates to be measured.

## 5. Open risks

1. Whether coreaudiod loads an ad-hoc-signed driver on Apple Silicon. This decides how an open-source driver can be distributed.
2. Races over the default input device, and USB auto-switching.
3. Falling back to the real mic when the agent dies.
4. Resampler quality under long-running clock drift.
5. Whether Voice Isolation applies to a virtual device.
6. Keeping the microphone indicator accurate: detecting per-device "running somewhere" in the mirrored-device setup.

## Sources

- libASPL: https://github.com/gavv/libASPL
- BlackHole: https://github.com/ExistentialAudio/BlackHole
- roc-vad (a virtual device built on libASPL): https://github.com/roc-streaming/roc-vad
- Background Music (default-device handling): https://github.com/kyleneideck/BackgroundMusic
- Apple, "Creating an Audio Server Driver Plug-in": https://developer.apple.com/documentation/coreaudio/creating-an-audio-server-driver-plug-in
- WWDC21, "Create audio drivers with DriverKit": https://developer.apple.com/videos/play/wwdc2021/10190/
- Audio EQ Cookbook (RBJ): https://www.w3.org/TR/audio-eq-cookbook/
- RNNoise: https://github.com/xiph/rnnoise
- DeepFilterNet: https://github.com/Rikorose/DeepFilterNet
- Rogue Amoeba, mic effects with Audio Hijack: https://rogueamoeba.com/support/knowledgebase/?showArticle=AH-MicEffects
