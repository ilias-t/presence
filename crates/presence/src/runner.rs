//! The interactive TUI loop: owns the terminal and the engine, feeds key
//! events to [`App`] and applies the resulting [`Action`]s.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event};
use presence_engine::{Engine, EngineConfig, EngineStatus, MicPermission};
use ratatui::DefaultTerminal;

use crate::app::{Action, App, EngineState, NoticeLevel};
use crate::config::{self, Config};
use crate::devices::{self, Want};
use crate::failure::{self, Failure, FailureKind, Gate};
use crate::lock;
use crate::spectrum::Analyzer;
use crate::ui;
use crate::watchdog::Watchdog;

const FRAME: Duration = Duration::from_millis(33);
const SAVE_DEBOUNCE: Duration = Duration::from_secs(1);
/// How often microphone permission is re-checked while running: revoking it
/// in System Settings silences capture without any stream error.
pub const PERMISSION_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug, PartialEq)]
enum PermissionChange {
    /// Access was revoked: drop the engine (capture would only be silence).
    Stop(Failure),
    /// Access was granted after a denial: start again.
    Restart,
    /// Whether the "click Allow" hint should show.
    Prompt(bool),
    None,
}

/// What a periodic permission check means for the engine.
fn permission_change(
    running: bool,
    state: &EngineState,
    permission: MicPermission,
) -> PermissionChange {
    if running {
        if failure::revoked(permission) {
            PermissionChange::Stop(Failure::mic_permission(permission))
        } else {
            PermissionChange::Prompt(permission == MicPermission::NotDetermined)
        }
    } else if permission == MicPermission::Authorized
        && matches!(state, EngineState::Failed(f) if f.kind == FailureKind::MicPermission)
    {
        PermissionChange::Restart
    } else {
        PermissionChange::None
    }
}

pub fn run(
    config_path: Option<PathBuf>,
    input: Option<String>,
    output: Option<String>,
) -> Result<()> {
    let _lock = lock::acquire(&lock::lock_path(config_path.as_deref()))?;
    let (config, mut warnings, corrupt) = match &config_path {
        Some(path) => {
            let loaded = config::load_from(path);
            (loaded.config, loaded.warnings, loaded.corrupt)
        }
        None => (
            Config::default(),
            vec!["no config directory; settings won't be saved".to_owned()],
            false,
        ),
    };
    if corrupt {
        warnings.push("the invalid config will be kept as config.toml.corrupt".to_owned());
    }
    let builtins = presence_dsp::builtin_presets();
    let ids: Vec<&str> = builtins.iter().map(|p| p.id).collect();
    warnings.extend(config::check_active_preset(&config, &ids));
    // Autosave baseline: the first save only happens if the session differs.
    let on_disk = config.clone();

    let mut app = App::new(&builtins, config);
    // Flags win for this session; they're remembered once they start.
    app.set_explicit_devices(input, output);
    for warning in warnings {
        app.notify(warning, NoticeLevel::Warn);
    }

    // In raw mode Ctrl-C arrives as a key, so this only fires for SIGTERM or
    // SIGHUP (terminal closed): quit through the normal path so settings are
    // saved and the terminal restored.
    let terminate = Arc::new(AtomicBool::new(false));
    {
        let terminate = Arc::clone(&terminate);
        // Failure just means the default signal behaviour; not worth aborting over.
        let _ = ctrlc::set_handler(move || terminate.store(true, Ordering::SeqCst));
    }
    let mut session = Session::new(
        config_path,
        on_disk,
        corrupt,
        &app,
        terminate,
        presence_engine::microphone_permission,
    );
    session.schedule_start(&mut app);
    // `ratatui::init` installs a panic hook that restores the terminal first.
    let mut terminal = ratatui::init();
    let result = session.run(&mut terminal, &mut app);
    ratatui::restore();
    drop(session.engine.take());
    if let Some(msg) = session.save(&app) {
        eprintln!("presence: {msg}");
    }
    if let Some(err) = session.save_error {
        eprintln!("presence: couldn't save settings: {err}");
    }
    result
}

struct Session {
    engine: Option<Engine>,
    /// Watches `engine` for a start that never delivers audio.
    watchdog: Option<Watchdog>,
    analyzer: Analyzer,
    analysis_buf: Vec<f32>,
    config_path: Option<PathBuf>,
    saved: Config,
    pending: Config,
    changed_at: Instant,
    save_error: Option<String>,
    /// The file on disk couldn't be parsed; move it aside before the first save.
    corrupt_on_disk: bool,
    /// Engine starts are deferred until after a frame is drawn: opening
    /// devices can block (e.g. on the macOS microphone permission prompt) and
    /// the user should see "starting audio…" rather than a blank screen.
    /// `Engine` isn't `Send`, so a background thread isn't an option.
    start_pending: bool,
    terminate: Arc<AtomicBool>,
    /// Injectable for tests; `presence_engine::microphone_permission` in the app.
    permission: fn() -> MicPermission,
    last_permission_check: Instant,
}

