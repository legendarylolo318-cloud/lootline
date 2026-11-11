//! Application state and actions. Lives in a thread-local on the UI thread; every other thread
//! (detector, network, HTTP fetches, hotkeys, tray) reaches it through `invoke_from_event_loop`.

use crate::util::*;
use crate::{Backend, MainWindow, OverlayWindow};
use lootline_core::dsp::TemplateFile;
use lootline_core::loot::{self, LootTable};
use lootline_core::settings::Settings;
use lootline_core::stats::{Scope, Who};
use lootline_core::storage::Storage;
use lootline_core::theme::{self, RarityColors, Theme};
use lootline_core::{Event, EventLog, Rarity};
use lootline_net::{NetCmd, NetEvent, NetHandle, PartyState};
use slint::{ComponentHandle, Timer, TimerMode};
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use uuid::Uuid;

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Run `f` on the app state. Returns None if the state is not installed yet or is already
/// borrowed (re-entrant call), which callers treat as "drop this update".
pub fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|a| a.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

pub fn install(app: App) {
    APP.with(|a| *a.borrow_mut() = Some(app));
}

/// Run `f` on the UI thread with access to the app (for use from other threads).
pub fn on_ui(f: impl FnOnce(&mut App) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || {
        with_app(f);
    });
}

pub const IDLE_MATCH_GAP_MS: i64 = 25 * 60 * 1000;

pub struct App {
    pub ui: slint::Weak<MainWindow>,
    pub overlay: OverlayWindow,
    pub paths: Paths,
    pub settings: Settings,
    pub log: Arc<Mutex<EventLog>>,
    pub db: Storage,
    pub table: LootTable,
    pub themes: Vec<Theme>,
    /// Working copy of the active theme (what the editor changes). Rarity overrides are applied on top.
    pub theme: Theme,
    pub me: String,
    pub session_id: i64,
    pub session_start: i64,
    pub match_start: i64,
    pub scope: Scope,
    pub who: Who,
    pub sort: (i32, bool),
    pub hist_sel: String,
    pub net: NetHandle,
    pub party: Option<PartyState>,
    pub net_notice: String,
    pub detect_status: String,
    pub detect_ok: bool,
    #[cfg(windows)]
    pub detector: Option<lootline_detect::DetectorHandle>,
    pub templates: Vec<TemplateFile>,
    pub overlay_chest: Option<Uuid>,
    pub overlay_timer: Timer,
    pub toast_timer: Timer,
    pub glow_timer: Timer,
    pub leave_timer: Timer,
    pub hotkeys: crate::glue::Hotkeys,
    #[cfg(windows)]
    pub tray: Option<crate::glue::TrayState>,
    pub glow: i32,
    pub recording: bool,
    pub template_status: String,
    pub loot_status: String,
    pub settings_msg: String,
    pub theme_msg: String,
    pub quitting: bool,
}

impl App {
    pub fn detection_available() -> bool {
        cfg!(windows)
    }

    pub fn detection_active(&self) -> bool {
        Self::detection_available() && self.settings.detection_enabled
    }

