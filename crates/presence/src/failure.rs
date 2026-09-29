//! Why the engine isn't running, and what the user can do about it. Shared by
//! the TUI and `presence run` so both give the same, situation-specific advice.

use presence_engine::{EngineError, MicPermission};

/// Shown before the engine start that will trigger the macOS prompt.
pub const MIC_PROMPT: &str = "macOS will ask for microphone access — click Allow";
/// Where to grant microphone access after declining the prompt.
pub const MIC_FIX: &str =
    "System Settings → Privacy & Security → Microphone → enable your terminal app";
/// The engine is running but the microphone delivers nothing (see [`crate::watchdog`]).
pub const NO_AUDIO: &str = "no audio from the mic";
/// Why [`NO_AUDIO`] happens despite granted access, and what to try.
pub const NO_AUDIO_HINT: &str = "macOS may be waiting for Microphone permission for this \
     terminal (check for a prompt, or System Settings → Privacy & Security → Microphone), or \
     try Terminal.app";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// No output given and no BlackHole / VB-Cable installed.
    NoVirtualDevice,
    /// A named device is missing, ambiguous, or only exists in the other direction.
    DeviceNotFound,
    /// Input and output are the same loopback device.
    SameDevice,
    /// Microphone access denied or restricted.
    MicPermission,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub kind: FailureKind,
    pub message: String,
}

impl Failure {
    pub fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn from_engine(e: &EngineError) -> Self {
        let message = e.to_string();
        let kind = match e {
            EngineError::NoVirtualOutput => FailureKind::NoVirtualDevice,
            EngineError::DeviceNotFound(_)
            | EngineError::NoDefaultInput
            | EngineError::NoMicrophone => FailureKind::DeviceNotFound,
            // The engine reports these through generic variants; tell them
            // apart by message so the hint fits.
            _ if message.contains("microphone access") => FailureKind::MicPermission,
            _ if message.contains("would feed back") => FailureKind::SameDevice,
            _ if message.contains("has no input channels")
                || message.contains("has no output channels") =>
            {
                FailureKind::DeviceNotFound
            }
            _ => FailureKind::Other,
        };
        let message = match kind {
            // The engine's text repeats the install advice the hint gives.
            FailureKind::NoVirtualDevice => "no virtual audio device found".to_owned(),
            FailureKind::MicPermission => "microphone access denied".to_owned(),
            _ => message,
        };
        Self { kind, message }
    }

    pub fn mic_permission(p: MicPermission) -> Self {
        let message = match p {
            MicPermission::Restricted => "microphone access is blocked by system policy",
            _ => "microphone access denied",
        };
        Self::new(FailureKind::MicPermission, message)
    }

    /// Advice for the TUI header; keys refer to TUI bindings.
    pub fn tui_hint(&self) -> String {
        match self.kind {
            FailureKind::NoVirtualDevice => {
                "install BlackHole (brew install blackhole-2ch) or VB-Cable, then press r · \
                 d picks an output"
                    .into()
            }
            FailureKind::DeviceNotFound => {
                "press d to pick a device (`presence devices` lists them) · r retries".into()
            }
            FailureKind::SameDevice => "your mic would be fed its own output; press d and pick \
                 your real microphone as the input"
                .into(),
            FailureKind::MicPermission => {
                format!("{MIC_FIX}, then press r (you may need to restart the terminal)")
            }
            FailureKind::Other => "press r to retry · d picks devices".into(),
        }
    }

    /// Advice for `presence run`; refers to CLI flags.
    pub fn cli_hint(&self) -> Option<String> {
        match self.kind {
            FailureKind::NoVirtualDevice => Some(
                "install BlackHole (`brew install blackhole-2ch`) or VB-Cable, or pass --output NAME"
                    .into(),
            ),
            FailureKind::DeviceNotFound => {
                Some("run `presence devices` to see the available device names".into())
            }
            FailureKind::SameDevice => Some(
                "the input and output are the same loopback device, so your mic would be fed \
                 its own output; pass --input with your real microphone"
                    .into(),
            ),
            FailureKind::MicPermission => Some(format!("{MIC_FIX}, then run presence again")),
            FailureKind::Other => None,
        }
    }
}

/// What to do about the microphone permission before starting the engine.
#[derive(Clone, Debug, PartialEq)]
pub enum Gate {
    Start,
    /// Start, but warn first: the start will block on the macOS prompt.
    StartAfterPrompt,
    Refuse(Failure),
}

pub fn gate(p: MicPermission) -> Gate {
    match p {
        MicPermission::Authorized => Gate::Start,
        MicPermission::NotDetermined => Gate::StartAfterPrompt,
        MicPermission::Denied | MicPermission::Restricted => {
            Gate::Refuse(Failure::mic_permission(p))
        }
    }
}

/// Whether a running engine has to be stopped. `NotDetermined` after start most
/// likely means the prompt is still up, so the engine keeps running.
pub fn revoked(p: MicPermission) -> bool {
    matches!(p, MicPermission::Denied | MicPermission::Restricted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_engine_errors() {
        let kind = |e: EngineError| Failure::from_engine(&e).kind;
        assert_eq!(
            kind(EngineError::NoVirtualOutput),
            FailureKind::NoVirtualDevice
        );
        assert_eq!(
            kind(EngineError::DeviceNotFound("USB Mic".into())),
            FailureKind::DeviceNotFound
        );
        assert_eq!(
            kind(EngineError::NoDefaultInput),
            FailureKind::DeviceNotFound
        );
        assert_eq!(
            kind(EngineError::UnsupportedConfig(
                "input and output are both the loopback device \"BlackHole 2ch\"; that would feed back"
                    .into()
            )),
            FailureKind::SameDevice
        );
        assert_eq!(
            kind(EngineError::UnsupportedConfig(
                "\"MacBook Pro Speakers\" has no input channels".into()
            )),
            FailureKind::DeviceNotFound
        );
        assert_eq!(
            kind(EngineError::Backend(
                "microphone access denied; allow this app".into()
            )),
            FailureKind::MicPermission
        );
        assert_eq!(
            kind(EngineError::Backend("stream exploded".into())),
            FailureKind::Other
        );
    }

    #[test]
    fn hints_are_specific_to_the_failure() {
        let hint = |k| Failure::new(k, "x").tui_hint();
        assert!(hint(FailureKind::NoVirtualDevice).contains("BlackHole"));
        for k in [
            FailureKind::DeviceNotFound,
            FailureKind::SameDevice,
            FailureKind::MicPermission,
            FailureKind::Other,
        ] {
            assert!(!hint(k).contains("BlackHole"), "{k:?}: {}", hint(k));
        }
        assert!(hint(FailureKind::DeviceNotFound).contains("press d"));
        assert!(hint(FailureKind::MicPermission).contains("Privacy & Security"));
        assert!(
            Failure::new(FailureKind::DeviceNotFound, "x")
                .cli_hint()
                .unwrap()
                .contains("presence devices")
        );
    }

    #[test]
    fn permission_gate() {
        assert_eq!(gate(MicPermission::Authorized), Gate::Start);
        assert_eq!(gate(MicPermission::NotDetermined), Gate::StartAfterPrompt);
        for p in [MicPermission::Denied, MicPermission::Restricted] {
            assert!(matches!(gate(p), Gate::Refuse(f) if f.kind == FailureKind::MicPermission));
            assert!(revoked(p));
        }
        assert!(!revoked(MicPermission::NotDetermined));
        assert!(!revoked(MicPermission::Authorized));
    }
}
