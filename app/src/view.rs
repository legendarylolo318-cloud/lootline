//! Turns app state into Slint models and pushes themes to the windows.

use crate::app::App;
use crate::util::*;
use crate::{
    Backend, BarDatum, ColorField, FeedRow, HistRow, ItemRow, OddsRow as UiOddsRow, PlayerRow, StatRow, ThemeChip,
};
use lootline_core::event::{Chest, PlayerInfo};
use lootline_core::odds::formula;
use lootline_core::stats::{self, OddsRow, Scope, ScopeCtx, Who};
use lootline_core::theme::Theme;
use lootline_core::Rarity;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use std::collections::{BTreeMap, HashMap, HashSet};

macro_rules! model {
    ($v:expr) => {
        ModelRc::new(VecModel::from($v))
    };
}

/// Push every colour/radius/font of `$t` into the `Theme` global of window `$w`.
macro_rules! apply_theme {
    ($w:expr, $t:expr, $r:expr) => {{
        let g = $w.global::<crate::Theme>();
        let c = &$t.colors;
        g.set_bg(color(&c.bg));
        g.set_surface(color(&c.surface));
        g.set_surface_alt(color(&c.surface_alt));
        g.set_border(color(&c.border));
        g.set_text(color(&c.text));
        g.set_text_dim(color(&c.text_dim));
        g.set_accent(color(&c.accent));
        g.set_accent_alt(color(&c.accent_alt));
        g.set_on_accent(color(&c.on_accent));
        g.set_good(color(&c.good));
        g.set_bad(color(&c.bad));
        g.set_warn(color(&c.warn));
        g.set_r_common(color(&$r.common));
        g.set_r_uncommon(color(&$r.uncommon));
        g.set_r_rare(color(&$r.rare));
        g.set_r_epic(color(&$r.epic));
        g.set_r_legendary(color(&$r.legendary));
        g.set_r_mythic(color(&$r.mythic));
        g.set_r_mythic_alt(color(&$r.mythic_alt));
        g.set_radius($t.shape.radius);
        g.set_radius_small($t.shape.radius_small);
        g.set_font($t.shape.font.clone().into());
        g.set_glow($t.shape.glow);
    }};
}

/// Player identity colours offered in Settings (what friends see next to your name). These are
/// per-player data shared over the network, deliberately independent of anyone's theme.
const PALETTE: [&str; 10] =
    ["#4f8cff", "#ff5470", "#3fd35b", "#ffb62e", "#b45cff", "#00d1ff", "#ff7a2f", "#ff4fa3", "#9aa3ad", "#e8e8e8"];

fn ridx(r: Option<Rarity>) -> i32 {
    r.map(|r| r.index() as i32).unwrap_or(-1)
}

fn luck_label(v: f64, tagged: usize) -> &'static str {
    if tagged == 0 {
        return "No data yet";
    }
    match v {
        v if v < 0.1 => "Cursed",
        v if v < 0.3 => "Unlucky",
        v if v < 0.7 => "Average",
        v if v < 0.9 => "Lucky",
        _ => "Blessed",
    }
}

fn odds_ui(r: &OddsRow) -> UiOddsRow {
    let s = &r.stat;
    UiOddsRow {
        name: r.name.clone().into(),
        rarity: r.rarity.index() as i32,
        is_group: r.is_rarity_row,
        p: fmt_pct(s.p).into(),
        expected: fmt_expected(s.expected).into(),
        got: s.got as i32,
        p1: fmt_pct(s.p_at_least_one).into(),
        luck: s.luck as f32,
        luck_text: fmt_pct(s.luck).into(),
        dry: fmt_pct(s.dry).into(),
        tip_p: format!(
            "p = per-chest probability from the loot table = {} (1 in {})",
            lootline_core::odds::fmt::num(s.p),
            if s.p > 0.0 { lootline_core::odds::fmt::num((1.0 / s.p).round()) } else { "∞".into() }
        )
        .into(),
        tip_expected: formula::expected(s.n, s.p, s.expected).into(),
        tip_p1: formula::at_least_one(s.n, s.p, s.p_at_least_one).into(),
        tip_luck: formula::luck(s.n, s.p, s.got, s.luck).into(),
        tip_dry: formula::dry(s.n, s.p, s.dry).into(),
    }
}

