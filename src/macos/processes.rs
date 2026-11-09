//! libproc polling. Only configured names are published, never process IDs.

use crate::core::engine::{self, Event, Events};
use std::collections::BTreeSet;
use std::mem::{MaybeUninit, size_of};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

pub struct Processes {
    commands: Option<Sender<BTreeSet<String>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Processes {
    pub fn new(events: Events) -> Result<Self, String> {
        let (sender, receiver) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("omapower-processes".into())
            .spawn(move || {
                // SAFETY: getuid has no preconditions and does not borrow memory.
                let uid = unsafe { libc::getuid() };
                let mut commands = BTreeSet::new();
                let mut previous = None;
                loop {
                    match scan(&commands, uid) {
                        Ok(active) => {
                            if previous.as_ref() != Some(&active) {
                                if events
                                    .send(Event::Processes {
                                        active: active.clone(),
                                        mode: "polling",
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                                previous = Some(active);
                            }
                        }
                        Err(error) => {
                            let _ = events.send(Event::Fatal(error));
                            break;
                        }
                    }
                    match receiver.recv_timeout(Duration::from_millis(1000)) {
                        Ok(next) => {
                            commands = next;
                            // Coalesce edits without delaying the next scan.
                            while let Ok(next) = receiver.try_recv() {
                                commands = next;
                            }
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|error| format!("start process watcher: {error}"))?;
        Ok(Self {
            commands: Some(sender),
            thread: Some(thread),
        })
    }
}

impl engine::Processes for Processes {
    fn set_commands(&mut self, commands: &BTreeSet<String>) {
        if let Some(sender) = &self.commands {
            let _ = sender.send(commands.clone());
        }
    }
}

impl Drop for Processes {
    fn drop(&mut self) {
        self.commands.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn name_matches(name: &[libc::c_char], command: &str) -> bool {
    name.iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .eq(command.bytes())
}

/// libproc can supply a longer name as well as the traditional comm name.
/// Match either exactly, preserving the spelling in the configuration.
fn matches(info: &libc::proc_bsdinfo, uid: libc::uid_t, command: &str) -> bool {
    info.pbi_status != libc::SZOMB
        && (info.pbi_uid == uid || info.pbi_ruid == uid)
        && (name_matches(&info.pbi_name, command) || name_matches(&info.pbi_comm, command))
}

fn scan(commands: &BTreeSet<String>, uid: libc::uid_t) -> Result<BTreeSet<String>, String> {
    // SAFETY: A null buffer with size zero asks libproc for the PID count.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count < 0 {
        return Err(format!(
            "list processes: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut pids = vec![0 as libc::pid_t; count as usize + 128];
    let count = loop {
        let bytes = i32::try_from(pids.len() * size_of::<libc::pid_t>())
            .map_err(|_| "process list is too large")?;
        // SAFETY: The initialized vector is writable for exactly `bytes` bytes.
        let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if count < 0 {
            return Err(format!(
                "list processes: {}",
                std::io::Error::last_os_error()
            ));
        }
        if (count as usize) < pids.len() {
            break count as usize;
        }
        pids.resize(pids.len() * 2, 0);
    };
    let mut active = BTreeSet::new();
    for pid in pids.into_iter().take(count).filter(|pid| *pid > 0) {
        let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
        // SAFETY: libproc receives an aligned buffer of the exact requested size.
        let bytes = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size_of::<libc::proc_bsdinfo>() as i32,
            )
        };
        if bytes != size_of::<libc::proc_bsdinfo>() as i32 {
            // Exits and permission changes between listing and reading are normal.
            continue;
        }
        // SAFETY: A full successful PROC_PIDTBSDINFO reply initialized the struct.
        let info = unsafe { info.assume_init() };
        for command in commands {
            if matches(&info, uid, command) {
                active.insert(command.clone());
            }
        }
    }
    Ok(active)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_names_real_or_effective_uid_and_zombies() {
        // SAFETY: All fields of this plain C integer/array structure admit zero.
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        info.pbi_uid = 501;
        info.pbi_ruid = 502;
        for (slot, byte) in info.pbi_name.iter_mut().zip(b"swift-frontend") {
            *slot = *byte as libc::c_char;
        }
        for (slot, byte) in info.pbi_comm.iter_mut().zip(b"swift") {
            *slot = *byte as libc::c_char;
        }
        assert!(matches(&info, 501, "swift-frontend"));
        assert!(matches(&info, 502, "swift"));
        assert!(!matches(&info, 503, "swift"));
        assert!(!matches(&info, 501, "Swift"));
        assert!(!matches(&info, 501, "swif"));
        info.pbi_name.fill(0);
        assert!(matches(&info, 501, "swift"));
        info.pbi_status = libc::SZOMB;
        assert!(!matches(&info, 501, "swift"));
    }

    #[test]
    fn names_need_not_have_a_terminator() {
        assert!(name_matches(
            &[b'x' as libc::c_char; 16],
            "xxxxxxxxxxxxxxxx"
        ));
        assert!(!name_matches(
            &[b'x' as libc::c_char; 16],
            "xxxxxxxxxxxxxxx"
        ));
        assert!(name_matches(&[b'x' as libc::c_char, 0, 1], "x"));
        assert!(name_matches(
            &[0xc3_u8 as libc::c_char, 0xa9_u8 as libc::c_char],
            "é"
        ));
    }
}
