//! Choosing which devices to hand the engine: remembered devices that are no
//! longer connected fall back to the defaults instead of failing, while
//! devices the user just named (CLI flags) are passed through so a typo errors.

use presence_engine::DeviceInfo;

/// Whether `name` is a virtual loopback device (BlackHole, VB-Cable, …), i.e.
/// an output that call apps can read rather than one that plays out loud.
pub fn is_virtual(name: &str) -> bool {
    presence_engine::is_virtual_device(name)
}

/// Whether `wanted` would resolve to a device in `devices`, using the engine's
/// lenient matching (exact, case-insensitive, or a case-insensitive substring).
pub fn is_present(wanted: &str, devices: &[DeviceInfo]) -> bool {
    let wanted = wanted.to_lowercase();
    devices
        .iter()
        .any(|d| d.name.to_lowercase().contains(&wanted))
}

/// One side (input or output) of a device request.
#[derive(Clone, Copy, Debug)]
pub struct Want<'a> {
    /// `None` = automatic: the engine's non-virtual input choice / the
    /// auto-detected virtual output.
    pub name: Option<&'a str>,
    /// Named by the user just now (CLI flag): never substituted.
    pub explicit: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    pub input: Option<String>,
    pub output: Option<String>,
    /// The remembered device was missing and the default is used instead.
    pub input_fell_back: bool,
    pub output_fell_back: bool,
    pub notices: Vec<String>,
}

/// Decides what to pass to `Engine::start`. `None` lists mean enumeration
/// failed; the request is then passed through and the engine reports errors.
///
/// `auto_input` is what the engine captures when no input is named
/// ([`presence_engine::auto_input_device`]); it's only used for notices.
pub fn plan(
    input: Want,
    output: Want,
    inputs: Option<&[DeviceInfo]>,
    outputs: Option<&[DeviceInfo]>,
    auto_input: Option<&str>,
) -> Plan {
    let resolve = |want: Want, list: Option<&[DeviceInfo]>| match (want.name, list) {
        (Some(name), Some(list)) if !want.explicit && !is_present(name, list) => (None, true),
        (name, _) => (name.map(str::to_owned), false),
    };
    let (input_name, input_fell_back) = resolve(input, inputs);
    let (output_name, output_fell_back) = resolve(output, outputs);
    let mut notices = Vec::new();
    let auto_label = auto_input.map_or_else(|| "the default".to_owned(), |a| format!("\"{a}\""));
    if input_fell_back {
        notices.push(format!(
            "mic \"{}\" not connected; using {auto_label}",
            input.name.unwrap_or_default()
        ));
    }
    if input_name.is_none() {
        let virtual_default = inputs
            .and_then(|list| list.iter().find(|d| d.is_default))
            .filter(|d| is_virtual(&d.name));
        if let Some(d) = virtual_default {
            notices.push(match auto_input {
                Some(auto) => format!(
                    "default input \"{}\" is a virtual device; using \"{auto}\"",
                    d.name
                ),
                None => format!(
                    "default input \"{}\" is a virtual device and no microphone was found",
                    d.name
                ),
            });
        }
    }
    if output_fell_back {
        notices.push(format!(
            "output \"{}\" not connected; using the virtual device",
            output.name.unwrap_or_default()
        ));
    }
    Plan {
        input: input_name,
        output: output_name,
        input_fell_back,
        output_fell_back,
        notices,
    }
}

