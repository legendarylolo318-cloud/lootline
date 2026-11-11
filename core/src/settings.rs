//! User settings, stored as TOML.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlaySettings {
    pub enabled: bool,
    /// Top-left position in logical pixels.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub timeout_secs: f32,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        OverlaySettings { enabled: true, x: 40, y: 300, width: 420, height: 150, timeout_secs: 3.0 }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub player_id: String,
    pub player_name: String,
    pub player_color: String,
    /// Key name of the in-game interact key, e.g. "E".
    pub interact_key: String,
    pub hotkey_add: String,
    pub hotkey_undo: String,
    /// 0.0 (strict) ..= 1.0 (loose).
    pub sensitivity: f32,
    pub detection_enabled: bool,
    pub overlay: OverlaySettings,
    pub theme: String,
    pub rarity_overrides: BTreeMap<String, String>,
    pub season: String,
    /// Raw base URL of the repo that hosts `loot_tables/` and `templates/`.
    pub repo_base_url: String,
    pub minimize_to_tray: bool,
    pub start_minimized: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            player_id: String::new(),
            player_name: whoami_fallback(),
            player_color: "#4f8cff".into(),
            interact_key: "E".into(),
            hotkey_add: "F8".into(),
            hotkey_undo: "F9".into(),
            sensitivity: 0.5,
            detection_enabled: true,
            overlay: OverlaySettings::default(),
            theme: "Dark".into(),
            rarity_overrides: BTreeMap::new(),
            season: "placeholder".into(),
            repo_base_url: "https://raw.githubusercontent.com/lootline/lootline/main".into(),
            minimize_to_tray: true,
            start_minimized: false,
        }
    }
}

fn whoami_fallback() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "Player".into())
}

impl Settings {
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
    /// Detection threshold for the matcher; sensitivity 1.0 is loosest.
    pub fn threshold(&self) -> f32 {
        threshold_for(self.sensitivity)
    }
}

pub fn threshold_for(sensitivity: f32) -> f32 {
    let s = sensitivity.clamp(0.0, 1.0);
    0.92 - 0.42 * s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("lootline-set-{}", uuid::Uuid::new_v4()));
        let p = dir.join("settings.toml");
        let mut s = Settings::default();
        s.interact_key = "F".into();
        s.overlay.x = 99;
        s.save(&p).unwrap();
        assert_eq!(Settings::load(&p), s);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn partial_and_corrupt_files_fall_back_to_defaults() {
        let dir = std::env::temp_dir().join(format!("lootline-set-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("s.toml");
        std::fs::write(&p, "interact_key = \"R\"\n").unwrap();
        let s = Settings::load(&p);
        assert_eq!(s.interact_key, "R");
        assert_eq!(s.hotkey_add, "F8");
        std::fs::write(&p, "{{{{").unwrap();
        assert_eq!(Settings::load(&p).interact_key, "E");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn threshold_is_monotonic() {
        assert!(threshold_for(0.0) > threshold_for(0.5));
        assert!(threshold_for(0.5) > threshold_for(1.0));
        assert!(threshold_for(1.0) > 0.4);
    }
}
