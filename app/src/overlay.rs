//! The rarity-tagging overlay: a small borderless always-on-top window.
//! It is created once, styled as "no-activate" (so it never steals focus from Fortnite),
//! and then only shown/hidden. It does not draw anything while hidden.

use crate::app::{with_app, App};
use crate::platform;
use slint::{ComponentHandle, LogicalPosition, LogicalSize, ModelRc, Timer, TimerMode, VecModel};
use std::time::Duration;
use uuid::Uuid;

const EXPANDED_EXTRA: f32 = 210.0;

impl App {
    pub fn prepare_overlay(&mut self) {
        // Show once off-screen so the native window exists, mark it no-activate, then hide it.
        let w = self.overlay.window();
        w.set_position(LogicalPosition::new(-4000.0, -4000.0));
        w.set_size(LogicalSize::new(self.settings.overlay.width as f32, self.settings.overlay.height as f32));
        let _ = self.overlay.show();
        platform::style_overlay(self.overlay.window());
        let _ = self.overlay.hide();
    }

    pub fn show_overlay(&mut self, chest: Uuid) {
        let s = self.settings.overlay.clone();
        self.overlay_chest = Some(chest);
        let o = &self.overlay;
        o.set_items(ModelRc::new(VecModel::from(self.overlay_items())));
        o.set_expanded(false);
        o.set_timeout((s.timeout_secs * 1000.0) as i64);
        o.set_running(false);
        let w = o.window();
        w.set_position(LogicalPosition::new(s.x as f32, s.y as f32));
        w.set_size(LogicalSize::new(s.width as f32, s.height as f32));
        let _ = o.show();
        platform::style_overlay(o.window());
        // start the countdown one tick later so the "running" transition is seen by the renderer
        Timer::single_shot(Duration::from_millis(40), || {
            with_app(|a| {
                if a.overlay_chest.is_some() {
                    a.overlay.set_running(true);
                }
            });
        });
        self.arm_overlay_timer();
    }

    fn arm_overlay_timer(&mut self) {
        let secs = self.settings.overlay.timeout_secs.max(0.5);
        self.overlay_timer.start(TimerMode::SingleShot, Duration::from_secs_f32(secs), || {
            with_app(|a| a.hide_overlay());
        });
    }

    pub fn hide_overlay(&mut self) {
        self.overlay_timer.stop();
        self.overlay_chest = None;
        self.overlay.set_running(false);
        self.overlay.set_expanded(false);
        let _ = self.overlay.hide();
    }

    pub fn overlay_tag(&mut self, rarity: i32, item: Option<String>) {
        // the nil id is the settings page's "Test overlay": nothing to tag
        if let Some(chest) = self.overlay_chest.filter(|c| !c.is_nil()) {
            self.tag(chest, lootline_core::Rarity::from_index(rarity as usize), item);
        }
        self.hide_overlay();
    }

    pub fn overlay_expand(&mut self, expanded: bool) {
        let s = &self.settings.overlay;
        let h = s.height as f32 + if expanded { EXPANDED_EXTRA } else { 0.0 };
        self.overlay.window().set_size(LogicalSize::new(s.width as f32, h));
        if expanded {
            self.overlay_timer.stop(); // don't vanish while the user is choosing an item
        } else {
            self.arm_overlay_timer();
        }
    }

    pub fn wire_overlay(&self) {
        let o = &self.overlay;
        o.on_tag(|r| {
            with_app(|a| a.overlay_tag(r, None));
        });
        o.on_tag_item(|name, r| {
            with_app(|a| a.overlay_tag(r, Some(name.to_string())));
        });
        o.on_dismiss(|| {
            with_app(|a| a.hide_overlay());
        });
        o.on_expand_changed(|e| {
            with_app(|a| a.overlay_expand(e));
        });
    }
}
