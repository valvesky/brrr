//! Windows has native process, power and notification-area implementations.

mod dialog;
mod icon;
mod login;
mod power;
mod processes;
mod ui;

use crate::core::{
    self, config,
    config_watch::ConfigWatch,
    engine::{Engine, Event, Events, Frontend},
};
use std::sync::{
    Condvar, Mutex,
    mpsc::{self, Receiver},
};
use std::time::Duration;
use windows_sys::Win32::System::Console::{
    ATTACH_PARENT_PROCESS, AttachConsole, CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT,
    CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
};

pub const UI_BY_DEFAULT: bool = true;

pub(super) fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

pub fn prepare_console() {
    // SAFETY: attaching to our parent is optional, and requires no owned handle.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

pub fn install() -> i32 {
    login::change(true)
}
pub fn uninstall() -> i32 {
    login::change(false)
}

pub fn already_running(ui: bool) {
    if ui {
        self::ui::information("brrr is already running");
    }
}

struct ConsoleState {
    events: Option<Events>,
    finished: bool,
}
static CONSOLE: (Mutex<ConsoleState>, Condvar) = (
    Mutex::new(ConsoleState {
        events: None,
        finished: false,
    }),
    Condvar::new(),
);

unsafe extern "system" fn console_control(control: u32) -> i32 {
    if ![
        CTRL_C_EVENT,
        CTRL_BREAK_EVENT,
        CTRL_CLOSE_EVENT,
        CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT,
    ]
    .contains(&control)
    {
        return 0;
    }
    let Ok(state) = CONSOLE.0.lock() else {
        return 0;
    };
    let Some(events) = &state.events else {
        return 0;
    };
    let _ = events.send(Event::Stop);
    if matches!(
        control,
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
    ) {
        // Returning from a close handler terminates the process. Give the
        // engine time to synchronously restore before Windows' own deadline.
        let _ = CONSOLE
            .1
            .wait_timeout_while(state, Duration::from_secs(4), |state| !state.finished);
    }
    1
}

fn finished() {
    if let Ok(mut state) = CONSOLE.0.lock() {
        state.finished = true;
        CONSOLE.1.notify_all();
    }
}

pub(super) fn run_engine(tx: Events, rx: Receiver<Event>, frontend: Frontend) -> i32 {
    let result = (|| -> Result<i32, String> {
        let path = config::config_path();
        let power = power::PowerBackend::new(tx.clone())?;
        let processes = processes::ProcessWatch::new(tx.clone())?;
        let mut watch = ConfigWatch::new(path.clone(), tx)?;
        let mut engine = Engine::new(Box::new(power), Box::new(processes), path, frontend);
        engine.on_config_reload(Box::new(move || watch.rearm()));
        Ok(engine.run(rx))
    })();
    let status = match result {
        Ok(status) => status,
        Err(error) => {
            eprintln!("brrr: {error}");
            1
        }
    };
    finished();
    status
}

pub fn run(supervised: bool, _poll: bool, ui: bool) -> i32 {
    login::migrate();
    let (tx, rx) = mpsc::channel();
    if let Ok(mut state) = CONSOLE.0.lock() {
        state.events = Some(tx.clone());
        state.finished = false;
    }
    // SAFETY: the static handler and its synchronized state outlive registration.
    unsafe { SetConsoleCtrlHandler(Some(console_control), 1) };
    let status = if ui && !supervised {
        self::ui::run(tx, rx)
    } else {
        let frontend = if supervised {
            core::watch_stdin(tx.clone());
            core::supervised_frontend()
        } else {
            Box::new(|_: &core::engine::State| true) as Frontend
        };
        run_engine(tx, rx, frontend)
    };
    finished();
    // SAFETY: remove the same static handler before clearing its event sender.
    unsafe { SetConsoleCtrlHandler(Some(console_control), 0) };
    if let Ok(mut state) = CONSOLE.0.lock() {
        state.events = None;
    }
    status
}
