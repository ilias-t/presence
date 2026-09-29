//! Device enumeration and stream-configuration choice.

use cpal::BufferSize;
use cpal::traits::{DeviceTrait, HostTrait};

use crate::{
    DeviceInfo, EngineError, KNOWN_VIRTUAL_DEVICES, PREFERRED_SAMPLE_RATE, is_virtual_device,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Input,
    Output,
}

impl Direction {
    fn other(self) -> Self {
        match self {
            Direction::Input => Direction::Output,
            Direction::Output => Direction::Input,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Direction::Input => "input",
            Direction::Output => "output",
        }
    }
}

fn backend<E: std::fmt::Display>(e: E) -> EngineError {
    EngineError::Backend(e.to_string())
}

/// What we need to know about a device to list it and pick a stream config.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DeviceCaps {
    /// HAL `AudioObjectID` on macOS (how the stream's device is resolved);
    /// enumeration index elsewhere.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub id: u32,
    pub name: String,
    pub channels: u16,
    /// Current nominal rate.
    pub default_rate: u32,
    /// Inclusive (min, max) Hz ranges the device can run at.
    pub rate_ranges: Vec<(u32, u32)>,
    /// Inclusive (min, max) IO buffer size in frames, if known.
    pub buffer_range: Option<(u32, u32)>,
    pub is_default: bool,
    /// The machine's own hardware (CoreAudio transport type "built-in"), e.g.
    /// the MacBook microphone. Always `false` off macOS.
    pub built_in: bool,
    /// CoreAudio transport type "virtual": a software device such as a
    /// loopback driver. Always `false` off macOS.
    pub virtual_transport: bool,
}

impl DeviceCaps {
    pub(crate) fn info(&self) -> DeviceInfo {
        DeviceInfo {
            name: self.name.clone(),
            is_default: self.is_default,
            channels: self.channels,
            default_sample_rate: self.default_rate,
        }
    }
}

/// All devices with at least one channel in `dir`. On macOS this reads HAL
/// properties directly and never touches the microphone (see `hal`).
#[cfg(target_os = "macos")]
pub(crate) fn caps(dir: Direction) -> Result<Vec<DeviceCaps>, EngineError> {
    Ok(crate::hal::devices(dir))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn caps(dir: Direction) -> Result<Vec<DeviceCaps>, EngineError> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let (devices, default) = match dir {
        Direction::Input => (
            host.input_devices().map_err(backend)?.collect::<Vec<_>>(),
            host.default_input_device(),
        ),
        Direction::Output => (
            host.output_devices().map_err(backend)?.collect::<Vec<_>>(),
            host.default_output_device(),
        ),
    };
    let default_name = default.and_then(|d| d.name().ok());
    let mut out = Vec::new();
    for (id, device) in (0u32..).zip(devices) {
        let Ok(name) = device.name() else { continue };
        let (config, ranges) = match dir {
            Direction::Input => (
                device.default_input_config(),
                device
                    .supported_input_configs()
                    .map(|r| r.collect::<Vec<_>>()),
            ),
            Direction::Output => (
                device.default_output_config(),
                device
                    .supported_output_configs()
                    .map(|r| r.collect::<Vec<_>>()),
            ),
        };
        let Ok(config) = config else { continue };
        let buffer_range = match config.buffer_size() {
            cpal::SupportedBufferSize::Range { min, max } => Some((*min, *max)),
            cpal::SupportedBufferSize::Unknown => None,
        };
        out.push(DeviceCaps {
            id,
            is_default: default_name.as_deref() == Some(name.as_str()),
            name,
            channels: config.channels(),
            default_rate: config.sample_rate().0,
            rate_ranges: ranges
                .unwrap_or_default()
                .iter()
                .map(|r| (r.min_sample_rate().0, r.max_sample_rate().0))
                .collect(),
            buffer_range,
            built_in: false,
            virtual_transport: false,
        });
    }
    Ok(out)
}

pub(crate) fn list(dir: Direction) -> Result<Vec<DeviceInfo>, EngineError> {
    Ok(caps(dir)?.iter().map(DeviceCaps::info).collect())
}

