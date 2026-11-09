//! A single worker owns power mutations and their durable undo record.
//!
//! The journal is flushed before changing Windows. Recovery and release only
//! undo a value that still belongs to us, preserving later manual changes.

use super::wide;
use crate::core::{
    config,
    engine::{AcquireError, Event, Events, Power, Profile},
};
use serde_json::{Value, json};
use std::ffi::c_void;
use std::fs::{self, File};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{ERROR_NO_MORE_ITEMS, FreeLibrary, HMODULE, LocalFree};
use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Power::{
    ACCESS_SCHEME, EFFECTIVE_POWER_MODE_CALLBACK, PowerEnumerate, PowerGetActiveScheme,
    PowerSetActiveScheme,
};
use windows_sys::core::GUID;

const BALANCED_PLAN: u128 = 0x381b4222_f694_41f0_9685_ff5bb260df2e;
const HIGH_PLAN: u128 = 0x8c5e7fda_e8bf_4a96_9a85_a6e23a8c635c;
const ULTIMATE_PLAN: u128 = 0xe9a42b02_d5df_448d_aa00_03f14749eb61;
const SAVER_PLAN: u128 = 0xa1841308_3541_4fab_bc81_f71556f20b4a;
const EFFICIENCY: u128 = 0x961cc777_2547_4f9d_8174_7d86181b8a7a;
const BALANCED_OVERLAY: u128 = 0;
const BETTER_PERFORMANCE: u128 = 0x3af9b8d9_7c97_431d_ad78_34a8bfea439f;
const BEST_PERFORMANCE: u128 = 0xded574b5_45a0_4f42_8737_46345c09c238;
const UNAVAILABLE: &str = "This version of Windows has no power mode setting";
const NO_PERFORMANCE: &str = "No performance power mode is available on this power plan";
const INTERVAL: Duration = Duration::from_secs(2);

type GetOverlay = unsafe extern "system" fn(*mut GUID) -> u32;
// The setter takes the GUID by value, unlike PowerSetActiveScheme.
type SetOverlay = unsafe extern "system" fn(GUID) -> u32;
type RegisterMode = unsafe extern "system" fn(
    u32,
    EFFECTIVE_POWER_MODE_CALLBACK,
    *const c_void,
    *mut *mut c_void,
) -> i32;
type UnregisterMode = unsafe extern "system" fn(*const c_void) -> i32;

struct Notifications {
    handle: *mut c_void,
    unregister: UnregisterMode,
    context: Option<Box<Events>>,
}

impl Drop for Notifications {
    fn drop(&mut self) {
        // SAFETY: this handle came from registration, and the context stays alive
        // until unregister has waited for all outstanding callbacks.
        if unsafe { (self.unregister)(self.handle) } != 0 {
            // A failed unregister cannot prove the callback has stopped. Keep
            // its small context alive rather than allowing a use-after-free.
            if let Some(context) = self.context.take() {
                Box::leak(context);
            }
            eprintln!("omapower: could not unregister effective power mode notifications");
        }
    }
}

unsafe extern "system" fn effective_mode(mode: i32, context: *const c_void) {
    // SAFETY: registration holds this boxed sender until callbacks have drained.
    let events = unsafe { &*context.cast::<Events>() };
    let _ = events.send(Event::Profile(mode_profile(mode)));
}

fn mode_profile(mode: i32) -> Profile {
    Profile::from_name(match mode {
        0 | 1 => "power-saver",
        2 | 6 => "balanced",
        3..=5 => "performance",
        _ => "unknown",
    })
}

struct Api {
    module: HMODULE,
    get_overlay: Option<GetOverlay>,
    set_overlay: Option<SetOverlay>,
    notifications: Option<Notifications>,
}

