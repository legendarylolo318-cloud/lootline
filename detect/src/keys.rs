//! Interact-key tracking via `GetAsyncKeyState` polling (~60 Hz). Read-only: no hooks.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

/// Parse a key name ("E", "F", "7", "F5", "Space", "Shift", "Mouse4", ...) to a virtual-key code.
pub fn parse_vk(name: &str) -> Option<u16> {
    let n = name.trim().to_ascii_uppercase();
    let b = n.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit()) {
        return Some(b[0] as u16);
    }
    if let Some(num) = n.strip_prefix('F').and_then(|x| x.parse::<u16>().ok()) {
        if (1..=24).contains(&num) {
            return Some(0x6F + num);
        }
    }
    if let Some(num) = n.strip_prefix("MOUSE").and_then(|x| x.parse::<u16>().ok()) {
        return match num {
            1 => Some(0x01),
            2 => Some(0x02),
            3 => Some(0x04),
            4 => Some(0x05),
            5 => Some(0x06),
            _ => None,
        };
    }
    Some(match n.as_str() {
        "SPACE" => 0x20,
        "SHIFT" => 0x10,
        "CTRL" | "CONTROL" => 0x11,
        "ALT" => 0x12,
        "TAB" => 0x09,
        "ENTER" | "RETURN" => 0x0D,
        "CAPSLOCK" => 0x14,
        "ESC" | "ESCAPE" => 0x1B,
        _ => return None,
    })
}

pub struct KeyTracker {
    vk: Arc<AtomicU32>,
    /// ms since `start`, plus one (0 = never pressed)
    last_down: Arc<AtomicU64>,
    start: Instant,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl KeyTracker {
    pub fn start(vk: u16) -> KeyTracker {
        let vk = Arc::new(AtomicU32::new(vk as u32));
        let last_down = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let start = Instant::now();
        let (v, l, s) = (vk.clone(), last_down.clone(), stop.clone());
        let handle = std::thread::Builder::new()
            .name("lootline-keys".into())
            .spawn(move || {
                while !s.load(Ordering::Relaxed) {
                    let code = v.load(Ordering::Relaxed) as i32;
                    // SAFETY: GetAsyncKeyState only reads the global key state table.
                    let st = unsafe { GetAsyncKeyState(code) } as u16;
                    if st & 0x8000 != 0 {
                        l.store(start.elapsed().as_millis() as u64 + 1, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_millis(16));
                }
            })
            .ok();
        KeyTracker { vk, last_down, start, stop, handle }
    }

    pub fn set_key(&self, vk: u16) {
        self.vk.store(vk as u32, Ordering::Relaxed);
        self.last_down.store(0, Ordering::Relaxed);
    }

    /// Was the key down at any point within the last `window`?
    pub fn held_within(&self, window: Duration) -> bool {
        let ld = self.last_down.load(Ordering::Relaxed);
        if ld == 0 {
            return false;
        }
        let now = self.start.elapsed().as_millis() as u64 + 1;
        now.saturating_sub(ld) <= window.as_millis() as u64
    }
}

impl Drop for KeyTracker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_names() {
        assert_eq!(parse_vk("e"), Some(0x45));
        assert_eq!(parse_vk("E"), Some(0x45));
        assert_eq!(parse_vk("7"), Some(0x37));
        assert_eq!(parse_vk("F1"), Some(0x70));
        assert_eq!(parse_vk("f12"), Some(0x7B));
        assert_eq!(parse_vk("Space"), Some(0x20));
        assert_eq!(parse_vk("Mouse4"), Some(0x05));
        assert_eq!(parse_vk("F25"), None);
        assert_eq!(parse_vk("nonsense"), None);
    }

    #[test]
    fn never_pressed_key_is_not_held() {
        // VK 0xFF is unassigned, so it never reads as down
        let t = KeyTracker::start(0xFF);
        std::thread::sleep(Duration::from_millis(50));
        assert!(!t.held_within(Duration::from_secs(5)));
    }
}
