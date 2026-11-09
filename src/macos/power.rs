//! A serial pmset worker. Changes are journalled before execution because
//! pmset settings outlive this process, including an abrupt process death.

use super::setup;
use crate::core::{
    config,
    engine::{self, AcquireError, Event, Events, Profile},
};
use block2::RcBlock;
use objc2::rc::autoreleasepool;
use objc2_foundation::{NSNotificationCenter, NSProcessInfoPowerStateDidChangeNotification};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const OWNER: &str = "pmset";
const SETUP_REASON: &str = "Allow Omapower to switch power modes from its menu";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Battery,
    Ac,
}

impl Source {
    fn flag(self) -> &'static str {
        match self {
            Self::Battery => "-b",
            Self::Ac => "-c",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    LowPower,
    Power,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Self::LowPower => "lowpowermode",
            Self::Power => "powermode",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Settings {
    low_power: Option<u8>,
    power: Option<u8>,
}

impl Settings {
    fn value(self, key: Key) -> Option<u8> {
        match key {
            Key::LowPower => self.low_power,
            Key::Power => self.power,
        }
    }

    fn profile(self) -> Profile {
        let name = if self.low_power == Some(1) || self.power == Some(1) {
            "power-saver"
        } else if self.power == Some(2) {
            "performance"
        } else if self.low_power.is_some() || self.power.is_some() {
            "balanced"
        } else {
            "unknown"
        };
        Profile::from_name(name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Change {
    source: Source,
    key: Key,
    previous: u8,
    ours: u8,
}

fn source(text: &str) -> Result<Source, String> {
    let first = text.lines().next().unwrap_or_default();
    if first.contains("AC Power") {
        Ok(Source::Ac)
    } else if first.contains("Battery Power") {
        Ok(Source::Battery)
    } else {
        Err("pmset did not report AC Power or Battery Power".into())
    }
}

fn settings(text: &str) -> Settings {
    let mut result = Settings::default();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (slot, maximum) = match words.next() {
            Some("lowpowermode") => (&mut result.low_power, 1),
            Some("powermode") => (&mut result.power, 2),
            _ => continue,
        };
        *slot = words
            .next()
            .and_then(|word| word.parse::<u8>().ok())
            .filter(|value| *value <= maximum);
    }
    result
}

/// `pmset -g` describes only the active source. Recovery must also inspect
/// the saved source when the machine was plugged in or unplugged meanwhile.
fn custom_settings(text: &str, wanted: Source) -> Result<Settings, String> {
    let mut selected = false;
    let mut lines = String::new();
    for line in text.lines() {
        match line.trim() {
            "Battery Power:" => selected = wanted == Source::Battery,
            "AC Power:" => selected = wanted == Source::Ac,
            heading if heading.ends_with(':') => selected = false,
            _ if selected => {
                lines.push_str(line);
                lines.push('\n');
            }
            _ => {}
        }
    }
    let result = settings(&lines);
    if result == Settings::default() {
        Err(format!(
            "pmset did not report saved settings for {}",
            wanted.flag()
        ))
    } else {
        Ok(result)
    }
}

/// Preserve every successful transition, including powermode 1 -> 0 -> 2.
/// The high-power transition is optional when the hardware rejects it.
fn plan(source: Source, values: Settings) -> Vec<Change> {
    let mut changes = Vec::new();
    let mut power = values.power;
    if values.low_power == Some(1) {
        changes.push(Change {
            source,
            key: Key::LowPower,
            previous: 1,
            ours: 0,
        });
    } else if power == Some(1) {
        changes.push(Change {
            source,
            key: Key::Power,
            previous: 1,
            ours: 0,
        });
        power = Some(0);
    }
    if let Some(previous) = power.filter(|value| *value != 2) {
        changes.push(Change {
            source,
            key: Key::Power,
            previous,
            ours: 2,
        });
    }
    changes
}

fn read_current() -> Result<(Source, Settings), String> {
    let source = source(&setup::output(
        Command::new("/usr/bin/pmset")
            .args(["-g", "batt"])
            .env("LC_ALL", "C"),
    )?)?;
    let values = setup::output(Command::new("/usr/bin/pmset").arg("-g").env("LC_ALL", "C"))?;
    Ok((source, settings(&values)))
}

fn read_saved(source: Source) -> Result<Settings, String> {
    let values = setup::output(
        Command::new("/usr/bin/pmset")
            .args(["-g", "custom"])
            .env("LC_ALL", "C"),
    )?;
    custom_settings(&values, source)
}

/// All privileged arguments come from these closed enums and literals.
/// Neither configuration text nor journal strings can become command arguments.
fn set(source: Source, key: Key, value: u8) -> Result<(), String> {
    let value = match (key, value) {
        (_, 0) => "0",
        (_, 1) => "1",
        (Key::Power, 2) => "2",
        _ => return Err("invalid pmset value".into()),
    };
    setup::output(
        Command::new("/usr/bin/sudo")
            .args(["-n", "/usr/bin/pmset", source.flag(), key.name(), value])
            .env("LC_ALL", "C"),
    )
    .map(|_| ())
}

fn permitted() -> bool {
    Command::new("/usr/bin/sudo")
        .args(["-n", "-l", "/usr/bin/pmset", "-b", "lowpowermode", "0"])
        .env("LC_ALL", "C")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn encode(changes: &[Change]) -> Value {
    json!({"version": 1, "changes": changes.iter().map(|change| json!({
        "source": change.source.flag(), "key": change.key.name(),
        "previous": change.previous, "ours": change.ours,
    })).collect::<Vec<_>>()})
}

fn decode(bytes: &[u8]) -> Result<Vec<Change>, String> {
    let document: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("invalid power recovery file: {error}"))?;
    let invalid = || "invalid power recovery file".to_string();
    if document.get("version").and_then(Value::as_u64) != Some(1) {
        return Err(invalid());
    }
    let entries = document
        .get("changes")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    if entries.len() > 4 {
        return Err(invalid());
    }
    entries
        .iter()
        .map(|entry| {
            let source = match entry.get("source").and_then(Value::as_str) {
                Some("-b") => Source::Battery,
                Some("-c") => Source::Ac,
                _ => return Err(invalid()),
            };
            let key = match entry.get("key").and_then(Value::as_str) {
                Some("lowpowermode") => Key::LowPower,
                Some("powermode") => Key::Power,
                _ => return Err(invalid()),
            };
            let value = |name| {
                entry
                    .get(name)
                    .and_then(Value::as_u64)
                    .filter(|value| *value <= if key == Key::Power { 2 } else { 1 })
                    .map(|value| value as u8)
                    .ok_or_else(invalid)
            };
            Ok(Change {
                source,
                key,
                previous: value("previous")?,
                ours: value("ours")?,
            })
        })
        .collect()
}

fn save_journal(path: &Path, changes: &[Change]) -> Result<(), String> {
    if changes.is_empty() {
        match fs::remove_file(path) {
            Ok(()) => fs::File::open(path.parent().ok_or("recovery file has no parent")?)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("sync power recovery directory: {error}")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("remove power recovery file: {error}")),
        }
    } else {
        super::write_atomic(path, encode(changes).to_string().as_bytes())
    }
}

/// Keep the recovery algorithm independent of subprocesses and storage so
/// interruptions can be tested at each command and journal boundary.
fn restore_with(
    changes: &mut Vec<Change>,
    mut read: impl FnMut(Source) -> Result<Settings, String>,
    mut write: impl FnMut(Change) -> Result<(), String>,
    mut save: impl FnMut(&[Change]) -> Result<(), String>,
) -> Result<(), String> {
    while let Some(change) = changes.last().copied() {
        let values = read(change.source)?;
        if values.value(change.key) == Some(change.ours) {
            write(change)?;
        }
        changes.pop();
        if let Err(error) = save(changes) {
            // Repeating a restore is safe: the equality guard skips a
            // transition already restored before a journal write failed.
            changes.push(change);
            return Err(error);
        }
    }
    save(changes)
}

enum Request {
    Acquire(u64),
    Release(u32, u64),
    Refresh,
    Recheck,
    Shutdown,
}

#[derive(Clone)]
pub struct Control(Sender<Request>);

impl Control {
    pub fn recheck(&self) {
        let _ = self.0.send(Request::Recheck);
    }
}

pub struct Power {
    control: Control,
    events: Events,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Power {
    pub fn new(events: Events) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        let worker_events = events.clone();
        let wake = sender.clone();
        let thread = std::thread::Builder::new()
            .name("omapower-pmset".into())
            .spawn(move || {
                autoreleasepool(|_| {
                    // Notifications cover Low Power Mode; the 30-second refresh
                    // also covers systems which do not emit them for every change.
                    let center = NSNotificationCenter::defaultCenter();
                    let block = RcBlock::new(move |_| {
                        let _ = wake.send(Request::Refresh);
                    });
                    let observer = if objc2::available!(macos = 12.0) {
                        // SAFETY: The availability check protects the notification
                        // symbol; the block captures only a thread-safe sender.
                        Some(unsafe {
                            center.addObserverForName_object_queue_usingBlock(
                                Some(NSProcessInfoPowerStateDidChangeNotification),
                                None,
                                None,
                                &block,
                            )
                        })
                    } else {
                        None
                    };
                    match Worker::new(worker_events.clone()) {
                        Ok(mut worker) => worker.run(receiver),
                        Err(error) => {
                            let _ = worker_events.send(Event::Fatal(error));
                        }
                    }
                    if let Some(observer) = observer {
                        // SAFETY: This is the observer token returned by this center.
                        unsafe { center.removeObserver((*observer).as_ref()) };
                    }
                });
            })
            .map_err(|error| format!("start power worker: {error}"))?;
        Ok(Self {
            control: Control(sender),
            events,
            thread: Some(thread),
        })
    }

    pub fn control(&self) -> Control {
        self.control.clone()
    }

    fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.control.0.send(Request::Shutdown);
            if thread.join().is_err() {
                eprintln!("omapower: power worker panicked; recovery journal retained");
            }
        }
    }
}

impl engine::Power for Power {
    fn name(&self) -> &'static str {
        OWNER
    }

