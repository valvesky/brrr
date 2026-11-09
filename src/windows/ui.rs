//! The UI thread owns every HWND, menu and icon. The engine only posts wakes.
//!
//! Window procedures use shared references and interior mutability because
//! TrackPopupMenuEx, modal dialogs and message boxes re-enter the message loop.

use super::{dialog, icon::Icon, login, run_engine, wide};
use crate::core::{
    config::{self, Edit},
    engine::{Event, Events, State},
};
use std::cell::{Cell, RefCell};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Condvar, Mutex, mpsc::Receiver};
use std::thread;
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{COLOR_BTNFACE, HBRUSH};
use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, GetSystemMetricsForDpi,
    SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, NOTIFYICONDATAW_0, NOTIFYICONIDENTIFIER,
    Shell_NotifyIconGetRect, Shell_NotifyIconW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const CLASS: &str = "Omapower.Tray";
const WAKE: u32 = WM_APP + 1;
const TRAY: u32 = WM_APP + 2;
const AUTO: u32 = 1;
const ADD: u32 = 2;
const OPEN: u32 = 3;
const LOGIN: u32 = 4;
const QUIT: u32 = 5;
const COMMAND: u32 = 1000;
// Shell's NIN_KEYSELECT macro is NIN_SELECT | NINF_KEY (not exported by windows-sys).
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;

#[derive(Default)]
struct Mailbox {
    state: State,
    errors: Vec<String>,
    exit: Option<i32>,
}
type Shared = Arc<(Mutex<Mailbox>, Condvar)>;

