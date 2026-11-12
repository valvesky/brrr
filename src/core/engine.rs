//! The platform-independent state machine: while an enabled, watched command
//! runs, hold the performance profile; release it when the last one exits.
//!
//! Everything arrives as an [`Event`] on one channel, so the engine runs on a
//! single thread. Platform code supplies a [`Power`] backend (which answers
//! with events) and a [`Processes`] watcher (which reports matches as events).

use super::config::{self, json_names};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

/// The effective power profile, for display only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Profile {
    Performance,
    Balanced,
    PowerSaver,
    #[default]
    Unknown,
}

impl Profile {
    pub fn from_name(name: &str) -> Profile {
        match name {
            "performance" => Profile::Performance,
            "balanced" => Profile::Balanced,
            "power-saver" => Profile::PowerSaver,
            _ => Profile::Unknown,
        }
    }
}

#[derive(Debug)]
pub enum AcquireError {
    /// The backend answered "no" (e.g. no performance profile): no hold exists.
    Rejected(String),
    /// It is unknown whether a hold was created. The engine exits so the
    /// platform drops anything the process owns.
    Ambiguous(String),
}

#[derive(Debug)]
pub enum Event {
    /// The configuration file (or its directory) changed.
    ConfigTouched,
    /// The set of running watched commands, and how they are detected.
    Processes { active: BTreeSet<String>, mode: &'static str },
    /// The power backend appeared (with an identity) or vanished.
    Owner(Option<String>),
    /// Why the backend is unavailable, for display (e.g. setup needed).
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    Unavailable(Option<String>),
    Acquired { generation: u64, result: Result<u32, AcquireError> },
    ReleaseDone { generation: u64, result: Result<(), String> },
    /// Someone else released our hold (a manual profile change).
    ExternalRelease { owner: String, cookie: u32 },
    Profile(Profile),
    /// An unrecoverable error: exit with status 1.
    Fatal(String),
    /// Exit normally (signal, supervisor gone, Quit).
    Stop,
}

/// A power backend. Calls are asynchronous: each answers with an
/// [`Event::Acquired`] or [`Event::ReleaseDone`] carrying the same generation.
pub trait Power {
    /// The name used in diagnostics ("power-profiles", "windows", "pmset").
    fn name(&self) -> &'static str;
    fn acquire(&mut self, owner: &str, generation: u64);
    fn release(&mut self, owner: &str, cookie: u32, generation: u64);
    /// Called once before exit. Backends whose holds do not vanish with the
    /// process restore the previous profile here, synchronously.
    fn shutdown(&mut self) {}
}

/// A process watcher. Matches are reported as [`Event::Processes`].
pub trait Processes {
    fn set_commands(&mut self, commands: &BTreeSet<String>);
}

/// What frontends show. `to_json` is the line protocol the Omarchy widget reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub enabled: bool,
    pub config_error: Option<String>,
    pub commands: BTreeSet<String>,
    pub active: BTreeSet<String>,
    pub holding: bool,
    pub suppressed: bool,
    pub rejected: bool,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub monitor: String,
    pub profile: Profile,
}

impl State {
    pub fn to_json(&self) -> String {
        let bool = |value: bool| if value { "true" } else { "false" };
        format!(
            "{{\"enabled\":{},\"configError\":{},\"commands\":{},\"active\":{},\"holding\":{},\
             \"suppressed\":{},\"rejected\":{},\"available\":{},\"monitor\":{}}}",
            bool(self.enabled),
            bool(self.config_error.is_some()),
            json_names(&self.commands),
            json_names(&self.active),
            bool(self.holding),
            bool(self.suppressed),
            bool(self.rejected),
            bool(self.available),
            serde_json::to_string(&self.monitor).expect("strings always encode"),
        )
    }