impl Session {
    /// `on_disk` is what was loaded, so the first autosave only happens if the
    /// session actually differs from it (first run, CLI overrides, recovered file).
    fn new(
        config_path: Option<PathBuf>,
        on_disk: Config,
        corrupt_on_disk: bool,
        app: &App,
        terminate: Arc<AtomicBool>,
        permission: fn() -> MicPermission,
    ) -> Self {
        let snapshot = app.to_config();
        Self {
            engine: None,
            watchdog: None,
            analyzer: Analyzer::new(),
            analysis_buf: Vec::with_capacity(16_384),
            config_path,
            saved: on_disk,
            pending: snapshot,
            changed_at: Instant::now(),
            save_error: None,
            corrupt_on_disk,
            start_pending: false,
            terminate,
            permission,
            last_permission_check: Instant::now(),
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
        let mut last_tick = Instant::now();
        while !app.should_quit {
            terminal.draw(|frame| ui::render(frame, app))?;
            if std::mem::take(&mut self.start_pending) {
                self.start_engine(app);
                last_tick = Instant::now();
                continue;
            }

            let deadline = last_tick + FRAME;
            while !app.should_quit
                && event::poll(deadline.saturating_duration_since(Instant::now()))?
            {
                if let Event::Key(key) = event::read()? {
                    for action in app.handle_key(key) {
                        self.apply(action, app);
                    }
                }
            }

            if self.terminate.load(Ordering::SeqCst) {
                app.should_quit = true;
            }
            let now = Instant::now();
            let dt = now - last_tick;
            last_tick = now;
            app.tick(dt);
            self.check_permission(app, now);
            self.poll_engine(app, now, dt);
            self.autosave(app, now);
        }
        Ok(())
    }

    fn apply(&mut self, action: Action, app: &mut App) {
        match action {
            Action::SetSettings(settings) => {
                if let Some(engine) = &self.engine {
                    engine.set_settings(settings);
                }
            }
            Action::SetBypass(bypass) => {
                if let Some(engine) = &self.engine {
                    engine.set_bypass(bypass);
                }
            }
            Action::RestartEngine => self.stop_engine(app),
            Action::OpenDevicePicker => {
                let devices = presence_engine::list_input_devices().and_then(|inputs| {
                    let outputs = presence_engine::list_output_devices()?;
                    let virt = presence_engine::find_virtual_output()?.map(|d| d.name);
                    let auto = presence_engine::auto_input_device()?.map(|d| d.name);
                    Ok((inputs, outputs, virt, auto))
                });
                match devices {
                    Ok((inputs, outputs, virt, auto)) => {
                        app.open_device_picker(&inputs, &outputs, virt.as_deref(), auto.as_deref())
                    }
                    Err(e) => app.notify(format!("couldn't list devices: {e}"), NoticeLevel::Warn),
                }
            }
            Action::Quit => {}
        }
    }

    /// Schedules an engine start after the next frame, unless microphone
    /// access is denied. Checked here rather than at start time so the frame
    /// drawn before the (possibly blocking) start already shows the prompt hint.
    fn schedule_start(&mut self, app: &mut App) {
        self.last_permission_check = Instant::now();
        match failure::gate((self.permission)()) {
            Gate::Refuse(failure) => {
                app.engine.mic_prompt = false;
                app.engine.state = EngineState::Failed(failure);
                self.start_pending = false;
            }
            gate => {
                app.engine.mic_prompt = gate == Gate::StartAfterPrompt;
                app.engine.state = EngineState::Starting;
                self.start_pending = true;
            }
        }
    }

    /// Releases the devices and schedules a restart.
    fn stop_engine(&mut self, app: &mut App) {
        self.release(app);
        self.schedule_start(app);
    }

    fn release(&mut self, app: &mut App) {
        drop(self.engine.take());
        self.watchdog = None;
        app.engine.no_audio = false;
        self.analyzer.reset();
        app.spectrum.clear();
        app.engine.meters = Default::default();
        app.engine.stats = None;
    }

    fn start_engine(&mut self, app: &mut App) {
        drop(self.engine.take());
        self.watchdog = None;
        app.engine.no_audio = false;
        let inputs = presence_engine::list_input_devices().ok();
        let outputs = presence_engine::list_output_devices().ok();
        let auto_input = presence_engine::auto_input_device().ok().flatten();
        let plan = devices::plan(
            Want {
                name: app.input_device.as_deref(),
                explicit: app.explicit_input,
            },
            Want {
                name: app.output_device.as_deref(),
                explicit: app.explicit_output,
            },
            inputs.as_deref(),
            outputs.as_deref(),
            auto_input.as_ref().map(|d| d.name.as_str()),
        );
        for notice in &plan.notices {
            app.notify(notice.clone(), NoticeLevel::Warn);
        }
        let config = EngineConfig {
            input_device: plan.input.clone(),
            output_device: plan.output.clone(),
            settings: app.settings,
            bypass: app.bypass,
        };
        let result = Engine::start(config);
        // The start may have shown the permission prompt; see what was answered.
        self.last_permission_check = Instant::now();
        let permission = (self.permission)();
        match result {
            Ok(engine) if failure::revoked(permission) => {
                drop(engine);
                app.engine.mic_prompt = false;
                app.engine.stats = None;
                app.engine.state = EngineState::Failed(Failure::mic_permission(permission));
            }
            Ok(engine) => {
                let stats = engine.stats();
                if let Some(warning) =
                    devices::virtual_input_warning(&stats.input_device, &stats.output_device)
                {
                    app.notify(warning, NoticeLevel::Warn);
                }
                app.engine_started(&plan, &stats);
                app.engine.mic_prompt = permission == MicPermission::NotDetermined;
                app.engine.stats = Some(stats);
                app.engine.state = EngineState::Running;
                self.watchdog = Some(Watchdog::new(Instant::now(), engine.callback_count()));
                self.engine = Some(engine);
            }
            Err(e) => {
                app.engine.mic_prompt = false;
                app.engine.stats = None;
                app.engine.state = EngineState::Failed(Failure::from_engine(&e));
            }
        }
    }

    /// Every [`PERMISSION_INTERVAL`]: stop a running engine whose microphone
    /// access was revoked, and start again once access is granted.
    fn check_permission(&mut self, app: &mut App, now: Instant) {
        if now.duration_since(self.last_permission_check) < PERMISSION_INTERVAL {
            return;
        }
        self.last_permission_check = now;
        let running = self.engine.is_some();
        match permission_change(running, &app.engine.state, (self.permission)()) {
            PermissionChange::Stop(failure) => {
                self.release(app);
                app.engine.mic_prompt = false;
                app.engine.state = EngineState::Failed(failure);
            }
            PermissionChange::Restart => {
                app.notify(
                    "Microphone access granted; starting audio",
                    NoticeLevel::Info,
                );
                self.stop_engine(app);
            }
            PermissionChange::Prompt(showing) => app.engine.mic_prompt = showing,
            PermissionChange::None => {}
        }
    }

    fn poll_engine(&mut self, app: &mut App, now: Instant, dt: Duration) {
        let Some(engine) = &self.engine else {
            return;
        };
        if let EngineStatus::Failed(msg) = engine.status() {
            self.release(app);
            app.engine.state = EngineState::Failed(Failure::new(FailureKind::Other, msg));
            return;
        }
        if let Some(watchdog) = &mut self.watchdog {
            watchdog.update(now, engine.callback_count());
            app.engine.no_audio = watchdog.stalled();
        }
        let stats = engine.stats();
        app.engine.meters = engine.meters();
        self.analysis_buf.clear();
        engine.drain_analysis(&mut self.analysis_buf);
        self.analyzer.push(&self.analysis_buf);
        let spectrum = self
            .analyzer
            .update(stats.input_sample_rate as f32, dt.as_secs_f32());
        app.spectrum.clear();
        app.spectrum.extend_from_slice(spectrum);
        app.engine.stats = Some(stats);
    }

    fn autosave(&mut self, app: &mut App, now: Instant) {
        let current = app.to_config();
        if current != self.pending {
            self.pending = current;
            self.changed_at = now;
        }
        if self.pending != self.saved && now.duration_since(self.changed_at) >= SAVE_DEBOUNCE {
            if let Some(msg) = self.save(app) {
                app.notify(msg, NoticeLevel::Warn);
            }
            if let Some(err) = &self.save_error {
                app.notify(format!("couldn't save settings: {err}"), NoticeLevel::Warn);
            }
        }
    }

    /// Writes the config if it changed. Returns a message worth showing (the
    /// location of a backed-up invalid config).
    fn save(&mut self, app: &App) -> Option<String> {
        let current = app.to_config();
        let path = self.config_path.clone()?;
        if current == self.saved {
            return None;
        }
        let mut message = None;
        if self.corrupt_on_disk {
            match config::back_up_corrupt(&path) {
                Ok(backup) => {
                    message = Some(format!("invalid config moved to {}", backup.display()))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    // Never overwrite what we couldn't preserve.
                    self.save_error = Some(format!(
                        "couldn't back up the invalid config ({e}); not overwriting it"
                    ));
                    self.saved = current;
                    return None;
                }
            }
            self.corrupt_on_disk = false;
        }
        self.save_error = config::save_to(&path, &current)
            .err()
            .map(|e| format!("{e:#}"));
        // On failure, don't retry every frame; the next edit tries again.
        self.saved = current;
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::test_app;

    fn session(app: &App, permission: fn() -> MicPermission) -> Session {
        Session::new(
            None,
            app.to_config(),
            false,
            app,
            Arc::new(AtomicBool::new(false)),
            permission,
        )
    }

    #[test]
    fn denied_permission_never_starts_the_engine() {
        for permission in [(|| MicPermission::Denied) as fn() -> MicPermission, || {
            MicPermission::Restricted
        }] {
            let mut app = test_app();
            let mut s = session(&app, permission);
            s.schedule_start(&mut app);
            assert!(!s.start_pending);
            assert!(
                matches!(&app.engine.state, EngineState::Failed(f) if f.kind == FailureKind::MicPermission)
            );
            assert!(!app.engine.mic_prompt);
        }
    }

    #[test]
    fn undecided_permission_starts_with_the_prompt_hint() {
        let mut app = test_app();
        let mut s = session(&app, || MicPermission::NotDetermined);
        s.schedule_start(&mut app);
        assert!(s.start_pending);
        assert!(app.engine.mic_prompt);
        assert_eq!(app.engine.state, EngineState::Starting);

        let mut s = session(&app, || MicPermission::Authorized);
        s.schedule_start(&mut app);
        assert!(s.start_pending);
        assert!(!app.engine.mic_prompt);
    }

    #[test]
    fn granting_access_after_a_denial_restarts() {
        let mut app = test_app();
        app.engine.state = EngineState::Failed(Failure::mic_permission(MicPermission::Denied));
        let mut s = session(&app, || MicPermission::Authorized);
        let later = s.last_permission_check + PERMISSION_INTERVAL;
        s.check_permission(&mut app, later - Duration::from_millis(1));
        assert!(!s.start_pending, "not checked before the interval");
        s.check_permission(&mut app, later);
        assert!(s.start_pending);
        assert_eq!(app.engine.state, EngineState::Starting);
    }

    #[test]
    fn permission_changes_while_running() {
        let running = EngineState::Running;
        assert_eq!(
            permission_change(true, &running, MicPermission::Denied),
            PermissionChange::Stop(Failure::mic_permission(MicPermission::Denied))
        );
        assert!(matches!(
            permission_change(true, &running, MicPermission::Restricted),
            PermissionChange::Stop(_)
        ));
        assert_eq!(
            permission_change(true, &running, MicPermission::NotDetermined),
            PermissionChange::Prompt(true)
        );
        assert_eq!(
            permission_change(true, &running, MicPermission::Authorized),
            PermissionChange::Prompt(false)
        );
        let other = EngineState::Failed(Failure::new(FailureKind::DeviceNotFound, "x"));
        assert_eq!(
            permission_change(false, &other, MicPermission::Authorized),
            PermissionChange::None,
            "only a permission failure restarts on its own"
        );
    }

    #[test]
    fn corrupt_config_is_backed_up_only_when_saving() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[[[ junk").unwrap();
        let loaded = config::load_from(&path);
        assert!(loaded.corrupt);
        assert!(path.exists(), "loading leaves the file alone");

        let app = App::new(&crate::app::tests::test_presets(), loaded.config.clone());
        let mut s = Session::new(
            Some(path.clone()),
            loaded.config,
            true,
            &app,
            Arc::new(AtomicBool::new(false)),
            || MicPermission::Authorized,
        );
        let msg = s.save(&app).expect("backup is reported");
        assert!(msg.contains("config.toml.corrupt"), "{msg}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("config.toml.corrupt")).unwrap(),
            "[[[ junk"
        );
        assert!(!config::load_from(&path).corrupt, "new config is valid");
    }
}
