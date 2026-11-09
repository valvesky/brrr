//! Native macOS process monitoring, power switching and menu bar frontend.

mod power;
mod processes;
mod setup;
mod ui;

use crate::core::{
    self,
    config_watch::ConfigWatch,
    engine::{Engine, Event, Events, Frontend},
};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};

pub const UI_BY_DEFAULT: bool = true;

pub fn prepare_console() {}

pub fn already_running(_ui: bool) {}

pub fn install() -> i32 {
    setup::report(setup::install())
}

pub fn uninstall() -> i32 {
    setup::report(setup::uninstall())
}

/// The signal iterator uses its own wakeable pipe; closing it also joins the
/// listener, so library-style returns do not leave a signal thread behind.
struct Signals {
    handle: signal_hook::iterator::Handle,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Signals {
    fn new(events: Events) -> Result<Self, String> {
        let mut signals = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
        ])
        .map_err(|error| format!("watch signals: {error}"))?;
        let handle = signals.handle();
        let thread = std::thread::Builder::new()
            .name("omapower-signals".into())
            .spawn(move || {
                for _ in signals.forever() {
                    if events.send(Event::Stop).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| format!("start signal listener: {error}"))?;
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn run(supervised: bool, _poll: bool, ui: bool) -> i32 {
    let (tx, rx) = mpsc::channel();
    let _signals = match Signals::new(tx.clone()) {
        Ok(signals) => signals,
        Err(error) => return setup::report(Err(error)),
    };
    let power = match power::Power::new(tx.clone()) {
        Ok(power) => power,
        Err(error) => return setup::report(Err(error)),
    };
    let processes = match processes::Processes::new(tx.clone()) {
        Ok(processes) => processes,
        Err(error) => return setup::report(Err(error)),
    };
    if ui {
        ui::run(tx, rx, power, processes)
    } else {
        let frontend = if supervised {
            core::watch_stdin(tx.clone());
            core::supervised_frontend()
        } else {
            Box::new(|_: &core::engine::State| true) as Frontend
        };
        run_engine(tx, rx, power, processes, frontend)
    }
}

/// Construct the non-Send engine on the thread which will actually run it.
fn run_engine(
    events: Events,
    receiver: Receiver<Event>,
    power: power::Power,
    processes: processes::Processes,
    frontend: Frontend,
) -> i32 {
    let path = core::config::config_path();
    let mut watch = match ConfigWatch::new(path.clone(), events) {
        Ok(watch) => watch,
        Err(error) => return setup::report(Err(error)),
    };
    let mut engine = Engine::new(Box::new(power), Box::new(processes), path, frontend);
    engine.on_config_reload(Box::new(move || watch.rearm()));
    engine.run(receiver)
}

/// Both the launch agent and the recovery journal must survive an interrupted
/// write. Create a private file, sync it, then rename it within its directory.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = path.parent().ok_or("file has no parent directory")?;
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let temporary = directory.join(format!(
        ".omapower-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        std::fs::File::open(directory)?.sync_all()
    })()
    .map_err(|error: std::io::Error| format!("{}: {error}", path.display()));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
