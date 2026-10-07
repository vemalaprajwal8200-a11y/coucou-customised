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
    #[serde(default)]
    pub automation_folders: Vec<String>,
    #[serde(default = "default_provider_mode")]
    pub provider_mode: String,
    #[serde(default = "default_ollama_model")]
    pub ollama_model: String,
    #[serde(default)]
    pub spoken_replies: bool,
    #[serde(default = "default_speak_replies_mode")]
    pub speak_replies_mode: String,
    #[serde(default = "default_tts_engine")]
    pub tts_engine: String,
    #[serde(default)]
    pub tts_voice: String,
    #[serde(default = "default_tts_rate")]
    pub tts_rate: f64,
    #[serde(default = "default_tts_volume")]
    pub tts_volume: f64,
    #[serde(default = "default_wake_word_enabled")]
    pub wake_word_enabled: bool,
    #[serde(default = "default_wake_word_pronunciation")]
    pub wake_word_pronunciation: String,
    #[serde(default = "default_wake_word_threshold")]
    pub wake_word_threshold: f64,
}

fn default_provider_mode() -> String {
    "auto".into()
}

fn default_ollama_model() -> String {
    "qwen2.5:7b".into()
}

fn default_speak_replies_mode() -> String {
    "voiceOnly".into()
}

fn default_tts_engine() -> String {
    "auto".into()
}

fn default_tts_rate() -> f64 {
    1.0
}

fn default_tts_volume() -> f64 {
    1.0
}

fn default_wake_word_enabled() -> bool {
    true
}

fn default_wake_word_pronunciation() -> String {
    "Hey Macha".into()
}

fn default_wake_word_threshold() -> f64 {
    0.012
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
            automation_folders: Vec::new(),
            provider_mode: default_provider_mode(),
            ollama_model: default_ollama_model(),
            spoken_replies: false,
            speak_replies_mode: default_speak_replies_mode(),
            tts_engine: default_tts_engine(),
            tts_voice: String::new(),
            tts_rate: default_tts_rate(),
            tts_volume: default_tts_volume(),
            wake_word_enabled: default_wake_word_enabled(),
            wake_word_pronunciation: default_wake_word_pronunciation(),
            wake_word_threshold: default_wake_word_threshold(),
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
        Ok(bytes) => {
            let document = serde_json::from_slice::<serde_json::Value>(&bytes)
                .unwrap_or(serde_json::Value::Null);
            let mut settings: Settings =
                serde_json::from_value(document.clone()).unwrap_or_default();
            migrate_speech_settings(&mut settings, &document);
            settings
        }
        Err(_) => Settings::default(),
    };
    settings.model = migrate_model(settings.model);
    settings
}

fn migrate_speech_settings(settings: &mut Settings, document: &serde_json::Value) {
    if document.get("speakRepliesMode").is_some() {
        return;
    }
    if let Some(legacy_enabled) = document
        .get("spokenReplies")
        .and_then(|value| value.as_bool())
    {
        settings.speak_replies_mode = if legacy_enabled {
            "always".into()
        } else {
            default_speak_replies_mode()
        };
    }
}

fn migrate_model(model: String) -> String {
    if matches!(
        model.as_str(),
        "claude-opus-5"
            | "claude-sonnet-5"
            | "claude-haiku-4-5"
            | "nvidia/nemotron-3-super-120b-a12b:free"
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
    use super::{
        default_auto_hide, default_model, default_speak_replies_mode, migrate_model,
        migrate_speech_settings, Settings,
    };

    #[test]
    fn voice_reply_preferences_default_to_voice_only_and_keep_legacy_choice() {
        let mut settings = Settings::default();
        assert_eq!(settings.speak_replies_mode, "voiceOnly");
        assert_eq!(settings.tts_engine, "auto");
        assert_eq!(settings.tts_rate, 1.0);
        assert_eq!(settings.tts_volume, 1.0);
        assert!(settings.wake_word_enabled);
        assert_eq!(settings.wake_word_pronunciation, "Hey Macha");
        assert_eq!(settings.wake_word_threshold, 0.012);
        assert_eq!(default_speak_replies_mode(), "voiceOnly");

        migrate_speech_settings(&mut settings, &serde_json::json!({ "spokenReplies": true }));
        assert_eq!(settings.speak_replies_mode, "always");

        migrate_speech_settings(
            &mut settings,
            &serde_json::json!({ "spokenReplies": false }),
        );
        assert_eq!(settings.speak_replies_mode, "voiceOnly");

        migrate_speech_settings(
            &mut settings,
            &serde_json::json!({ "spokenReplies": true, "speakRepliesMode": "off" }),
        );
        assert_eq!(settings.speak_replies_mode, "voiceOnly");
    }

    #[test]
    fn default_and_legacy_models_use_free_auto_router() {
        assert_eq!(default_model(), "openrouter/free");
        for old_model in [
            "claude-opus-5",
            "claude-sonnet-5",
            "claude-haiku-4-5",
            "nvidia/nemotron-3-super-120b-a12b:free",
        ] {
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