impl Api {
    fn load(events: &Events) -> Self {
        // SAFETY: powrprof is a Windows system DLL and the name is terminated.
        let module = unsafe { LoadLibraryW(wide("powrprof.dll").as_ptr()) };
        let mut api = Self {
            module,
            get_overlay: None,
            set_overlay: None,
            notifications: None,
        };
        if module.is_null() {
            return api;
        }
        // SAFETY: each export is cast to its documented/native ABI, only after
        // GetProcAddress returns a non-null function, while the DLL stays loaded.
        unsafe {
            api.get_overlay = GetProcAddress(
                module,
                c"PowerGetEffectiveOverlayScheme".as_ptr().cast(),
            )
            .map(|address| {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, GetOverlay>(address)
            });
            api.set_overlay = GetProcAddress(
                module,
                c"PowerSetActiveOverlayScheme".as_ptr().cast(),
            )
            .map(|address| {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, SetOverlay>(address)
            });
            let register = GetProcAddress(
                module,
                c"PowerRegisterForEffectivePowerModeNotifications"
                    .as_ptr()
                    .cast(),
            )
            .map(|address| {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, RegisterMode>(address)
            });
            let unregister = GetProcAddress(
                module,
                c"PowerUnregisterFromEffectivePowerModeNotifications"
                    .as_ptr()
                    .cast(),
            )
            .map(|address| {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, UnregisterMode>(address)
            });
            if let (Some(register), Some(unregister)) = (register, unregister) {
                let context = Box::new(events.clone());
                let mut handle = null_mut();
                let pointer = (&*context as *const Events).cast();
                // V2 includes game/VR states; Windows 10 1809 accepts V1.
                if register(2, Some(effective_mode), pointer, &mut handle) == 0
                    || register(1, Some(effective_mode), pointer, &mut handle) == 0
                {
                    api.notifications = Some(Notifications {
                        handle,
                        unregister,
                        context: Some(context),
                    });
                }
            }
        }
        api
    }

    fn available(&self) -> bool {
        self.get_overlay.is_some() && self.set_overlay.is_some()
    }

    fn overlay(&self) -> Result<u128, String> {
        let get = self.get_overlay.ok_or(UNAVAILABLE)?;
        let mut guid = GUID::default();
        // SAFETY: the loaded function accepts a writable GUID, and the DLL is live.
        check(unsafe { get(&mut guid) }, "read power mode")?;
        Ok(guid_value(guid))
    }

    fn set_overlay(&self, value: u128) -> Result<(), String> {
        let set = self.set_overlay.ok_or(UNAVAILABLE)?;
        // SAFETY: this export takes a GUID by value and the DLL remains loaded.
        check(unsafe { set(GUID::from_u128(value)) }, "set power mode")
    }

    fn current_is_ours(&self, restore: Restore) -> Result<bool, String> {
        let plan = active_plan()?;
        let overlay = if restore.kind == Kind::Overlay && plan == BALANCED_PLAN {
            Some(self.overlay()?)
        } else {
            None
        };
        Ok(still_ours(restore.kind, plan, overlay))
    }

    fn restore(&self, restore: Restore) -> Result<(), String> {
        if self.current_is_ours(restore)? {
            match restore.kind {
                Kind::Overlay => self.set_overlay(restore.previous)?,
                Kind::Plan => set_plan(restore.previous)?,
            }
        }
        Ok(())
    }

    fn profile(&self) -> Profile {
        let Ok(plan) = active_plan() else {
            return Profile::Unknown;
        };
        if plan == BALANCED_PLAN {
            match self.overlay() {
                Ok(BEST_PERFORMANCE | BETTER_PERFORMANCE) => Profile::Performance,
                Ok(BALANCED_OVERLAY) => Profile::Balanced,
                Ok(EFFICIENCY) => Profile::PowerSaver,
                _ => Profile::Unknown,
            }
        } else if plan == HIGH_PLAN || plan == ULTIMATE_PLAN {
            Profile::Performance
        } else if plan == SAVER_PLAN {
            Profile::PowerSaver
        } else {
            Profile::Unknown
        }
    }
}

impl Drop for Api {
    fn drop(&mut self) {
        drop(self.notifications.take());
        if !self.module.is_null() {
            // SAFETY: our LoadLibrary reference is released after unregistering.
            unsafe { FreeLibrary(self.module) };
        }
    }
}

fn check(status: u32, operation: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "{operation}: {}",
            std::io::Error::from_raw_os_error(status as i32)
        ))
    }
}

fn guid_value(guid: GUID) -> u128 {
    ((guid.data1 as u128) << 96)
        | ((guid.data2 as u128) << 80)
        | ((guid.data3 as u128) << 64)
        | u64::from_be_bytes(guid.data4) as u128
}

fn active_plan() -> Result<u128, String> {
    let mut pointer = null_mut();
    // SAFETY: null selects the current user; Windows allocates the GUID output.
    check(
        unsafe { PowerGetActiveScheme(null_mut(), &mut pointer) },
        "read power plan",
    )?;
    if pointer.is_null() {
        return Err("Windows returned an empty power plan".into());
    }
    // SAFETY: a successful call allocated this GUID with LocalAlloc.
    let guid = unsafe { *pointer };
    // SAFETY: the allocation belongs to us and has been copied above.
    unsafe { LocalFree(pointer.cast()) };
    Ok(guid_value(guid))
}