/// Resolve a device with channels in `dir` by name (see [`match_name`]), or,
/// when `name` is `None`, the automatic choice: [`pick_auto_input`] for
/// inputs, the system default for outputs.
pub(crate) fn open(
    dir: Direction,
    name: Option<&str>,
) -> Result<(cpal::Device, DeviceCaps), EngineError> {
    let mut all = caps(dir)?;
    let index = match (name, dir) {
        (Some(wanted), _) => find_named(&all, dir, wanted)?,
        (None, Direction::Input) => auto_input_index(&all)?,
        (None, Direction::Output) => all
            .iter()
            .position(|c| c.is_default)
            .ok_or_else(|| EngineError::DeviceNotFound("default output".into()))?,
    };
    let caps = all.swap_remove(index);
    let device = cpal_device(&caps)?;
    Ok((device, caps))
}

/// [`pick_auto_input`] over `inputs`, as an error when nothing qualifies.
pub(crate) fn auto_input_index(inputs: &[DeviceCaps]) -> Result<usize, EngineError> {
    if inputs.is_empty() {
        return Err(EngineError::NoDefaultInput);
    }
    pick_auto_input(inputs).ok_or(EngineError::NoMicrophone)
}

/// The input to capture when none was named. Never a virtual loopback device
/// ([`is_virtual_device`]): people make VB-Cable / BlackHole the system default
/// input so call apps pick up presence, and capturing it would feed presence
/// its own output. In order of preference:
///
/// 1. the system default input, unless it's virtual;
/// 2. the built-in microphone (by transport type);
/// 3. a device named like a built-in mic ("MacBook …", "Built-in …"), for
///    when the transport type is unknown;
/// 4. the first other non-virtual input.
pub(crate) fn pick_auto_input(inputs: &[DeviceCaps]) -> Option<usize> {
    let real = |c: &DeviceCaps| !is_virtual_device(&c.name);
    let named_built_in = |c: &DeviceCaps| {
        let name = c.name.to_lowercase();
        name.contains("macbook") || name.contains("built-in")
    };
    let tiers: [&dyn Fn(&DeviceCaps) -> bool; 4] =
        [&|c| c.is_default, &|c| c.built_in, &named_built_in, &|_| {
            true
        }];
    tiers
        .iter()
        .find_map(|tier| inputs.iter().position(|c| real(c) && tier(c)))
}

/// Index of the device in `all` (devices with channels in `dir`) that `wanted`
/// names, with a specific error when it only names a device of the other
/// direction or several devices.
fn find_named(all: &[DeviceCaps], dir: Direction, wanted: &str) -> Result<usize, EngineError> {
    let names: Vec<&str> = all.iter().map(|c| c.name.as_str()).collect();
    match match_name(&names, wanted) {
        NameMatch::Found(i) => Ok(i),
        NameMatch::Ambiguous(candidates) => Err(ambiguous(wanted, &names, &candidates)),
        NameMatch::None => {
            let other = caps(dir.other())?;
            let other_names: Vec<&str> = other.iter().map(|c| c.name.as_str()).collect();
            match match_name(&other_names, wanted) {
                NameMatch::Found(i) => Err(EngineError::UnsupportedConfig(format!(
                    "{:?} has no {} channels",
                    other_names[i],
                    dir.label()
                ))),
                NameMatch::Ambiguous(_) | NameMatch::None => {
                    Err(EngineError::DeviceNotFound(wanted.to_string()))
                }
            }
        }
    }
}

