//! Linux: kernel process events, power-profiles-daemon, and either a
//! StatusNotifierItem tray icon, the Omarchy shell plugin (`--supervised`) or
//! nothing at all (a systemd user service).

mod ppd;
mod process;
mod tray;

use crate::core::config_watch::ConfigWatch;
use crate::core::engine::{Engine, Event, Frontend};
use crate::core::{self, config};
use std::sync::mpsc;

/// Without arguments the helper runs headless, as systemd units expect.
pub const UI_BY_DEFAULT: bool = false;

pub fn prepare_console() {}

pub fn already_running(_ui: bool) {}

pub fn install() -> i32 {
    match tray::set_autostart(true) {
        Ok(()) => {
            eprintln!("brrr: the tray icon starts at login ({})", tray::autostart_path().display());
            0
        }
        Err(error) => {
            eprintln!("brrr: {error}");
            1
        }
    }
}

pub fn uninstall() -> i32 {
    match tray::set_autostart(false) {
        Ok(()) => {
            eprintln!("brrr: removed the login item");
            0
        }
        Err(error) => {
            eprintln!("brrr: {error}");
            1
        }
    }
}

pub fn run(supervised: bool, poll: bool, ui: bool) -> i32 {
    tray::migrate_autostart();
    let (events, receiver) = mpsc::channel();
    match signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT]) {
        Ok(mut signals) => {
            let events = events.clone();
            std::thread::spawn(move || {
                if signals.forever().next().is_some() {
                    let _ = events.send(Event::Stop);
                }
            });
        }
        Err(error) => {
            eprintln!("brrr: signals: {error}");
            return 1;
        }
    }
    if supervised {
        core::watch_stdin(events.clone());
    }
    let path = config::config_path();
    let started = (|| {
        let watch = ConfigWatch::new(path.clone(), events.clone())?;
        let processes = process::ProcessWatch::new(poll, events.clone())?;
        let power = ppd::PowerProfiles::new(events.clone())?;
        Ok::<_, String>((watch, processes, power))
    })();
    let (mut watch, processes, power) = match started {
        Ok(parts) => parts,
        Err(error) => {
            eprintln!("brrr: {error}");
            return 1;
        }
    };
    let frontend: Frontend = if supervised {
        core::supervised_frontend()
    } else if ui {
        use ksni::blocking::TrayMethods;
        match tray::Tray::new(events.clone()).assume_sni_available(true).spawn() {
            Ok(handle) => Box::new(move |state| {
                let state = state.clone();
                handle.update(move |tray| tray.state = state);
                true
            }),
            Err(error) => {
                eprintln!("brrr: tray: {error}");
                return 1;
            }
        }
    } else {
        Box::new(|_| true)
    };
    let mut engine = Engine::new(Box::new(power), Box::new(processes), path, frontend);
    engine.on_config_reload(Box::new(move || watch.rearm()));
    engine.run(receiver)
}