fn set_plan(plan: u128) -> Result<(), String> {
    // SAFETY: null selects the current user and the GUID lives through the call.
    check(
        unsafe { PowerSetActiveScheme(null_mut(), &GUID::from_u128(plan)) },
        "set power plan",
    )
}

fn high_plan_exists() -> Result<bool, String> {
    for index in 0.. {
        let mut guid = GUID::default();
        let mut size = size_of::<GUID>() as u32;
        // SAFETY: ACCESS_SCHEME returns one GUID into the correctly sized buffer.
        let status = unsafe {
            PowerEnumerate(
                null_mut(),
                null(),
                null(),
                ACCESS_SCHEME,
                index,
                (&mut guid as *mut GUID).cast(),
                &mut size,
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            return Ok(false);
        }
        check(status, "enumerate power plans")?;
        if guid_value(guid) == HIGH_PLAN {
            return Ok(true);
        }
    }
    unreachable!("the finite power plan enumeration terminates")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Overlay,
    Plan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Restore {
    kind: Kind,
    previous: u128,
}

fn still_ours(kind: Kind, plan: u128, overlay: Option<u128>) -> bool {
    match kind {
        Kind::Overlay => plan == BALANCED_PLAN && overlay == Some(BEST_PERFORMANCE),
        Kind::Plan => plan == HIGH_PLAN,
    }
}

/// Already-fast modes deliberately have no undo record: we changed nothing.
fn choose_boost(plan: u128, overlay: u128, high_exists: bool) -> Result<Option<Restore>, String> {
    if plan == BALANCED_PLAN {
        Ok((overlay != BEST_PERFORMANCE).then_some(Restore {
            kind: Kind::Overlay,
            previous: overlay,
        }))
    } else if plan == HIGH_PLAN || plan == ULTIMATE_PLAN {
        Ok(None)
    } else if high_exists {
        Ok(Some(Restore {
            kind: Kind::Plan,
            previous: plan,
        }))
    } else {
        Err(NO_PERFORMANCE.into())
    }
}

fn guid_text(value: u128) -> String {
    let digits = format!("{value:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &digits[..8],
        &digits[8..12],
        &digits[12..16],
        &digits[16..20],
        &digits[20..]
    )
}

fn parse_guid(value: &str) -> Option<u128> {
    if value.len() != 36
        || !value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    u128::from_str_radix(&value.replace('-', ""), 16).ok()
}

impl Restore {
    fn target(self) -> u128 {
        match self.kind {
            Kind::Overlay => BEST_PERFORMANCE,
            Kind::Plan => HIGH_PLAN,
        }
    }

    fn json(self) -> Value {
        json!({"version": 1, "kind": match self.kind { Kind::Overlay => "overlay", Kind::Plan => "plan" },
            "previous": guid_text(self.previous), "set": guid_text(self.target())})
    }

    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let invalid = || "invalid Windows power restore file".to_string();
        let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if value["version"].as_u64() != Some(1) {
            return Err(invalid());
        }
        let kind = match value["kind"].as_str() {
            Some("overlay") => Kind::Overlay,
            Some("plan") => Kind::Plan,
            _ => return Err(invalid()),
        };
        let previous = value["previous"]
            .as_str()
            .and_then(parse_guid)
            .ok_or_else(invalid)?;
        let restore = Self { kind, previous };
        if value["set"].as_str().and_then(parse_guid) != Some(restore.target()) {
            return Err(invalid());
        }
        Ok(restore)
    }
}

fn save(path: &Path, restore: Restore) -> Result<(), String> {
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let result = (|| -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // A crash before rename leaves only an unused temporary. A crash
        // after rename leaves a complete undo record, even before the setter.
        let mut file = File::create(&temporary)?;
        file.write_all(restore.json().to_string().as_bytes())?;
        file.sync_all()?;
        drop(file);
        let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: both names are terminated. Omitting REPLACE_EXISTING protects
        // any pending undo record; WRITE_THROUGH completes the move before use.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|error| format!("save power restore file: {error}"))
}

fn forget(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("delete power restore file: {error}")),
    }
}