    /// One line for tooltips and menus.
    pub fn summary(&self) -> String {
        let active = || self.active.iter().cloned().collect::<Vec<_>>().join(", ");
        if self.config_error.is_some() {
            "The configuration file has an error".into()
        } else if !self.enabled {
            "Automatic boost is off".into()
        } else if !self.available {
            self.unavailable_reason.clone().unwrap_or_else(|| "No power profile service".into())
        } else if self.holding {
            format!("Boosting for {}", active())
        } else if self.rejected {
            "The performance profile isn't available".into()
        } else if self.suppressed {
            format!("Paused after a manual change until {} finishes", active())
        } else {
            "Idle — waiting for a heavy app".into()
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    Idle,
    Acquiring,
    Releasing,
}

/// Receives every state change. Returning false means the frontend is gone
/// (e.g. the supervisor closed stdout): the engine exits with status 1.
pub type Frontend = Box<dyn FnMut(&State) -> bool>;

pub struct Engine {
    power: Box<dyn Power>,
    processes: Box<dyn Processes>,
    frontend: Frontend,
    config_path: PathBuf,
    on_config_reload: Option<Box<dyn FnMut()>>,
    config_reload_at: Option<Instant>,
    state: State,
    last_status: String,
    last_state: Option<State>,
    owner: Option<String>,
    generation: u64,
    cookie: u32,
    released_cookie: Option<u32>,
    pending: Pending,
    release_failures: u32,
    retry_at: Option<Instant>,
    exit: Option<i32>,
}

impl Engine {
    pub fn new(
        power: Box<dyn Power>,
        processes: Box<dyn Processes>,
        config_path: PathBuf,
        frontend: Frontend,
    ) -> Engine {
        let mut engine = Engine {
            power,
            processes,
            frontend,
            config_path,
            on_config_reload: None,
            config_reload_at: None,
            state: State { monitor: "connecting".into(), ..State::default() },
            last_status: String::new(),
            last_state: None,
            owner: None,
            generation: 0,
            cookie: 0,
            released_cookie: None,
            pending: Pending::Idle,
            release_failures: 0,
            retry_at: None,
            exit: None,
        };
        engine.read_configuration();
        engine
    }

    /// Run before each configuration reload, e.g. to re-arm file watches after
    /// the directory was replaced.
    pub fn on_config_reload(&mut self, callback: Box<dyn FnMut()>) {
        self.on_config_reload = Some(callback);
    }

    /// Process events until told to stop. Returns the exit status.
    pub fn run(mut self, events: Receiver<Event>) -> i32 {
        let commands = self.state.commands.clone();
        self.processes.set_commands(&commands);
        self.reconcile();
        while self.exit.is_none() {
            let deadline = [self.retry_at, self.config_reload_at].into_iter().flatten().min();
            let event = match deadline {
                Some(deadline) => events.recv_timeout(deadline.saturating_duration_since(Instant::now())),
                None => events.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match event {
                Ok(event) => self.handle(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => self.exit = Some(0),
            }
            let now = Instant::now();
            if self.config_reload_at.is_some_and(|at| at <= now) {
                self.config_reload_at = None;
                self.config_changed();
            }
            if self.retry_at.is_some_and(|at| at <= now) {
                self.retry_at = None;
            }
            if self.exit.is_none() {
                self.reconcile();
            }
        }
        self.power.shutdown();
        self.exit.unwrap_or(0)
    }

    fn fail(&mut self, message: &str) {
        eprintln!("brrr: {message}");
        if self.exit.is_none() {
            self.exit = Some(1);
        }
    }

    fn running(&self) -> bool {
        !self.state.active.is_empty()
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::ConfigTouched => {
                // Editors and atomic replacements produce bursts of events.
                self.config_reload_at = Some(Instant::now() + Duration::from_millis(100));
            }
            Event::Processes { active, mode } => {
                // Names removed from the configuration stop counting at once;
                // the watcher catches up on its own.
                self.state.active = active.intersection(&self.state.commands).cloned().collect();
                self.state.monitor = mode.into();
            }
            Event::Owner(owner) => self.owner_changed(owner),
            Event::Unavailable(reason) => self.state.unavailable_reason = reason,
            // Answers from an older generation were abandoned when the owner
            // changed (which also reset `pending`); they must not touch a call
            // made to the new owner.
            Event::Acquired { generation, result } if generation == self.generation => {
                self.pending = Pending::Idle;
                self.acquired(result);
            }
            Event::ReleaseDone { generation, result } if generation == self.generation => {
                self.pending = Pending::Idle;
                self.release_done(result);
            }
            Event::Acquired { .. } | Event::ReleaseDone { .. } => {}
            Event::ExternalRelease { owner, cookie } => self.external_release(&owner, cookie),
            Event::Profile(profile) => self.state.profile = profile,
            Event::Fatal(message) => self.fail(&message),
            Event::Stop => {
                if self.exit.is_none() {
                    self.exit = Some(0);
                }
            }
        }
    }

    fn acquired(&mut self, result: Result<u32, AcquireError>) {
        match result {
            Ok(cookie) => {
                self.cookie = cookie;
                self.state.holding = self.released_cookie != Some(cookie);
                eprintln!(
                    "brrr: hold acquired: cookie={cookie} retained={}",
                    if self.state.holding { "yes" } else { "no (already released by daemon)" }
                );
                if !self.state.holding {
                    self.cookie = 0;
                    self.state.suppressed = self.running();
                }
            }
            Err(AcquireError::Rejected(message)) => {
                // Don't retry or restart in a loop: wait until the matching
                // commands have all exited.
                eprintln!("brrr: performance hold rejected: {message}");
                self.state.rejected = true;
            }
            Err(AcquireError::Ambiguous(message)) => {
                // A timed-out acquisition may still have succeeded remotely.
                // Never retry it and leak another hold: exit instead.
                self.fail(&message);
            }
        }
    }

    fn release_done(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => {
                eprintln!("brrr: hold release confirmed");
                self.state.holding = false;
                self.cookie = 0;
                self.release_failures = 0;
            }
            Err(message) if self.state.holding => {
                // A failed release is not a release. Keep the cookie and retry;
                // after three failures exit, which drops the hold.
                eprintln!("brrr: release: {message}");
                self.release_failures += 1;
                if self.release_failures >= 3 {
                    self.fail("release failed three times; disconnecting to drop the hold");
                } else {
                    self.retry_at = Some(Instant::now() + Duration::from_secs(self.release_failures.into()));
                }
            }
            Err(_) => {}
        }
    }

    fn owner_changed(&mut self, owner: Option<String>) {
        if self.owner == owner {
            return;
        }
        eprintln!(
            "brrr: {} owner: {} -> {}",
            self.power.name(),
            self.owner.as_deref().unwrap_or("unavailable"),
            owner.as_deref().unwrap_or("unavailable")
        );
        self.owner = owner;
        self.state.available = self.owner.is_some();
        self.generation += 1;
        self.state.holding = false;
        self.state.rejected = false;
        self.cookie = 0;
        self.release_failures = 0;
        self.retry_at = None;
        // A call in flight belongs to the old generation; its answer is ignored.
        self.pending = Pending::Idle;
    }

    fn external_release(&mut self, owner: &str, cookie: u32) {
        if self.owner.as_deref() != Some(owner) {
            return;
        }
        let pending = match self.pending {
            Pending::Acquiring => "acquire",
            Pending::Releasing => "release",
            Pending::Idle => "none",
        };
        eprintln!("brrr: ProfileReleased: cookie={cookie} held-cookie={} pending={pending}", self.cookie);
        if self.pending == Pending::Acquiring {
            self.released_cookie = Some(cookie);
        }
        if self.state.holding && self.cookie == cookie {
            self.state.holding = false;
            self.cookie = 0;
            if self.pending != Pending::Releasing {
                self.state.suppressed = self.running();
            }
            self.retry_at = None;
        }
    }

    /// An invalid configuration watches nothing (dropping any hold) and is
    /// reported, so it can be fixed without a restart.
    fn read_configuration(&mut self) {
        match config::load(&self.config_path) {
            Ok(config) => {
                self.state.config_error = None;
                self.state.enabled = config.enabled;
                self.state.commands = config.commands;
            }
            Err(error) => {
                eprintln!(
                    "brrr: {}: {error}; watching nothing until it is fixed",
                    self.config_path.display()
                );
                self.state.config_error = Some(error);
                self.state.enabled = false;
                self.state.commands.clear();
            }
        }
    }

    fn config_changed(&mut self) {
        if let Some(callback) = self.on_config_reload.as_mut() {
            callback();
        }
        self.read_configuration();
        let commands = self.state.commands.clone();
        self.state.active = self.state.active.intersection(&commands).cloned().collect();
        self.processes.set_commands(&commands);
    }

    fn reconcile(&mut self) {
        let running = self.running();
        if !running {
            self.state.suppressed = false;
            self.state.rejected = false;
        }
        let wanted = self.state.enabled && running && !self.state.suppressed && !self.state.rejected;
        if wanted && self.state.holding && self.retry_at.is_some() {
            self.retry_at = None;
            self.release_failures = 0;
        }
        if self.pending == Pending::Idle && self.retry_at.is_none() {
            if let Some(owner) = self.owner.clone() {
                if wanted && !self.state.holding {
                    self.start(Pending::Acquiring, &owner);
                } else if !wanted && self.state.holding {
                    self.start(Pending::Releasing, &owner);
                }
            }
        }
        self.report();
    }

    fn start(&mut self, operation: Pending, owner: &str) {
        let acquiring = operation == Pending::Acquiring;
        eprintln!(
            "brrr: {}: owner={owner} cookie={}",
            if acquiring { "requesting performance hold" } else { "releasing performance hold" },
            self.cookie
        );
        self.pending = operation;
        if acquiring {
            self.released_cookie = None;
            self.release_failures = 0;
            self.power.acquire(owner, self.generation);
        } else {
            self.power.release(owner, self.cookie, self.generation);
        }
    }

    /// Log and publish once per change.
    fn report(&mut self) {
        let json = self.state.to_json();
        if json != self.last_status {
            eprintln!("brrr: state {json}");
            self.last_status = json;
        }
        if self.last_state.as_ref() != Some(&self.state) {
            self.last_state = Some(self.state.clone());
            if !(self.frontend)(&self.state) {
                self.fail("status output closed");
            }
        }
    }
}

/// A Sender that platform threads clone to post events.
pub type Events = Sender<Event>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_summary_names_a_heavy_app() {
        let state = State { enabled: true, available: true, ..State::default() };
        assert_eq!(state.summary(), "Idle — waiting for a heavy app");
    }
}