fn ambiguous(wanted: &str, names: &[&str], candidates: &[usize]) -> EngineError {
    let list: Vec<String> = candidates
        .iter()
        .filter_map(|&i| names.get(i))
        .map(|n| format!("{n:?}"))
        .collect();
    EngineError::DeviceNotFound(format!(
        "{wanted:?} is ambiguous; matches {}",
        list.join(", ")
    ))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NameMatch {
    Found(usize),
    /// Indices of every equally good match.
    Ambiguous(Vec<usize>),
    None,
}

/// Find `wanted` among `names`: an exact match first, then a case-insensitive
/// exact match, then a case-insensitive substring match. The fuzzy tiers only
/// succeed when exactly one name matches.
pub(crate) fn match_name<S: AsRef<str>>(names: &[S], wanted: &str) -> NameMatch {
    if let Some(i) = names.iter().position(|n| n.as_ref() == wanted) {
        return NameMatch::Found(i);
    }
    let wanted = wanted.to_lowercase();
    let lowered: Vec<String> = names.iter().map(|n| n.as_ref().to_lowercase()).collect();
    let tiers: [&dyn Fn(&str) -> bool; 2] = [&|n| n == wanted, &|n| n.contains(&wanted)];
    for tier in tiers {
        let hits: Vec<usize> = (0..lowered.len()).filter(|&i| tier(&lowered[i])).collect();
        match hits.len() {
            0 => continue,
            1 => return NameMatch::Found(hits[0]),
            _ => return NameMatch::Ambiguous(hits),
        }
    }
    NameMatch::None
}

/// The cpal device for a HAL device ID.
///
/// cpal 0.16 exposes no ID on its devices, but its `Host::devices()`
/// enumerates `kAudioHardwarePropertyDevices` in order, the same list
/// [`crate::hal::device_ids`] reads. So look the ID up by position, with the
/// list read on both sides of cpal's enumeration to rule out a device being
/// added or removed in between, plus a name check. (Unfiltered `devices()`
/// only reads IDs; cpal's `input_devices()` / `output_devices()` would open an
/// AUHAL per device.)
#[cfg(target_os = "macos")]
fn cpal_device(caps: &DeviceCaps) -> Result<cpal::Device, EngineError> {
    let host = cpal::default_host();
    for _ in 0..3 {
        let before = crate::hal::device_ids();
        let Some(index) = before.iter().position(|&id| id == caps.id) else {
            break;
        };
        let devices: Vec<cpal::Device> = host.devices().map_err(backend)?.collect();
        if devices.len() != before.len() || crate::hal::device_ids() != before {
            continue;
        }
        if let Some(device) = devices.into_iter().nth(index)
            && device.name().is_ok_and(|n| n == caps.name)
        {
            return Ok(device);
        }
    }
    Err(EngineError::DeviceNotFound(caps.name.clone()))
}

#[cfg(not(target_os = "macos"))]
fn cpal_device(caps: &DeviceCaps) -> Result<cpal::Device, EngineError> {
    cpal::default_host()
        .devices()
        .map_err(backend)?
        .find(|d| d.name().is_ok_and(|n| n == caps.name))
        .ok_or_else(|| EngineError::DeviceNotFound(caps.name.clone()))
}

/// Index of the preferred virtual loopback device: the earliest
/// [`KNOWN_VIRTUAL_DEVICES`] entry that matches any name wins, regardless of
/// device order, so e.g. BlackHole beats VB-Cable when both are installed.
pub(crate) fn pick_virtual<S: AsRef<str>>(names: &[S]) -> Option<usize> {
    let lowered: Vec<String> = names.iter().map(|n| n.as_ref().to_lowercase()).collect();
    KNOWN_VIRTUAL_DEVICES.iter().find_map(|fragment| {
        let fragment = fragment.to_lowercase();
        lowered.iter().position(|n| n.contains(&fragment))
    })
}

/// Candidate sample rates, best first: each `preferred` rate the device
/// supports, then the device's current default.
pub(crate) fn rate_candidates(
    ranges: &[(u32, u32)],
    preferred: &[u32],
    default_rate: u32,
) -> Vec<u32> {
    let mut rates: Vec<u32> = preferred
        .iter()
        .copied()
        .filter(|&rate| ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&rate)))
        .collect();
    rates.push(default_rate);
    let mut seen = Vec::with_capacity(rates.len());
    rates.retain(|r| {
        let new = !seen.contains(r);
        seen.push(*r);
        new
    });
    rates
}

