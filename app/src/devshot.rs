//! Developer aid: `LOOTLINE_SNAPSHOT=<dir>` renders every page in every built-in theme at a set
//! of window sizes to `<dir>/<theme>-p<page>-<w>x<h>.bmp`, then quits. Combine with
//! `LOOTLINE_DEMO=1` for populated pages and `SLINT_SCALE_FACTOR=1.5` to check 150% DPI.
//! Nothing is saved to the user's settings.

use crate::app::with_app;
use crate::{Backend, MainWindow};
use slint::{ComponentHandle, LogicalSize, Timer, TimerMode};
use std::path::PathBuf;
use std::time::Duration;

/// Logical window sizes: minimum width but tall (whole settings page), the minimum size, 1080p at 150% (work area), 1080p and 1440p at 100%.
const SIZES: [(f32, f32); 5] = [(1000.0, 1500.0), (1000.0, 560.0), (1280.0, 640.0), (1920.0, 1000.0), (2560.0, 1360.0)];
const PAGES: i32 = 5;

pub fn start(ui: &MainWindow, dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    let n_themes = with_app(|a| a.themes.len()).unwrap_or(0);
    let total = n_themes * PAGES as usize * SIZES.len();
    let weak = ui.as_weak();
    let mut step = 0usize;
    let mut armed = false;
    let timer = Box::leak(Box::new(Timer::default()));
    timer.start(TimerMode::Repeated, Duration::from_millis(250), move || {
        let Some(ui) = weak.upgrade() else { return };
        if step >= total {
            let _ = slint::quit_event_loop();
            return;
        }
        let ti = step / (PAGES as usize * SIZES.len());
        let page = ((step / SIZES.len()) % PAGES as usize) as i32;
        let (w, h) = SIZES[step % SIZES.len()];
        if !armed {
            // configure, then capture on the next tick once layout has settled
            with_app(|a| {
                if let Some(t) = a.themes.get(ti).cloned() {
                    a.theme = t;
                    a.apply_theme_only();
                }
            });
            ui.global::<Backend>().set_page(page);
            ui.window().set_size(LogicalSize::new(w, h));
            with_app(|a| a.refresh());
            armed = true;
            return;
        }
        armed = false;
        let name = with_app(|a| a.themes.get(ti).map(|t| t.name.clone())).flatten().unwrap_or_default();
        let file = dir.join(format!("{}-p{page}-{}x{}.bmp", crate::app::sanitize(&name), w as u32, h as u32));
        if let Ok(buf) = ui.window().take_snapshot() {
            let _ = std::fs::write(file, bmp(buf.width(), buf.height(), buf.as_bytes()));
        }
        step += 1;
    });
}

/// 32-bit top-down BMP from RGBA8.
fn bmp(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let data = (w * h * 4) as usize;
    let mut out = Vec::with_capacity(54 + data);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + data as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    for px in rgba.as_chunks::<4>().0 {
        out.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    out
}
