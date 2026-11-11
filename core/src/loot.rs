//! Loot tables: parsing, bundled fallback, local cache, and fetching from the repo.

use crate::event::Rarity;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const BUNDLED_PLACEHOLDER: &str = include_str!("../../loot_tables/placeholder.json");

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LootItem {
    pub name: String,
    pub rarity: Rarity,
    /// Per-chest probability of this item, 0..=1.
    pub p: f64,
    #[serde(default)]
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LootTable {
    pub season: String,
    #[serde(default)]
    pub placeholder: bool,
    #[serde(default)]
    pub note: String,
    pub items: Vec<LootItem>,
}

impl LootTable {
    pub fn parse(json: &str) -> Result<LootTable> {
        let t: LootTable = serde_json::from_str(json).context("invalid loot table JSON")?;
        t.validate()?;
        Ok(t)
    }

    pub fn bundled() -> LootTable {
        LootTable::parse(BUNDLED_PLACEHOLDER).expect("bundled loot table must parse")
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(!self.items.is_empty(), "loot table has no items");
        for i in &self.items {
            anyhow::ensure!(
                i.p.is_finite() && (0.0..=1.0).contains(&i.p),
                "item '{}' has probability {} outside 0..=1",
                i.name,
                i.p
            );
            anyhow::ensure!(!i.name.trim().is_empty(), "item with empty name");
        }
        Ok(())
    }

    /// Probability that a single chest yields something of this rarity (sum over its items).
    pub fn rarity_p(&self, r: Rarity) -> f64 {
        self.items.iter().filter(|i| i.rarity == r).map(|i| i.p).sum::<f64>().min(1.0)
    }

    pub fn item(&self, name: &str) -> Option<&LootItem> {
        self.items.iter().find(|i| i.name == name)
    }

    pub fn item_names(&self) -> Vec<String> {
        self.items.iter().map(|i| i.name.clone()).collect()
    }
}

/// Season ids are used in a URL and a file name, so only [A-Za-z0-9_-] survive.
pub fn safe_season(season: &str) -> String {
    let s: String = season.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    if s.is_empty() {
        "placeholder".into()
    } else {
        s
    }
}

fn cache_path(dir: &Path, season: &str) -> PathBuf {
    dir.join(format!("{}.json", safe_season(season)))
}

/// Load the cached table for `season`, falling back to the bundled placeholder.
pub fn load_cached_or_bundled(cache_dir: &Path, season: &str) -> LootTable {
    std::fs::read_to_string(cache_path(cache_dir, season))
        .ok()
        .and_then(|s| LootTable::parse(&s).ok())
        .unwrap_or_else(LootTable::bundled)
}

/// Fetch `<base_url>/loot_tables/<season>.json`, validate it, and cache it. Blocking.
pub fn fetch_and_cache(base_url: &str, season: &str, cache_dir: &Path) -> Result<LootTable> {
    let url = format!("{}/loot_tables/{}.json", base_url.trim_end_matches('/'), safe_season(season));
    let body = http_get(&url)?;
    let t = LootTable::parse(&body)?;
    std::fs::create_dir_all(cache_dir)?;
    std::fs::write(cache_path(cache_dir, season), body)?;
    Ok(t)
}

/// Largest response accepted from the repo (loot tables and templates are a few KB).
pub const MAX_DOWNLOAD: u64 = 2 * 1024 * 1024;

pub fn http_get(url: &str) -> Result<String> {
    String::from_utf8(http_get_bytes(url)?).context("response is not UTF-8")
}

pub fn http_get_bytes(url: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    anyhow::ensure!(url.starts_with("https://") || url.starts_with("http://"), "not an http(s) URL: {url}");
    let client =
        reqwest::blocking::Client::builder().timeout(Duration::from_secs(10)).user_agent("lootline").build()?;
    let resp = client.get(url).send()?.error_for_status()?;
    if let Some(len) = resp.content_length() {
        anyhow::ensure!(len <= MAX_DOWNLOAD, "response too large ({len} bytes)");
    }
    let mut out = Vec::new();
    resp.take(MAX_DOWNLOAD + 1).read_to_end(&mut out)?;
    anyhow::ensure!(out.len() as u64 <= MAX_DOWNLOAD, "response too large");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn season_is_sanitised_for_url_and_path() {
        assert_eq!(safe_season("../../etc"), "etc");
        assert_eq!(safe_season("c6s1"), "c6s1");
        assert_eq!(safe_season("???"), "placeholder");
    }

    #[test]
    fn fetch_failure_falls_back_cleanly() {
        let dir = std::env::temp_dir().join(format!("lootline-fetch-{}", uuid::Uuid::new_v4()));
        // unroutable / invalid URLs fail without panicking and without touching the cache
        assert!(fetch_and_cache("not a url", "x", &dir).is_err());
        assert!(fetch_and_cache("http://127.0.0.1:9", "x", &dir).is_err());
        assert!(!dir.exists());
        assert_eq!(load_cached_or_bundled(&dir, "x"), LootTable::bundled());
    }

    #[test]
    fn bundled_table_is_flagged_placeholder() {
        assert!(LootTable::bundled().placeholder);
    }

    #[test]
    fn bundled_table_parses() {
        assert!(!LootTable::bundled().items.is_empty());
    }

    #[test]
    fn parses_optional_fields() {
        let t = LootTable::parse(
            r#"{"season":"s1","items":[{"name":"A","rarity":"Rare","p":0.5},
                {"name":"B","rarity":"Mythic","p":0.1,"icon":"icons/b.png"}]}"#,
        )
        .unwrap();
        assert!(!t.placeholder);
        assert_eq!(t.items[1].icon.as_deref(), Some("icons/b.png"));
        assert_eq!(t.items[0].icon, None);
        assert!((t.rarity_p(Rarity::Rare) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn bundled_has_treasure_rates() {
        let t = LootTable::bundled();
        let tok = t.items.iter().find(|i| i.name.contains("Pirate Token")).unwrap();
        assert!((tok.p - 1.0 / 137_500.0).abs() < 1e-12);
        assert_eq!(t.items.len(), 7);
    }

    #[test]
    fn rejects_bad_probability_and_empty() {
        assert!(LootTable::parse(r#"{"season":"s","items":[{"name":"A","rarity":"Rare","p":1.5}]}"#).is_err());
        assert!(LootTable::parse(r#"{"season":"s","items":[]}"#).is_err());
        assert!(LootTable::parse("not json").is_err());
        assert!(LootTable::parse(r#"{"season":"s","items":[{"name":"A","rarity":"Bogus","p":0.1}]}"#).is_err());
    }

    #[test]
    fn rarity_p_sums_items_of_that_rarity() {
        let t = LootTable::parse(
            r#"{"season":"s","items":[{"name":"A","rarity":"Epic","p":0.1},
                {"name":"B","rarity":"Epic","p":0.05},{"name":"C","rarity":"Common","p":0.85}]}"#,
        )
        .unwrap();
        assert!((t.rarity_p(Rarity::Epic) - 0.15).abs() < 1e-12);
        assert_eq!(t.rarity_p(Rarity::Mythic), 0.0);
    }

    #[test]
    fn cache_roundtrip_and_fallback() {
        let dir = std::env::temp_dir().join(format!("lootline-test-{}", uuid::Uuid::new_v4()));
        // nothing cached -> bundled
        assert_eq!(load_cached_or_bundled(&dir, "x"), LootTable::bundled());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(cache_path(&dir, "x"), r#"{"season":"x","items":[{"name":"Z","rarity":"Rare","p":1.0}]}"#)
            .unwrap();
        assert_eq!(load_cached_or_bundled(&dir, "x").items[0].name, "Z");
        // corrupt cache -> bundled
        std::fs::write(cache_path(&dir, "x"), "garbage").unwrap();
        assert_eq!(load_cached_or_bundled(&dir, "x"), LootTable::bundled());
        let _ = std::fs::remove_dir_all(dir);
    }
}
