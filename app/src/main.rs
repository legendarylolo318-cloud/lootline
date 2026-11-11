// Lootline: Fortnite chest tracker. UI = Slint (software renderer), see README.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod devshot;
mod glue;
mod overlay;
mod platform;
mod util;
mod view;

slint::include_modules!();

use app::{on_ui, with_app, App};
use lootline_core::loot;
use lootline_core::settings::Settings;
use lootline_core::stats::{Scope, Who};
use lootline_core::storage::Storage;
use lootline_core::theme;
use lootline_core::EventLog;
use slint::{ComponentHandle, Timer, TimerMode};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use util::*;

fn main() {
    let paths = Paths::discover();
    let mut settings = Settings::load(&paths.settings());
    let secret = load_or_create_identity(&paths.identity());
    let me = lootline_net::identity_from_secret(&secret);
    settings.player_id = me.clone();

    let _ = theme::install_builtin(&paths.themes());
    let (themes, theme_errors) = theme::load_dir(&paths.themes());
    let theme = themes.iter().find(|t| t.name == settings.theme).unwrap_or(&themes[0]).clone();

    let demo = std::env::var_os("LOOTLINE_DEMO").is_some();
    let db = if demo { Storage::memory() } else { Storage::open(&paths.db()) }
        .or_else(|_| Storage::memory())
        .expect("storage");
    let log = Arc::new(Mutex::new(db.load_log().unwrap_or_else(|_| EventLog::new())));
    let now = now_ms();
    let session_id = db.begin_session(now).unwrap_or(0);
    let table = loot::load_cached_or_bundled(&paths.loot_cache(), &settings.season);
    let templates = App::load_templates(&paths.templates());

    let ui = MainWindow::new().expect("create window");
    let overlay = OverlayWindow::new().expect("create overlay");

    let net = lootline_net::spawn(lootline_net::NetConfig { secret }, log.clone(), |e| on_ui(move |a| a.on_net(e)));

    let app = App {
        ui: ui.as_weak(),
        overlay,
        paths,
        settings,
        log,
        db,
        table,
        themes,
        theme,
        me,
        session_id,
        session_start: now,
        match_start: now,
        scope: Scope::Session,
        who: Who::Party,
        sort: (1, false),
        hist_sel: String::new(),
        net,
        party: None,
        net_notice: String::new(),
        detect_status: "Waiting for Fortnite".into(),
        detect_ok: false,
        #[cfg(windows)]
        detector: None,
        #[cfg(windows)]
        game_pid: None,
        #[cfg(windows)]
        detect_meter: String::new(),
        #[cfg(windows)]
        detect_reject: String::new(),
        templates,
        overlay_chest: None,
        overlay_timer: Timer::default(),
        toast_timer: Timer::default(),
        glow_timer: Timer::default(),
        leave_timer: Timer::default(),
        hotkeys: Default::default(),
        #[cfg(windows)]
        tray: None,
        glow: -1,
        recording: false,
        template_status: String::new(),
        loot_status: String::new(),
        settings_msg: String::new(),
        theme_msg: theme_errors.first().map(|e| format!("Skipped invalid theme file: {e}")).unwrap_or_default(),
        quitting: false,
    };
    let _ = app.db.begin_match(app.current_match_id(), app.session_id, app.match_start);
    let mut app = app;
    if demo {
        app.seed_demo();
    }
    app::install(app);

    wire(&ui);
    let start_hidden = with_app(|a| {
        a.wire_overlay();
        a.apply_theme_only();
        a.push_settings_fields();
        a.ensure_join();
        if let Err(e) = glue::register_hotkeys(a) {
            a.settings_msg = e;
        }
        a.prepare_overlay();
        #[cfg(windows)]
        {
            glue::make_tray(a);
            glue::start_detector(a);
        }
        a.fetch_loot(true);
        a.refresh();
        cfg!(windows) && a.settings.start_minimized && a.tray.is_some()
    })
    .unwrap_or(false);

    if let Some(p) = std::env::var("LOOTLINE_PAGE").ok().and_then(|p| p.parse::<i32>().ok()) {
        ui.global::<Backend>().set_page(p);
    }
    if !start_hidden {
        let _ = ui.show();
        platform::fit_to_work_area(ui.window());
        // the start-up refresh ran before the window was visible
        with_app(|a| a.refresh());
    }

    if let Some(dir) = std::env::var_os("LOOTLINE_SNAPSHOT") {
        devshot::start(&ui, dir.into());
    }

    // keep "12s ago" labels fresh without a render loop
    let ticker = Timer::default();
    ticker.start(TimerMode::Repeated, Duration::from_secs(20), || {
        with_app(|a| {
            if a.ui.upgrade().is_some_and(|u| u.window().is_visible()) {
                a.refresh();
            }
        });
    });

    let _ = slint::run_event_loop_until_quit();

    with_app(|a| {
        let _ = a.db.end_session(a.session_id, now_ms());
    });
    // Skip destructors (detector/net threads): everything important is already persisted.
    std::process::exit(0);
}

