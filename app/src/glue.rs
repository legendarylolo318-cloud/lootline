//! Hotkeys, tray icon, detector start-up and HTTP fetches.

use crate::app::{on_ui, sanitize, with_app, App};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use lootline_core::dsp::TemplateFile;
use lootline_core::loot;
use std::str::FromStr;

// ---------------------------------------------------------------- global hotkeys

#[derive(Default)]
pub struct Hotkeys {
    manager: Option<GlobalHotKeyManager>,
    add: Option<HotKey>,
    undo: Option<HotKey>,
    handler_set: bool,
}

/// (Re)register the add/undo hotkeys from settings (RegisterHotKey under the hood).
pub fn register_hotkeys(app: &mut App) -> Result<(), String> {
    let hk = &mut app.hotkeys;
    if hk.manager.is_none() {
        hk.manager = Some(GlobalHotKeyManager::new().map_err(|e| format!("global hotkeys unavailable: {e}"))?);
    }
    let Some(m) = hk.manager.as_ref() else { return Ok(()) };
    for old in [hk.add.take(), hk.undo.take()].into_iter().flatten() {
        let _ = m.unregister(old);
    }
    let mut errors = Vec::new();
    let mut reg = |name: &str, text: &str| -> Option<HotKey> {
        match HotKey::from_str(text) {
            Ok(h) => match m.register(h) {
                Ok(()) => Some(h),
                Err(e) => {
                    errors.push(format!("{name} hotkey '{text}' could not be registered ({e})"));
                    None
                }
            },
            Err(_) => {
                errors.push(format!("'{text}' is not a valid hotkey"));
                None
            }
        }
    };
    let add = reg("add", &app.settings.hotkey_add);
    let undo = reg("undo", &app.settings.hotkey_undo);
    hk.add = add;
    hk.undo = undo;
    if !hk.handler_set {
        hk.handler_set = true;
        GlobalHotKeyEvent::set_event_handler(Some(|e: GlobalHotKeyEvent| {
            if e.state != HotKeyState::Pressed {
                return;
            }
            on_ui(move |a| {
                if a.hotkeys.add.is_some_and(|h| h.id() == e.id) {
                    a.add_chest();
                } else if a.hotkeys.undo.is_some_and(|h| h.id() == e.id) {
                    a.undo_last();
                }
            });
        }));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

// ---------------------------------------------------------------- tray (Windows)

#[cfg(windows)]
pub struct TrayState {
    icon: tray_icon::TrayIcon,
    last: std::cell::RefCell<String>,
}

#[cfg(windows)]
impl TrayState {
    /// Show the chest count in the tray tooltip (only touches the OS when the text changes).
    pub fn set_count(&self, mine: usize, party: usize) {
        let t = format!("Lootline: {mine} chests (party {party})");
        if *self.last.borrow() != t {
            let _ = self.icon.set_tooltip(Some(&t));
            *self.last.borrow_mut() = t;
        }
    }
}

#[cfg(windows)]
pub fn make_tray(app: &mut App) {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let menu = Menu::new();
    let show = MenuItem::new("Show Lootline", true, None);
    let add = MenuItem::new("Add chest", true, None);
    let undo = MenuItem::new("Undo last chest", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let _ = menu.append_items(&[&show, &add, &undo, &PredefinedMenuItem::separator(), &quit]);
    let (show_id, add_id, undo_id, quit_id) =
        (show.id().clone(), add.id().clone(), undo.id().clone(), quit.id().clone());

    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let id = e.id.clone();
        let (show_id, add_id, undo_id, quit_id) = (show_id.clone(), add_id.clone(), undo_id.clone(), quit_id.clone());
        let _ = slint::invoke_from_event_loop(move || {
            if id == quit_id {
                with_app(|a| a.quitting = true);
                let _ = slint::quit_event_loop();
            } else if id == show_id {
                show_main();
            } else if id == add_id {
                with_app(|a| a.add_chest());
            } else if id == undo_id {
                with_app(|a| a.undo_last());
            }
        });
    }));
    TrayIconEvent::set_event_handler(Some(|e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            let _ = slint::invoke_from_event_loop(show_main);
        }
    }));

    match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Lootline")
        .with_icon(make_icon())
        .build()
    {
        Ok(icon) => app.tray = Some(TrayState { icon, last: Default::default() }),
        Err(e) => app.toast(format!("tray icon unavailable: {e}")),
    }
}

