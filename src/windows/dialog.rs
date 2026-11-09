//! A small modal Win32 window, using the system message font at each DPI.

use super::{ui, wide};
use crate::core::config;
use std::cell::{Cell, RefCell};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DEFAULT_GUI_FONT, DeleteObject, GetMonitorInfoW, GetStockObject, HFONT,
    InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::EM_SETLIMITTEXT;
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, SetFocus, VK_ESCAPE, VK_RETURN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "Omapower.AddCommand";
const WIDTH: i32 = 380;
const HEIGHT: i32 = 132;

struct Dialog {
    controls: [Cell<HWND>; 4],
    font: Cell<HFONT>,
    owns_font: Cell<bool>,
    done: Cell<bool>,
    result: RefCell<Option<String>>,
}

impl Dialog {
    fn accept(&self, hwnd: HWND) {
        let edit = self.controls[1].get();
        // SAFETY: edit is our live child control, and the buffer includes its terminator.
        let text = unsafe {
            let length = GetWindowTextLengthW(edit).max(0) as usize;
            let mut buffer = vec![0u16; length + 1];
            let count =
                GetWindowTextW(edit, buffer.as_mut_ptr(), buffer.len() as i32).max(0) as usize;
            String::from_utf16_lossy(&buffer[..count])
                .trim()
                .to_string()
        };
        if !config::valid_name(&text) {
            ui::alert(
                hwnd,
                &format!(
                    "Enter a process name of 1–{} bytes, without slashes or newlines.",
                    config::MAX_NAME_BYTES
                ),
            );
            // SAFETY: edit remains live after the synchronous message box.
            unsafe { SetFocus(edit) };
            return;
        }
        *self.result.borrow_mut() = Some(text);
        self.done.set(true);
    }

    fn layout(&self, hwnd: HWND, dpi: u32) {
        let scale = |value| scale(value, dpi);
        let mut metrics = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        // SAFETY: metrics is sized and writable; DPI-specific metrics already scale the font.
        let font = unsafe {
            if SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS,
                metrics.cbSize,
                (&mut metrics as *mut NONCLIENTMETRICSW).cast(),
                0,
                dpi,
            ) != 0
            {
                CreateFontIndirectW(&metrics.lfMessageFont)
            } else {
                null_mut()
            }
        };
        let owned = !font.is_null();
        // SAFETY: DEFAULT_GUI_FONT is an immortal stock GDI object.
        let font = if owned {
            font
        } else {
            unsafe { GetStockObject(DEFAULT_GUI_FONT) }
        };
        let positions = [
            (16, 14, 348, 24),
            (16, 42, 348, 26),
            (202, 88, 76, 28),
            (288, 88, 76, 28),
        ];
        for (control, (x, y, width, height)) in self.controls.iter().zip(positions) {
            // SAFETY: all children belong to this dialog; WM_SETFONT borrows the live font.
            unsafe {
                SetWindowPos(
                    control.get(),
                    null_mut(),
                    scale(x),
                    scale(y),
                    scale(width),
                    scale(height),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                SendMessageW(control.get(), WM_SETFONT, font as usize, 1);
            }
        }
        let previous = self.font.replace(font);
        if self.owns_font.replace(owned) {
            // SAFETY: controls now use the replacement; previous was our own created font.
            unsafe { DeleteObject(previous) };
        }
        // SAFETY: invalidating our live window schedules repainting after the layout change.
        unsafe { InvalidateRect(hwnd, null(), 1) };
    }
}

fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi as i64 + 48) / 96) as i32
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: Windows supplies CREATESTRUCTW during WM_NCCREATE, containing our boxed Dialog.
        unsafe {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    // SAFETY: user data is null before creation and otherwise points to the live boxed Dialog.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Dialog;
    if !pointer.is_null() {
        // SAFETY: prompt keeps the box alive until after DestroyWindow returns.
        let dialog = unsafe { &*pointer };
        match message {
            WM_COMMAND => {
                match (wparam & 0xffff) as i32 {
                    IDOK => dialog.accept(hwnd),
                    IDCANCEL => dialog.done.set(true),
                    _ => {}
                }
                return 0;
            }
            WM_CLOSE => {
                dialog.done.set(true);
                return 0;
            }
            WM_DPICHANGED => {
                // SAFETY: WM_DPICHANGED supplies a suggested screen rectangle for this window.
                unsafe {
                    let rect = &*(lparam as *const RECT);
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        rect.left,
                        rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                dialog.layout(hwnd, (wparam & 0xffff) as u32);
                return 0;
            }
            WM_NCDESTROY => {
                // SAFETY: disconnect the pointer before prompt drops its allocation.
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            }
            _ => {}
        }
    }
    // SAFETY: all messages not handled here go to the standard window procedure.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

pub fn prompt(owner: HWND) -> Result<Option<String>, String> {
    ui::register_class(CLASS, Some(procedure))?;
    let dialog = Box::new(Dialog {
        controls: std::array::from_fn(|_| Cell::new(null_mut())),
        font: Cell::new(null_mut()),
        owns_font: Cell::new(false),
        done: Cell::new(false),
        result: RefCell::new(None),
    });
    // SAFETY: owner is live; the module and monitor APIs use valid local outputs.
    let (instance, dpi, work) = unsafe {
        let mut monitor = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let handle = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(handle, &mut monitor) == 0 {
            return Err(format!(
                "find dialog screen: {}",
                std::io::Error::last_os_error()
            ));
        }
        (
            GetModuleHandleW(null()),
            GetDpiForWindow(owner).max(96),
            monitor.rcWork,
        )
    };
    let style = WS_CAPTION | WS_SYSMENU | WS_POPUP;
    let extended = WS_EX_DLGMODALFRAME | WS_EX_CONTROLPARENT;
    let mut rect = RECT {
        right: scale(WIDTH, dpi),
        bottom: scale(HEIGHT, dpi),
        ..Default::default()
    };
    // SAFETY: rect is a writable client rectangle, transformed for the supplied styles/DPI.
    unsafe { AdjustWindowRectExForDpi(&mut rect, style, 0, extended, dpi) };
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    // SAFETY: Dialog remains boxed throughout the modal loop; all strings are terminated.
    let hwnd = unsafe {
        CreateWindowExW(
            extended,
            wide(CLASS).as_ptr(),
            wide("Add Command").as_ptr(),
            style,
            work.left + (work.right - work.left - width) / 2,
            work.top + (work.bottom - work.top - height) / 2,
            width,
            height,
            owner,
            null_mut(),
            instance,
            (&*dialog as *const Dialog).cast(),
        )
    };
    if hwnd.is_null() {
        return Err(format!(
            "create command dialog: {}",
            std::io::Error::last_os_error()
        ));
    }
    let definitions = [
        ("STATIC", "Process name (without .exe):", 0, 0, 0),
        (
            "EDIT",
            "",
            WS_TABSTOP | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE,
            100,
        ),
        (
            "BUTTON",
            "OK",
            WS_TABSTOP | BS_DEFPUSHBUTTON as u32,
            0,
            IDOK,
        ),
        (
            "BUTTON",
            "Cancel",
            WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            IDCANCEL,
        ),
    ];
    for (control, (class, label, style, extended, id)) in dialog.controls.iter().zip(definitions) {
        // SAFETY: hwnd is a live parent; child IDs are integer HMENU values for WS_CHILD.
        let child = unsafe {
            CreateWindowExW(
                extended,
                wide(class).as_ptr(),
                wide(label).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                0,
                0,
                hwnd,
                id as usize as HMENU,
                instance,
                null(),
            )
        };
        if child.is_null() {
            let error = format!("create dialog control: {}", std::io::Error::last_os_error());
            // SAFETY: destroying the parent destroys any children already created.
            unsafe { DestroyWindow(hwnd) };
            return Err(error);
        }
        control.set(child);
    }
    dialog.layout(hwnd, dpi);
    // SAFETY: the owner and dialog are live on this UI thread, as is the edit control.
    unsafe {
        SendMessageW(
            dialog.controls[1].get(),
            EM_SETLIMITTEXT,
            config::MAX_NAME_BYTES + 1,
            0,
        );
        EnableWindow(owner, 0);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus(dialog.controls[1].get());
    }
    let mut error = None;
    while !dialog.done.get() {
        let mut message = MSG::default();
        // SAFETY: message is writable; this thread owns both the dialog and its owner.
        let status = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if status <= 0 {
            if status == 0 {
                // SAFETY: preserve WM_QUIT for the outer tray message loop.
                unsafe { PostQuitMessage(message.wParam as i32) };
            } else {
                error = Some(format!(
                    "command dialog: {}",
                    std::io::Error::last_os_error()
                ));
            }
            break;
        }
        if message.message == WM_KEYDOWN {
            match message.wParam as u16 {
                VK_RETURN => {
                    // SAFETY: GetFocus returns the focus HWND on this UI thread.
                    if unsafe { GetFocus() } == dialog.controls[3].get() {
                        dialog.done.set(true);
                    } else {
                        dialog.accept(hwnd);
                    }
                    continue;
                }
                VK_ESCAPE => {
                    dialog.done.set(true);
                    continue;
                }
                _ => {}
            }
        }
        // SAFETY: dialog navigation handles Tab; remaining messages are dispatched normally.
        unsafe {
            if IsDialogMessageW(hwnd, &message) == 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    // SAFETY: destroy the controls before the font; re-enable the still-live owner when present.
    unsafe {
        DestroyWindow(hwnd);
        EnableWindow(owner, 1);
        SetForegroundWindow(owner);
        if dialog.owns_font.get() {
            DeleteObject(dialog.font.get());
        }
    }
    if let Some(error) = error {
        Err(error)
    } else {
        Ok(dialog.result.borrow_mut().take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_dialog_units_scale_with_dpi() {
        assert_eq!(scale(380, 96), 380);
        assert_eq!(scale(380, 144), 570);
        assert_eq!(scale(16, 192), 32);
    }
}
