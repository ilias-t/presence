# Built-in presets

Defined in `crates/presence-dsp/src/presets.rs`. Every preset is EQ only:
high-pass → up to 8 RBJ-cookbook bands → output gain → −1 dBFS safety limiter
(always on). Lists show them in three sections: **VOICE**, **FUN**, then your
own presets under **USER**. The default is `clarity`.

- **Voice** presets are what you use every day. They follow standard voice-EQ
  practice (see [Sources](#sources)): a proper low cut, a small cut in the
  200–550 Hz "mud/boxiness" region, a gentle presence lift at 3–4.5 kHz and a
  soft air shelf at 8–12 kHz. Every band stays within ±4 dB with broad Qs
  (0.7–1.2), nothing is boosted in the 5–8 kHz sibilance zone except the tail
  of a shelf, and the output gain is set so the preset is no louder or quieter
  than `flat`. A preset that is simply louder always sounds "better" in an A/B;
  loudness matching removes that bias.
- **Fun** presets are deliberately exaggerated so you can hear instantly that
  audio really flows through presence. They are loudness-matched too, and stay
  clear of the limiter at normal levels.

| id | Name | Category | What it does | When to use |
|---|---|---|---|---|
| `flat` | Flat | voice | No EQ: unity gain, safety limiter only. | Reference for A/B; unlike bypass (`b`), the limiter stays on. |
| `natural` | Natural | voice | Subtle cleanup: less rumble and mud, your voice as is. | Good mics (USB condenser, dynamic) that only need tidying. |
| `clarity` (default) | Clarity | voice | Crisp calls: less boxiness, more presence and air. | Most mics and most calls. Start here. |
| `broadcast` | Warm broadcast | voice | Radio/podcast voice: fuller chest, less mud, smooth top. | Thin-sounding voices or mics; podcasts and streams. |
| `laptop` | Laptop mic | voice | Built-in mics: less fan rumble and boxiness, more body. | MacBook and other built-in mics. |
| `headset` | Headset / AirPods | voice | Headset and Bluetooth mics: less nasal, clearer words. | Boom headsets, AirPods and other Bluetooth mics. |
| `telephone` | Old telephone | fun | Squeezes your voice into a 1990s phone line. | Sanity check: unmistakably band-limited. |
| `megaphone` | Megaphone | fun | Honky, ringing bullhorn. Attention, please. | Sanity check: loud, narrow and resonant. |
| `next-room` | Through the wall | fun | Muffled, like you're talking from the next room. | Sanity check: almost all treble gone. |
| `late-night` | Late-night DJ | fun | Big, boomy radio bass with extra sizzle. | Sanity check: extreme "smile" curve. |

## Exact settings

PK = peak, LS/HS = low/high shelf (Q 0.707, the cookbook equivalent of slope
S = 1), LP/HP = low/high-pass band. The high-pass filter is either 12 dB/oct
(one Butterworth biquad, −3 dB at the cutoff) or 24 dB/oct (two cascaded
Butterworth biquads, −6 dB at the cutoff). Frequencies in Hz.

| id | High-pass | Bands | Output gain |
|---|---|---|---:|
| `flat` | off | – | 0 dB |
| `natural` | 80, 12 dB/oct | PK 300 −1.5 dB Q0.8; PK 4.5k +1 dB Q0.7; HS 12k +1.5 dB | +0.5 dB |
| `clarity` | 80, 24 dB/oct | PK 280 −2 dB Q1.0; PK 3.5k +2.5 dB Q1.0; HS 10k +2.5 dB | +0.5 dB |
| `broadcast` | 70, 12 dB/oct | LS 150 +3 dB; PK 400 −2.5 dB Q1.2; PK 3k +1.5 dB Q0.8; HS 12k +2 dB | 0 dB |
| `laptop` | 110, 24 dB/oct | PK 200 +2 dB Q0.9; PK 550 −2.5 dB Q1.2; PK 3k +1.5 dB Q1.0; HS 10k +1.5 dB | 0 dB |
| `headset` | 90, 12 dB/oct | PK 400 −1.5 dB Q1.0; PK 1.1k −1.5 dB Q1.2; PK 3.2k +2 dB Q0.9; HS 8k +1.5 dB | +0.5 dB |
| `telephone` | 300, 24 dB/oct | LP 3.4k (4th-order Butterworth: Q 0.541 + Q 1.307); PK 1.5k +4 dB Q1.0 | +3.5 dB |
| `megaphone` | 450, 24 dB/oct | HP 450 Q0.707 (36 dB/oct in total); LP 4k (4th-order Butterworth); PK 1.1k +6 dB Q2.5; PK 2.4k +5 dB Q3.0 | +4.5 dB |
| `next-room` | 50, 12 dB/oct | LP 500 (4th-order Butterworth) + LP 500 Q0.707: ~36 dB/oct above 500 Hz | +2 dB |
| `late-night` | 45, 24 dB/oct | LS 220 +12 dB; PK 1k −8 dB Q0.7; HS 5k +6 dB | −4.5 dB |

Why each voice curve looks the way it does:

- **`natural`**: the smallest useful move. A gentle 80 Hz cut for rumble and
  plosives, 1.5 dB less mud, and just a hint of presence and air.
- **`clarity`**: the classic voice curve. The 24 dB/oct cut at 80 Hz removes
  desk thumps and HVAC rumble; the response is back within 2 dB by 120 Hz, so
  low male voices keep their body. A 2 dB dip at 280 Hz takes out the boxy "small room"
  sound; +2.5 dB at 3.5 kHz is where consonants live, and the 10 kHz shelf
  adds air above the sibilance zone instead of in it.
- **`broadcast`**: a low shelf adds chest (the "proximity" warmth of a radio
  mic) while a 400 Hz cut keeps it from turning muddy; presence is lighter and
  broader so the top stays smooth.
- **`laptop`**: built-in mics pick up fan and chassis rumble and sound small
  and roomy. A steep 110 Hz cut removes the rumble, +2 dB at 200 Hz restores
  some body, and −2.5 dB at 550 Hz removes the boxy room tone.
- **`headset`**: headset and earbud mics tend to sound nasal and boxy. Broad
  cuts at 400 Hz and 1.1 kHz, then presence at 3.2 kHz. The shelf starts at
  8 kHz so it still lifts the top of a Bluetooth mic's band without pushing
  sibilance on wired boom mics.

Across the audible range above twice the high-pass cutoff, no voice preset
moves more than 2.5 dB from its output gain (the unit tests allow ±4 dB).

## Measurements

Every preset was rendered over two synthesised voices (macOS `say`: Samantha,
US female, and Daniel, UK male, ~27 s in total, resampled to 48 kHz mono and
normalised to a −6 dBFS peak) with the render example below, and compared with
`flat`:

- **K-wtd Δ**: loudness change, K-weighted RMS (ITU-R BS.1770 weighting,
  ungated). This is what the output gains are tuned on.
- **RMS Δ**: unweighted RMS change.
- **peak / pre-limiter peak**: sample peak in dBFS with and without the
  limiter. Equal values mean the limiter never engaged.
- **Octave columns**: energy change per octave band (centre frequency, dB).

| id | K-wtd Δ | RMS Δ | peak | pre-limiter peak | 125 | 250 | 500 | 1k | 2k | 4k | 8k | 16k |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| `flat` | +0.0 | +0.0 | -6.0 | -6.0 | +0.0 | +0.0 | +0.0 | +0.0 | +0.0 | +0.0 | +0.0 | +0.0 |
| `natural` | -0.2 | -0.4 | -6.2 | -6.2 | -0.6 | -0.7 | -0.5 | +0.2 | +0.8 | +1.3 | +1.2 | +1.2 |
| `clarity` | -0.2 | -0.5 | -6.7 | -6.7 | -1.1 | -1.1 | -0.5 | +0.4 | +1.5 | +2.6 | +1.8 | +1.9 |
| `broadcast` | -0.2 | -0.3 | -6.5 | -6.5 | +0.6 | -0.3 | -1.7 | -0.4 | +0.8 | +1.2 | +0.6 | +0.6 |
| `laptop` | +0.0 | -0.0 | -8.2 | -8.2 | -0.0 | +0.5 | -1.1 | -0.9 | +0.6 | +1.1 | +0.7 | +0.8 |
| `headset` | -0.2 | -0.4 | -6.8 | -6.8 | -0.3 | -0.4 | -0.9 | -0.8 | +0.9 | +2.0 | +1.7 | +1.8 |
| `telephone` | +0.1 | -0.3 | -2.9 | -2.9 | -13.8 | -4.9 | +2.3 | +5.2 | +6.1 | +1.4 | -16.2 | -32.0 |
| `megaphone` | -0.2 | -1.2 | -2.1 | -2.1 | -31.7 | -14.4 | -1.5 | +6.7 | +8.0 | +4.7 | -11.8 | -26.1 |
| `next-room` | -0.2 | +0.3 | -8.8 | -8.8 | +1.9 | +1.6 | -1.8 | -10.8 | -28.0 | -49.1 | -77.0 | -97.7 |
| `late-night` | +0.2 | +0.5 | -7.6 | -7.6 | +3.5 | +1.0 | -5.9 | -10.4 | -8.3 | -3.9 | +0.6 | +1.0 |

`flat`: −21.8 dB K-weighted RMS, peak −6.0 dBFS. Taking each voice on its own,
every voice preset is within ±0.5 dB of `flat` and every fun preset within
±1.0 dB. No preset reaches the limiter at this level (the fun presets peak at
−2.1 dBFS or lower); for hotter input the limiter holds the output at −1 dBFS.

Caveats:

- `say` synthesises at 22.05 kHz, so the voices have no content above 11 kHz.
  The 8k and 16k columns only see the 6–11 kHz part of the speech (the 16k
  band's lower skirt), so they understate the air shelves.
- To check the top octave, the same render over 10 s of speech-shaped noise
  (flat to 500 Hz, −6 dB/oct above, rolled off below 100 Hz; full band at
  48 kHz) shows the air shelves at work: +1.4 to +2.8 dB above 11.3 kHz for
  the voice presets, whose loudness stays within ±0.6 dB. The fun presets
  diverge by up to ±4 dB on noise, because they keep a narrow slice of the
  spectrum where noise and speech are balanced differently; they are matched
  on speech, which is what you'll feed them.

The unit test `presets_are_loudness_matched_to_flat` repeats the check on
synthetic speech-shaped noise: voice presets must stay within ±1.5 dB of flat.

## Hear them offline

Render any WAV (any sample rate; stereo is downmixed to mono) through a preset,
without touching your audio devices:

```sh
cargo run -p presence-dsp --example render -- --preset clarity in.wav out.wav
```

It prints the loudness and peak compared with `flat`. `--table IN.wav ...`
renders every built-in preset over the inputs and prints the measurement table
above. To make test speech on macOS:

```sh
say -v Samantha -o s1.aiff "Thanks for joining the call. Can everyone hear me clearly?"
ffmpeg -loglevel error -i s1.aiff -ar 48000 -ac 1 s1.wav
```

## EQ is not noise removal

"Cutting the highs removes background noise" is mostly wrong. Fan and HVAC
rumble and mains hum are *low*: the high-pass filter already does that work in
every voice preset. Keyboard clicks, dogs and other voices overlap the speech
band, so no EQ can remove them without removing your voice too; that takes
noise suppression, which is on the roadmap. Reverb is not a frequency band at
all. Only hiss lives in the highs, and cutting it costs crispness.

## Sources

- Sound On Sound, "How To Make Your Vocals Twice As Good! Part 2":
  <https://www.soundonsound.com/techniques/how-make-your-vocals-twice-good-part-2>
- Shure, "EQ I.Q.: A Quick Primer": <http://blog.shure.com/eq-i-q-a-quick-primer/>
- Behind The Mixer, "How to EQ Speech for Maximum Intelligibility":
  <https://www.behindthemixer.com/how-eq-speech-maximum-intelligibility/>
- Music Guy Mixing, "Vocal EQ Guide": <https://www.musicguymixing.com/vocal-eq/>
- Proximity effect: <https://en.wikipedia.org/wiki/Proximity_effect_(audio)>
- ITU-R BS.1770, loudness measurement and K-weighting:
  <https://www.itu.int/rec/R-REC-BS.1770>
- Robert Bristow-Johnson, Audio EQ Cookbook (filter designs):
  <https://www.w3.org/TR/audio-eq-cookbook/>
- The telephone preset's 300–3400 Hz band is the classic analogue telephone
  voice channel (ITU-T G.712).