    fn acquire(&mut self, _owner: &str, generation: u64) {
        if self.control.0.send(Request::Acquire(generation)).is_err() {
            let _ = self.events.send(Event::Acquired {
                generation,
                result: Err(AcquireError::Rejected("power worker stopped".into())),
            });
        }
    }

    fn release(&mut self, _owner: &str, cookie: u32, generation: u64) {
        if self
            .control
            .0
            .send(Request::Release(cookie, generation))
            .is_err()
        {
            let _ = self.events.send(Event::ReleaseDone {
                generation,
                result: Err("power worker stopped".into()),
            });
        }
    }

    fn shutdown(&mut self) {
        self.stop();
    }
}

impl Drop for Power {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Worker {
    events: Events,
    journal: PathBuf,
    changes: Vec<Change>,
    cookie: Option<u32>,
    next_cookie: u32,
    available: Option<bool>,
    profile: Option<Profile>,
}

impl Worker {
    fn new(events: Events) -> Result<Self, String> {
        let journal = config::config_dir().join("macos-power-restore.json");
        let changes = match fs::read(&journal) {
            Ok(bytes) => decode(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(format!("read power recovery file: {error}")),
        };
        Ok(Self {
            events,
            journal,
            changes,
            cookie: None,
            next_cookie: 0,
            available: None,
            profile: None,
        })
    }

    fn save(&self) -> Result<(), String> {
        save_journal(&self.journal, &self.changes)
    }

    fn publish_profile(&mut self, profile: Profile) {
        if self.profile != Some(profile) {
            self.profile = Some(profile);
            let _ = self.events.send(Event::Profile(profile));
        }
    }

    fn refresh(&mut self) {
        self.publish_profile(
            read_current()
                .map(|(_, values)| values.profile())
                .unwrap_or(Profile::Unknown),
        );
    }

    fn availability(&mut self) {
        let available = permitted();
        // Before advertising a reappearing backend, finish any crash recovery
        // or hold left behind when the authorization was removed mid-build.
        if available && self.available != Some(true) && !self.changes.is_empty() {
            if let Err(error) = self.restore() {
                let _ = self
                    .events
                    .send(Event::Fatal(format!("restore saved power mode: {error}")));
                return;
            }
            self.cookie = None;
        }
        if self.available != Some(available) {
            self.available = Some(available);
            if !available {
                self.cookie = None;
            }
            let _ = self.events.send(Event::Unavailable(if available {
                None
            } else {
                Some(SETUP_REASON.into())
            }));
            let _ = self
                .events
                .send(Event::Owner(available.then(|| OWNER.into())));
        }
    }

    fn acquire(&mut self) -> Result<u32, AcquireError> {
        if !self.changes.is_empty() || self.cookie.is_some() {
            return Err(AcquireError::Ambiguous(
                "a previous power change still needs restoration".into(),
            ));
        }
        let (source, values) = read_current().map_err(AcquireError::Rejected)?;
        if values.profile() == Profile::Unknown {
            return Err(AcquireError::Rejected(
                "pmset did not report a power mode".into(),
            ));
        }
        for mut change in plan(source, values) {
            // Some machines expose both keys as related settings. Re-read
            // after each command so the next record describes its actual
            // previous value, including any side effect of the first command.
            let current = read_saved(source).map_err(|error| {
                if self.changes.is_empty() {
                    AcquireError::Rejected(error)
                } else {
                    AcquireError::Ambiguous(error)
                }
            })?;
            let Some(previous) = current.value(change.key) else {
                return Err(if self.changes.is_empty() {
                    AcquireError::Rejected("the power mode setting disappeared".into())
                } else {
                    AcquireError::Ambiguous("the power mode setting disappeared".into())
                });
            };
            if previous == change.ours {
                continue;
            }
            change.previous = previous;
            self.changes.push(change);
            if let Err(error) = self.save() {
                self.changes.pop();
                return Err(if self.changes.is_empty() {
                    AcquireError::Rejected(error)
                } else {
                    AcquireError::Ambiguous(error)
                });
            }
            if let Err(error) = set(change.source, change.key, change.ours) {
                self.changes.pop();
                self.save().map_err(AcquireError::Ambiguous)?;
                if self.changes.is_empty() {
                    return Err(AcquireError::Rejected(error));
                }
                eprintln!("omapower: optional High Power Mode unavailable: {error}");
                break;
            }
            self.refresh();
        }
        self.next_cookie = self.next_cookie.wrapping_add(1).max(1);
        self.cookie = Some(self.next_cookie);
        Ok(self.next_cookie)
    }

    /// Pop each transition only after restoring it (or observing it is no
    /// longer ours). A failed command keeps the journal for retry/recovery.
    fn restore(&mut self) -> Result<(), String> {
        let profile = &mut self.profile;
        let events = &self.events;
        restore_with(
            &mut self.changes,
            read_saved,
            |change| {
                set(change.source, change.key, change.previous)?;
                let current = read_current()
                    .map(|(_, values)| values.profile())
                    .unwrap_or(Profile::Unknown);
                if *profile != Some(current) {
                    *profile = Some(current);
                    let _ = events.send(Event::Profile(current));
                }
                Ok(())
            },
            |changes| save_journal(&self.journal, changes),
        )
    }

    fn check_hold(&mut self) {
        let Some(cookie) = self.cookie else { return };
        if self.changes.is_empty() {
            return;
        }
        let Ok((source, values)) = read_current() else {
            return;
        };
        self.publish_profile(values.profile());
        let mut seen = BTreeSet::new();
        for change in self.changes.iter().rev() {
            // Only the last transition for each key is currently expected.
            if !seen.insert((change.source, change.key)) {
                continue;
            }
            let current = if change.source == source {
                Ok(values)
            } else {
                read_saved(change.source)
            };
            if current.is_ok_and(|values| values.value(change.key) != Some(change.ours)) {
                self.changes.clear();
                self.cookie = None;
                if let Err(error) = self.save() {
                    let _ = self.events.send(Event::Fatal(error));
                }
                let _ = self.events.send(Event::ExternalRelease {
                    owner: OWNER.into(),
                    cookie,
                });
                break;
            }
        }
    }

    fn run(&mut self, receiver: Receiver<Request>) {
        if let Err(error) = self.restore() {
            eprintln!("omapower: saved power mode awaits restoration: {error}");
        }
        self.availability();
        self.refresh();
        let mut hold_at = Instant::now() + Duration::from_secs(3);
        let mut profile_at = Instant::now() + Duration::from_secs(30);
        let mut availability_at = Instant::now() + Duration::from_secs(30);
        loop {
            let deadline = hold_at.min(profile_at).min(availability_at);
            match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Request::Acquire(generation)) => {
                    let result = self.acquire();
                    self.refresh();
                    let _ = self.events.send(Event::Acquired { generation, result });
                }
                Ok(Request::Release(cookie, generation)) => {
                    let result = if self.cookie == Some(cookie) {
                        self.restore()
                    } else {
                        Ok(())
                    };
                    if result.is_ok() {
                        self.cookie = None;
                    }
                    self.refresh();
                    let _ = self.events.send(Event::ReleaseDone { generation, result });
                }
                Ok(Request::Refresh) => self.refresh(),
                Ok(Request::Recheck) => {
                    self.availability();
                    self.refresh();
                }
                Ok(Request::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = Instant::now();
            if now >= hold_at {
                self.check_hold();
                hold_at = Instant::now() + Duration::from_secs(3);
            }
            if now >= profile_at {
                self.refresh();
                profile_at = Instant::now() + Duration::from_secs(30);
            }
            // Once allowed, a removed rule shows up as a failing switch; don't
            // keep asking sudo (each check can land in the system log).
            if now >= availability_at && self.available != Some(true) {
                self.availability();
                availability_at = Instant::now() + Duration::from_secs(30);
            }
        }
        if let Err(error) = self.restore() {
            eprintln!(
                "omapower: restore power mode at shutdown: {error}; recovery journal retained"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Model the powermode chain without invoking pmset or touching disk.
    fn restore_model(
        changes: &mut Vec<Change>,
        actual: &RefCell<u8>,
        writes: &RefCell<Vec<u8>>,
        save: impl FnMut(&[Change]) -> Result<(), String>,
    ) -> Result<(), String> {
        restore_with(
            changes,
            |source| {
                assert_eq!(source, Source::Ac);
                Ok(Settings {
                    low_power: None,
                    power: Some(*actual.borrow()),
                })
            },
            |change| {
                assert_eq!(change.key, Key::Power);
                *actual.borrow_mut() = change.previous;
                writes.borrow_mut().push(change.previous);
                Ok(())
            },
            save,
        )
    }

    #[test]
    fn restores_in_reverse_order_and_recovers_before_a_staged_command() {
        for (initial, expected_writes) in [(2, vec![0, 1]), (0, vec![1]), (1, vec![])] {
            let mut changes = plan(Source::Ac, settings("powermode 1"));
            let actual = RefCell::new(initial);
            let writes = RefCell::new(Vec::new());
            let mut saved_lengths = Vec::new();
            restore_model(&mut changes, &actual, &writes, |remaining| {
                saved_lengths.push(remaining.len());
                Ok(())
            })
            .unwrap();
            assert_eq!(*actual.borrow(), 1);
            assert_eq!(*writes.borrow(), expected_writes);
            assert!(changes.is_empty());
            assert_eq!(saved_lengths, [1, 0, 0]);
        }
    }

    #[test]
    fn failed_restore_and_failed_persistence_remain_retryable() {
        let original = plan(Source::Ac, settings("powermode 1"));
        let mut changes = original.clone();
        let result = restore_with(
            &mut changes,
            |_| Ok(settings("powermode 2")),
            |_| Err("permission revoked".into()),
            |_| panic!("a failed command must not discard recovery information"),
        );
        assert_eq!(result, Err("permission revoked".into()));
        assert_eq!(changes, original);

        let actual = RefCell::new(2);
        let writes = RefCell::new(Vec::new());
        assert!(
            restore_model(&mut changes, &actual, &writes, |_| Err("disk full".into())).is_err()
        );
        assert_eq!(changes, original);
        assert_eq!(*actual.borrow(), 0);
        restore_model(&mut changes, &actual, &writes, |_| Ok(())).unwrap();
        assert_eq!(*actual.borrow(), 1);
        assert_eq!(*writes.borrow(), [0, 1]);
        assert!(changes.is_empty());
    }

    #[test]
    fn release_preserves_manual_changes_and_uses_the_recorded_source() {
        let mut changes = plan(Source::Battery, settings("lowpowermode 1"));
        restore_with(
            &mut changes,
            |source| {
                assert_eq!(source, Source::Battery);
                Ok(settings("lowpowermode 1"))
            },
            |_| panic!("a manually changed setting must be left alone"),
            |_| Ok(()),
        )
        .unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn current_source_uses_only_the_first_line() {
        assert_eq!(
            source("Now drawing from 'AC Power'\nBattery Power"),
            Ok(Source::Ac)
        );
        assert_eq!(
            source("Now drawing from 'Battery Power'\n"),
            Ok(Source::Battery)
        );
        assert!(source("unknown\nAC Power").is_err());
    }

    #[test]
    fn parses_exact_setting_names_and_maps_profiles() {
        let values = settings(
            "System-wide power settings:\nCurrently in use:\n lowpowermode 0\n powermode 2\n lesspowermode 1\n",
        );
        assert_eq!(
            values,
            Settings {
                low_power: Some(0),
                power: Some(2)
            }
        );
        assert_eq!(values.profile(), Profile::Performance);
        assert_eq!(
            settings("lowpowermode 1\npowermode 2").profile(),
            Profile::PowerSaver
        );
        assert_eq!(settings("powermode 1").profile(), Profile::PowerSaver);
        assert_eq!(settings("lowpowermode 0").profile(), Profile::Balanced);
        assert_eq!(settings("standby 1").profile(), Profile::Unknown);
    }

    #[test]
    fn custom_settings_keep_sources_separate() {
        let text = "Battery Power:\n lowpowermode 1\n powermode 0\nAC Power:\n lowpowermode 0\n powermode 2\nUPS Power:\n powermode 1\n";
        assert_eq!(
            custom_settings(text, Source::Battery).unwrap().low_power,
            Some(1)
        );
        assert_eq!(custom_settings(text, Source::Ac).unwrap().power, Some(2));
        assert!(custom_settings("AC Power:\n standby 1", Source::Battery).is_err());
    }

    #[test]
    fn plans_automatic_then_optional_high_power_and_noops() {
        let changes = plan(Source::Ac, settings("powermode 1"));
        assert_eq!(
            changes,
            vec![
                Change {
                    source: Source::Ac,
                    key: Key::Power,
                    previous: 1,
                    ours: 0
                },
                Change {
                    source: Source::Ac,
                    key: Key::Power,
                    previous: 0,
                    ours: 2
                },
            ]
        );
        assert_eq!(
            plan(Source::Battery, settings("lowpowermode 1")),
            vec![Change {
                source: Source::Battery,
                key: Key::LowPower,
                previous: 1,
                ours: 0
            },]
        );
        assert!(plan(Source::Ac, settings("powermode 2\nlowpowermode 0")).is_empty());
        assert!(plan(Source::Battery, settings("lowpowermode 0")).is_empty());
        assert_eq!(
            plan(Source::Ac, settings("lowpowermode 1\npowermode 0")).len(),
            2
        );
    }

    #[test]
    fn journal_roundtrip_and_untrusted_arguments_are_rejected() {
        let changes = plan(Source::Ac, settings("powermode 1"));
        assert_eq!(
            decode(encode(&changes).to_string().as_bytes()).unwrap(),
            changes
        );
        for text in [
            r#"{"version":2,"changes":[]}"#,
            r#"{"version":1,"changes":[{"source":"-a","key":"powermode","previous":0,"ours":2}]}"#,
            r#"{"version":1,"changes":[{"source":"-b","key":"lowpowermode","previous":0,"ours":2}]}"#,
            r#"{"version":1,"changes":[{"source":"-b","key":"powermode; reboot","previous":0,"ours":2}]}"#,
            r#"{"version":1,"changes":[{"source":"-b","key":"powermode","previous":-1,"ours":2}]}"#,
        ] {
            assert!(decode(text.as_bytes()).is_err(), "{text}");
        }
    }
}