#[cfg(windows)]
fn make_icon() -> tray_icon::Icon {
    // 32x32: rounded accent square with a white "L"
    let n = 32usize;
    let mut px = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = ((x as f32 - 15.5).abs() - 11.0, (y as f32 - 15.5).abs() - 11.0);
            let d = (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() - 4.0;
            if d < 0.5 {
                let t = (x + y) as f32 / (2.0 * n as f32);
                let (r, g, b) = (79.0 + t * 59.0, 140.0 - t * 48.0, 255.0);
                let in_l = ((10..=13).contains(&x) && (7..=24).contains(&y))
                    || ((21..=24).contains(&y) && (10..=22).contains(&x));
                let i = (y * n + x) * 4;
                if in_l {
                    px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                } else {
                    px[i..i + 4].copy_from_slice(&[r as u8, g as u8, b as u8, 255]);
                }
            }
        }
    }
    tray_icon::Icon::from_rgba(px, n as u32, n as u32).expect("valid icon")
}

pub fn show_main() {
    with_app(|a| {
        if let Some(ui) = a.ui.upgrade() {
            use slint::ComponentHandle;
            let _ = ui.show();
            a.refresh();
        }
    });
}

// ---------------------------------------------------------------- detector (Windows)

#[cfg(windows)]
pub fn start_detector(app: &mut App) {
    let cfg = lootline_detect::Config {
        sensitivity: app.settings.sensitivity,
        interact_key: app.settings.interact_key.clone(),
        templates: app.templates.clone(),
        enabled: app.settings.detection_enabled,
    };
    app.detector = Some(lootline_detect::spawn(cfg, |e| on_ui(move |a| a.on_detect(e))));
}

// ---------------------------------------------------------------- HTTP fetches

impl App {
    pub fn fetch_loot(&mut self, silent: bool) {
        let (base, season, dir) =
            (self.settings.repo_base_url.clone(), self.settings.season.clone(), self.paths.loot_cache());
        if !silent {
            self.loot_status = "Fetching…".into();
            self.refresh();
        }
        std::thread::spawn(move || {
            let res = loot::fetch_and_cache(&base, &season, &dir);
            on_ui(move |a| {
                if a.settings.season != season || a.settings.repo_base_url != base {
                    // settings changed while this fetch was in flight: its result is stale
                    return;
                }
                match res {
                    Ok(t) => {
                        a.loot_status = format!(
                            "Loaded '{}' ({} items){}",
                            t.season,
                            t.items.len(),
                            if t.placeholder { ", PLACEHOLDER values" } else { "" }
                        );
                        a.table = t;
                    }
                    Err(e) => {
                        if !silent {
                            a.loot_status = format!("Fetch failed ({e}); using the cached or bundled table");
                        }
                    }
                }
                a.refresh();
            });
        });
    }

    pub fn fetch_templates(&mut self) {
        let (base, dir) = (self.settings.repo_base_url.clone(), self.paths.templates());
        self.template_status = "Fetching templates…".into();
        self.refresh();
        std::thread::spawn(move || {
            let res = (|| -> anyhow::Result<usize> {
                #[derive(serde::Deserialize)]
                struct Index {
                    templates: Vec<String>,
                }
                let idx: Index = serde_json::from_str(&loot::http_get(&format!(
                    "{}/templates/index.json",
                    base.trim_end_matches('/')
                ))?)?;
                std::fs::create_dir_all(&dir)?;
                let mut n = 0;
                for name in idx.templates {
                    // only plain file names ending in .json: never a path
                    anyhow::ensure!(
                        name.ends_with(".json") && name == sanitize_file(&name),
                        "refusing suspicious template name '{name}'"
                    );
                    let body = loot::http_get(&format!("{}/templates/{}", base.trim_end_matches('/'), name))?;
                    TemplateFile::parse(&body)?;
                    std::fs::write(dir.join(&name), body)?;
                    n += 1;
                }
                Ok(n)
            })();
            on_ui(move |a| {
                a.template_status = match res {
                    Ok(0) => "The repo has no templates yet. Record your own below.".to_string(),
                    Ok(n) => format!("Downloaded {n} template(s)"),
                    Err(e) => format!("Fetch failed: {e}"),
                };
                a.reload_templates();
                a.refresh();
            });
        });
    }
}

fn sanitize_file(name: &str) -> String {
    let stem = name.strip_suffix(".json").unwrap_or(name);
    format!("{}.json", sanitize(stem))
}
