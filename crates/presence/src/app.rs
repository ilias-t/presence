//! Pure application state and key handling. No terminal, no engine: key events
//! go in, [`Action`]s come out, and the runner applies them to the engine. This
//! is what keeps the UI logic unit-testable.

use std::collections::VecDeque;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use presence_dsp::{
    BandKind, EqSettings, HighPassSlope, MAX_BANDS, MAX_FREQ_HZ, MAX_GAIN_DB, MAX_Q, MIN_FREQ_HZ,
    MIN_GAIN_DB, MIN_Q, Preset,
};
use presence_engine::{DeviceInfo, EngineStats, Meters};

use crate::config::{CUSTOM, Config, DEFAULT_PRESET};
use crate::devices::{self, Plan};
use crate::failure::Failure;
use crate::presets::{self, PresetEntry, UserPreset};

/// A high-pass above ~1 kHz would gut a voice; the UI caps it well below the
/// DSP's full range.
pub const MAX_HIGH_PASS_HZ: f32 = 1_000.0;

const FINE_FREQ_OCTAVES: f32 = 1.0 / 12.0;
const COARSE_FREQ_OCTAVES: f32 = 1.0 / 3.0;
const FINE_GAIN_DB: f32 = 0.5;
const COARSE_GAIN_DB: f32 = 3.0;
const FINE_Q_FACTOR: f32 = 1.1;
const COARSE_Q_FACTOR: f32 = 1.5;
const NOTICE_TTL: Duration = Duration::from_secs(4);
/// Warnings (device fallbacks, config problems) stay up longer.
const WARN_TTL: Duration = Duration::from_secs(7);
const MAX_PRESET_NAME_LEN: usize = 32;

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    SetSettings(EqSettings),
    SetBypass(bool),
    /// (Re)start the engine with the app's current devices and settings.
    RestartEngine,
    /// The runner should enumerate devices and call [`App::open_device_picker`].
    OpenDevicePicker,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Presets,
    Bands,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    HighPass,
    Band(usize),
    InputGain,
    OutputGain,
    Limiter,
}

pub const ROWS: [Row; MAX_BANDS + 4] = {
    let mut rows = [Row::HighPass; MAX_BANDS + 4];
    let mut i = 0;
    while i < MAX_BANDS {
        rows[i + 1] = Row::Band(i);
        i += 1;
    }
    rows[MAX_BANDS + 1] = Row::InputGain;
    rows[MAX_BANDS + 2] = Row::OutputGain;
    rows[MAX_BANDS + 3] = Row::Limiter;
    rows
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    On,
    Type,
    Freq,
    Gain,
    Q,
}

pub const COLUMNS: [Column; 5] = [
    Column::On,
    Column::Type,
    Column::Freq,
    Column::Gain,
    Column::Q,
];