/// A warning for a running engine that captures a virtual loopback device.
/// Automatic selection never does that, so the user picked it (flag, device
/// picker or a remembered choice); presence allows it, but whatever plays into
/// that device ends up in the processed mic, and if it's presence's own output
/// under another name, the audio loops.
pub fn virtual_input_warning(input: &str, output: &str) -> Option<String> {
    is_virtual(input).then(|| {
        format!(
            "input \"{input}\" is a virtual loopback device, not a microphone: presence \
             processes whatever plays into it, and loops if its own output (\"{output}\") \
             reaches it"
        )
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn dev(name: &str, is_default: bool) -> DeviceInfo {
        DeviceInfo {
            name: name.into(),
            is_default,
            channels: 2,
            default_sample_rate: 48_000,
        }
    }

    fn saved(name: &str) -> Want<'_> {
        Want {
            name: Some(name),
            explicit: false,
        }
    }

    fn flag(name: &str) -> Want<'_> {
        Want {
            name: Some(name),
            explicit: true,
        }
    }

    const DEFAULT: Want<'static> = Want {
        name: None,
        explicit: false,
    };

    #[test]
    fn detects_virtual_devices() {
        assert!(is_virtual("BlackHole 2ch"));
        assert!(is_virtual("blackhole 16ch"));
        assert!(is_virtual("VB-Cable"));
        assert!(!is_virtual("MacBook Pro Speakers"));
        assert!(!is_virtual("External Headphones"));
    }

    #[test]
    fn present_devices_are_used_as_is() {
        let inputs = [dev("MacBook Pro Microphone", true), dev("USB Mic", false)];
        let outputs = [dev("BlackHole 2ch", false)];
        let p = plan(
            saved("USB Mic"),
            saved("blackhole"),
            Some(&inputs),
            Some(&outputs),
            None,
        );
        assert_eq!(p.input.as_deref(), Some("USB Mic"));
        assert_eq!(p.output.as_deref(), Some("blackhole"));
        assert!(!p.input_fell_back && !p.output_fell_back);
        assert!(p.notices.is_empty());
    }

    #[test]
    fn missing_saved_devices_fall_back_with_a_notice() {
        let inputs = [dev("MacBook Pro Microphone", true)];
        let outputs = [dev("BlackHole 2ch", false)];
        let p = plan(
            saved("USB Mic"),
            saved("VB-Cable"),
            Some(&inputs),
            Some(&outputs),
            None,
        );
        assert_eq!(p.input, None);
        assert_eq!(p.output, None);
        assert!(p.input_fell_back && p.output_fell_back);
        assert_eq!(p.notices.len(), 2);
        assert!(p.notices[0].contains("USB Mic"));
        assert!(p.notices[1].contains("VB-Cable"));
    }

    // The user's usual setup: VB-Cable is the system default input so call
    // apps pick up presence. Automatic selection must not capture it.
    fn vb_cable_default() -> [DeviceInfo; 3] {
        [
            dev("iPhone Microphone", false),
            dev("MacBook Pro Microphone", false),
            dev("VB-Cable", true),
        ]
    }

    #[test]
    fn virtual_default_input_is_replaced_with_a_notice() {
        let inputs = vb_cable_default();
        let outputs = [dev("VB-Cable", false)];
        let p = plan(
            DEFAULT,
            DEFAULT,
            Some(&inputs),
            Some(&outputs),
            Some("MacBook Pro Microphone"),
        );
        // The engine resolves `None` itself, to the same device.
        assert_eq!(p.input, None);
        assert_eq!(
            p.notices,
            ["default input \"VB-Cable\" is a virtual device; using \"MacBook Pro Microphone\""]
        );
    }

    #[test]
    fn missing_saved_mic_names_the_replacement() {
        let inputs = vb_cable_default();
        let p = plan(
            saved("Yeti"),
            DEFAULT,
            Some(&inputs),
            Some(&[]),
            Some("MacBook Pro Microphone"),
        );
        assert!(p.input_fell_back);
        assert_eq!(
            p.notices,
            [
                "mic \"Yeti\" not connected; using \"MacBook Pro Microphone\"",
                "default input \"VB-Cable\" is a virtual device; using \"MacBook Pro Microphone\"",
            ]
        );
    }

    #[test]
    fn virtual_default_without_any_microphone() {
        let inputs = [dev("VB-Cable", true)];
        let p = plan(DEFAULT, DEFAULT, Some(&inputs), Some(&[]), None);
        assert_eq!(
            p.notices,
            ["default input \"VB-Cable\" is a virtual device and no microphone was found"]
        );
    }

    #[test]
    fn real_default_or_named_input_gets_no_default_notice() {
        let inputs = [dev("MacBook Pro Microphone", true), dev("VB-Cable", false)];
        let p = plan(
            DEFAULT,
            DEFAULT,
            Some(&inputs),
            Some(&[]),
            Some("MacBook Pro Microphone"),
        );
        assert!(p.notices.is_empty());
        let inputs = vb_cable_default();
        let p = plan(flag("VB-Cable"), DEFAULT, Some(&inputs), Some(&[]), None);
        assert_eq!(p.input.as_deref(), Some("VB-Cable"));
        assert!(p.notices.is_empty());
    }

    #[test]
    fn capturing_a_virtual_device_warns() {
        let w = virtual_input_warning("VB-Cable", "BlackHole 2ch").expect("warns");
        assert!(w.contains("\"VB-Cable\" is a virtual loopback device"));
        assert!(w.contains("BlackHole 2ch"));
        assert_eq!(
            virtual_input_warning("MacBook Pro Microphone", "VB-Cable"),
            None
        );
    }

    #[test]
    fn explicit_devices_are_never_substituted() {
        let inputs = [dev("MacBook Pro Microphone", true)];
        let p = plan(flag("USB Mic"), DEFAULT, Some(&inputs), Some(&[]), None);
        assert_eq!(p.input.as_deref(), Some("USB Mic"));
        assert!(!p.input_fell_back);
        assert!(p.notices.is_empty());
    }

    #[test]
    fn enumeration_failure_passes_the_request_through() {
        let p = plan(saved("USB Mic"), saved("BlackHole 2ch"), None, None, None);
        assert_eq!(p.input.as_deref(), Some("USB Mic"));
        assert_eq!(p.output.as_deref(), Some("BlackHole 2ch"));
        assert!(p.notices.is_empty());
    }
}
