use directories::ProjectDirs;
use lootline_core::theme::parse_color;
use std::path::PathBuf;
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn color(hex: &str) -> slint::Color {
    let [r, g, b, a] = parse_color(hex).unwrap_or([255, 0, 255, 255]);
    slint::Color::from_argb_u8(a, r, g, b)
}

pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
}

impl Paths {
    pub fn discover() -> Paths {
        let (config, data) = match ProjectDirs::from("dev", "lootline", "Lootline") {
            Some(d) => (d.config_dir().to_path_buf(), d.data_dir().to_path_buf()),
            None => (PathBuf::from("lootline-config"), PathBuf::from("lootline-data")),
        };
        let _ = std::fs::create_dir_all(&config);
        let _ = std::fs::create_dir_all(&data);
        Paths { config, data }
    }
    pub fn settings(&self) -> PathBuf {
        self.config.join("settings.toml")
    }
    pub fn themes(&self) -> PathBuf {
        self.config.join("themes")
    }
    pub fn db(&self) -> PathBuf {
        self.data.join("lootline.db")
    }
    pub fn identity(&self) -> PathBuf {
        self.data.join("identity.key")
    }
    pub fn templates(&self) -> PathBuf {
        self.data.join("templates")
    }
    pub fn loot_cache(&self) -> PathBuf {
        self.data.join("loot_tables")
    }
}

/// A stable 32-byte secret key (the player's identity), created on first run.
pub fn load_or_create_identity(path: &std::path::Path) -> [u8; 32] {
    if let Some(k) = std::fs::read_to_string(path).ok().and_then(|s| parse_hex32(s.trim())) {
        return k;
    }
    let mut k = [0u8; 32];
    k[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    k[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    let hex: String = k.iter().map(|b| format!("{b:02x}")).collect();
    let _ = std::fs::write(path, hex);
    k
}

/// Exactly 64 hex digits -> 32 bytes. Works on bytes, so a corrupt (non-ASCII) file can never
/// panic on a char boundary.
fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let nib = |c: u8| (c as char).to_digit(16).map(|v| v as u8);
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = nib(b[2 * i])? << 4 | nib(b[2 * i + 1])?;
    }
    Some(out)
}

fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

fn local(ts_ms: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(ts_ms / 1000).unwrap_or(OffsetDateTime::UNIX_EPOCH).to_offset(local_offset())
}

/// "14:05"
pub fn fmt_clock(ts_ms: i64) -> String {
    local(ts_ms).format(format_description!("[hour]:[minute]")).unwrap_or_default()
}

/// "14:05:09"
pub fn fmt_clock_secs(ts_ms: i64) -> String {
    local(ts_ms).format(format_description!("[hour]:[minute]:[second]")).unwrap_or_default()
}

/// "Nov 10, 14:05"
pub fn fmt_date_time(ts_ms: i64) -> String {
    local(ts_ms)
        .format(format_description!("[month repr:short] [day padding:none], [hour]:[minute]"))
        .unwrap_or_default()
}

pub fn fmt_ago(now: i64, ts: i64) -> String {
    let s = ((now - ts) / 1000).max(0);
    match s {
        0..=4 => "now".into(),
        5..=59 => format!("{s}s ago"),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn fmt_pct(v: f64) -> String {
    if !v.is_finite() {
        return "–".into();
    }
    let p = v * 100.0;
    if v > 0.0 && v < 0.0001 {
        // too small for a percentage to be readable: show "1 in N"
        return format!("1 in {}", thousands((1.0 / v).round() as u64));
    }
    if v < 1.0 && p >= 99.5 {
        // never round "almost certain" up to a flat 100%
        return if p >= 99.95 { ">99.9%".into() } else { format!("{p:.1}%") };
    }
    if p >= 10.0 {
        format!("{p:.0}%")
    } else if p >= 1.0 {
        format!("{p:.1}%")
    } else {
        format!("{p:.2}%")
    }
}

/// Expected hit counts for the odds table: two decimals normally, more for the rare items
/// (the old "{:.2}" showed the weekly treasures as a flat 0.00).
pub fn fmt_expected(v: f64) -> String {
    if !v.is_finite() {
        return "–".into();
    }
    if v == 0.0 || v >= 0.01 {
        format!("{v:.2}")
    } else if v >= 0.0001 {
        format!("{v:.4}").trim_end_matches('0').to_string()
    } else {
        "<0.0001".into()
    }
}

/// 1 -> "1st", 2 -> "2nd", 11 -> "11th", 23 -> "23rd"
pub fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

pub fn short_id(id: &str) -> String {
    // ids come from peers: never slice by bytes
    format!("player-{}", id.chars().take(5).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_formatting() {
        assert_eq!(fmt_ago(10_000, 9_000), "now");
        assert_eq!(fmt_ago(100_000, 70_000), "30s ago");
        assert_eq!(fmt_ago(1_000_000, 700_000), "5m ago");
        assert_eq!(fmt_ago(100_000_000, 90_000_000), "2h ago");
    }

    #[test]
    fn pct_formatting() {
        assert_eq!(fmt_pct(0.5), "50%");
        assert_eq!(fmt_pct(0.052), "5.2%");
        assert_eq!(fmt_pct(0.005), "0.50%");
        assert_eq!(fmt_pct(1.0 / 137_500.0), "1 in 137,500");
    }

    #[test]
    fn pct_never_rounds_to_certainty() {
        assert_eq!(fmt_pct(0.99996), ">99.9%");
        assert_eq!(fmt_pct(0.99920), "99.9%"); // was "100%"
        assert_eq!(fmt_pct(0.994), "99%");
        assert_eq!(fmt_pct(1.0), "100%");
        assert_eq!(fmt_pct(f64::NAN), "–");
    }

    #[test]
    fn expected_formatting() {
        assert_eq!(fmt_expected(1.6), "1.60");
        assert_eq!(fmt_expected(0.0), "0.00");
        assert_eq!(fmt_expected(8.0e-4), "0.0008");
        assert_eq!(fmt_expected(1.4e-3), "0.0014");
        assert_eq!(fmt_expected(7.04e-5), "<0.0001");
    }

    #[test]
    fn ordinals() {
        let v: Vec<String> = [1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 100].iter().map(|&n| ordinal(n)).collect();
        assert_eq!(v, ["1st", "2nd", "3rd", "4th", "11th", "12th", "13th", "21st", "22nd", "23rd", "100th"]);
    }

    #[test]
    fn short_id_handles_non_ascii_peer_ids() {
        assert_eq!(short_id("ééééééé"), "player-ééééé");
        assert_eq!(short_id("ab"), "player-ab");
    }

    #[test]
    fn corrupt_identity_file_is_replaced_not_panicked_on() {
        let p = std::env::temp_dir().join(format!("ll-id-bad-{}", uuid::Uuid::new_v4()));
        std::fs::write(&p, "é".repeat(40)).unwrap();
        let k = load_or_create_identity(&p);
        assert_eq!(load_or_create_identity(&p), k);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn identity_is_stable() {
        let p = std::env::temp_dir().join(format!("ll-id-{}", uuid::Uuid::new_v4()));
        let a = load_or_create_identity(&p);
        let b = load_or_create_identity(&p);
        assert_eq!(a, b);
        assert_ne!(a, [0u8; 32]);
        let _ = std::fs::remove_file(p);
    }
}