#[derive(Clone, Debug, PartialEq)]
pub enum Active {
    /// Settings are exactly this preset (built-in id or user preset name).
    Preset(String),
    /// Edited away from a preset.
    Custom { base: Option<String> },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum EngineState {
    /// Not started yet, or being restarted.
    #[default]
    Starting,
    Running,
    Failed(Failure),
}

#[derive(Clone, Debug, Default)]
pub struct EngineView {
    pub state: EngineState,
    pub stats: Option<EngineStats>,
    pub meters: Meters,
    /// Microphone permission hasn't been decided: the macOS prompt is (or is
    /// about to be) on screen.
    pub mic_prompt: bool,
    /// Running, but no input callbacks for a while: the mic delivers nothing.
    pub no_audio: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warn,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub text: String,
    pub level: NoticeLevel,
    ttl: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceChoice {
    pub label: String,
    /// `None` = automatic: the default input unless it's virtual (see
    /// [`presence_engine::auto_input_device`]) / the auto-detected virtual output.
    pub name: Option<String>,
    /// Output only: a device that plays out loud rather than into a call app.
    pub audible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerSide {
    Input,
    Output,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DevicePicker {
    pub inputs: Vec<DeviceChoice>,
    pub outputs: Vec<DeviceChoice>,
    pub input_idx: usize,
    pub output_idx: usize,
    pub side: PickerSide,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Modal {
    Help,
    SavePrompt {
        name: String,
        error: Option<String>,
    },
    ConfirmDelete {
        index: usize,
    },
    DevicePicker(DevicePicker),
    /// The picked output isn't a virtual device; confirm before playing the
    /// mic out loud.
    ConfirmAudibleOutput(DevicePicker),
}

pub struct App {
    pub presets: Vec<PresetEntry>,
    pub preset_cursor: usize,
    pub active: Active,
    pub settings: EqSettings,
    pub bypass: bool,
    pub focus: Focus,
    pub row: usize,
    pub col: usize,
    pub modal: Option<Modal>,
    /// Devices to start with (`None` = automatic input / auto virtual output).
    /// A remembered device that's currently unplugged stays here, so it is
    /// used again once it's back.
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// Devices named on the command line, not yet started successfully: they
    /// are not substituted when missing, and not persisted until they work.
    pub explicit_input: bool,
    pub explicit_output: bool,
    /// What goes into the config: the devices of the last successful start.
    saved_input_device: Option<String>,
    saved_output_device: Option<String>,
    pub engine: EngineView,
    /// Smoothed spectrum in dBFS at [`crate::spectrum::display_freqs`]; empty until audio arrives.
    pub spectrum: Vec<f32>,
    pub notice: Option<Notice>,
    queued_notices: VecDeque<Notice>,
    /// First session: keep telling the user how to route the virtual device.
    pub show_onboarding: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new(builtins: &[Preset], config: Config) -> Self {
        let presets = presets::library(builtins, &config.user_presets);
        let found = presets::find(&presets, &config.active_preset).cloned();
        let (settings, active) = match (config.settings, found) {
            _ if config.active_preset == CUSTOM => (
                config.settings.unwrap_or(EqSettings::FLAT),
                Active::Custom {
                    base: config.custom_base.clone(),
                },
            ),
            (None, Some(p)) => (p.settings, Active::Preset(p.id)),
            // A preset whose definition changed since the settings were saved:
            // the user's sound wins, labelled as derived from that preset.
            (Some(s), Some(p)) if s != p.settings => (s, Active::Custom { base: Some(p.id) }),
            (Some(s), Some(p)) => (s, Active::Preset(p.id)),
            (Some(s), None) => (s, Active::Custom { base: None }),
            // Unknown preset and nothing saved: start from the default preset.
            (None, None) => match presets::find(&presets, DEFAULT_PRESET) {
                Some(p) => (p.settings, Active::Preset(p.id.clone())),
                None => (EqSettings::FLAT, Active::Custom { base: None }),
            },
        };
        // Custom settings put the cursor on the preset they came from, so the
        // panel never describes an unrelated preset next to "active custom".
        let preset_cursor = match &active {
            Active::Preset(id) | Active::Custom { base: Some(id) } => {
                presets.iter().position(|p| &p.id == id).unwrap_or(0)
            }
            Active::Custom { base: None } => 0,
        };
        Self {
            presets,
            preset_cursor,
            active,
            settings,
            bypass: config.bypass,
            focus: Focus::Bands,
            row: 1,
            col: 2,
            modal: None,
            input_device: config.input_device.clone(),
            output_device: config.output_device.clone(),
            explicit_input: false,
            explicit_output: false,
            saved_input_device: config.input_device,
            saved_output_device: config.output_device,
            engine: EngineView::default(),
            spectrum: Vec::new(),
            notice: None,
            queued_notices: VecDeque::new(),
            show_onboarding: !config.onboarded,
            should_quit: false,
        }
    }

    pub fn to_config(&self) -> Config {
        let (active_preset, custom_base) = match &self.active {
            Active::Preset(id) => (id.clone(), None),
            Active::Custom { base } => (CUSTOM.to_owned(), base.clone()),
        };
        Config {
            input_device: self.saved_input_device.clone(),
            output_device: self.saved_output_device.clone(),
            active_preset,
            custom_base,
            bypass: self.bypass,
            onboarded: true,
            settings: Some(self.settings),
            user_presets: self
                .presets
                .iter()
                .filter(|p| p.user)
                .map(|p| UserPreset {
                    name: p.name.clone(),
                    settings: p.settings,
                })
                .collect(),
        }
    }

    pub fn selected_row(&self) -> Row {
        ROWS[self.row]
    }

    pub fn selected_column(&self) -> Column {
        COLUMNS[self.col]
    }

    /// The band index the curve should highlight, if a band row is selected.
    pub fn selected_band(&self) -> Option<usize> {
        match self.selected_row() {
            Row::Band(i) => Some(i),
            _ => None,
        }
    }

    pub fn active_label(&self) -> String {
        let name_of = |id: &str| {
            self.presets
                .iter()
                .find(|p| p.id == id)
                .map_or_else(|| id.to_owned(), |p| p.name.clone())
        };
        match &self.active {
            Active::Preset(id) => name_of(id),
            Active::Custom { base: Some(base) } => format!("custom (from {})", name_of(base)),
            Active::Custom { base: None } => "custom".to_owned(),
        }
    }

    pub fn is_active(&self, entry: &PresetEntry) -> bool {
        matches!(&self.active, Active::Preset(id) if *id == entry.id)
    }

    /// Shows a transient message in the footer. Info replaces whatever is
    /// showing (it answers the key just pressed); warnings queue up behind
    /// each other so none is lost.
    pub fn notify(&mut self, text: impl Into<String>, level: NoticeLevel) {
        let notice = Notice {
            text: text.into(),
            level,
            ttl: match level {
                NoticeLevel::Info => NOTICE_TTL,
                NoticeLevel::Warn => WARN_TTL,
            },
        };
        match (&self.notice, level) {
            (Some(current), NoticeLevel::Warn) if current.level == NoticeLevel::Warn => {
                self.queued_notices.push_back(notice)
            }
            _ => self.notice = Some(notice),
        }
    }

    /// Advance time-based UI state (notice expiry).
    pub fn tick(&mut self, dt: Duration) {
        if let Some(n) = &mut self.notice {
            n.ttl = n.ttl.saturating_sub(dt);
            if n.ttl.is_zero() {
                self.notice = self.queued_notices.pop_front();
            }
        }
    }

    /// Devices from command-line flags: used for this session's start, and
    /// remembered only once they have started successfully.
    pub fn set_explicit_devices(&mut self, input: Option<String>, output: Option<String>) {
        if input.is_some() {
            self.input_device = input;
            self.explicit_input = true;
        }
        if output.is_some() {
            self.output_device = output;
            self.explicit_output = true;
        }
    }

    /// Records a successful start. Sides that fell back to a default keep the
    /// remembered (unplugged) device; the others remember the resolved name,
    /// so a flag like `--input usb` is saved as the real device name.
    pub fn engine_started(&mut self, plan: &Plan, stats: &EngineStats) {
        if !plan.input_fell_back {
            if self.input_device.is_some() {
                self.input_device = Some(stats.input_device.clone());
            }
            self.saved_input_device = self.input_device.clone();
            self.explicit_input = false;
        }
        if !plan.output_fell_back {
            if self.output_device.is_some() {
                self.output_device = Some(stats.output_device.clone());
            }
            self.saved_output_device = self.output_device.clone();
            self.explicit_output = false;
        }
    }

    /// The output the running engine plays into is audible (speakers,
    /// headphones) rather than a virtual device.
    pub fn output_is_audible(&self) -> Option<String> {
        match (&self.engine.state, &self.engine.stats) {
            (EngineState::Running, Some(s)) if !devices::is_virtual(&s.output_device) => {
                Some(s.output_device.clone())
            }
            _ => None,
        }
    }

    pub fn open_device_picker(
        &mut self,
        inputs: &[DeviceInfo],
        outputs: &[DeviceInfo],
        detected_virtual: Option<&str>,
        auto_input: Option<&str>,
    ) {
        let mut input_choices = vec![DeviceChoice {
            label: match auto_input {
                Some(mic) => format!("Auto — {mic}"),
                None => "Auto — no microphone found".into(),
            },
            name: None,
            audible: false,
        }];
        input_choices.extend(inputs.iter().map(|d| {
            let mut tags = Vec::new();
            if d.is_default {
                tags.push("default");
            }
            if devices::is_virtual(&d.name) {
                tags.push("virtual");
            }
            DeviceChoice {
                label: if tags.is_empty() {
                    d.name.clone()
                } else {
                    format!("{}  ({})", d.name, tags.join(", "))
                },
                name: Some(d.name.clone()),
                audible: false,
            }
        }));
        let mut output_choices = vec![DeviceChoice {
            label: match detected_virtual {
                Some(v) => format!("Auto — {v}"),
                None => "Auto — no virtual device found".into(),
            },
            name: None,
            audible: false,
        }];
        output_choices.extend(outputs.iter().map(|d| {
            let audible = !devices::is_virtual(&d.name);
            DeviceChoice {
                label: format!("{}{}", d.name, if audible { "" } else { "  (virtual)" }),
                name: Some(d.name.clone()),
                audible,
            }
        }));
        // A remembered device that's unplugged stays selectable (and selected)
        // so changing only the other side doesn't silently forget it.
        let input_idx = select_or_insert(&mut input_choices, inputs, &self.input_device, false);
        let output_idx = select_or_insert(&mut output_choices, outputs, &self.output_device, true);
        self.modal = Some(Modal::DevicePicker(DevicePicker {
            inputs: input_choices,
            outputs: output_choices,
            input_idx,
            output_idx,
            side: PickerSide::Input,
        }));
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.kind != KeyEventKind::Press {
            return Vec::new();
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return self.quit();
        }
        if let Some(modal) = self.modal.take() {
            return self.handle_modal_key(modal, key);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit(),
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
                Vec::new()
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Presets => Focus::Bands,
                    Focus::Bands => Focus::Presets,
                };
                Vec::new()
            }
            KeyCode::Char('b') => {
                self.bypass = !self.bypass;
                vec![Action::SetBypass(self.bypass)]
            }
            KeyCode::Char('d') => vec![Action::OpenDevicePicker],
            KeyCode::Char('r') => vec![Action::RestartEngine],
            KeyCode::Char('s') => {
                self.modal = Some(Modal::SavePrompt {
                    name: self.save_name_suggestion(),
                    error: None,
                });
                Vec::new()
            }
            _ => match self.focus {
                Focus::Presets => self.handle_presets_key(key),
                Focus::Bands => self.handle_bands_key(key),
            },
        }
    }

    fn quit(&mut self) -> Vec<Action> {
        self.should_quit = true;
        vec![Action::Quit]
    }

    fn save_name_suggestion(&self) -> String {
        let id = match &self.active {
            Active::Preset(id) | Active::Custom { base: Some(id) } => id,
            Active::Custom { base: None } => return String::new(),
        };
        self.presets
            .iter()
            .find(|p| p.user && &p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_default()
    }

    fn handle_presets_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let last = self.presets.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.preset_cursor = self.preset_cursor.saturating_sub(1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.preset_cursor = (self.preset_cursor + 1).min(last)
            }
            KeyCode::Home | KeyCode::Char('g') => self.preset_cursor = 0,
            KeyCode::End | KeyCode::Char('G') => self.preset_cursor = last,
            KeyCode::Enter => return self.apply_preset(self.preset_cursor),
            KeyCode::Char('x') | KeyCode::Delete => match self.presets.get(self.preset_cursor) {
                Some(p) if p.user => {
                    self.modal = Some(Modal::ConfirmDelete {
                        index: self.preset_cursor,
                    })
                }
                Some(_) => self.notify("Built-in presets can't be deleted", NoticeLevel::Warn),
                None => {}
            },
            _ => {}
        }
        Vec::new()
    }

    pub fn apply_preset(&mut self, index: usize) -> Vec<Action> {
        let Some(p) = self.presets.get(index) else {
            return Vec::new();
        };
        self.settings = p.settings;
        self.active = Active::Preset(p.id.clone());
        self.preset_cursor = index;
        vec![Action::SetSettings(self.settings)]
    }

    fn handle_bands_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let (row, col) = (self.selected_row(), self.selected_column());
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.row = self.row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.row = (self.row + 1).min(ROWS.len() - 1),
            KeyCode::Left | KeyCode::Char('h') => self.col = self.col.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') => self.col = (self.col + 1).min(COLUMNS.len() - 1),
            // Map on the character, not the SHIFT flag: terminals disagree on
            // whether Shift is reported for punctuation. The shifted glyph of
            // each key is the coarse step.
            KeyCode::Char('=') => return self.edit(|s| adjust(s, row, col, 1.0, false)),
            KeyCode::Char('+') => return self.edit(|s| adjust(s, row, col, 1.0, true)),
            KeyCode::Char('-') => return self.edit(|s| adjust(s, row, col, -1.0, false)),
            KeyCode::Char('_') => return self.edit(|s| adjust(s, row, col, -1.0, true)),
            KeyCode::Char('t') => return self.edit(|s| cycle_type(s, row)),
            KeyCode::Char('T') => return self.edit(|s| cycle_type_back(s, row)),
            KeyCode::Char(' ') => return self.edit(|s| toggle(s, row)),
            KeyCode::Char('0') => return self.edit(|s| reset_gain(s, row)),
            _ => {}
        }
        Vec::new()
    }