fn recover(api: &Api, path: &Path) -> Result<(), String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("read power restore file: {error}")),
    };
    let restore = Restore::parse(&bytes)?;
    api.restore(restore)?;
    forget(path)
}

enum Request {
    Acquire(u64),
    Release { cookie: u32, generation: u64 },
    Stop,
}

struct Worker {
    api: Api,
    events: Events,
    journal: PathBuf,
    held: Option<(u32, Restore)>,
    cookie: u32,
}

impl Worker {
    fn acquire(&mut self) -> Result<u32, AcquireError> {
        let reject = AcquireError::Rejected;
        if !self.api.available() {
            return Err(reject(UNAVAILABLE.into()));
        }
        if self.held.is_some() {
            return Err(reject("a power change is already held".into()));
        }
        let plan = active_plan().map_err(reject)?;
        let overlay = if plan == BALANCED_PLAN {
            self.api.overlay().map_err(reject)?
        } else {
            0
        };
        let high_exists = if ![BALANCED_PLAN, HIGH_PLAN, ULTIMATE_PLAN].contains(&plan) {
            high_plan_exists().map_err(reject)?
        } else {
            false
        };
        let restore = choose_boost(plan, overlay, high_exists).map_err(reject)?;
        self.cookie = self.cookie.wrapping_add(1).max(1);
        if let Some(restore) = restore {
            save(&self.journal, restore).map_err(reject)?;
            // Own the undo record before calling the setter: even a setter
            // failure with uncertain state must be retried during shutdown.
            self.held = Some((self.cookie, restore));
            let result = match restore.kind {
                Kind::Overlay => self.api.set_overlay(BEST_PERFORMANCE),
                Kind::Plan => set_plan(HIGH_PLAN),
            };
            if let Err(error) = result {
                // A failed native setter is a rejection. If it nevertheless
                // took effect, undo it before declaring the acquisition absent.
                self.api.restore(restore).map_err(AcquireError::Ambiguous)?;
                forget(&self.journal).map_err(AcquireError::Ambiguous)?;
                self.held = None;
                return Err(reject(error));
            }
        }
        Ok(self.cookie)
    }

    fn release(&mut self, cookie: u32) -> Result<(), String> {
        if let Some((held_cookie, restore)) = self.held {
            if held_cookie == cookie {
                self.api.restore(restore)?;
                forget(&self.journal)?;
                self.held = None;
            }
        }
        Ok(())
    }

    fn poll(&mut self) {
        if let Some((cookie, restore)) = self.held {
            match self.api.current_is_ours(restore) {
                Ok(false) => {
                    self.held = None;
                    if let Err(error) = forget(&self.journal) {
                        let _ = self.events.send(Event::Fatal(error));
                    }
                    let _ = self.events.send(Event::ExternalRelease {
                        owner: "windows".into(),
                        cookie,
                    });
                }
                Ok(true) => {}
                Err(error) => eprintln!("omapower: check power mode: {error}"),
            }
        }
        if self.api.notifications.is_none() {
            let _ = self.events.send(Event::Profile(self.api.profile()));
        }
    }
}

pub struct PowerBackend {
    requests: Sender<Request>,
    worker: Option<JoinHandle<()>>,
}

impl PowerBackend {
    pub fn new(events: Events) -> Result<Self, String> {
        let (requests, receive) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("windows-power".into())
            .spawn(move || {
                let api = Api::load(&events);
                let journal = config::config_dir().join("windows-power-restore.json");
                if let Err(error) = recover(&api, &journal) {
                    let _ = events.send(Event::Fatal(format!(
                        "restore power mode on startup: {error}"
                    )));
                    return;
                }
                if api.available() {
                    let _ = events.send(Event::Owner(Some("windows".into())));
                    let _ = events.send(Event::Unavailable(None));
                } else {
                    let _ = events.send(Event::Owner(None));
                    let _ = events.send(Event::Unavailable(Some(UNAVAILABLE.into())));
                }
                let mut worker = Worker {
                    api,
                    events,
                    journal,
                    held: None,
                    cookie: 0,
                };
                worker.poll();
                let mut deadline = Instant::now() + INTERVAL;
                loop {
                    match receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                        Ok(Request::Acquire(generation)) => {
                            let result = worker.acquire();
                            let _ = worker.events.send(Event::Acquired { generation, result });
                            worker.poll();
                        }
                        Ok(Request::Release { cookie, generation }) => {
                            let result = worker.release(cookie);
                            let _ = worker
                                .events
                                .send(Event::ReleaseDone { generation, result });
                            worker.poll();
                        }
                        Ok(Request::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                    if Instant::now() >= deadline {
                        worker.poll();
                        deadline = Instant::now() + INTERVAL;
                    }
                }
                if let Some((cookie, _)) = worker.held {
                    if let Err(error) = worker.release(cookie) {
                        // Retain the journal for the next startup if Windows refuses.
                        eprintln!("omapower: restore power mode on shutdown: {error}");
                    }
                }
            })
            .map_err(|error| format!("start power backend: {error}"))?;
        Ok(Self {
            requests,
            worker: Some(worker),
        })
    }
}

impl Power for PowerBackend {
    fn name(&self) -> &'static str {
        "windows"
    }

