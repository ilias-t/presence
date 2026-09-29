//! The preset library shown in the UI: built-ins from `presence-dsp` followed by
//! the user's saved presets.

use presence_dsp::{EqSettings, Preset, PresetCategory};
use serde::{Deserialize, Serialize};

/// A user-saved preset as stored in the config file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UserPreset {
    pub name: String,
    pub settings: EqSettings,
}

/// Owned view over both kinds of preset; `dsp::Preset` has `&'static` fields so
/// it cannot represent user presets.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetEntry {
    /// Built-ins use their stable id; user presets are identified by name.
    pub id: String,
    pub name: String,
    pub description: String,
    pub settings: EqSettings,
    pub user: bool,
    /// Section a built-in is listed under; `None` for user presets.
    pub category: Option<PresetCategory>,
}

impl PresetEntry {
    /// Heading of the list section this entry belongs to.
    pub fn section(&self) -> &'static str {
        self.category.map_or("USER", PresetCategory::label)
    }
}

impl From<&Preset> for PresetEntry {
    fn from(p: &Preset) -> Self {
        Self {
            id: p.id.to_owned(),
            name: p.name.to_owned(),
            description: p.description.to_owned(),
            settings: p.settings,
            user: false,
            category: Some(p.category),
        }
    }
}

impl From<&UserPreset> for PresetEntry {
    fn from(p: &UserPreset) -> Self {
        Self {
            id: p.name.clone(),
            name: p.name.clone(),
            description: "User preset".to_owned(),
            settings: p.settings,
            user: true,
            category: None,
        }
    }
}

/// Built-ins grouped by category (voice, then fun), followed by user presets.
/// Lists print a section heading wherever [`PresetEntry::section`] changes, so
/// each section must be contiguous.
pub fn library(builtins: &[Preset], user: &[UserPreset]) -> Vec<PresetEntry> {
    let in_category = |category| {
        builtins
            .iter()
            .filter(move |p| p.category == category)
            .map(PresetEntry::from)
    };
    in_category(PresetCategory::Voice)
        .chain(in_category(PresetCategory::Fun))
        .chain(user.iter().map(PresetEntry::from))
        .collect()
}

/// Case-insensitive lookup by id or name, built-ins first.
pub fn find<'a>(entries: &'a [PresetEntry], key: &str) -> Option<&'a PresetEntry> {
    entries
        .iter()
        .find(|p| p.id.eq_ignore_ascii_case(key) || p.name.eq_ignore_ascii_case(key))
}

/// Why a name can't be used for a new user preset.
pub fn validate_user_name(builtins: &[PresetEntry], name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("name can't be empty".into());
    }
    if name.eq_ignore_ascii_case("custom") {
        return Err("\"custom\" is reserved".into());
    }
    // Built-in and user presets share one namespace in the config file and on the CLI.
    if builtins
        .iter()
        .any(|p| !p.user && (p.id.eq_ignore_ascii_case(name) || p.name.eq_ignore_ascii_case(name)))
    {
        return Err(format!("\"{name}\" is a built-in preset"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_groups_voice_then_fun_then_user() {
        let mut builtins = presence_dsp::builtin_presets();
        // Interleave the categories: the library must still group them.
        builtins.reverse();
        let user = [UserPreset {
            name: "Mine".into(),
            settings: EqSettings::FLAT,
        }];
        let entries = library(&builtins, &user);
        let sections: Vec<&str> = entries.iter().map(PresetEntry::section).collect();
        let first_fun = sections.iter().position(|s| *s == "FUN").unwrap();
        assert!(sections[..first_fun].iter().all(|s| *s == "VOICE"));
        assert_eq!(sections.last(), Some(&"USER"));
        assert!(
            sections[first_fun..sections.len() - 1]
                .iter()
                .all(|s| *s == "FUN")
        );
        assert_eq!(entries.len(), builtins.len() + 1);
    }

    #[test]
    fn builtin_names_do_not_shadow_other_ids() {
        // `find` matches names as well as ids, so a name must never equal
        // another preset's id.
        let entries = library(&presence_dsp::builtin_presets(), &[]);
        for a in &entries {
            for b in &entries {
                if a.id != b.id {
                    assert!(
                        !a.name.eq_ignore_ascii_case(&b.id),
                        "{} vs {}",
                        a.name,
                        b.id
                    );
                }
            }
        }
    }
}