    /// Apply an edit; if it changed anything, the settings no longer match the
    /// preset they came from.
    fn edit(&mut self, f: impl FnOnce(&mut EqSettings)) -> Vec<Action> {
        let before = self.settings;
        f(&mut self.settings);
        if self.settings == before {
            return Vec::new();
        }
        if let Active::Preset(id) = &self.active {
            self.active = Active::Custom {
                base: Some(id.clone()),
            };
        }
        vec![Action::SetSettings(self.settings)]
    }

    fn handle_modal_key(&mut self, modal: Modal, key: KeyEvent) -> Vec<Action> {
        match modal {
            Modal::Help => Vec::new(), // any key closes help
            Modal::SavePrompt { mut name, .. } => match key.code {
                KeyCode::Esc => Vec::new(),
                KeyCode::Enter => {
                    if let Err(error) = self.save_user_preset(&name) {
                        self.modal = Some(Modal::SavePrompt {
                            name,
                            error: Some(error),
                        });
                    }
                    Vec::new()
                }
                KeyCode::Backspace => {
                    name.pop();
                    self.modal = Some(Modal::SavePrompt { name, error: None });
                    Vec::new()
                }
                KeyCode::Char(c) if !c.is_control() => {
                    if name.chars().count() < MAX_PRESET_NAME_LEN {
                        name.push(c);
                    }
                    self.modal = Some(Modal::SavePrompt { name, error: None });
                    Vec::new()
                }
                _ => {
                    self.modal = Some(Modal::SavePrompt { name, error: None });
                    Vec::new()
                }
            },
            Modal::ConfirmDelete { index } => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        self.delete_user_preset(index)
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {}
                    _ => self.modal = Some(Modal::ConfirmDelete { index }),
                }
                Vec::new()
            }
            Modal::DevicePicker(mut picker) => {
                let (list_len, idx) = match picker.side {
                    PickerSide::Input => (picker.inputs.len(), &mut picker.input_idx),
                    PickerSide::Output => (picker.outputs.len(), &mut picker.output_idx),
                };
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Vec::new(),
                    KeyCode::Up | KeyCode::Char('k') => *idx = idx.saturating_sub(1),
                    KeyCode::Down | KeyCode::Char('j') => {
                        *idx = (*idx + 1).min(list_len.saturating_sub(1))
                    }
                    KeyCode::Tab
                    | KeyCode::BackTab
                    | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('h')
                    | KeyCode::Char('l') => {
                        picker.side = match picker.side {
                            PickerSide::Input => PickerSide::Output,
                            PickerSide::Output => PickerSide::Input,
                        }
                    }
                    KeyCode::Enter => {
                        let audible = picker
                            .outputs
                            .get(picker.output_idx)
                            .is_some_and(|c| c.audible);
                        if audible {
                            self.modal = Some(Modal::ConfirmAudibleOutput(picker));
                            return Vec::new();
                        }
                        return self.apply_picker(&picker);
                    }
                    _ => {}
                }
                self.modal = Some(Modal::DevicePicker(picker));
                Vec::new()
            }
            Modal::ConfirmAudibleOutput(picker) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => self.apply_picker(&picker),
                // Default is No: back to the picker to choose again.
                _ => {
                    self.modal = Some(Modal::DevicePicker(picker));
                    Vec::new()
                }
            },
        }
    }

    fn apply_picker(&mut self, picker: &DevicePicker) -> Vec<Action> {
        self.input_device = picker
            .inputs
            .get(picker.input_idx)
            .and_then(|c| c.name.clone());
        self.output_device = picker
            .outputs
            .get(picker.output_idx)
            .and_then(|c| c.name.clone());
        // Chosen from the connected list (or the remembered unplugged entry,
        // which may fall back), so nothing needs protecting from typos.
        self.explicit_input = false;
        self.explicit_output = false;
        vec![Action::RestartEngine]
    }

    /// Saves the current settings under `name` (overwriting a user preset of
    /// the same name) and makes it the active preset.
    pub fn save_user_preset(&mut self, name: &str) -> Result<(), String> {
        let name = name.trim();
        presets::validate_user_name(&self.presets, name)?;
        let index = match self
            .presets
            .iter()
            .position(|p| p.user && p.name.eq_ignore_ascii_case(name))
        {
            Some(i) => {
                self.presets[i].settings = self.settings;
                i
            }
            None => {
                self.presets.push(PresetEntry::from(&UserPreset {
                    name: name.to_owned(),
                    settings: self.settings,
                }));
                self.presets.len() - 1
            }
        };
        let id = self.presets[index].id.clone();
        self.notify(format!("Saved preset \"{id}\""), NoticeLevel::Info);
        self.active = Active::Preset(id);
        self.preset_cursor = index;
        Ok(())
    }

    fn delete_user_preset(&mut self, index: usize) {
        let Some(p) = self.presets.get(index).filter(|p| p.user) else {
            return;
        };
        let id = p.id.clone();
        self.presets.remove(index);
        self.active = match std::mem::replace(&mut self.active, Active::Custom { base: None }) {
            Active::Preset(a) | Active::Custom { base: Some(a) } if a == id => {
                Active::Custom { base: None }
            }
            other => other,
        };
        self.preset_cursor = self.preset_cursor.min(self.presets.len().saturating_sub(1));
        self.notify(format!("Deleted preset \"{id}\""), NoticeLevel::Info);
    }

    /// Sample rate used for drawing the response curve.
    pub fn sample_rate(&self) -> f32 {
        self.engine
            .stats
            .as_ref()
            .map_or(48_000.0, |s| s.input_sample_rate as f32)
    }
}