pub(super) fn register_class(name: &str, procedure: WNDPROC) -> Result<(), String> {
    // SAFETY: the class strings are terminated; stock brush/cursor need no cleanup.
    let status = unsafe {
        let name = wide(name);
        let class = WNDCLASSW {
            lpfnWndProc: procedure,
            hInstance: GetModuleHandleW(null()),
            lpszClassName: name.as_ptr(),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_BTNFACE + 1) as usize as HBRUSH,
            ..Default::default()
        };
        RegisterClassW(&class)
    };
    // SAFETY: GetLastError has no preconditions and is read before another API call.
    if status == 0 && unsafe { GetLastError() } != ERROR_CLASS_ALREADY_EXISTS {
        Err(format!(
            "register native window: {}",
            std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    }
}

pub(super) fn alert(owner: HWND, message: &str) {
    // SAFETY: the owner may be null and both strings live throughout the modal call.
    unsafe {
        MessageBoxW(
            owner,
            wide(message).as_ptr(),
            wide("Omapower").as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

pub fn information(message: &str) {
    // SAFETY: null is a valid owner and strings are terminated for the modal call.
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(message).as_ptr(),
            wide("Omapower").as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        )
    };
}

fn wake(hwnd: usize) -> bool {
    // SAFETY: a stale HWND simply makes PostMessage fail; no pointer data crosses threads.
    unsafe { PostMessageW(hwnd as HWND, WAKE, 0, 0) != 0 }
}

/// Avoid ending a tooltip with the first half of a UTF-16 surrogate pair.
fn tooltip(text: &str) -> [u16; 128] {
    let mut result = [0u16; 128];
    let mut length = 0;
    for character in text.chars() {
        let mut units = [0u16; 2];
        let encoded = character.encode_utf16(&mut units);
        if length + encoded.len() >= result.len() {
            break;
        }
        result[length..length + encoded.len()].copy_from_slice(encoded);
        length += encoded.len();
    }
    result
}

fn menu_text(text: &str) -> String {
    text.replace('&', "&&")
}

struct Menu(HMENU);
impl Menu {
    fn new() -> Result<Self, String> {
        // SAFETY: CreatePopupMenu has no preconditions and returns an owned menu.
        let menu = unsafe { CreatePopupMenu() };
        if menu.is_null() {
            Err(format!("create menu: {}", std::io::Error::last_os_error()))
        } else {
            Ok(Self(menu))
        }
    }

    fn item(&self, flags: u32, id: usize, label: &str) -> Result<(), String> {
        // SAFETY: the menu is owned, and the label lives through AppendMenuW's copy.
        if unsafe { AppendMenuW(self.0, flags, id, wide(&menu_text(label)).as_ptr()) } == 0 {
            Err(format!("build menu: {}", std::io::Error::last_os_error()))
        } else {
            Ok(())
        }
    }
}
impl Drop for Menu {
    fn drop(&mut self) {
        // SAFETY: this owns the menu and its attached submenus.
        unsafe { DestroyMenu(self.0) };
    }
}

struct Ui {
    hwnd: Cell<HWND>,
    shared: Shared,
    events: Events,
    icon: RefCell<Option<Icon>>,
    added: Cell<bool>,
    taskbar_created: u32,
    menu_open: Cell<bool>,
    refreshing: Cell<bool>,
    dpi: Cell<u32>,
}

impl Ui {
    fn snapshot(&self) -> State {
        self.shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .state
            .clone()
    }

    fn notification(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd.get(),
            uID: 1,
            ..Default::default()
        }
    }

    fn position_on_taskbar(&self) {
        let identifier = NOTIFYICONIDENTIFIER {
            cbSize: size_of::<NOTIFYICONIDENTIFIER>() as u32,
            hWnd: self.hwnd.get(),
            uID: 1,
            ..Default::default()
        };
        let mut rect = RECT::default();
        // SAFETY: the identifier/output are sized and live, and the hidden HWND
        // can be moved without activating or showing it. Its DPI then follows the tray.
        unsafe {
            if Shell_NotifyIconGetRect(&identifier, &mut rect) == 0 {
                SetWindowPos(
                    self.hwnd.get(),
                    null_mut(),
                    rect.left,
                    rect.top,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }

    fn refresh(&self) -> Result<(), String> {
        if self.refreshing.replace(true) {
            return Ok(());
        }
        let result = (|| {
            self.position_on_taskbar();
            let state = self.snapshot();
            // SAFETY: hwnd is our hidden top-level window; the metric uses its current DPI.
            let (dpi, size) = unsafe {
                let dpi = GetDpiForWindow(self.hwnd.get()).max(96);
                (dpi, GetSystemMetricsForDpi(SM_CXSMICON, dpi))
            };
            self.dpi.set(dpi);
            let icon = Icon::new(state.profile, size)?;
            let mut data = self.notification();
            data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP | NIF_SHOWTIP;
            data.uCallbackMessage = TRAY;
            data.hIcon = icon.handle();
            data.szTip = tooltip(&format!("Omapower — {}", state.summary()));
            // SAFETY: the sized data borrows our live icon; Shell copies the icon image.
            let success = unsafe {
                let mut result = 0;
                if self.added.get() {
                    result = Shell_NotifyIconW(NIM_MODIFY, &data);
                }
                if result == 0 {
                    result = Shell_NotifyIconW(NIM_ADD, &data);
                    if result != 0 {
                        data.Anonymous = NOTIFYICONDATAW_0 {
                            uVersion: NOTIFYICON_VERSION_4,
                        };
                        Shell_NotifyIconW(NIM_SETVERSION, &data);
                    }
                }
                result != 0
            };
            self.added.set(success);
            self.icon.replace(Some(icon));
            // Explorer may be absent briefly at login or while restarting. The
            // timer retries, and TaskbarCreated also forces immediate re-add.
            Ok(())
        })();
        self.refreshing.set(false);
        result
    }

    fn remove_icon(&self) {
        if self.added.replace(false) {
            // SAFETY: only our notification-area entry is removed.
            unsafe { Shell_NotifyIconW(NIM_DELETE, &self.notification()) };
        }
        self.icon.replace(None);
    }

    fn edit(&self, edit: Edit) {
        self.work(move || config::edit(&edit));
    }

    fn work(&self, action: impl FnOnce() -> Result<(), String> + Send + 'static) {
        let shared = self.shared.clone();
        let hwnd = self.hwnd.get() as usize;
        let result = thread::Builder::new()
            .name("windows-menu-action".into())
            .spawn(move || {
                if let Err(error) = action() {
                    shared
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .errors
                        .push(error);
                    wake(hwnd);
                }
            });
        if let Err(error) = result {
            alert(self.hwnd.get(), &format!("start menu action: {error}"));
        }
    }

    fn menu(&self, mut point: POINT) -> Result<(), String> {
        if self.menu_open.replace(true) {
            return Ok(());
        }
        let result = self.show_menu(&mut point);
        self.menu_open.set(false);
        result
    }

    fn show_menu(&self, point: &mut POINT) -> Result<(), String> {
        let state = self.snapshot();
        let menu = Menu::new()?;
        menu.item(MF_STRING | MF_GRAYED, 0, &state.summary())?;
        menu.item(MF_SEPARATOR, 0, "")?;
        menu.item(
            MF_STRING
                | if state.enabled { MF_CHECKED } else { 0 }
                | if state.config_error.is_some() {
                    MF_GRAYED
                } else {
                    0
                },
            AUTO as usize,
            "Automatic boost",
        )?;
        let commands = Menu::new()?;
        let names: Vec<String> = state.commands.iter().cloned().collect();
        for (index, name) in names.iter().enumerate() {
            commands.item(
                MF_STRING
                    | if state.active.contains(name) {
                        MF_CHECKED
                    } else {
                        0
                    },
                COMMAND as usize + index,
                name,
            )?;
        }
        if !names.is_empty() {
            commands.item(MF_SEPARATOR, 0, "")?;
        }
        commands.item(MF_STRING, ADD as usize, "Add command…")?;
        menu.item(MF_POPUP, commands.0 as usize, "Watched commands")?;
        // The parent menu now owns the submenu, including on later errors.
        std::mem::forget(commands);
        menu.item(MF_STRING, OPEN as usize, "Open configuration file")?;
        menu.item(
            MF_STRING | if login::enabled() { MF_CHECKED } else { 0 },
            LOGIN as usize,
            "Start at login",
        )?;
        menu.item(MF_SEPARATOR, 0, "")?;
        menu.item(MF_STRING, QUIT as usize, "Quit")?;
        let hwnd = self.hwnd.get();
        // SAFETY: the hidden owner is live. Foreground ownership makes clicking
        // outside dismiss the menu; WM_NULL completes the documented tray pattern.
        let selected = unsafe {
            if point.x == -1 && point.y == -1 {
                GetCursorPos(point);
            }
            SetForegroundWindow(hwnd);
            let choice = TrackPopupMenuEx(
                menu.0,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                hwnd,
                null(),
            );
            PostMessageW(hwnd, WM_NULL, 0, 0);
            choice as u32
        };
        // WM_QUIT or an engine failure can end the nested menu loop.
        if self
            .shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .exit
            .is_some()
        {
            return Ok(());
        }
        match selected {
            AUTO if state.config_error.is_none() => self.edit(Edit::Enable(!state.enabled)),
            ADD => {
                if let Some(name) = dialog::prompt(hwnd)? {
                    self.edit(Edit::Add(name));
                }
            }
            OPEN => self.work(move || open_configuration(state.enabled)),
            LOGIN => {
                let enable = !login::enabled();
                self.work(move || {
                    let result = login::set(enable);
                    match &result {
                        Ok(()) => eprintln!(
                            "omapower: {} start at login",
                            if enable { "enabled" } else { "disabled" }
                        ),
                        Err(error) => eprintln!("omapower: {error}"),
                    }
                    result
                });
            }
            QUIT => {
                let _ = self.events.send(Event::Stop);
            }
            id if id >= COMMAND => {
                if let Some(name) = names.get((id - COMMAND) as usize) {
                    // SAFETY: hwnd is live and the two strings survive the synchronous confirmation.
                    let confirmed = unsafe {
                        MessageBoxW(
                            hwnd,
                            wide(&format!("Stop watching “{name}”?")).as_ptr(),
                            wide("Omapower").as_ptr(),
                            MB_YESNO | MB_ICONQUESTION | MB_DEFBUTTON2,
                        ) == IDYES
                    };
                    if confirmed {
                        self.edit(Edit::Remove(name.clone()));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn updated(&self) {
        let (errors, exit) = {
            let mut mailbox = self
                .shared
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (std::mem::take(&mut mailbox.errors), mailbox.exit)
        };
        if let Some(status) = exit {
            self.remove_icon();
            // SAFETY: ending any nested menu and destroying our window ends the UI loop.
            unsafe {
                EndMenu();
                DestroyWindow(self.hwnd.get());
                PostQuitMessage(status);
            }
            return;
        }
        if let Err(error) = self.refresh() {
            let _ = self.events.send(Event::Fatal(error));
        }
        for error in errors {
            alert(self.hwnd.get(), &error);
        }
    }

    fn end_session(&self) {
        let _ = self.events.send(Event::Stop);
        let mailbox = self
            .shared
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Logoff has a bounded Windows deadline. Keep the UI alive while the
        // engine restores on its worker, before agreeing to session shutdown.
        let _ = self
            .shared
            .1
            .wait_timeout_while(mailbox, Duration::from_secs(4), |mailbox| {
                mailbox.exit.is_none()
            });
    }
}

fn open_configuration(enabled: bool) -> Result<(), String> {
    let file = config::config_path();
    if !file.exists() {
        config::edit(&Edit::Enable(enabled))?;
    }
    let path: Vec<u16> = file.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: this dedicated action thread initializes its apartment for shell extensions.
    let apartment = unsafe {
        CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    // SAFETY: ShellExecute accepts a null owner and terminated verb/path; no shell command is constructed.
    let status = unsafe {
        ShellExecuteW(
            null_mut(),
            wide("open").as_ptr(),
            path.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    if apartment >= 0 {
        // SAFETY: balance this thread's successful CoInitializeEx, including S_FALSE.
        unsafe { CoUninitialize() };
    }
    if status > 32 {
        Ok(())
    } else {
        Err(format!(
            "open configuration file: Windows shell error {status}"
        ))
    }
}

/// Only inspect the setting name during WM_SETTINGCHANGE, when Windows owns it.
unsafe fn immersive_colour(lparam: LPARAM) -> bool {
    if lparam == 0 {
        return false;
    }
    let expected = wide("ImmersiveColorSet");
    // SAFETY: WM_SETTINGCHANGE lParam is either null or a terminated setting string.
    // Compare only the fixed expected length, stopping at the first mismatch/NUL.
    unsafe {
        for (index, character) in expected.iter().enumerate() {
            if *((lparam as *const u16).add(index)) != *character {
                return false;
            }
        }
    }
    true
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        // SAFETY: Windows supplies CREATESTRUCTW with our boxed Ui as creation data.
        unsafe {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    // SAFETY: user data is null before WM_NCCREATE and cleared before the box is dropped.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Ui;
    if !pointer.is_null() {
        // SAFETY: run holds the Ui allocation until the window and nested loops finish.
        let ui = unsafe { &*pointer };
        if message == WM_NCCREATE {
            ui.hwnd.set(hwnd);
        }
        if message == ui.taskbar_created && ui.taskbar_created != 0 {
            ui.added.set(false);
            if let Err(error) = ui.refresh() {
                let _ = ui.events.send(Event::Fatal(error));
            }
            return 0;
        }
        match message {
            WAKE => {
                ui.updated();
                return 0;
            }
            TRAY => {
                let notification = (lparam as u32) & 0xffff;
                if [NIN_SELECT, NIN_KEYSELECT, WM_CONTEXTMENU].contains(&notification) {
                    let point = POINT {
                        x: (wparam as u16 as i16) as i32,
                        y: ((wparam >> 16) as u16 as i16) as i32,
                    };
                    if let Err(error) = ui.menu(point) {
                        alert(hwnd, &error);
                    }
                }
                return 0;
            }
            WM_SETTINGCHANGE => {
                // SAFETY: this is exactly the message for which lParam is a setting string.
                if unsafe { immersive_colour(lparam) } {
                    if let Err(error) = ui.refresh() {
                        let _ = ui.events.send(Event::Fatal(error));
                    }
                }
                return 0;
            }
            WM_DPICHANGED | WM_DISPLAYCHANGE => {
                if let Err(error) = ui.refresh() {
                    let _ = ui.events.send(Event::Fatal(error));
                }
                return 0;
            }
            WM_TIMER => {
                if ui
                    .shared
                    .0
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .exit
                    .is_some()
                {
                    ui.updated();
                    return 0;
                }
                ui.position_on_taskbar();
                // SAFETY: hwnd is our live window.
                if !ui.added.get() || unsafe { GetDpiForWindow(hwnd) }.max(96) != ui.dpi.get() {
                    if let Err(error) = ui.refresh() {
                        let _ = ui.events.send(Event::Fatal(error));
                    }
                }
                return 0;
            }
            WM_CLOSE => {
                let _ = ui.events.send(Event::Stop);
                return 0;
            }
            WM_QUERYENDSESSION => {
                ui.end_session();
                return 1;
            }
            WM_ENDSESSION if wparam != 0 => {
                ui.end_session();
                return 0;
            }
            WM_DESTROY => {
                ui.remove_icon();
                // SAFETY: this window owns timer 1; WM_QUIT belongs to its UI thread.
                unsafe {
                    KillTimer(hwnd, 1);
                    PostQuitMessage(0);
                }
                return 0;
            }
            WM_NCDESTROY => {
                // SAFETY: disconnect the allocation from the HWND before it can be dropped.
                unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) };
            }
            _ => {}
        }
    }
    // SAFETY: delegate unhandled messages to Windows' standard procedure.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

pub fn run(events: Events, receive: Receiver<Event>) -> i32 {
    // SAFETY: the manifest normally set this already; an already-set context
    // simply rejects the call. This also covers launchers that omit manifests.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let shared = Arc::new((Mutex::new(Mailbox::default()), Condvar::new()));
    // SAFETY: the message string is terminated and RegisterWindowMessage copies it.
    let taskbar_created = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
    if let Err(error) = register_class(CLASS, Some(procedure)) {
        alert(null_mut(), &error);
        return 1;
    }
    let ui = Box::new(Ui {
        hwnd: Cell::new(null_mut()),
        shared: shared.clone(),
        events: events.clone(),
        icon: RefCell::new(None),
        added: Cell::new(false),
        taskbar_created,
        menu_open: Cell::new(false),
        refreshing: Cell::new(false),
        dpi: Cell::new(96),
    });
    // A hidden top-level window receives Explorer/theme broadcasts; a
    // message-only HWND would miss TaskbarCreated after Explorer restarts.
    // SAFETY: the Ui allocation stays live until after window destruction.
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            wide(CLASS).as_ptr(),
            wide("Omapower").as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            (&*ui as *const Ui).cast(),
        )
    };
    if hwnd.is_null() {
        alert(
            null_mut(),
            &format!("create tray window: {}", std::io::Error::last_os_error()),
        );
        return 1;
    }
    ui.hwnd.set(hwnd);
    // SAFETY: timer 1 belongs to our hidden window and dispatches WM_TIMER on this thread.
    unsafe { SetTimer(hwnd, 1, 2000, None) };
    if let Err(error) = ui.refresh() {
        alert(hwnd, &error);
        // SAFETY: no engine is running yet; destroying this owned window is sufficient cleanup.
        unsafe { DestroyWindow(hwnd) };
        return 1;
    }
    let address = hwnd as usize;
    let engine_shared = shared.clone();
    let worker = thread::Builder::new()
        .name("omapower-engine".into())
        .spawn(move || {
            let frontend_shared = engine_shared.clone();
            // Construct Engine on this thread: it intentionally is not Send.
            let frontend = Box::new(move |state: &State| {
                frontend_shared
                    .0
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .state = state.clone();
                wake(address)
            });
            let status = run_engine(events, receive, frontend);
            engine_shared
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .exit = Some(status);
            engine_shared.1.notify_all();
            wake(address);
            status
        });
    let worker = match worker {
        Ok(worker) => worker,
        Err(error) => {
            alert(hwnd, &format!("start engine: {error}"));
            // SAFETY: no engine started, so our owned window can be destroyed immediately.
            unsafe { DestroyWindow(hwnd) };
            return 1;
        }
    };
    loop {
        let mut message = MSG::default();
        // SAFETY: the UI thread owns this writable MSG and all its windows.
        let result = unsafe { GetMessageW(&mut message, null_mut(), 0, 0) };
        if result <= 0 {
            if result < 0 {
                alert(
                    hwnd,
                    &format!("tray message loop: {}", std::io::Error::last_os_error()),
                );
            }
            let _ = ui.events.send(Event::Stop);
            break;
        }
        // SAFETY: GetMessage initialized the message for this thread's window procedures.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    let status = worker.join().unwrap_or(1);
    ui.remove_icon();
    // SAFETY: DestroyWindow safely fails if the exit wake already destroyed this HWND.
    unsafe { DestroyWindow(hwnd) };
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_is_terminated_without_splitting_surrogates() {
        let tip = tooltip(&format!("{}😀", "x".repeat(126)));
        assert_eq!(tip[126], 0);
        assert!(String::from_utf16(&tip[..126]).is_ok());
        assert_eq!(tooltip("Omapower — Waiting")[18], 0);
        assert_eq!(menu_text("cc&link"), "cc&&link");
    }
}
