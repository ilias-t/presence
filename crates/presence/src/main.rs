mod app;
mod cli;
mod commands;
mod config;
mod devices;
mod failure;
mod lock;
mod presets;
mod runner;
mod spectrum;
mod ui;
mod watchdog;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Command};

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config_path = cli.config.or_else(config::default_path);
    match cli.command {
        None => runner::run(config_path, cli.input, cli.output),
        Some(Command::Devices) => commands::devices(),
        Some(Command::Presets) => commands::presets(config_path.as_deref()),
        Some(Command::Run { preset }) => commands::run(
            config_path.as_deref(),
            cli.input,
            cli.output,
            preset,
            presence_engine::microphone_permission,
        ),
    }
}
