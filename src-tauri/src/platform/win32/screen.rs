//! Screenshots on Windows: GDI screen copy, the clipboard, the Pictures folder
//! and the shell's folder picker. The rect maths and PNG encoder are shared,
//! in `platform::screenshot`.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::DataExchange::*;

use crate::platform::screenshot::{css_rect_to_screen, encode_png, ScreenRect};

fn hwnd_of(window: &tauri::WebviewWindow) -> Option<isize> {
    window.hwnd().ok().map(|h| h.0 as isize)
}

/// Where a window's client area (what the page draws into) sits on screen.
fn client_origin(hwnd_raw: isize) -> (i32, i32) {
    let mut point = POINT { x: 0, y: 0 };
    unsafe {
        let _ = ClientToScreen(HWND(hwnd_raw as *mut _), &mut point);
    }
    (point.x, point.y)
}

/// A window's whole client area on screen.
fn client_rect(hwnd_raw: isize) -> ScreenRect {
    let hwnd = HWND(hwnd_raw as *mut _);
    let mut rect = RECT::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rect);
    }
    let (left, top) = client_origin(hwnd_raw);
    ScreenRect { left, top, width: rect.right - rect.left, height: rect.bottom - rect.top }
}

/// Capture a CSS-pixel rect of `caller` (plus all of `meter`, if given), put it
/// on the clipboard and optionally write it to `png_path`. Blocking. Returns
/// (clipboard ok, file ok).
#[allow(clippy::too_many_arguments)]
pub fn capture(
    caller: &tauri::WebviewWindow,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
    meter: Option<&tauri::WebviewWindow>,
    png_path: Option<&Path>,
) -> (bool, bool) {
    let Some(owner) = hwnd_of(caller) else { return (false, false) };
    let mut rect = css_rect_to_screen(client_origin(owner), x, y, width, height, scale);
    if let Some(meter) = meter.and_then(hwnd_of) {
        rect = rect.union(client_rect(meter));
    }
    capture_rect(owner, rect, png_path)
}

fn capture_rect(owner_raw: isize, rect: ScreenRect, png_path: Option<&Path>) -> (bool, bool) {
    if rect.width <= 0 || rect.height <= 0 || rect.width > 8192 || rect.height > 8192
        || rect.width as u64 * rect.height as u64 > 16_777_216 {
        return (false, false);
    }
    unsafe {
        let hdc_screen = GetDC(None);
        let hdc_mem = CreateCompatibleDC(Some(hdc_screen));
        let hbm = CreateCompatibleBitmap(hdc_screen, rect.width, rect.height);
        let old = SelectObject(hdc_mem, hbm.into());
        let copied = BitBlt(
            hdc_mem, 0, 0, rect.width, rect.height,
            Some(hdc_screen), rect.left, rect.top, SRCCOPY,
        )
        .is_ok();
        SelectObject(hdc_mem, old);

        let mut file_ok = false;
        if copied {
            if let Some(path) = png_path {
                file_ok = write_png(hdc_mem, hbm, rect, path);
            }
        }
        let _ = DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);

        let mut clipboard_ok = false;
        if copied && OpenClipboard(Some(HWND(owner_raw as *mut _))).is_ok() {
            let _ = EmptyClipboard();
            // CF_BITMAP = 2. On success the clipboard owns the bitmap.
            clipboard_ok = SetClipboardData(2, Some(HANDLE(hbm.0 as *mut _))).is_ok();
            let _ = CloseClipboard();
        }
        if !clipboard_ok {
            let _ = DeleteObject(hbm.into());
        }
        (clipboard_ok, file_ok)
    }
}

unsafe fn write_png(hdc: HDC, hbm: HBITMAP, rect: ScreenRect, path: &Path) -> bool {
    let mut info = BITMAPINFO::default();
    info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = rect.width;
    info.bmiHeader.biHeight = -rect.height; // negative: top-down rows
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB.0;
    let mut pixels = vec![0u8; rect.width as usize * rect.height as usize * 4];
    let rows = unsafe {
        GetDIBits(
            hdc, hbm, 0, rect.height as u32,
            Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS,
        )
    };
    if rows != rect.height {
        return false;
    }
    // BGRx -> RGBA, opaque: screen captures carry no meaningful alpha.
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2);
        px[3] = 255;
    }
    let png = encode_png(rect.width as u32, rect.height as u32, &pixels);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, png).is_ok()
}

/// `Pictures\A2Tools DPS Meter`, the default place screenshots are saved.
pub fn default_folder() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Pictures, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let raw = SHGetKnownFolderPath(&FOLDERID_Pictures, KF_FLAG_DEFAULT, None).ok()?;
        let path = raw.to_string().ok();
        CoTaskMemFree(Some(raw.0 as *const _));
        Some(PathBuf::from(path?).join("A2Tools DPS Meter"))
    }
}

/// The standard Windows folder picker, owned by `owner`. Blocks until the
/// player chooses or cancels; it runs on its own thread because the dialog is
/// COM and wants an apartment of its own.
pub fn pick_folder(owner: &tauri::WebviewWindow, start_in: Option<&str>) -> Option<String> {
    let owner_raw = hwnd_of(owner)?;
    let start_in = start_in.map(str::to_string);
    std::thread::spawn(move || pick_folder_blocking(owner_raw, start_in.as_deref()))
        .join()
        .ok()
        .flatten()
}

fn pick_folder_blocking(owner_raw: isize, start_in: Option<&str>) -> Option<String> {
    use windows::core::HSTRING;
    use windows::Win32::System::Com::*;
    use windows::Win32::UI::Shell::*;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let result = (|| -> Option<String> {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let options = dialog.GetOptions().ok()?;
            dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
            if let Some(start) = start_in.filter(|s| !s.is_empty()) {
                if let Ok(item) =
                    SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(start), None)
                {
                    let _ = dialog.SetFolder(&item);
                }
            }
            dialog.Show(Some(HWND(owner_raw as *mut _))).ok()?; // Err when cancelled
            let item = dialog.GetResult().ok()?;
            let raw = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let path = raw.to_string().ok();
            CoTaskMemFree(Some(raw.0 as *const _));
            path
        })();
        CoUninitialize();
        result
    }
}
