//! Platform bits that only touch *our own* windows.

/// Make the overlay a no-activate tool window that stays above borderless-fullscreen games,
/// so clicking it never takes keyboard focus away from Fortnite and it has no taskbar button.
/// This only modifies Lootline's own window; it never touches another process.
#[cfg(windows)]
pub fn style_overlay(w: &slint::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    };
    let handle = w.window_handle();
    let Ok(h) = HasWindowHandle::window_handle(&handle) else { return };
    if let RawWindowHandle::Win32(win) = h.as_raw() {
        let hwnd = HWND(win.hwnd.get() as *mut core::ffi::c_void);
        // SAFETY: hwnd is this process's own overlay window.
        unsafe {
            let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
            let add = (WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0 | WS_EX_TOPMOST.0) as isize;
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | add);
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
    }
}

#[cfg(not(windows))]
pub fn style_overlay(_w: &slint::Window) {}

/// Shrink and centre the main window if its preferred size doesn't fit the primary monitor's
/// work area (e.g. 1920x1080 at 150% leaves ~1280x672 logical pixels). Only our own window.
#[cfg(windows)]
pub fn fit_to_work_area(w: &slint::Window) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    let mut r = RECT::default();
    // SAFETY: SPI_GETWORKAREA writes one RECT into the buffer we pass.
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut r as *mut RECT as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    if ok.is_err() {
        return;
    }
    let scale = w.scale_factor().max(0.5);
    // leave room for the title bar and frame
    let (area_w, area_h) = ((r.right - r.left) as f32 / scale, (r.bottom - r.top) as f32 / scale - 40.0);
    if let Some((x, y, cw, ch)) = fit(w.size().to_logical(scale), area_w, area_h) {
        w.set_size(slint::LogicalSize::new(cw, ch));
        w.set_position(slint::LogicalPosition::new(r.left as f32 / scale + x, r.top as f32 / scale + y));
    }
}

#[cfg(not(windows))]
pub fn fit_to_work_area(_w: &slint::Window) {}

/// New (x, y, w, h) inside an `area_w` x `area_h` area if `cur` doesn't fit, else None.
pub fn fit(cur: slint::LogicalSize, area_w: f32, area_h: f32) -> Option<(f32, f32, f32, f32)> {
    if cur.width <= area_w && cur.height <= area_h {
        return None;
    }
    let (w, h) = (cur.width.min(area_w), cur.height.min(area_h));
    Some((((area_w - w) / 2.0).max(0.0), ((area_h - h) / 2.0).max(0.0) + 4.0, w, h))
}

#[cfg(test)]
mod tests {
    use super::fit;
    use slint::LogicalSize;

    #[test]
    fn shrinks_only_when_needed() {
        // 1080p at 150%: 1280 x 672 work area, minus title bar
        assert_eq!(fit(LogicalSize::new(1280.0, 820.0), 1280.0, 632.0), Some((0.0, 4.0, 1280.0, 632.0)));
        // 1440p at 100%: fits, untouched
        assert_eq!(fit(LogicalSize::new(1280.0, 820.0), 2560.0, 1352.0), None);
    }
}