pub fn sort_odds(rows: &mut [OddsRow], col: i32, asc: bool) {
    use std::cmp::Ordering;
    rows.sort_by(|a, b| {
        let f = |x: f64, y: f64| x.partial_cmp(&y).unwrap_or(Ordering::Equal);
        let ord = match col {
            0 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            1 => a.rarity.cmp(&b.rarity),
            2 => f(a.stat.p, b.stat.p),
            3 => f(a.stat.expected, b.stat.expected),
            4 => a.stat.got.cmp(&b.stat.got),
            5 => f(a.stat.p_at_least_one, b.stat.p_at_least_one),
            6 => f(a.stat.luck, b.stat.luck),
            _ => f(a.stat.dry, b.stat.dry),
        };
        let ord = if asc { ord } else { ord.reverse() };
        // keep rarity groups above their items, then the table order
        ord.then(b.is_rarity_row.cmp(&a.is_rarity_row)).then(a.name.cmp(&b.name))
    });
}

fn bars(chests: &[&Chest]) -> Vec<BarDatum> {
    let (d, untagged) = stats::rarity_distribution(chests);
    let max = d.iter().copied().chain(std::iter::once(untagged)).max().unwrap_or(0).max(1) as f32;
    let mut v: Vec<BarDatum> = Rarity::ALL
        .iter()
        .map(|r| BarDatum {
            label: r.name().into(),
            value: d[r.index()] as i32,
            rarity: r.index() as i32,
            frac: d[r.index()] as f32 / max,
        })
        .collect();
    v.push(BarDatum { label: "Untagged".into(), value: untagged as i32, rarity: -1, frac: untagged as f32 / max });
    v
}

impl App {
    /// A player's chosen colour (it is identity data, not theme data), falling back to the
    /// theme's dim text colour for unknown players or invalid colours received from peers.
    fn player_color(&self, p: Option<&PlayerInfo>) -> slint::Color {
        match p.map(|p| p.color.as_str()).filter(|c| lootline_core::theme::parse_color(c).is_some()) {
            Some(c) => color(c),
            None => color(&self.theme.colors.text_dim),
        }
    }

    fn party_ids(&self) -> HashSet<String> {
        let mut s: HashSet<String> = HashSet::new();
        s.insert(self.me.clone());
        if let Some(p) = &self.party {
            s.extend(p.members.iter().map(|m| m.id.clone()));
        }
        s
    }

