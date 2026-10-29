//! Native message boxes, for the moments the webview is not the right place
//! (the update prompt runs before and around a restart).

use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::*;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A Yes/No question. Blocking; true for Yes.
pub fn ask_yes_no(title: &str, message: &str) -> bool {
    ask(title, message, false)
}

pub fn ask_yes_no_default_no(title: &str, message: &str) -> bool {
    ask(title, message, true)
}

fn ask(title: &str, message: &str, default_no: bool) -> bool {
    let (title, message) = (wide(title), wide(message));
    let result = unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_YESNO | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND | if default_no { MB_DEFBUTTON2 } else { MB_DEFBUTTON1 },
        )
    };
    result == IDYES
}

/// An error the player has to acknowledge. Blocking.
pub fn show_error(title: &str, message: &str) {
    let (title, message) = (wide(title), wide(message));
    unsafe {
        MessageBoxW(None, PCWSTR(message.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | MB_ICONERROR | MB_TOPMOST);
    }
}