fn wire(ui: &MainWindow) {
    let b = ui.global::<Backend>();
    b.on_add_chest(|| {
        with_app(|a| a.add_chest());
    });
    b.on_undo_chest(|| {
        with_app(|a| a.undo_last());
    });
    b.on_new_match(|| {
        with_app(|a| {
            a.start_match();
            a.toast("New match started");
            a.refresh();
        });
    });
    b.on_scope_changed(|i| {
        with_app(|a| {
            a.scope = match i {
                0 => Scope::Match,
                1 => Scope::Session,
                _ => Scope::Lifetime,
            };
            a.refresh();
        });
    });
    b.on_who_changed(|i| {
        with_app(|a| {
            a.who = if i == 0 { Who::Me } else { Who::Party };
            a.refresh();
        });
    });
    b.on_sort_odds(|col| {
        with_app(|a| {
            if a.sort.0 == col {
                a.sort.1 = !a.sort.1;
            } else {
                a.sort = (col, col != 1 && col != 6 && col != 2 && col != 4 && col != 3 && col != 5 && col != 7);
            }
            a.refresh();
        });
    });
    b.on_select_history(|id| {
        with_app(|a| {
            a.hist_sel = id.to_string();
            a.refresh();
        });
    });
    b.on_create_party(|| {
        with_app(|a| a.create_party());
    });
    b.on_join_party(|code| {
        with_app(|a| a.join_party(&code));
    });
    b.on_leave_party(|| {
        with_app(|a| a.leave_party());
    });
    b.on_copy_text(|t| {
        with_app(|a| match arboard::Clipboard::new().and_then(|mut c| c.set_text(t.to_string())) {
            Ok(()) => a.toast("Copied to clipboard"),
            Err(e) => a.toast(format!("Clipboard unavailable: {e}")),
        });
    });
    b.on_paste_join(|| {
        with_app(|a| {
            if let Ok(t) = arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
                if let Some(ui) = a.ui.upgrade() {
                    ui.global::<Backend>().set_join_input(t.trim().into());
                }
            }
        });
    });
    b.on_kick(|id| {
        with_app(|a| a.kick(&id));
    });
    b.on_apply_settings(|| {
        with_app(|a| a.apply_settings_from_ui());
    });
    b.on_record_template(|| {
        with_app(|a| {
            #[cfg(windows)]
            if let Some(ui) = a.ui.upgrade() {
                let name = app::sanitize(ui.global::<Backend>().get_template_name().as_str());
                if let Some(d) = &a.detector {
                    d.send(lootline_detect::Command::Record { name, secs: 4.0 });
                    a.recording = true;
                    a.template_status = "Recording for 4 seconds: play the chest sound in Fortnite now…".into();
                }
            }
            a.refresh();
        });
    });
    b.on_fetch_loot(|| {
        with_app(|a| a.fetch_loot(false));
    });
    b.on_fetch_templates(|| {
        with_app(|a| a.fetch_templates());
    });
    b.on_test_overlay(|| {
        with_app(|a| {
            a.show_overlay(uuid::Uuid::nil());
        });
    });
    b.on_pick_theme(|n| {
        with_app(|a| a.pick_theme(&n));
    });
    b.on_edit_color(|k, v| {
        with_app(|a| a.edit_color(&k, &v));
    });
    b.on_edit_rarity(|k, v| {
        with_app(|a| a.edit_rarity(&k, &v));
    });
    b.on_reset_rarity(|| {
        with_app(|a| a.reset_rarity());
    });
    b.on_save_theme(|| {
        with_app(|a| a.save_theme());
    });
    b.on_export_theme(|| {
        with_app(|a| a.export_theme());
    });
    b.on_import_theme(|| {
        with_app(|a| a.import_theme());
    });
    b.on_open_themes_folder(|| {
        with_app(|a| a.open_themes_folder());
    });
    b.on_hide_to_tray(|| {
        with_app(|a| {
            if let Some(ui) = a.ui.upgrade() {
                let _ = ui.hide();
            }
        });
    });

    ui.window().on_close_requested(|| {
        let to_tray = with_app(|a| {
            #[cfg(windows)]
            let can = a.settings.minimize_to_tray && a.tray.is_some() && !a.quitting;
            #[cfg(not(windows))]
            let can = false;
            if !can {
                a.quitting = true;
            }
            can
        })
        .unwrap_or(false);
        if !to_tray {
            let _ = slint::quit_event_loop();
        }
        slint::CloseRequestResponse::HideWindow
    });
}