    pub fn load_templates(dir: &std::path::Path) -> Vec<TemplateFile> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.path().extension().and_then(|x| x.to_str()) == Some("json") {
                    if let Some(t) = std::fs::read_to_string(e.path()).ok().and_then(|s| TemplateFile::parse(&s).ok()) {
                        out.push(t);
                    }
                }
            }
        }
        out
    }

    pub fn effective_rarity(&self) -> RarityColors {
        self.theme.rarity.clone().with_overrides(&self.settings.rarity_overrides)
    }

    // ------------------------------------------------------------ toasts
    pub fn toast(&mut self, msg: impl Into<String>) {
        let msg: String = msg.into();
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<Backend>().set_toast(msg.into());
        }
        self.toast_timer.start(TimerMode::SingleShot, Duration::from_millis(3200), || {
            with_app(|a| {
                if let Some(ui) = a.ui.upgrade() {
                    ui.global::<Backend>().set_toast("".into());
                }
            });
        });
    }

    // ------------------------------------------------------------ event plumbing
    /// Persist + share a locally created event.
    pub fn commit(&mut self, e: Event) {
        if let Ok(mut l) = self.log.lock() {
            l.insert(e.clone());
        }
        let _ = self.db.insert_event(&e);
        self.net.send(NetCmd::Publish(e));
    }

    pub fn add_chest(&mut self) {
        let now = now_ms();
        // A long gap since my last chest means a new match probably started.
        let last_mine =
            self.log.lock().ok().and_then(|l| l.chests().into_iter().rev().find(|c| c.player == self.me).map(|c| c.ts));
        if last_mine.is_some_and(|t| now - t > IDLE_MATCH_GAP_MS && t >= self.match_start) {
            self.start_match();
        }
        let ev = Event::new_chest(&self.me, now, self.current_match_id());
        let id = ev.id();
        self.commit(ev);
        if self.settings.overlay.enabled {
            self.show_overlay(id);
        }
        self.refresh();
    }

    pub fn undo_last(&mut self) {
        let target = self.log.lock().ok().and_then(|l| l.last_chest_of(&self.me));
        let Some(target) = target else {
            self.toast("Nothing to undo");
            return;
        };
        let ev = Event::Undo { id: Uuid::new_v4(), target, player: self.me.clone(), ts: now_ms() };
        self.commit(ev);
        if self.overlay_chest == Some(target) {
            self.hide_overlay();
        }
        self.toast("Last chest undone");
        self.refresh();
    }

    pub fn tag(&mut self, chest: Uuid, rarity: Option<Rarity>, item: Option<String>) {
        let ev = Event::Tag { id: Uuid::new_v4(), target: chest, player: self.me.clone(), ts: now_ms(), rarity, item };
        self.commit(ev);
        if let Some(r) = rarity {
            self.start_glow(r);
        }
        self.refresh();
    }

    pub fn start_glow(&mut self, r: Rarity) {
        if r >= Rarity::Legendary {
            self.glow = r.index() as i32;
            self.glow_timer.start(TimerMode::SingleShot, Duration::from_secs(5), || {
                with_app(|a| {
                    a.glow = -1;
                    a.refresh();
                });
            });
        }
    }

    pub fn current_match_id(&self) -> Uuid {
        // Matches are identified by their start time for the event's `match_id` field.
        Uuid::from_u128(self.match_start as u128)
    }

    pub fn start_match(&mut self) {
        self.match_start = now_ms();
        let _ = self.db.begin_match(self.current_match_id(), self.session_id, self.match_start);
    }

    pub fn ensure_join(&mut self) {
        let detecting = self.detection_active();
        let cur = self.log.lock().ok().and_then(|l| l.players().into_iter().find(|p| p.id == self.me));
        let same = cur.as_ref().is_some_and(|p| {
            p.active
                && p.name == self.settings.player_name
                && p.color == self.settings.player_color
                && p.detecting == detecting
        });
        if same {
            return;
        }
        let ev = Event::Join {
            id: Uuid::new_v4(),
            player: self.me.clone(),
            name: self.settings.player_name.clone(),
            color: self.settings.player_color.clone(),
            detecting,
            ts: now_ms(),
        };
        self.commit(ev);
    }

    // ------------------------------------------------------------ party
    pub fn create_party(&mut self) {
        self.net_notice.clear();
        self.ensure_join();
        self.net.send(NetCmd::Host);
    }

    pub fn join_party(&mut self, code: &str) {
        self.net_notice.clear();
        self.ensure_join();
        self.net.send(NetCmd::Join(code.to_string()));
    }

    pub fn leave_party(&mut self) {
        let ev = Event::Leave { id: Uuid::new_v4(), player: self.me.clone(), ts: now_ms() };
        self.commit(ev);
        // let the Leave event flush before the connection closes
        self.leave_timer.start(TimerMode::SingleShot, Duration::from_millis(350), || {
            with_app(|a| {
                a.net.send(NetCmd::Leave);
                a.ensure_join();
                a.refresh();
            });
        });
        self.refresh();
    }

    pub fn kick(&mut self, player: &str) {
        let ev = Event::Leave { id: Uuid::new_v4(), player: player.to_string(), ts: now_ms() };
        self.commit(ev);
        self.net.send(NetCmd::Kick(player.to_string()));
    }

    pub fn on_net(&mut self, e: NetEvent) {
        match e {
            NetEvent::Identity { .. } => {}
            NetEvent::State(st) => {
                let was_online = self.party.as_ref().is_some_and(|p| p.link == lootline_net::LinkState::Online);
                let now_online = st.link == lootline_net::LinkState::Online;
                self.party = if st.link == lootline_net::LinkState::Idle { None } else { Some(st) };
                if now_online && !was_online {
                    // (re)announce ourselves; the Have exchange on connect delivers the rest
                    self.ensure_join();
                    let join = self.log.lock().ok().and_then(|l| {
                        l.all()
                            .into_iter()
                            .rev()
                            .find(|e| matches!(e, Event::Join { player, .. } if *player == self.me))
                    });
                    if let Some(j) = join {
                        self.net.send(NetCmd::Publish(j));
                    }
                }
            }
            NetEvent::Merged(events) => {
                let _ = self.db.insert_events(&events);
                for e in &events {
                    if let Event::Tag { rarity: Some(r), player, .. } = e {
                        if *player != self.me {
                            self.start_glow(*r);
                        }
                    }
                }
            }
            NetEvent::Kicked => {
                self.net_notice = "You were removed from the party by the host.".into();
                self.party = None;
            }
            NetEvent::Notice(n) => self.net_notice = n,
        }
        self.refresh();
    }

    // ------------------------------------------------------------ detection events
    #[cfg(windows)]
    pub fn on_detect(&mut self, e: lootline_detect::Event) {
        use lootline_detect::{Event as D, Status};
        match e {
            D::Status(Status::WaitingForGame) => {
                self.detect_status = "Waiting for Fortnite".into();
                self.detect_ok = false;
            }
            D::Status(Status::Attached { .. }) => {
                self.detect_status = "Listening to Fortnite".into();
                self.detect_ok = true;
                self.start_match();
            }
            D::Status(Status::Error(e)) => {
                self.detect_status = format!("Audio error: {e}");
                self.detect_ok = false;
            }
            D::ChestDetected { .. } => {
                if self.settings.detection_enabled {
                    self.add_chest();
                }
            }
            D::Ignored { .. } => {}
            D::RecordingDone(res) => {
                self.recording = false;
                match res {
                    Ok(t) => {
                        let dir = self.paths.templates();
                        let _ = std::fs::create_dir_all(&dir);
                        let file = dir.join(format!("{}.json", sanitize(&t.name)));
                        match serde_json::to_string(&t)
                            .map_err(anyhow::Error::from)
                            .and_then(|s| Ok(std::fs::write(&file, s)?))
                        {
                            Ok(()) => {
                                self.template_status = format!(
                                    "Saved '{}' ({} frames, ~{:.1}s)",
                                    t.name,
                                    t.frames.len(),
                                    t.frames.len() as f32 * lootline_core::dsp::FRAME_SECS
                                );
                                self.reload_templates();
                            }
                            Err(e) => self.template_status = format!("Could not save template: {e}"),
                        }
                    }
                    Err(e) => self.template_status = format!("Recording failed: {e}"),
                }
            }
        }
        self.refresh();
    }

    pub fn reload_templates(&mut self) {
        self.templates = Self::load_templates(&self.paths.templates());
        #[cfg(windows)]
        if let Some(d) = &self.detector {
            d.send(lootline_detect::Command::SetTemplates(self.templates.clone()));
        }
    }

    // ------------------------------------------------------------ settings
    pub fn apply_settings_from_ui(&mut self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let b = ui.global::<Backend>();
        let mut msgs: Vec<String> = Vec::new();
        let s = &mut self.settings;
        let name = b.get_player_name().trim().to_string();
        if !name.is_empty() {
            s.player_name = name.chars().take(24).collect();
        }
        let col = b.get_player_color().to_string();
        if theme::parse_color(&col).is_some() {
            s.player_color = col;
        }
        s.sensitivity = b.get_sensitivity().clamp(0.0, 1.0);
        s.detection_enabled = b.get_detection_enabled();
        let key = b.get_interact_key().trim().to_string();
        #[cfg(windows)]
        if lootline_detect::keys::parse_vk(&key).is_some() {
            s.interact_key = key;
        } else {
            msgs.push(format!("'{key}' is not a known key; interact key unchanged"));
        }
        #[cfg(not(windows))]
        let _ = key;
        s.hotkey_add = b.get_hotkey_add().trim().to_string();
        s.hotkey_undo = b.get_hotkey_undo().trim().to_string();
        s.overlay.enabled = b.get_overlay_enabled();
        let num = |t: slint::SharedString, d: i32| t.trim().parse::<i32>().unwrap_or(d);
        s.overlay.x = num(b.get_overlay_x(), s.overlay.x);
        s.overlay.y = num(b.get_overlay_y(), s.overlay.y);
        s.overlay.width = num(b.get_overlay_w(), s.overlay.width as i32).clamp(260, 1200) as u32;
        s.overlay.height = num(b.get_overlay_h(), s.overlay.height as i32).clamp(110, 600) as u32;
        s.overlay.timeout_secs = b.get_overlay_timeout().trim().parse::<f32>().unwrap_or(3.0).clamp(1.0, 30.0);
        s.minimize_to_tray = b.get_minimize_to_tray();
        s.start_minimized = b.get_start_minimized();
        let season = b.get_season().trim().to_string();
        if !season.is_empty() {
            s.season = season;
        }
        s.repo_base_url = b.get_repo_url().trim().to_string();

        #[cfg(windows)]
        if let Some(d) = &self.detector {
            d.send(lootline_detect::Command::SetSensitivity(self.settings.sensitivity));
            d.send(lootline_detect::Command::SetInteractKey(self.settings.interact_key.clone()));
        }
        if let Err(e) = crate::glue::register_hotkeys(self) {
            msgs.push(e);
        }
        if let Err(e) = self.settings.save(&self.paths.settings()) {
            msgs.push(format!("could not save settings: {e}"));
        }
        self.ensure_join();
        self.table = loot::load_cached_or_bundled(&self.paths.loot_cache(), &self.settings.season);
        self.settings_msg = if msgs.is_empty() { "Saved.".into() } else { msgs.join(" · ") };
        self.push_settings_fields();
        self.refresh();
    }

    // ------------------------------------------------------------ themes
    pub fn reload_themes(&mut self) {
        let (themes, errors) = theme::load_dir(&self.paths.themes());
        self.themes = themes;
        if !errors.is_empty() {
            self.theme_msg = format!("Skipped invalid theme file: {}", errors[0]);
        }
    }

    pub fn pick_theme(&mut self, name: &str) {
        if let Some(t) = self.themes.iter().find(|t| t.name == name).cloned() {
            self.theme = t;
            self.settings.theme = name.to_string();
            let _ = self.settings.save(&self.paths.settings());
            self.theme_msg = format!("Theme: {name}");
            self.refresh_theme();
        }
    }

    pub fn edit_color(&mut self, key: &str, hex: &str) {
        if theme::parse_color(hex).is_none() {
            return;
        }
        let c = &mut self.theme.colors;
        let slot = match key {
            "bg" => &mut c.bg,
            "surface" => &mut c.surface,
            "surface_alt" => &mut c.surface_alt,
            "border" => &mut c.border,
            "text" => &mut c.text,
            "text_dim" => &mut c.text_dim,
            "accent" => &mut c.accent,
            "accent_alt" => &mut c.accent_alt,
            "on_accent" => &mut c.on_accent,
            "good" => &mut c.good,
            "bad" => &mut c.bad,
            "warn" => &mut c.warn,
            _ => return,
        };
        *slot = hex.to_string();
        self.apply_theme_only();
    }

    pub fn edit_rarity(&mut self, key: &str, hex: &str) {
        if theme::parse_color(hex).is_none() {
            return;
        }
        self.settings.rarity_overrides.insert(key.to_string(), hex.to_string());
        let _ = self.settings.save(&self.paths.settings());
        self.apply_theme_only();
    }

    pub fn reset_rarity(&mut self) {
        self.settings.rarity_overrides.clear();
        self.theme.rarity = RarityColors::default();
        let _ = self.settings.save(&self.paths.settings());
        self.theme_msg = "Rarity colours reset to defaults".into();
        self.refresh_theme();
    }

    pub fn save_theme(&mut self) {
        let name =
            self.ui.upgrade().map(|u| u.global::<Backend>().get_theme_name().trim().to_string()).unwrap_or_default();
        if name.is_empty() {
            self.theme_msg = "Give the theme a name first".into();
            self.refresh();
            return;
        }
        let mut t = self.theme.clone();
        t.name = name.clone();
        t.rarity = self.effective_rarity();
        if t.author.is_empty() || t.author == "Lootline" {
            t.author = self.settings.player_name.clone();
        }
        let _ = std::fs::create_dir_all(self.paths.themes());
        let file = self.paths.themes().join(format!("{}.toml", sanitize(&name)));
        match std::fs::write(&file, t.to_toml()) {
            Ok(()) => {
                self.settings.rarity_overrides.clear();
                self.settings.theme = name.clone();
                let _ = self.settings.save(&self.paths.settings());
                self.reload_themes();
                self.theme = t;
                self.theme_msg = format!("Saved {}", file.display());
            }
            Err(e) => self.theme_msg = format!("Could not save theme: {e}"),
        }
        self.refresh_theme();
    }

    pub fn export_theme(&mut self) {
        let mut t = self.theme.clone();
        t.rarity = self.effective_rarity();
        match arboard::Clipboard::new().and_then(|mut c| c.set_text(t.to_toml())) {
            Ok(()) => self.theme_msg = "Theme TOML copied to the clipboard. Paste it to a friend!".into(),
            Err(e) => self.theme_msg = format!("Clipboard unavailable: {e}"),
        }
        self.refresh();
    }

    pub fn import_theme(&mut self) {
        let raw =
            self.ui.upgrade().map(|u| u.global::<Backend>().get_theme_import_text().to_string()).unwrap_or_default();
        let raw = raw.trim().trim_matches('"').to_string();
        let src = if raw.is_empty() {
            arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default()
        } else if std::path::Path::new(&raw).is_file() {
            std::fs::read_to_string(&raw).unwrap_or_default()
        } else {
            raw
        };
        match Theme::parse(&src) {
            Ok(t) => {
                let _ = std::fs::create_dir_all(self.paths.themes());
                let file = self.paths.themes().join(format!("{}.toml", sanitize(&t.name)));
                if std::fs::write(&file, t.to_toml()).is_ok() {
                    self.reload_themes();
                    let name = t.name.clone();
                    self.theme_msg = format!("Imported '{name}'");
                    self.pick_theme(&name);
                }
            }
            Err(e) => {
                self.theme_msg = format!("Import failed: {e}");
                self.refresh();
            }
        }
    }

    pub fn open_themes_folder(&mut self) {
        let _ = std::fs::create_dir_all(self.paths.themes());
        let dir = self.paths.themes();
        #[cfg(windows)]
        let _ = std::process::Command::new("explorer").arg(&dir).spawn();
        #[cfg(not(windows))]
        let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
    }
}

