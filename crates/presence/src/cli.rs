use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// System-wide microphone EQ for macOS.
///
/// Captures your mic, runs a voice EQ, and plays the result into a virtual
/// loopback device (BlackHole, VB-Cable) that call apps use as their microphone.
/// Run without a subcommand to open the interactive TUI.
#[derive(Debug, Parser)]
#[command(name = "presence", version, about)]
pub struct Cli {
    /// Input (microphone) device name; defaults to the last used, else the system default
    /// input unless that is a virtual device (then the built-in mic).
    #[arg(long, global = true, value_name = "NAME")]
    pub input: Option<String>,

    /// Output device name, normally the virtual device; defaults to the last used or auto-detected.
    #[arg(long, global = true, value_name = "NAME")]
    pub output: Option<String>,

    /// Config file to use instead of ~/Library/Application Support/presence/config.toml.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List audio input and output devices.
    Devices,
    /// List built-in and user presets.
    Presets,
    /// Run the EQ without the TUI, printing a status line until Ctrl-C.
    Run {
        /// Preset id (see `presence presets`); defaults to the saved settings.
        #[arg(long, value_name = "ID")]
        preset: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_device_flags_work_after_subcommand() {
        let cli = Cli::parse_from([
            "presence",
            "run",
            "--preset",
            "clarity",
            "--output",
            "BlackHole 2ch",
        ]);
        assert_eq!(cli.output.as_deref(), Some("BlackHole 2ch"));
        assert!(
            matches!(cli.command, Some(Command::Run { preset: Some(ref p) }) if p == "clarity")
        );
    }
}
