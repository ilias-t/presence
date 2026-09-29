//! Run the engine for a few seconds and print meters/stats every 500 ms.
//!
//! cargo run -p presence-engine --example smoke -- \
//!     [--input NAME] [--output NAME] [--seconds N] [--bypass] [--preset ID] [--list]

use std::process::ExitCode;
use std::time::{Duration, Instant};

use presence_dsp::{EqSettings, builtin_presets};
use presence_engine::{Engine, EngineConfig, EngineStatus};

struct Args {
    input: Option<String>,
    output: Option<String>,
    seconds: f32,
    bypass: bool,
    preset: Option<String>,
    list: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        input: None,
        output: None,
        seconds: 5.0,
        bypass: false,
        preset: None,
        list: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--input" => args.input = Some(value("--input")?),
            "--output" => args.output = Some(value("--output")?),
            "--seconds" => {
                args.seconds = value("--seconds")?
                    .parse()
                    .map_err(|e| format!("--seconds: {e}"))?
            }
            "--preset" => args.preset = Some(value("--preset")?),
            "--bypass" => args.bypass = true,
            "--list" => args.list = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(args)
}

fn list() -> Result<(), presence_engine::EngineError> {
    for (label, devices) in [
        ("inputs", presence_engine::list_input_devices()?),
        ("outputs", presence_engine::list_output_devices()?),
    ] {
        println!("{label}:");
        for d in devices {
            let star = if d.is_default { "*" } else { " " };
            println!(
                "  {star} {:<28} {} ch  {} Hz",
                d.name, d.channels, d.default_sample_rate
            );
        }
    }
    let virt = presence_engine::find_virtual_output()?;
    println!("virtual output: {:?}", virt.map(|d| d.name));
    println!(
        "microphone permission: {:?}",
        presence_engine::microphone_permission()
    );
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.list {
        return match list() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let settings = match &args.preset {
        None => EqSettings::default(),
        Some(id) => match builtin_presets().into_iter().find(|p| p.id == id) {
            Some(p) => p.settings,
            None => {
                eprintln!("error: unknown preset {id:?}");
                return ExitCode::FAILURE;
            }
        },
    };

    let engine = match Engine::start(EngineConfig {
        input_device: args.input,
        output_device: args.output,
        settings,
        bypass: args.bypass,
    }) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let s = engine.stats();
    println!(
        "started: {:?} @ {} Hz -> {:?} @ {} Hz, bypass={}",
        s.input_device,
        s.input_sample_rate,
        s.output_device,
        s.output_sample_rate,
        engine.bypass()
    );

    let start = Instant::now();
    let mut analysis = Vec::new();
    let mut next_print = Duration::from_millis(500);
    let mut failed = false;
    while start.elapsed().as_secs_f32() < args.seconds {
        // Poll at ~30 Hz like a UI would, so meter ballistics behave normally.
        std::thread::sleep(Duration::from_millis(33));
        let m = engine.meters();
        engine.drain_analysis(&mut analysis);
        if start.elapsed() < next_print {
            continue;
        }
        next_print += Duration::from_millis(500);
        let s = engine.stats();
        let status = engine.status();
        println!(
            "t={:4.1}s in pk {:6.1} rms {:6.1} | out pk {:6.1} rms {:6.1} dBFS | \
             lat {:5.1} ms  under {} over {} | analysis +{} | {:?}",
            start.elapsed().as_secs_f32(),
            m.input_peak_db,
            m.input_rms_db,
            m.output_peak_db,
            m.output_rms_db,
            s.latency_ms,
            s.underruns,
            s.overruns,
            analysis.len(),
            status,
        );
        analysis.clear();
        if let EngineStatus::Failed(_) = status {
            failed = true;
            break;
        }
    }
    drop(engine);
    println!("stopped");
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
