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
