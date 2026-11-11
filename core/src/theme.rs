//! Theme system. Themes are TOML files; the built-ins are embedded and written to the user's
//! themes folder on first run so they can be copied and edited.

use crate::event::Rarity;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const BUILTIN: [(&str, &str); 6] = [
    ("dark", include_str!("../../themes/dark.toml")),
    ("light", include_str!("../../themes/light.toml")),
    ("oled", include_str!("../../themes/oled.toml")),
    ("storm", include_str!("../../themes/storm.toml")),
    ("high-contrast", include_str!("../../themes/high-contrast.toml")),
    ("ember", include_str!("../../themes/ember.toml")),
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Colors {
    pub bg: String,
    pub surface: String,
    pub surface_alt: String,
    pub border: String,
    pub text: String,
    pub text_dim: String,
    pub accent: String,
    pub accent_alt: String,
    pub on_accent: String,
    pub good: String,
    pub bad: String,
    pub warn: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RarityColors {
    pub common: String,
    pub uncommon: String,
    pub rare: String,
    pub epic: String,
    pub legendary: String,
    pub mythic: String,
    pub mythic_alt: String,
}

impl Default for RarityColors {
    fn default() -> Self {
        RarityColors {
            common: "#9aa3ad".into(),
            uncommon: "#3fd35b".into(),
            rare: "#3d9bff".into(),
            epic: "#b45cff".into(),
            legendary: "#ffb62e".into(),
            mythic: "#ff4fa3".into(),
            mythic_alt: "#ffd84a".into(),
        }
    }
}

impl RarityColors {
    pub fn get(&self, r: Rarity) -> &str {
        match r {
            Rarity::Common => &self.common,
            Rarity::Uncommon => &self.uncommon,
            Rarity::Rare => &self.rare,
            Rarity::Epic => &self.epic,
            Rarity::Legendary => &self.legendary,
            Rarity::Mythic => &self.mythic,
        }
    }
    /// Apply user overrides keyed by lowercase rarity name (plus "mythic_alt").
    pub fn with_overrides(mut self, o: &BTreeMap<String, String>) -> Self {
        for (k, v) in o {
            if parse_color(v).is_none() {
                continue;
            }
            match k.as_str() {
                "common" => self.common = v.clone(),
                "uncommon" => self.uncommon = v.clone(),
                "rare" => self.rare = v.clone(),
                "epic" => self.epic = v.clone(),
                "legendary" => self.legendary = v.clone(),
                "mythic" => self.mythic = v.clone(),
                "mythic_alt" => self.mythic_alt = v.clone(),
                _ => {}
            }
        }
        self
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    #[serde(default = "d_radius")]
    pub radius: f32,
    #[serde(default = "d_radius_small")]
    pub radius_small: f32,
    #[serde(default = "d_font")]
    pub font: String,
    /// 0.0 disables glow effects, 1.0 is the default intensity.
    #[serde(default = "d_glow")]
    pub glow: f32,
}
fn d_radius() -> f32 {
    14.0
}
fn d_radius_small() -> f32 {
    8.0
}
fn d_font() -> String {
    "Segoe UI".into()
}
fn d_glow() -> f32 {
    1.0
}
impl Default for Shape {
    fn default() -> Self {
        Shape { radius: d_radius(), radius_small: d_radius_small(), font: d_font(), glow: d_glow() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub dark: bool,
    pub colors: Colors,
    #[serde(default)]
    pub rarity: RarityColors,
    #[serde(default)]
    pub shape: Shape,
}

impl Theme {
    pub fn parse(toml_src: &str) -> Result<Theme> {
        let t: Theme = toml::from_str(toml_src).context("invalid theme TOML")?;
        t.validate()?;
        Ok(t)
    }
    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.name.trim().is_empty(), "theme needs a name");
        let c = &self.colors;
        let r = &self.rarity;
        for (n, v) in [
            ("bg", &c.bg),
            ("surface", &c.surface),
            ("surface_alt", &c.surface_alt),
            ("border", &c.border),
            ("text", &c.text),
            ("text_dim", &c.text_dim),
            ("accent", &c.accent),
            ("accent_alt", &c.accent_alt),
            ("on_accent", &c.on_accent),
            ("good", &c.good),
            ("bad", &c.bad),
            ("warn", &c.warn),
            ("rarity.common", &r.common),
            ("rarity.uncommon", &r.uncommon),
            ("rarity.rare", &r.rare),
            ("rarity.epic", &r.epic),
            ("rarity.legendary", &r.legendary),
            ("rarity.mythic", &r.mythic),
            ("rarity.mythic_alt", &r.mythic_alt),
        ] {
            anyhow::ensure!(parse_color(v).is_some(), "colour '{n}' is not a valid #rrggbb[aa] value: {v}");
        }
        let sh = &self.shape;
        for (n, v, max) in
            [("radius", sh.radius, 64.0), ("radius_small", sh.radius_small, 32.0), ("glow", sh.glow, 2.0)]
        {
            anyhow::ensure!(v.is_finite() && (0.0..=max).contains(&v), "shape.{n} = {v} is outside 0..={max}");
        }
        anyhow::ensure!(
            !sh.font.trim().is_empty() && sh.font.len() <= 64,
            "shape.font must be a font family name (1-64 characters)"
        );
        anyhow::ensure!(self.name.chars().count() <= 48, "theme name is longer than 48 characters");
        Ok(())
    }
}

/// Parse `#rgb`, `#rrggbb` or `#rrggbbaa` into RGBA.
pub fn parse_color(s: &str) -> Option<[u8; 4]> {
    let h = s.trim().strip_prefix('#')?;
    if !h.is_ascii() {
        return None;
    }
    let p = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
    match h.len() {
        3 => {
            let d = |i: usize| u8::from_str_radix(&h[i..i + 1], 16).ok().map(|v| v * 17);
            Some([d(0)?, d(1)?, d(2)?, 255])
        }
        6 => Some([p(0)?, p(2)?, p(4)?, 255]),
        8 => Some([p(0)?, p(2)?, p(4)?, p(6)?]),
        _ => None,
    }
}

pub fn format_color(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

pub fn builtin_themes() -> Vec<Theme> {
    BUILTIN.iter().map(|(_, s)| Theme::parse(s).expect("built-in theme must parse")).collect()
}

/// Write built-in themes to `dir` if they are not there yet (never overwrites user edits).
pub fn install_builtin(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    for (id, src) in BUILTIN {
        let p = dir.join(format!("{id}.toml"));
        if !p.exists() {
            std::fs::write(p, src)?;
        }
    }
    Ok(())
}

/// Load every valid `*.toml` in `dir`. Invalid files are skipped and reported in the second value.
pub fn load_dir(dir: &Path) -> (Vec<Theme>, Vec<String>) {
    let mut themes = Vec::new();
    let mut errors = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut paths: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        paths.sort();
        for p in paths {
            if p.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            match std::fs::read_to_string(&p).map_err(anyhow::Error::from).and_then(|s| Theme::parse(&s)) {
                Ok(t) => themes.push(t),
                Err(e) => errors.push(format!("{}: {e}", p.display())),
            }
        }
    }
    if themes.is_empty() {
        themes = builtin_themes();
    }
    (themes, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_builtins_parse_and_have_unique_names() {
        let t = builtin_themes();
        assert!(t.len() >= 5);
        let mut names: Vec<_> = t.iter().map(|t| t.name.clone()).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), t.len());
    }

    #[test]
    fn roundtrip_through_toml() {
        let t = builtin_themes().remove(0);
        let back = Theme::parse(&t.to_toml()).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn colour_parsing() {
        assert_eq!(parse_color("#ff8000"), Some([255, 128, 0, 255]));
        assert_eq!(parse_color("#f80"), Some([255, 136, 0, 255]));
        assert_eq!(parse_color("#ff800080"), Some([255, 128, 0, 128]));
        assert_eq!(parse_color("ff8000"), None);
        assert_eq!(parse_color("#12345"), None);
        assert_eq!(parse_color("#gg0000"), None);
        assert_eq!(parse_color("#é€é"), None);
    }

    #[test]
    fn rejects_bad_colour_and_missing_name() {
        let mut t = builtin_themes().remove(0);
        t.colors.bg = "red".into();
        assert!(t.validate().is_err());
        let mut t = builtin_themes().remove(0);
        t.name = " ".into();
        assert!(t.validate().is_err());
    }

    #[test]
    fn invalid_theme_files_fail_gracefully() {
        let good = builtin_themes().remove(0).to_toml();
        assert!(good.contains("radius = 14") && good.contains("glow = 1.0"), "{good}");
        for (bad, why) in [
            (good.replace("radius = 14", "radius = nan"), "NaN radius"),
            (good.replace("radius = 14", "radius = -3"), "negative radius"),
            (good.replace("glow = 1.0", "glow = 99.0"), "huge glow"),
            (good.replace(r#"font = "Segoe UI""#, r#"font = """#), "empty font"),
            (good.replace("[colors]", "[colours]"), "missing colours table"),
            (String::new(), "empty file"),
            ("\u{0}\u{1}binary".to_string(), "binary junk"),
        ] {
            assert!(Theme::parse(&bad).is_err(), "{why} should be rejected");
        }
        // a directory with only broken files still yields usable (built-in) themes
        let dir = std::env::temp_dir().join(format!("lootline-badthemes-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broken.toml"), "name = 3").unwrap();
        let (themes, errors) = load_dir(&dir);
        assert_eq!(themes, builtin_themes());
        assert_eq!(errors.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn export_import_roundtrip_of_every_builtin() {
        for t in builtin_themes() {
            assert_eq!(Theme::parse(&t.to_toml()).unwrap(), t, "{}", t.name);
        }
    }

    #[test]
    fn rarity_section_is_optional() {
        let src = r##"
name = "Mini"
[colors]
bg="#000000" surface="#111111" surface_alt="#222222" border="#333333" text="#ffffff"
text_dim="#aaaaaa" accent="#ff0000" accent_alt="#00ff00" on_accent="#ffffff"
good="#00ff00" bad="#ff0000" warn="#ffff00"
"##;
        // TOML requires one key per line; write it properly.
        let fixed = src.replace("\" ", "\"\n");
        let t = Theme::parse(&fixed).unwrap();
        assert_eq!(t.rarity, RarityColors::default());
    }

    #[test]
    fn overrides_apply_and_ignore_bad_values() {
        let mut o = BTreeMap::new();
        o.insert("epic".to_string(), "#112233".to_string());
        o.insert("rare".to_string(), "nonsense".to_string());
        let r = RarityColors::default().with_overrides(&o);
        assert_eq!(r.epic, "#112233");
        assert_eq!(r.rare, RarityColors::default().rare);
    }

    #[test]
    fn install_does_not_overwrite_user_edits() {
        let dir = std::env::temp_dir().join(format!("lootline-theme-{}", uuid::Uuid::new_v4()));
        install_builtin(&dir).unwrap();
        std::fs::write(dir.join("dark.toml"), "edited").unwrap();
        install_builtin(&dir).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("dark.toml")).unwrap(), "edited");
        let (themes, errors) = load_dir(&dir);
        assert!(themes.len() >= 5);
        assert_eq!(errors.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