/// Index of `want` in `choices`, adding a "(not connected)" entry right after
/// the default one when it's a remembered device that's currently missing.
fn select_or_insert(
    choices: &mut Vec<DeviceChoice>,
    connected: &[DeviceInfo],
    want: &Option<String>,
    output: bool,
) -> usize {
    let Some(name) = want else {
        return 0;
    };
    if let Some(i) = choices.iter().position(|c| c.name.as_ref() == Some(name)) {
        return i;
    }
    if devices::is_present(name, connected) {
        // Saved as a lenient match (e.g. "blackhole"): select the device it resolves to.
        let lower = name.to_lowercase();
        return choices
            .iter()
            .position(|c| {
                c.name
                    .as_ref()
                    .is_some_and(|n| n.to_lowercase().contains(&lower))
            })
            .unwrap_or(0);
    }
    choices.insert(
        1,
        DeviceChoice {
            label: format!("{name}  (not connected)"),
            name: Some(name.clone()),
            audible: output && !devices::is_virtual(name),
        },
    );
    1
}

pub fn kind_uses_gain(kind: BandKind) -> bool {
    matches!(
        kind,
        BandKind::Peak | BandKind::LowShelf | BandKind::HighShelf
    )
}

const KIND_CYCLE: [BandKind; 6] = [
    BandKind::Peak,
    BandKind::LowShelf,
    BandKind::HighShelf,
    BandKind::LowPass,
    BandKind::HighPass,
    BandKind::Notch,
];