    fn stat_rows(
        &self,
        chests: &[&Chest],
        info: &HashMap<String, PlayerInfo>,
        lifetime: &HashMap<String, usize>,
    ) -> Vec<StatRow> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for c in chests {
            *counts.entry(c.player.as_str()).or_default() += 1;
        }
        let max = counts.values().copied().max().unwrap_or(1).max(1) as f32;
        let mut v: Vec<StatRow> = counts
            .iter()
            .map(|(id, n)| StatRow {
                name: info.get(*id).map(|p| p.name.clone()).unwrap_or_else(|| short_id(id)).into(),
                color: self.player_color(info.get(*id)),
                count: *n as i32,
                lifetime: lifetime.get(*id).copied().unwrap_or(0) as i32,
                frac: *n as f32 / max,
            })
            .collect();
        v.sort_by_key(|r| std::cmp::Reverse(r.count));
        v
    }

    /// While the window is hidden (in the tray) only the tray tooltip is updated: rebuilding
    /// every model would cost CPU for nothing. `show_main` refreshes fully when it reappears.
    #[cfg(windows)]
    fn refresh_tray_only(&self) {
        let Some(t) = &self.tray else { return };
        let Ok(chests) = self.log.lock().map(|l| l.chests()) else { return };
        let ctx = ScopeCtx { me: &self.me, match_start: self.match_start, session_start: self.session_start };
        let party_ids = self.party_ids();
        let scoped = stats::select(&chests, self.scope, Who::Party, &ctx);
        let party = scoped.iter().filter(|c| party_ids.contains(&c.player)).count();
        let mine = scoped.iter().filter(|c| c.player == self.me).count();
        t.set_count(mine, party);
    }

    pub fn refresh(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        #[cfg(windows)]
        if !ui.window().is_visible() {
            self.refresh_tray_only();
            return;
        }
        let b = ui.global::<Backend>();
        let (chests, players) = match self.log.lock() {
            Ok(l) => (l.chests(), l.players()),
            Err(_) => return,
        };
        let info: HashMap<String, PlayerInfo> = players.iter().cloned().map(|p| (p.id.clone(), p)).collect();
        let now = now_ms();
        let ctx = ScopeCtx { me: &self.me, match_start: self.match_start, session_start: self.session_start };
        let party_ids = self.party_ids();
        let scoped_all = stats::select(&chests, self.scope, Who::Party, &ctx);
        let scoped_party: Vec<&Chest> = scoped_all.iter().copied().filter(|c| party_ids.contains(&c.player)).collect();
        let scoped_me: Vec<&Chest> = scoped_all.iter().copied().filter(|c| c.player == self.me).collect();
        let selected: &Vec<&Chest> = if self.who == Who::Me { &scoped_me } else { &scoped_party };

        // ---- dashboard numbers
        b.set_my_chests(scoped_me.len() as i32);
        b.set_party_chests(scoped_party.len() as i32);
        b.set_scope_chests(selected.len() as i32);
        let luck = stats::overall_luck(&self.table, selected);
        let tagged = selected.len();
        b.set_luck(luck as f32);
        b.set_luck_tagged(tagged > 0);
        b.set_luck_label(luck_label(luck, tagged).into());
        b.set_luck_sub(
            if tagged == 0 {
                "open some chests to see your luck".into()
            } else {
                format!("{tagged} chests · {} percentile", ordinal((luck * 100.0).round() as u32))
            }
            .into(),
        );
        b.set_glow_rarity(self.glow);
        b.set_scope(match self.scope {
            Scope::Match => 0,
            Scope::Session => 1,
            Scope::Lifetime => 2,
        });
        b.set_who(if self.who == Who::Me { 0 } else { 1 });
        b.set_hotkey_hint(
            format!("{} add chest  ·  {} undo", self.settings.hotkey_add, self.settings.hotkey_undo).into(),
        );
        let (status, ok) = if !Self::detection_available() {
            ("Viewer mode (no detection)".to_string(), false)
        } else if !self.settings.detection_enabled {
            ("Detection off".to_string(), false)
        } else {
            (self.detect_status.clone(), self.detect_ok)
        };
        b.set_detect_status(status.into());
        b.set_detect_ok(ok);
        b.set_detection_available(Self::detection_available());
        b.set_placeholder_table(self.table.placeholder);
        b.set_table_note(
            if self.table.placeholder {
                "PLACEHOLDER loot table: the odds below use made-up values. Fetch a real one in Settings."
            } else {
                ""
            }
            .into(),
        );

        // ---- feed
        let session_party: Vec<&Chest> =
            chests.iter().filter(|c| c.ts >= self.session_start && party_ids.contains(&c.player)).collect();
        let feed: Vec<FeedRow> = session_party
            .iter()
            .rev()
            .take(24)
            .map(|c| FeedRow {
                who: info.get(&c.player).map(|p| p.name.clone()).unwrap_or_else(|| short_id(&c.player)).into(),
                who_color: self.player_color(info.get(&c.player)),
                rarity: ridx(c.rarity),
                item: c.item.clone().unwrap_or_default().into(),
                time: fmt_ago(now, c.ts).into(),
            })
            .collect();
        b.set_feed(model!(feed));

        // ---- players / roster
        let mut member_list: Vec<(String, bool, bool)> = Vec::new(); // id, connected, host
        match &self.party {
            Some(p) => {
                for m in &p.members {
                    member_list.push((m.id.clone(), m.connected, m.id == p.host));
                }
                if !member_list.iter().any(|m| m.0 == self.me) {
                    member_list.push((self.me.clone(), true, false));
                }
            }
            None => member_list.push((self.me.clone(), true, false)),
        }
        let mut counts: HashMap<&str, i32> = HashMap::new();
        for c in &scoped_all {
            *counts.entry(c.player.as_str()).or_default() += 1;
        }
        let rows: Vec<PlayerRow> = member_list
            .iter()
            .map(|(id, connected, host)| {
                let pi = info.get(id);
                let name = pi.map(|p| p.name.clone()).unwrap_or_else(|| short_id(id));
                PlayerRow {
                    id: id.clone().into(),
                    initial: name
                        .chars()
                        .next()
                        .map(|c| c.to_uppercase().to_string())
                        .unwrap_or_else(|| "?".into())
                        .into(),
                    name: name.into(),
                    color: self.player_color(pi),
                    chests: counts.get(id.as_str()).copied().unwrap_or(0),
                    connected: *connected,
                    detecting: pi.is_some_and(|p| p.detecting),
                    is_me: *id == self.me,
                    is_host: *host,
                }
            })
            .collect();
        b.set_players(model!(rows.clone()));
        b.set_roster(model!(rows));

        // ---- party page
        let (in_party, link, code, i_am_host) = match &self.party {
            Some(p) => (
                true,
                match p.link {
                    lootline_net::LinkState::Online => "Connected".to_string(),
                    lootline_net::LinkState::Connecting => "Connecting…".to_string(),
                    lootline_net::LinkState::Reconnecting => "Reconnecting…".to_string(),
                    lootline_net::LinkState::Idle => "Not in a party".to_string(),
                },
                p.code.clone(),
                p.i_am_host,
            ),
            None => (false, "Not in a party".to_string(), String::new(), false),
        };
        b.set_in_party(in_party);
        b.set_link_text(link.into());
        b.set_party_code(code.into());
        b.set_my_code(lootline_net::make_code(&self.me).into());
        b.set_i_am_host(i_am_host);
        b.set_net_notice(self.net_notice.clone().into());

        // ---- odds
        let mut orows = stats::odds_rows(&self.table, selected);
        sort_odds(&mut orows, self.sort.0, self.sort.1);
        b.set_odds(model!(orows.iter().map(odds_ui).collect::<Vec<_>>()));
        b.set_sort_col(self.sort.0);
        b.set_sort_asc(self.sort.1);

        // ---- history
        self.refresh_history(&b, &chests, &info);

        // ---- settings status strings
        b.set_recording(self.recording);
        b.set_template_status(self.template_status.clone().into());
        b.set_detect_debug(self.detect_debug_text().into());
        b.set_loot_status(self.loot_status.clone().into());
        b.set_settings_msg(self.settings_msg.clone().into());
        b.set_theme_msg(self.theme_msg.clone().into());
        b.set_templates_info(
            if self.templates.is_empty() {
                "No templates installed: detection needs at least one (fetch or record).".to_string()
            } else {
                format!(
                    "{} template(s) active: {}",
                    self.templates.len(),
                    self.templates.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
                )
            }
            .into(),
        );

        #[cfg(windows)]
        if let Some(t) = &self.tray {
            t.set_count(scoped_me.len(), scoped_party.len());
        }
    }

    fn refresh_history(&mut self, b: &Backend<'_>, chests: &[Chest], info: &HashMap<String, PlayerInfo>) {
        let now = now_ms();
        let sessions = self.db.sessions().unwrap_or_default();
        let matches = self.db.matches().unwrap_or_default();
        if self.hist_sel.is_empty() {
            self.hist_sel = format!("s:{}", self.session_id);
        }
        let in_win =
            |from: i64, to: i64| -> Vec<&Chest> { chests.iter().filter(|c| c.ts >= from && c.ts < to).collect() };
        let mut rows: Vec<HistRow> = Vec::new();
        let mut sel_window: Option<(String, i64, i64)> = None;
        for s in sessions.iter().take(40) {
            let end = s.ended.unwrap_or(now + 1);
            let w = in_win(s.started, end);
            let players = w.iter().map(|c| c.player.as_str()).collect::<HashSet<_>>().len();
            let id = format!("s:{}", s.id);
            let title =
                format!("Session · {}{}", fmt_date_time(s.started), if s.ended.is_none() { " (now)" } else { "" });
            if id == self.hist_sel {
                sel_window = Some((title.clone(), s.started, end));
            }
            rows.push(HistRow {
                is_header: true,
                title: title.into(),
                sub: format!("{} chests · {} player{}", w.len(), players, if players == 1 { "" } else { "s" }).into(),
                count: w.len() as i32,
                id: id.clone().into(),
                selected: id == self.hist_sel,
            });
            let mut ms: Vec<_> = matches.iter().filter(|m| m.session_id == s.id).collect();
            ms.sort_by_key(|m| m.started);
            for (i, m) in ms.iter().enumerate() {
                let m_end = ms.get(i + 1).map(|n| n.started).unwrap_or(end);
                let mw = in_win(m.started, m_end);
                let id = format!("m:{}", m.id);
                let title = format!("Match · {}", fmt_clock(m.started));
                if id == self.hist_sel {
                    sel_window = Some((title.clone(), m.started, m_end));
                }
                rows.push(HistRow {
                    is_header: false,
                    title: title.into(),
                    sub: format!("{} chests", mw.len()).into(),
                    count: mw.len() as i32,
                    id: id.clone().into(),
                    selected: id == self.hist_sel,
                });
            }
        }
        // the selection may point at something that no longer exists
        let (title, from, to) = sel_window.unwrap_or_else(|| ("Current session".into(), self.session_start, now + 1));
        let win = in_win(from, to);
        let mut lifetime: HashMap<String, usize> = HashMap::new();
        for c in chests {
            *lifetime.entry(c.player.clone()).or_default() += 1;
        }
        b.set_history(model!(rows));
        b.set_hist_title(title.into());
        b.set_hist_players(model!(self.stat_rows(&win, info, &lifetime)));
        b.set_hist_bars(model!(bars(&win)));
        let all: Vec<&Chest> = chests.iter().collect();
        b.set_lifetime_players(model!(self.stat_rows(&all, info, &lifetime)));
        b.set_lifetime_bars(model!(bars(&all)));
        b.set_lifetime_total(all.len() as i32);
    }

    // ------------------------------------------------------------ settings + theme push
    pub fn push_settings_fields(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let b = ui.global::<Backend>();
        let s = &self.settings;
        b.set_player_name(s.player_name.clone().into());
        b.set_player_color(s.player_color.clone().into());
        b.set_interact_key(s.interact_key.clone().into());
        b.set_hotkey_add(s.hotkey_add.clone().into());
        b.set_hotkey_undo(s.hotkey_undo.clone().into());
        b.set_sensitivity(s.sensitivity);
        b.set_detection_enabled(s.detection_enabled);
        b.set_overlay_enabled(s.overlay.enabled);
        b.set_overlay_x(s.overlay.x.to_string().into());
        b.set_overlay_y(s.overlay.y.to_string().into());
        b.set_overlay_w(s.overlay.width.to_string().into());
        b.set_overlay_h(s.overlay.height.to_string().into());
        b.set_overlay_timeout(format!("{}", s.overlay.timeout_secs).into());
        b.set_minimize_to_tray(s.minimize_to_tray);
        b.set_start_minimized(s.start_minimized);
        b.set_season(s.season.clone().into());
        b.set_repo_url(s.repo_base_url.clone().into());
        b.set_palette(model!(PALETTE
            .iter()
            .map(|h| ColorField { key: (*h).into(), label: "".into(), hex: (*h).into(), value: color(h) })
            .collect::<Vec<_>>()));
    }

    pub fn apply_theme_only(&mut self) {
        let r = self.effective_rarity();
        if let Some(ui) = self.ui.upgrade() {
            apply_theme!(ui, self.theme, r);
        }
        apply_theme!(self.overlay, self.theme, r);
        self.push_theme_lists();
    }

    pub fn refresh_theme(&mut self) {
        self.apply_theme_only();
        self.refresh();
    }

    fn push_theme_lists(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let b = ui.global::<Backend>();
        let t: &Theme = &self.theme;
        let field = |key: &str, label: &str, hex: &str| ColorField {
            key: key.into(),
            label: label.into(),
            hex: hex.into(),
            value: color(hex),
        };
        let c = &t.colors;
        b.set_color_fields(model!(vec![
            field("bg", "Background", &c.bg),
            field("surface", "Surface", &c.surface),
            field("surface_alt", "Surface alt", &c.surface_alt),
            field("border", "Border", &c.border),
            field("text", "Text", &c.text),
            field("text_dim", "Text dim", &c.text_dim),
            field("accent", "Accent", &c.accent),
            field("accent_alt", "Accent alt", &c.accent_alt),
            field("on_accent", "On accent", &c.on_accent),
            field("good", "Good", &c.good),
            field("bad", "Bad", &c.bad),
            field("warn", "Warning", &c.warn),
        ]));
        let r = self.effective_rarity();
        b.set_rarity_fields(model!(vec![
            field("common", "Common", &r.common),
            field("uncommon", "Uncommon", &r.uncommon),
            field("rare", "Rare", &r.rare),
            field("epic", "Epic", &r.epic),
            field("legendary", "Legendary", &r.legendary),
            field("mythic", "Mythic", &r.mythic),
            field("mythic_alt", "Mythic (2nd)", &r.mythic_alt),
        ]));
        b.set_themes(model!(self
            .themes
            .iter()
            .map(|th| ThemeChip {
                name: th.name.clone().into(),
                bg: color(&th.colors.bg),
                surface: color(&th.colors.surface),
                accent: color(&th.colors.accent),
                active: th.name == self.settings.theme,
            })
            .collect::<Vec<_>>()));
    }

    pub fn overlay_items(&self) -> Vec<ItemRow> {
        let mut items: Vec<_> = self.table.items.iter().collect();
        items.sort_by(|a, b| b.rarity.cmp(&a.rarity).then(a.name.cmp(&b.name)));
        items
            .into_iter()
            .map(|i| ItemRow { name: SharedString::from(i.name.as_str()), rarity: i.rarity.index() as i32 })
            .collect()
    }
}