    fn acquire(&mut self, _owner: &str, generation: u64) {
        let _ = self.requests.send(Request::Acquire(generation));
    }

    fn release(&mut self, _owner: &str, cookie: u32, generation: u64) {
        let _ = self.requests.send(Request::Release { cookie, generation });
    }

    fn shutdown(&mut self) {
        let _ = self.requests.send(Request::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for PowerBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boost_preserves_existing_fast_modes() {
        assert_eq!(
            choose_boost(BALANCED_PLAN, BEST_PERFORMANCE, false),
            Ok(None)
        );
        assert_eq!(choose_boost(HIGH_PLAN, 0, false), Ok(None));
        assert_eq!(choose_boost(ULTIMATE_PLAN, 0, false), Ok(None));
        assert_eq!(
            choose_boost(BALANCED_PLAN, EFFICIENCY, false),
            Ok(Some(Restore {
                kind: Kind::Overlay,
                previous: EFFICIENCY
            }))
        );
        assert_eq!(
            choose_boost(SAVER_PLAN, 0, true),
            Ok(Some(Restore {
                kind: Kind::Plan,
                previous: SAVER_PLAN
            }))
        );
        assert_eq!(
            choose_boost(SAVER_PLAN, 0, false),
            Err(NO_PERFORMANCE.into())
        );
    }

    #[test]
    fn effective_mode_mapping() {
        for mode in 0..=6 {
            assert_eq!(
                mode_profile(mode),
                match mode {
                    0 | 1 => Profile::PowerSaver,
                    2 | 6 => Profile::Balanced,
                    _ => Profile::Performance,
                }
            );
        }
        assert_eq!(mode_profile(7), Profile::Unknown);
        assert_eq!(mode_profile(-1), Profile::Unknown);
    }

    #[test]
    fn manual_changes_are_never_restored() {
        assert!(still_ours(
            Kind::Overlay,
            BALANCED_PLAN,
            Some(BEST_PERFORMANCE)
        ));
        assert!(!still_ours(Kind::Overlay, BALANCED_PLAN, Some(EFFICIENCY)));
        assert!(!still_ours(
            Kind::Overlay,
            HIGH_PLAN,
            Some(BEST_PERFORMANCE)
        ));
        assert!(still_ours(Kind::Plan, HIGH_PLAN, None));
        assert!(!still_ours(Kind::Plan, ULTIMATE_PLAN, None));
        assert!(!still_ours(Kind::Plan, BALANCED_PLAN, None));
    }

    #[test]
    fn journal_round_trip_and_validation() {
        for restore in [
            Restore {
                kind: Kind::Overlay,
                previous: 0,
            },
            Restore {
                kind: Kind::Plan,
                previous: SAVER_PLAN,
            },
        ] {
            assert_eq!(
                Restore::parse(restore.json().to_string().as_bytes()),
                Ok(restore)
            );
            let mut value = restore.json();
            value["set"] = json!(guid_text(ULTIMATE_PLAN));
            assert!(Restore::parse(value.to_string().as_bytes()).is_err());
        }
        assert_eq!(
            parse_guid("961CC777-2547-4F9D-8174-7D86181B8A7A"),
            Some(EFFICIENCY)
        );
        assert_eq!(guid_value(GUID::from_u128(BALANCED_PLAN)), BALANCED_PLAN);
        for invalid in ["", "{}", "null", r#"{"version":2}"#] {
            assert!(Restore::parse(invalid.as_bytes()).is_err());
        }
        assert!(parse_guid("961cc77702547-4f9d-8174-7d86181b8a7a").is_none());
    }
}