fn step_kind(kind: BandKind, forward: bool) -> BandKind {
    let i = KIND_CYCLE.iter().position(|k| *k == kind).unwrap_or(0);
    let n = KIND_CYCLE.len();
    KIND_CYCLE[if forward {
        (i + 1) % n
    } else {
        (i + n - 1) % n
    }]
}

fn step_freq(freq: f32, dir: f32, coarse: bool, max: f32) -> f32 {
    let octaves = if coarse {
        COARSE_FREQ_OCTAVES
    } else {
        FINE_FREQ_OCTAVES
    };
    (freq * 2f32.powf(dir * octaves)).clamp(MIN_FREQ_HZ, max)
}

fn step_gain(gain: f32, dir: f32, coarse: bool) -> f32 {
    let step = if coarse { COARSE_GAIN_DB } else { FINE_GAIN_DB };
    // Round to 0.1 dB so repeated steps don't accumulate float noise.
    (((gain + dir * step) * 10.0).round() / 10.0).clamp(MIN_GAIN_DB, MAX_GAIN_DB)
}

fn step_q(q: f32, dir: f32, coarse: bool) -> f32 {
    let factor = if coarse {
        COARSE_Q_FACTOR
    } else {
        FINE_Q_FACTOR
    };
    (q * factor.powf(dir)).clamp(MIN_Q, MAX_Q)
}

fn adjust(s: &mut EqSettings, row: Row, col: Column, dir: f32, coarse: bool) {
    let up = dir > 0.0;
    match row {
        Row::Band(i) => {
            let band = &mut s.bands[i];
            match col {
                Column::On => band.enabled = up,
                Column::Type => band.kind = step_kind(band.kind, up),
                Column::Freq => band.freq_hz = step_freq(band.freq_hz, dir, coarse, MAX_FREQ_HZ),
                Column::Gain if kind_uses_gain(band.kind) => {
                    band.gain_db = step_gain(band.gain_db, dir, coarse)
                }
                Column::Gain => {}
                Column::Q => band.q = step_q(band.q, dir, coarse),
            }
        }
        Row::HighPass => {
            let hp = &mut s.high_pass;
            match col {
                Column::On => hp.enabled = up,
                Column::Type => {
                    hp.slope = if up {
                        HighPassSlope::Db24
                    } else {
                        HighPassSlope::Db12
                    }
                }
                Column::Freq => hp.freq_hz = step_freq(hp.freq_hz, dir, coarse, MAX_HIGH_PASS_HZ),
                Column::Gain | Column::Q => {}
            }
        }
        // Gain rows have a single value, so every column edits it.
        Row::InputGain => s.input_gain_db = step_gain(s.input_gain_db, dir, coarse),
        Row::OutputGain => s.output_gain_db = step_gain(s.output_gain_db, dir, coarse),
        Row::Limiter => s.limiter = up,
    }
}

fn cycle_type(s: &mut EqSettings, row: Row) {
    match row {
        Row::Band(i) => s.bands[i].kind = step_kind(s.bands[i].kind, true),
        Row::HighPass => toggle_slope(s),
        _ => {}
    }
}

fn cycle_type_back(s: &mut EqSettings, row: Row) {
    match row {
        Row::Band(i) => s.bands[i].kind = step_kind(s.bands[i].kind, false),
        Row::HighPass => toggle_slope(s),
        _ => {}
    }
}

fn toggle_slope(s: &mut EqSettings) {
    s.high_pass.slope = match s.high_pass.slope {
        HighPassSlope::Db12 => HighPassSlope::Db24,
        HighPassSlope::Db24 => HighPassSlope::Db12,
    };
}

fn toggle(s: &mut EqSettings, row: Row) {
    match row {
        Row::Band(i) => s.bands[i].enabled = !s.bands[i].enabled,
        Row::HighPass => s.high_pass.enabled = !s.high_pass.enabled,
        Row::Limiter => s.limiter = !s.limiter,
        Row::InputGain | Row::OutputGain => {}
    }
}

