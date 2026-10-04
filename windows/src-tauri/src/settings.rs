// Preferences, stored as plain JSON in settings.json under platform::config_dir().
// No secret ever lands here — API keys live in the OS keychain (see secrets.rs).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub sound_enabled: bool,
    pub sound_volume: f64,
    pub auto_close_interval: f64,
    pub absence_interval: f64,
    pub active_integrations: Vec<String>,
    /// "primary" = the main display, "cursor" = whichever display the mouse is on.
    pub screen: String,
    pub autostart: bool,
    pub hooks_installed: bool,
    #[serde(default = "default_auto_hide")]
    pub auto_hide: bool,
    /// Chat model used by the Windows app. Changeable in the settings window.
    /// Defaulted explicitly so a settings.json written by an older build still loads.
    #[serde(default = "default_model")]
    pub model: String,
}

fn default_model() -> String {
    crate::claude::DEFAULT_MODEL.to_string()
}

fn default_auto_hide() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sound_enabled: true,
            sound_volume: 0.12,
            auto_close_interval: 15.0,
            absence_interval: 180.0,
            active_integrations: vec![
                "integration_resend".into(),
                "integration_n8n".into(),
                "integration_vercel".into(),
                "integration_github".into(),
            ],
            screen: "primary".into(),
            autostart: false,
            hooks_installed: false,
            auto_hide: default_auto_hide(),
            model: default_model(),
        }
    }
}

pub use crate::platform::{config_dir, local_dir};

pub fn hook_exe_path() -> PathBuf {
    local_dir().join("bin").join(crate::platform::HOOK_EXE)
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn load() -> Settings {
    let mut settings = match std::fs::read(settings_path()) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Settings::default(),
    };
    settings.model = migrate_model(settings.model);
    settings
}

fn migrate_model(model: String) -> String {
    if matches!(
        model.as_str(),
        "claude-opus-5" | "claude-sonnet-5" | "claude-haiku-4-5"
    ) {
        default_model()
    } else {
        model
    }
}

pub fn save(settings: &Settings) -> std::io::Result<()> {
    let dir = config_dir();
    crate::platform::ensure_private_dir(&dir)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(settings_path(), json)
}

#[cfg(test)]
mod tests {
    use super::{default_auto_hide, default_model, migrate_model};

    #[test]
    fn default_and_migrated_models_use_nemotron_free() {
        assert_eq!(default_model(), "nvidia/nemotron-3-super-120b-a12b:free");
        for old_model in ["claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"] {
            assert_eq!(migrate_model(old_model.into()), default_model());
        }
    }

    #[test]
    fn custom_model_preference_is_preserved() {
        let custom_model = "openai/gpt-model";
        assert_eq!(migrate_model(custom_model.into()), custom_model);
    }

    #[test]
    fn auto_hide_is_enabled_by_default() {
        assert!(default_auto_hide());
        assert!(super::Settings::default().auto_hide);
    }
}