pub fn sanitize(name: &str) -> String {
    let s: String =
        name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "untitled".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn sanitizes_file_names() {
        assert_eq!(sanitize("My Theme!"), "My-Theme");
        assert_eq!(sanitize("../../evil"), "evil");
        assert_eq!(sanitize("???"), "untitled");
    }
}

impl App {
    /// Developer aid (`LOOTLINE_DEMO=1`): fill an in-memory log with a fake party so every page
    /// can be inspected. Never touches the real database.
    pub fn seed_demo(&mut self) {
        use lootline_net::{LinkState, RosterMember};
        let friends = [("demo-aria", "Aria", "#ff5470"), ("demo-bo", "Bo", "#3fd35b"), ("demo-cy", "Cy", "#ffb62e")];
        let now = now_ms();
        let mut evs = Vec::new();
        for (id, name, color) in friends {
            evs.push(Event::Join {
                id: Uuid::new_v4(),
                player: id.into(),
                name: name.into(),
                color: color.into(),
                detecting: id != "demo-bo",
                ts: now - 3_600_000,
            });
        }
        let mut seed = 12345u64;
        let mut rnd = move || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as f64 / (1u64 << 31) as f64
        };
        let players: Vec<String> =
            std::iter::once(self.me.clone()).chain(friends.iter().map(|f| f.0.to_string())).collect();
        for i in 0..110 {
            let player = players[i % players.len()].clone();
            let ts = now - 3_000_000 + i as i64 * 24_000;
            let id = Uuid::new_v4();
            // sample an item from the loot table
            let mut x = rnd();
            let item = self.table.items.iter().find(|it| {
                x -= it.p;
                x <= 0.0
            });
            let (rarity, name) = item.map(|i| (Some(i.rarity), Some(i.name.clone()))).unwrap_or((None, None));
            evs.push(Event::ChestOpened {
                id,
                player: player.clone(),
                ts,
                rarity: None,
                item: None,
                match_id: Uuid::nil(),
            });
            evs.push(Event::Tag { id: Uuid::new_v4(), target: id, player, ts: ts + 1000, rarity, item: name });
        }
        for e in evs {
            self.commit_local_only(e);
        }
        self.party = Some(PartyState {
            link: LinkState::Online,
            host: self.me.clone(),
            i_am_host: true,
            code: lootline_net::make_code(&self.me),
            members: std::iter::once(RosterMember { id: self.me.clone(), connected: true })
                .chain(friends.iter().map(|f| RosterMember { id: f.0.into(), connected: f.0 != "demo-cy" }))
                .collect(),
        });
        self.session_start = now - 3_100_000;
        self.match_start = now - 1_500_000;
    }

    fn commit_local_only(&mut self, e: Event) {
        if let Ok(mut l) = self.log.lock() {
            l.insert(e.clone());
        }
        let _ = self.db.insert_event(&e);
    }
}