fn reset_gain(s: &mut EqSettings, row: Row) {
    match row {
        Row::Band(i) => s.bands[i].gain_db = 0.0,
        Row::InputGain => s.input_gain_db = 0.0,
        Row::OutputGain => s.output_gain_db = 0.0,
        Row::HighPass | Row::Limiter => {}
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use presence_dsp::{Band, PresetCategory};

    pub(crate) fn test_presets() -> Vec<Preset> {
        let mut clarity = EqSettings::FLAT;
        clarity.high_pass.enabled = true;
        clarity.high_pass.freq_hz = 90.0;
        clarity.bands[0] = Band {
            enabled: true,
            kind: BandKind::Peak,
            freq_hz: 250.0,
            gain_db: -2.0,
            q: 1.0,
        };
        clarity.bands[1] = Band {
            enabled: true,
            kind: BandKind::Peak,
            freq_hz: 3_500.0,
            gain_db: 2.5,
            q: 1.0,
        };
        clarity.bands[2] = Band {
            enabled: true,
            kind: BandKind::HighShelf,
            freq_hz: 10_000.0,
            gain_db: 2.0,
            q: 0.707,
        };
        vec![
            Preset {
                id: "flat",
                name: "Flat",
                category: PresetCategory::Voice,
                description: "No processing",
                settings: EqSettings::FLAT,
            },
            Preset {
                id: "clarity",
                name: "Clarity",
                category: PresetCategory::Voice,
                description: "Crisp, present voice",
                settings: clarity,
            },
        ]
    }

    pub(crate) fn test_app() -> App {
        App::new(&test_presets(), Config::default())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(app: &mut App, code: KeyCode) -> Vec<Action> {
        app.handle_key(key(code))
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn select(app: &mut App, row: Row, col: Column) {
        app.focus = Focus::Bands;
        app.row = ROWS.iter().position(|r| *r == row).unwrap();
        app.col = COLUMNS.iter().position(|c| *c == col).unwrap();
    }

    #[test]
    fn defaults_to_clarity_preset() {
        let app = test_app();
        assert_eq!(app.active, Active::Preset("clarity".into()));
        assert_eq!(app.settings, test_presets()[1].settings);
        assert_eq!(app.active_label(), "Clarity");
        assert!(app.show_onboarding);
    }

    #[test]
    fn fine_and_coarse_gain_steps() {
        let mut app = test_app();
        select(&mut app, Row::Band(1), Column::Gain);
        let actions = press(&mut app, KeyCode::Char('='));
        assert_eq!(app.settings.bands[1].gain_db, 3.0);
        assert_eq!(actions, vec![Action::SetSettings(app.settings)]);
        press(&mut app, KeyCode::Char('+'));
        assert_eq!(app.settings.bands[1].gain_db, 6.0);
        press(&mut app, KeyCode::Char('-'));
        assert_eq!(app.settings.bands[1].gain_db, 5.5);
        press(&mut app, KeyCode::Char('_'));
        assert_eq!(app.settings.bands[1].gain_db, 2.5);
    }

    #[test]
    fn gain_clamps_at_limits() {
        let mut app = test_app();
        select(&mut app, Row::Band(1), Column::Gain);
        for _ in 0..20 {
            press(&mut app, KeyCode::Char('+'));
        }
        assert_eq!(app.settings.bands[1].gain_db, MAX_GAIN_DB);
        assert!(
            press(&mut app, KeyCode::Char('+')).is_empty(),
            "no-op edits emit nothing"
        );
        for _ in 0..30 {
            press(&mut app, KeyCode::Char('_'));
        }
        assert_eq!(app.settings.bands[1].gain_db, MIN_GAIN_DB);
    }

    #[test]
    fn frequency_steps_by_octave_fractions_and_clamps() {
        let mut app = test_app();
        select(&mut app, Row::Band(0), Column::Freq);
        press(&mut app, KeyCode::Char('='));
        let expected = 250.0 * 2f32.powf(1.0 / 12.0);
        assert!((app.settings.bands[0].freq_hz - expected).abs() < 1e-3);
        press(&mut app, KeyCode::Char('-'));
        press(&mut app, KeyCode::Char('+'));
        assert!((app.settings.bands[0].freq_hz - 250.0 * 2f32.powf(1.0 / 3.0)).abs() < 1e-2);
        for _ in 0..50 {
            press(&mut app, KeyCode::Char('+'));
        }
        assert_eq!(app.settings.bands[0].freq_hz, MAX_FREQ_HZ);
        for _ in 0..50 {
            press(&mut app, KeyCode::Char('_'));
        }
        assert_eq!(app.settings.bands[0].freq_hz, MIN_FREQ_HZ);
    }

    #[test]
    fn high_pass_frequency_has_its_own_ceiling() {
        let mut app = test_app();
        select(&mut app, Row::HighPass, Column::Freq);
        for _ in 0..40 {
            press(&mut app, KeyCode::Char('+'));
        }
        assert_eq!(app.settings.high_pass.freq_hz, MAX_HIGH_PASS_HZ);
    }

    #[test]
    fn q_multiplies_and_clamps() {
        let mut app = test_app();
        select(&mut app, Row::Band(0), Column::Q);
        press(&mut app, KeyCode::Char('='));
        assert!((app.settings.bands[0].q - 1.1).abs() < 1e-6);
        press(&mut app, KeyCode::Char('_'));
        assert!((app.settings.bands[0].q - 1.1 / 1.5).abs() < 1e-6);
        for _ in 0..40 {
            press(&mut app, KeyCode::Char('_'));
        }
        assert_eq!(app.settings.bands[0].q, MIN_Q);
        for _ in 0..40 {
            press(&mut app, KeyCode::Char('+'));
        }
        assert_eq!(app.settings.bands[0].q, MAX_Q);
    }

    #[test]
    fn gain_is_ignored_for_filters_without_gain() {
        let mut app = test_app();
        select(&mut app, Row::Band(0), Column::Gain);
        app.settings.bands[0].kind = BandKind::Notch;
        assert!(press(&mut app, KeyCode::Char('=')).is_empty());
    }

    #[test]
    fn selection_is_clamped() {
        let mut app = test_app();
        select(&mut app, Row::HighPass, Column::On);
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Char('h'));
        assert_eq!((app.row, app.col), (0, 0));
        for _ in 0..30 {
            press(&mut app, KeyCode::Char('j'));
            press(&mut app, KeyCode::Right);
        }
        assert_eq!(app.selected_row(), Row::Limiter);
        assert_eq!(app.selected_column(), Column::Q);
    }

    #[test]
    fn type_toggle_and_reset_keys() {
        let mut app = test_app();
        select(&mut app, Row::Band(0), Column::Freq);
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.settings.bands[0].kind, BandKind::LowShelf);
        press(&mut app, KeyCode::Char('T'));
        assert_eq!(app.settings.bands[0].kind, BandKind::Peak);
        press(&mut app, KeyCode::Char(' '));
        assert!(!app.settings.bands[0].enabled);
        press(&mut app, KeyCode::Char('0'));
        assert_eq!(app.settings.bands[0].gain_db, 0.0);

        select(&mut app, Row::HighPass, Column::Freq);
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(app.settings.high_pass.slope, HighPassSlope::Db24);

        select(&mut app, Row::Limiter, Column::On);
        press(&mut app, KeyCode::Char(' '));
        assert!(!app.settings.limiter);
    }

    #[test]
    fn apply_preset_then_edit_marks_custom() {
        let mut app = test_app();
        app.focus = Focus::Presets;
        press(&mut app, KeyCode::Up);
        let actions = press(&mut app, KeyCode::Enter);
        assert_eq!(app.active, Active::Preset("flat".into()));
        assert_eq!(actions, vec![Action::SetSettings(EqSettings::FLAT)]);

        select(&mut app, Row::Band(3), Column::On);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(
            app.active,
            Active::Custom {
                base: Some("flat".into())
            }
        );
        assert_eq!(app.active_label(), "custom (from Flat)");

        // Further edits keep the original base.
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(
            app.active,
            Active::Custom {
                base: Some("flat".into())
            }
        );
        let config = app.to_config();
        assert_eq!(config.active_preset, CUSTOM);
        assert_eq!(config.custom_base.as_deref(), Some("flat"));
    }

    #[test]
    fn bypass_toggles_and_emits_action() {
        let mut app = test_app();
        assert_eq!(
            press(&mut app, KeyCode::Char('b')),
            vec![Action::SetBypass(true)]
        );
        assert!(app.bypass);
        assert_eq!(
            press(&mut app, KeyCode::Char('b')),
            vec![Action::SetBypass(false)]
        );
        assert_eq!(
            app.active,
            Active::Preset("clarity".into()),
            "bypass is not an edit"
        );
    }

    #[test]
    fn save_user_preset_via_prompt() {
        let mut app = test_app();
        select(&mut app, Row::Band(1), Column::Gain);
        press(&mut app, KeyCode::Char('+'));
        press(&mut app, KeyCode::Char('s'));
        assert!(matches!(app.modal, Some(Modal::SavePrompt { .. })));
        type_str(&mut app, "Podcastx");
        press(&mut app, KeyCode::Backspace);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.modal, None);
        let saved = app.presets.last().unwrap();
        assert!(saved.user);
        assert_eq!(saved.name, "Podcast");
        assert_eq!(saved.settings, app.settings);
        assert_eq!(app.active, Active::Preset("Podcast".into()));
        assert_eq!(app.to_config().user_presets.len(), 1);
    }

    #[test]
    fn save_rejects_builtin_names_and_keeps_prompt_open() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char('s'));
        type_str(&mut app, "clarity");
        press(&mut app, KeyCode::Enter);
        match &app.modal {
            Some(Modal::SavePrompt { error: Some(e), .. }) => assert!(e.contains("built-in")),
            other => panic!("expected prompt with error, got {other:?}"),
        }
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.modal, None);
        assert!(app.presets.iter().all(|p| !p.user));
    }

    #[test]
    fn saving_existing_name_overwrites() {
        let mut app = test_app();
        app.save_user_preset("Mine").unwrap();
        app.settings.bands[0].gain_db = -6.0;
        app.save_user_preset("mine").unwrap();
        let user: Vec<_> = app.presets.iter().filter(|p| p.user).collect();
        assert_eq!(user.len(), 1);
        assert_eq!(user[0].settings.bands[0].gain_db, -6.0);
    }

    #[test]
    fn delete_user_preset_requires_confirmation() {
        let mut app = test_app();
        app.save_user_preset("Mine").unwrap();
        app.focus = Focus::Presets;
        let idx = app.presets.len() - 1;
        app.preset_cursor = idx;

        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.modal, Some(Modal::ConfirmDelete { index: idx }));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.presets.len(), 3, "declined delete keeps the preset");

        press(&mut app, KeyCode::Delete);
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.presets.len(), 2);
        assert_eq!(app.active, Active::Custom { base: None });
        assert!(app.preset_cursor < app.presets.len());
    }

    #[test]
    fn builtin_presets_cannot_be_deleted() {
        let mut app = test_app();
        app.focus = Focus::Presets;
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.modal, None);
        assert_eq!(app.notice.as_ref().unwrap().level, NoticeLevel::Warn);
    }

    #[test]
    fn global_keys() {
        let mut app = test_app();
        assert_eq!(
            press(&mut app, KeyCode::Char('d')),
            vec![Action::OpenDevicePicker]
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('r')),
            vec![Action::RestartEngine]
        );
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.focus, Focus::Presets);
        press(&mut app, KeyCode::BackTab);
        assert_eq!(app.focus, Focus::Bands);
        press(&mut app, KeyCode::Char('?'));
        assert_eq!(app.modal, Some(Modal::Help));
        assert!(
            press(&mut app, KeyCode::Char('q')).is_empty(),
            "q closes help first"
        );
        assert_eq!(app.modal, None);
        assert_eq!(press(&mut app, KeyCode::Char('q')), vec![Action::Quit]);
        assert!(app.should_quit);

        let mut app = test_app();
        app.modal = Some(Modal::Help);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.handle_key(ctrl_c), vec![Action::Quit]);
    }

    #[test]
    fn device_picker_selects_devices_and_restarts() {
        let dev = |name: &str, is_default| DeviceInfo {
            name: name.into(),
            is_default,
            channels: 2,
            default_sample_rate: 48_000,
        };
        let mut app = test_app();
        app.open_device_picker(
            &[dev("MacBook Pro Microphone", true), dev("USB Mic", false)],
            &[
                dev("MacBook Pro Speakers", true),
                dev("BlackHole 2ch", false),
            ],
            Some("BlackHole 2ch"),
            Some("MacBook Pro Microphone"),
        );
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Enter), vec![Action::RestartEngine]);
        assert_eq!(app.input_device.as_deref(), Some("USB Mic"));
        assert_eq!(app.output_device.as_deref(), Some("BlackHole 2ch"));
        assert_eq!(app.modal, None);
    }

    #[test]
    fn config_restores_custom_state() {
        let mut app = test_app();
        select(&mut app, Row::OutputGain, Column::On);
        press(&mut app, KeyCode::Char('+'));
        app.bypass = true;
        let restored = App::new(&test_presets(), app.to_config());
        assert_eq!(restored.settings, app.settings);
        assert_eq!(restored.active, app.active);
        assert!(restored.bypass);
        assert!(!restored.show_onboarding);
    }

    #[test]
    fn notices_expire() {
        let mut app = test_app();
        app.notify("hello", NoticeLevel::Info);
        app.tick(Duration::from_secs(1));
        assert!(app.notice.is_some());
        app.tick(Duration::from_secs(5));
        assert!(app.notice.is_none());
    }

    fn stats(input: &str, output: &str) -> EngineStats {
        EngineStats {
            input_device: input.into(),
            output_device: output.into(),
            input_sample_rate: 48_000,
            output_sample_rate: 48_000,
            latency_ms: 10.0,
            underruns: 0,
            overruns: 0,
        }
    }

    fn open_picker(app: &mut App) {
        use crate::devices::tests::dev;
        app.open_device_picker(
            &[dev("MacBook Pro Microphone", true), dev("USB Mic", false)],
            &[
                dev("MacBook Pro Speakers", true),
                dev("BlackHole 2ch", false),
            ],
            Some("BlackHole 2ch"),
            Some("MacBook Pro Microphone"),
        );
    }

    #[test]
    fn audible_output_needs_confirmation() {
        let mut app = test_app();
        open_picker(&mut app);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down); // MacBook Pro Speakers
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(matches!(app.modal, Some(Modal::ConfirmAudibleOutput(_))));
        // Anything but y goes back to the picker, nothing applied.
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(matches!(app.modal, Some(Modal::DevicePicker(_))));
        assert_eq!(app.output_device, None);

        press(&mut app, KeyCode::Enter);
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            vec![Action::RestartEngine]
        );
        assert_eq!(app.output_device.as_deref(), Some("MacBook Pro Speakers"));
        assert_eq!(app.modal, None);
    }

    #[test]
    fn virtual_output_applies_without_confirmation() {
        let mut app = test_app();
        open_picker(&mut app);
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down); // BlackHole 2ch
        assert_eq!(press(&mut app, KeyCode::Enter), vec![Action::RestartEngine]);
        assert_eq!(app.modal, None);
        assert_eq!(app.output_device.as_deref(), Some("BlackHole 2ch"));
    }

    #[test]
    fn picker_keeps_a_missing_saved_input() {
        let mut app = App::new(
            &test_presets(),
            Config {
                input_device: Some("Yeti".into()),
                ..Config::default()
            },
        );
        open_picker(&mut app);
        let Some(Modal::DevicePicker(picker)) = &app.modal else {
            panic!("no picker");
        };
        assert_eq!(
            picker.inputs[picker.input_idx].label,
            "Yeti  (not connected)"
        );
        // Changing only the output keeps the remembered mic.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_device.as_deref(), Some("Yeti"));
        assert_eq!(app.output_device.as_deref(), Some("BlackHole 2ch"));
    }

    #[test]
    fn explicit_devices_persist_only_after_a_successful_start() {
        let mut app = App::new(
            &test_presets(),
            Config {
                input_device: Some("MacBook Pro Microphone".into()),
                ..Config::default()
            },
        );
        app.set_explicit_devices(Some("usb".into()), None);
        assert!(app.explicit_input);
        assert_eq!(app.input_device.as_deref(), Some("usb"));
        // Not started yet (or it failed): the old device is what gets saved.
        assert_eq!(
            app.to_config().input_device.as_deref(),
            Some("MacBook Pro Microphone")
        );

        app.engine_started(&Plan::default(), &stats("USB Mic", "BlackHole 2ch"));
        assert!(!app.explicit_input);
        // The resolved name is remembered, not the fragment typed.
        assert_eq!(app.to_config().input_device.as_deref(), Some("USB Mic"));
        // Auto output stays auto rather than pinning the detected device.
        assert_eq!(app.to_config().output_device, None);
    }

    #[test]
    fn fallback_start_keeps_remembering_the_missing_device() {
        let mut app = App::new(
            &test_presets(),
            Config {
                input_device: Some("Yeti".into()),
                ..Config::default()
            },
        );
        let plan = Plan {
            input_fell_back: true,
            ..Plan::default()
        };
        app.engine_started(&plan, &stats("MacBook Pro Microphone", "BlackHole 2ch"));
        assert_eq!(app.input_device.as_deref(), Some("Yeti"));
        assert_eq!(app.to_config().input_device.as_deref(), Some("Yeti"));
    }

    #[test]
    fn picked_devices_are_not_saved_until_they_start() {
        let mut app = test_app();
        open_picker(&mut app);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down); // USB Mic
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.input_device.as_deref(), Some("USB Mic"));
        assert_eq!(app.to_config().input_device, None, "start hasn't succeeded");
        app.engine_started(&Plan::default(), &stats("USB Mic", "BlackHole 2ch"));
        assert_eq!(app.to_config().input_device.as_deref(), Some("USB Mic"));
    }

    #[test]
    fn audible_output_is_detected_from_the_running_engine() {
        let mut app = test_app();
        app.engine.state = EngineState::Running;
        app.engine.stats = Some(stats("Mic", "BlackHole 2ch"));
        assert_eq!(app.output_is_audible(), None);
        app.engine.stats = Some(stats("Mic", "MacBook Pro Speakers"));
        assert_eq!(
            app.output_is_audible().as_deref(),
            Some("MacBook Pro Speakers")
        );
        app.engine.state = EngineState::Starting;
        assert_eq!(app.output_is_audible(), None);
    }

    #[test]
    fn custom_settings_put_the_cursor_on_their_base() {
        let mut config = test_app().to_config();
        config.active_preset = CUSTOM.into();
        config.custom_base = Some("clarity".into());
        let app = App::new(&test_presets(), config);
        assert_eq!(app.presets[app.preset_cursor].id, "clarity");
    }

    #[test]
    fn unknown_preset_without_settings_uses_the_default() {
        let app = App::new(
            &test_presets(),
            Config {
                active_preset: "gone".into(),
                ..Config::default()
            },
        );
        assert_eq!(app.active, Active::Preset("clarity".into()));
    }

    #[test]
    fn warnings_queue_instead_of_replacing_each_other() {
        let mut app = test_app();
        app.notify("first", NoticeLevel::Warn);
        app.notify("second", NoticeLevel::Warn);
        assert_eq!(app.notice.as_ref().unwrap().text, "first");
        app.tick(WARN_TTL);
        assert_eq!(app.notice.as_ref().unwrap().text, "second");
        app.tick(WARN_TTL);
        assert!(app.notice.is_none());
    }
}
