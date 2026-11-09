//! Toolhelp snapshots include elevated processes without opening their handles.

use crate::core::engine::{Event, Events, Processes};
use std::collections::BTreeSet;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_NO_MORE_FILES, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;

enum Request {
    Commands(BTreeSet<String>),
    Stop,
}

pub struct ProcessWatch {
    requests: Sender<Request>,
    worker: Option<JoinHandle<()>>,
}

/// Only the executable suffix is stripped; names retain their other dots.
fn executable_name(name: &str) -> &str {
    match name.get(name.len().saturating_sub(4)..) {
        Some(suffix) if suffix.eq_ignore_ascii_case(".exe") => &name[..name.len() - 4],
        _ => name,
    }
}

fn matches(commands: &BTreeSet<String>, names: &[String]) -> BTreeSet<String> {
    commands
        .iter()
        .filter(|command| {
            names
                .iter()
                .any(|name| command.eq_ignore_ascii_case(executable_name(name)))
        })
        .cloned()
        .collect()
}

fn scan(session: u32) -> Result<Vec<String>, String> {
    // SAFETY: Toolhelp owns the returned snapshot; it is closed below on every path.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "process snapshot: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut names = Vec::new();
    // SAFETY: entry is writable, correctly sized, and snapshot is valid.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) };
    while more != 0 {
        let mut process_session = 0;
        // SAFETY: the session output points to a live u32; no process handle is needed.
        if unsafe { ProcessIdToSessionId(entry.th32ProcessID, &mut process_session) } != 0
            && process_session == session
        {
            let length = entry
                .szExeFile
                .iter()
                .position(|ch| *ch == 0)
                .unwrap_or(entry.szExeFile.len());
            names.push(String::from_utf16_lossy(&entry.szExeFile[..length]));
        }
        // SAFETY: the same valid snapshot and sized entry remain live.
        more = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    // SAFETY: read the enumeration error before CloseHandle can overwrite it.
    let error = unsafe { GetLastError() };
    // SAFETY: this is the one close of our owned snapshot handle.
    unsafe { CloseHandle(snapshot) };
    if error != ERROR_NO_MORE_FILES {
        Err(format!(
            "enumerate processes: {}",
            std::io::Error::from_raw_os_error(error as i32)
        ))
    } else {
        Ok(names)
    }
}

impl ProcessWatch {
    pub fn new(events: Events) -> Result<Self, String> {
        let (requests, receive) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("windows-processes".into())
            .spawn(move || {
                let mut session = 0;
                // SAFETY: session is a valid output and our own PID exists throughout this call.
                if unsafe { ProcessIdToSessionId(std::process::id(), &mut session) } == 0 {
                    let _ = events.send(Event::Fatal(format!(
                        "find process session: {}",
                        std::io::Error::last_os_error()
                    )));
                    return;
                }
                let mut commands = BTreeSet::new();
                let mut previous = None;
                loop {
                    match scan(session) {
                        Ok(names) => {
                            let active = matches(&commands, &names);
                            if previous.as_ref() != Some(&active) {
                                previous = Some(active.clone());
                                if events
                                    .send(Event::Processes {
                                        active,
                                        mode: "polling",
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                        Err(error) => {
                            let _ = events.send(Event::Fatal(error));
                            break;
                        }
                    }
                    match receive.recv_timeout(Duration::from_millis(1000)) {
                        Ok(Request::Commands(next)) => commands = next,
                        Ok(Request::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .map_err(|error| format!("start process watcher: {error}"))?;
        Ok(Self {
            requests,
            worker: Some(worker),
        })
    }
}

impl Processes for ProcessWatch {
    fn set_commands(&mut self, commands: &BTreeSet<String>) {
        let _ = self.requests.send(Request::Commands(commands.clone()));
    }
}

impl Drop for ProcessWatch {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_suffix_and_ascii_case() {
        assert_eq!(executable_name("CL.ExE"), "CL");
        assert_eq!(executable_name("compiler.exe.exe"), "compiler.exe");
        assert_eq!(executable_name("e"), "e");
        assert_eq!(executable_name("日本.exe"), "日本");
        assert_eq!(executable_name("é日本"), "é日本");
        let commands = BTreeSet::from(["Cl".into(), "cargo".into(), "Ä".into()]);
        assert_eq!(
            matches(&commands, &["cl.EXE".into(), "ä.exe".into()]),
            BTreeSet::from(["Cl".into()])
        );
    }
}