/// Candidate input rates, best first: the device's current nominal rate
/// (opening a stream at any other rate changes the device's rate for every app
/// using it), then [`PREFERRED_SAMPLE_RATE`] if supported.
pub(crate) fn input_rate_candidates(ranges: &[(u32, u32)], current: u32) -> Vec<u32> {
    let mut rates = Vec::with_capacity(2);
    if current > 0 {
        rates.push(current);
    }
    for rate in rate_candidates(ranges, &[PREFERRED_SAMPLE_RATE], current) {
        if rate > 0 && !rates.contains(&rate) {
            rates.push(rate);
        }
    }
    rates
}

/// Candidate buffer sizes, best first: `preferred` frames clamped into the
/// device's range (when it reports one), then the device default.
pub(crate) fn buffer_candidates(range: Option<(u32, u32)>, preferred: u32) -> Vec<BufferSize> {
    match range {
        Some((min, max)) if min <= max => {
            vec![
                BufferSize::Fixed(preferred.clamp(min, max)),
                BufferSize::Default,
            ]
        }
        _ => vec![BufferSize::Default],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(min: u32, max: u32) -> (u32, u32) {
        (min, max)
    }

    fn input(name: &str, is_default: bool, built_in: bool) -> DeviceCaps {
        DeviceCaps {
            id: 0,
            name: name.into(),
            channels: 1,
            default_rate: 48_000,
            rate_ranges: vec![(48_000, 48_000)],
            buffer_range: None,
            is_default,
            built_in,
            virtual_transport: false,
        }
    }

    #[test]
    fn auto_input_skips_a_virtual_default_for_the_built_in_mic() {
        // HAL order as seen on a MacBook with an iPhone nearby and VB-Cable
        // set as the default input: "first non-virtual" alone would pick the
        // phone.
        let inputs = [
            input("iPhone Microphone", false, false),
            input("MacBook Pro Microphone", false, true),
            input("VB-Cable", true, false),
            input("ZoomAudioDevice", false, false),
        ];
        assert_eq!(pick_auto_input(&inputs), Some(1));
        assert_eq!(auto_input_index(&inputs).ok(), Some(1));
    }

    #[test]
    fn auto_input_prefers_a_real_default() {
        let inputs = [
            input("MacBook Pro Microphone", false, true),
            input("USB Mic", true, false),
        ];
        assert_eq!(pick_auto_input(&inputs), Some(1));
    }

    #[test]
    fn auto_input_falls_back_by_name_then_to_any_real_input() {
        // Transport type unknown: recognise the built-in mic by name.
        let inputs = [
            input("USB Mic", false, false),
            input("Built-in Microphone", false, false),
            input("BlackHole 2ch", true, false),
        ];
        assert_eq!(pick_auto_input(&inputs), Some(1));
        // No built-in mic at all: the first non-virtual input.
        let inputs = [
            input("Loopback Audio", false, false),
            input("USB Mic", false, false),
            input("vb-cable", true, false),
        ];
        assert_eq!(pick_auto_input(&inputs), Some(1));
    }

    #[test]
    fn auto_input_never_picks_a_virtual_device() {
        // Even one reporting a built-in transport or a built-in-like name.
        let inputs = [
            input("VB-Cable", true, true),
            input("BlackHole (MacBook)", false, false),
        ];
        assert_eq!(pick_auto_input(&inputs), None);
        assert!(matches!(
            auto_input_index(&inputs),
            Err(EngineError::NoMicrophone)
        ));
        assert!(matches!(
            auto_input_index(&[]),
            Err(EngineError::NoDefaultInput)
        ));
    }

    #[test]
    fn virtual_match_is_case_insensitive_substring() {
        let names = ["MacBook Pro Microphone", "vb-cable", "ZoomAudioDevice"];
        assert_eq!(pick_virtual(&names), Some(1));
        assert_eq!(pick_virtual(&["BlackHole 2ch"]), Some(0));
        assert_eq!(pick_virtual(&["Rogue Amoeba LOOPBACK Audio"]), Some(0));
        assert_eq!(
            pick_virtual(&["MacBook Pro Speakers", "ZoomAudioDevice"]),
            None
        );
        assert_eq!(pick_virtual::<&str>(&[]), None);
    }

    #[test]
    fn virtual_match_follows_preference_not_device_order() {
        let names = [
            "Loopback Audio",
            "VB-Cable",
            "BlackHole 16ch",
            "presence (EQ)",
        ];
        assert_eq!(pick_virtual(&names), Some(3));
        assert_eq!(pick_virtual(&names[..3]), Some(2));
        assert_eq!(pick_virtual(&names[..2]), Some(1));
    }

    #[test]
    fn rates_prefer_requested_then_default() {
        let ranges = [range(44_100, 44_100), range(48_000, 48_000)];
        assert_eq!(
            rate_candidates(&ranges, &[48_000], 44_100),
            [48_000, 44_100]
        );
        assert_eq!(rate_candidates(&ranges, &[48_000], 48_000), [48_000]);
        assert_eq!(
            rate_candidates(&ranges, &[44_100, 48_000], 48_000),
            [44_100, 48_000]
        );
        let only_96k = [range(96_000, 96_000)];
        assert_eq!(rate_candidates(&only_96k, &[48_000], 96_000), [96_000]);
        let continuous = [range(8_000, 192_000)];
        assert_eq!(
            rate_candidates(&continuous, &[48_000], 44_100),
            [48_000, 44_100]
        );
    }

    #[test]
    fn input_rates_keep_current_nominal_first() {
        let ranges = [range(44_100, 44_100), range(48_000, 48_000)];
        assert_eq!(input_rate_candidates(&ranges, 44_100), [44_100, 48_000]);
        assert_eq!(input_rate_candidates(&ranges, 48_000), [48_000]);
        let only_96k = [range(96_000, 96_000)];
        assert_eq!(input_rate_candidates(&only_96k, 96_000), [96_000]);
        let continuous = [range(8_000, 192_000)];
        assert_eq!(input_rate_candidates(&continuous, 16_000), [16_000, 48_000]);
        assert_eq!(input_rate_candidates(&[], 44_100), [44_100]);
    }

    #[test]
    fn name_match_tiers() {
        let names = [
            "MacBook Pro Microphone",
            "VB-Cable",
            "MacBook Pro Speakers",
            "ZoomAudioDevice",
        ];
        assert_eq!(match_name(&names, "VB-Cable"), NameMatch::Found(1));
        assert_eq!(match_name(&names, "vb-cable"), NameMatch::Found(1));
        assert_eq!(match_name(&names, "zoom"), NameMatch::Found(3));
        assert_eq!(match_name(&names, "microphone"), NameMatch::Found(0));
        assert_eq!(
            match_name(&names, "macbook"),
            NameMatch::Ambiguous(vec![0, 2])
        );
        assert_eq!(match_name(&names, "BlackHole"), NameMatch::None);
    }

    #[test]
    fn name_match_prefers_stronger_tiers() {
        // An exact name wins over a case-insensitive twin and over names that
        // merely contain it.
        let names = ["Mic Pro", "mic", "Mic"];
        assert_eq!(match_name(&names, "Mic"), NameMatch::Found(2));
        // Case-insensitive exact beats substring.
        let names = ["USB Mic 2", "usb mic"];
        assert_eq!(match_name(&names, "USB MIC"), NameMatch::Found(1));
        // Two case-insensitive exact hits are ambiguous.
        let names = ["usb mic", "USB MIC"];
        assert_eq!(
            match_name(&names, "Usb Mic"),
            NameMatch::Ambiguous(vec![0, 1])
        );
    }

    #[test]
    fn buffers_clamp_into_range_and_fall_back() {
        let r = Some((15, 4096));
        assert_eq!(
            buffer_candidates(r, 256),
            [BufferSize::Fixed(256), BufferSize::Default]
        );
        let r = Some((512, 4096));
        assert_eq!(
            buffer_candidates(r, 256),
            [BufferSize::Fixed(512), BufferSize::Default]
        );
        assert_eq!(buffer_candidates(None, 256), [BufferSize::Default]);
    }
}
