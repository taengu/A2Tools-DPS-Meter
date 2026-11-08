//! Finding the AION 2 window on Windows.
//!
//! By title first (`AION2…`), which is how the meter always did it. Failing
//! that, by the program that owns the window (`AION2.exe`): a player's log
//! showed ~600 game packets every 30 s ignored because his game window was not
//! titled `AION2…` (a localised or wrapped client), so the title alone is not
//! enough.

use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};

fn is_game_title(title: &str) -> bool {
    title.starts_with("AION2")
}

fn is_game_exe(path: &str) -> bool {
    path.rsplit(['\\', '/']).next().is_some_and(|name| name.eq_ignore_ascii_case("AION2.exe"))
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..len])
}

/// The full path of the program that owns `hwnd`. Needs no elevation for the
/// player's own processes.
pub(crate) fn exe_path(hwnd: HWND) -> Option<String> {
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn top_level_windows() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let windows = unsafe { &mut *(lparam.0 as *mut Vec<HWND>) };
        windows.push(hwnd);
        BOOL(1)
    }
    let mut windows: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut windows as *mut Vec<HWND> as isize));
    }
    windows
}

/// Find the AION 2 game window and return its title, or None if not found.
pub fn find_aion2_window_title() -> Option<String> {
    let windows = top_level_windows();
    if let Some(title) = windows.iter().map(|&h| window_title(h)).find(|t| is_game_title(t)) {
        return Some(title);
    }
    // Titled some other way: look at who owns each visible, titled window.
    windows.into_iter().find_map(|h| {
        if !unsafe { IsWindowVisible(h) }.as_bool() {
            return None;
        }
        let title = window_title(h);
        if title.is_empty() {
            return None;
        }
        exe_path(h).filter(|p| is_game_exe(p)).map(|_| title)
    })
}

/// The process id of the AION 2 game window, found as `find_aion2_window_title`
/// finds it.
pub(super) fn game_pid() -> Option<u32> {
    let windows = top_level_windows();
    let hwnd = windows.iter().copied().find(|&h| is_game_title(&window_title(h))).or_else(|| {
        windows.iter().copied().find(|&h| {
            unsafe { IsWindowVisible(h) }.as_bool()
                && !window_title(h).is_empty()
                && exe_path(h).is_some_and(|p| is_game_exe(&p))
        })
    })?;
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

/// Check if the AION 2 game window is currently running.
pub fn find_aion2_window() -> bool {
    find_aion2_window_title().is_some()
}

/// Nothing to start: the foreground window is asked for when needed.
/// KDE only (see the Linux version): nothing here draws under the meter.
pub fn watch_screenshot_tool(_on_change: impl Fn(bool) + Send + Sync + 'static) {}

pub fn watch_focus(_on_change: impl Fn() + Send + Sync + 'static) -> Result<(), String> {
    Ok(())
}

/// Check if the foreground window belongs to AION 2, by title or by program.
pub fn is_aion2_foreground() -> bool {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return false;
    }
    is_game_title(&window_title(hwnd)) || exe_path(hwnd).is_some_and(|p| is_game_exe(&p))
}

/// Windows that look like they might be the game (title or program mentions
/// "aion"), for the log when none is recognised. Deliberately not every window:
/// other titles (browser tabs above all) are none of the meter's business.
pub fn describe_candidates() -> Vec<String> {
    let mut out = Vec::new();
    for h in top_level_windows() {
        let title = window_title(h);
        let exe = exe_path(h).unwrap_or_default();
        let exe_name = exe.rsplit(['\\', '/']).next().unwrap_or("").to_string();
        if title.to_lowercase().contains("aion") || exe_name.to_lowercase().contains("aion") {
            let visible = unsafe { IsWindowVisible(h) }.as_bool();
            out.push(format!("title={title:?} exe={exe_name:?} visible={visible}"));
            if out.len() >= 8 {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_game_by_title_or_program() {
        assert!(is_game_title("AION2"));
        assert!(is_game_title("AION2 | Amber1"));
        assert!(!is_game_title("아이온2"));
        assert!(is_game_exe(r"D:\SteamLibrary\steamapps\common\AION2\Aion2\Binaries\Win64\AION2.exe"));
        assert!(is_game_exe(r"C:\Games\aion2.EXE"));
        assert!(!is_game_exe(r"C:\Games\AION2Launcher.exe"));
        assert!(!is_game_exe(""));
    }
}
