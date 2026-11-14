//! Windows-only helpers: console window sizing (the "window size" setting)
//! and opening the system voice-typing panel.
//!
//! Applied on launch before the TUI starts: resizes the console buffer and
//! window, optionally centering it on the primary monitor. No-ops on other
//! platforms.

#[cfg(windows)]
pub fn apply_window_config(cfg: &crate::config::WindowConfig) -> bool {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        GetConsoleScreenBufferInfo, GetConsoleWindow, GetStdHandle, SetConsoleScreenBufferSize,
        SetConsoleWindowInfo, CONSOLE_SCREEN_BUFFER_INFO, COORD, SMALL_RECT, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetSystemMetrics, GetWindowRect, IsWindowVisible, SetWindowPos, SM_CXSCREEN, SM_CYSCREEN,
        SWP_NOSIZE, SWP_NOZORDER,
    };

    unsafe {
        // Windows Terminal (and every other modern terminal) talks to us
        // through a pseudo console: the "console window" is a hidden stub and
        // the real window can't be resized from here. Resizing the buffer
        // anyway only makes the app believe the screen is bigger than it is,
        // which draws everything wrapped and doubled. So: leave it alone.
        let hwnd = GetConsoleWindow();
        if std::env::var_os("WT_SESSION").is_some() || hwnd.is_null() || IsWindowVisible(hwnd) == 0 {
            return false;
        }
        // Launched by double-click with Windows Terminal as the default
        // terminal, WT_SESSION isn't set and the stub can even report itself
        // visible. Its window class gives it away: only the classic console
        // host uses "ConsoleWindowClass".
        let mut class = [0u16; 64];
        let n = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
        if String::from_utf16_lossy(&class[..n]) != "ConsoleWindowClass" {
            return false;
        }

        let out: HANDLE = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(out, &mut info) == 0 {
            return false; // not a real console (piped output etc.)
        }

        let cur_cols = (info.srWindow.Right - info.srWindow.Left + 1).max(0) as u16;
        let cur_rows = (info.srWindow.Bottom - info.srWindow.Top + 1).max(0) as u16;
        let cols = cfg.cols.max(cur_cols);
        let rows = cfg.rows.max(cur_rows);

        // 1) enlarge the buffer first (it must be >= window size)
        let buf = COORD { X: cols as i16, Y: (rows + 500) as i16 };
        if SetConsoleScreenBufferSize(out, buf) == 0 {
            return false;
        }

        // 2) then the window itself
        let win = SMALL_RECT { Left: 0, Top: 0, Right: (cols - 1) as i16, Bottom: (rows - 1) as i16 };
        if SetConsoleWindowInfo(out, 1, &win) == 0 {
            return false;
        }

        // 3) optionally center on the primary monitor
        if cfg.center {
            let mut r: RECT = std::mem::zeroed();
            if GetWindowRect(hwnd, &mut r) != 0 {
                // center using the window's real pixel size
                let x = ((GetSystemMetrics(SM_CXSCREEN) - (r.right - r.left)) / 2).max(0);
                let y = ((GetSystemMetrics(SM_CYSCREEN) - (r.bottom - r.top)) / 2).max(0);
                SetWindowPos(hwnd, std::ptr::null_mut(), x, y, 0, 0, SWP_NOZORDER | SWP_NOSIZE);
            }
        }
        true
    }
}

/// Open Windows' own voice typing panel by pressing Win+H for the user.
/// It types into whichever window has focus — this terminal.
pub fn open_voice_typing() {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{keybd_event, KEYEVENTF_KEYUP, VK_LWIN};
    const VK_H: u8 = 0x48;
    unsafe {
        keybd_event(VK_LWIN as u8, 0, 0, 0);
        keybd_event(VK_H, 0, 0, 0);
        keybd_event(VK_H, 0, KEYEVENTF_KEYUP, 0);
        keybd_event(VK_LWIN as u8, 0, KEYEVENTF_KEYUP, 0);
    }
}

#[cfg(not(windows))]
pub fn apply_window_config(_cfg: &crate::config::WindowConfig) -> bool {
    false
}
