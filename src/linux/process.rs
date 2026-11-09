//! Linux process watching: kernel process events (the netlink proc connector)
//! with a recovery scan of /proc every five seconds, or a /proc scan every
//! 500 ms when events aren't available. Matched processes are also watched
//! through pidfds, so their exits are noticed even if netlink drops an event.
//!
//! Everything runs on one thread that owns the state; the engine talks to it
//! through a channel plus an eventfd that wakes its poll().

use crate::core::engine::{Event, Events, Processes};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::CStr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

const FALLBACK: Duration = Duration::from_millis(500);
/// Kernel events are primary; a slower audit repairs silent event loss.
const RECONCILE: Duration = Duration::from_millis(5000);
const ACK_TIMEOUT: Duration = Duration::from_secs(1);

// <linux/connector.h>, <linux/cn_proc.h>, <linux/netlink.h>
const NETLINK_CONNECTOR: libc::c_int = 11;
const CN_IDX_PROC: u32 = 1;
const CN_VAL_PROC: u32 = 1;
const PROC_CN_MCAST_LISTEN: u32 = 1;
const PROC_CN_MCAST_IGNORE: u32 = 2;
const NLMSG_HDRLEN: usize = 16;
const CN_MSG_LEN: usize = 20;
/// sizeof(struct proc_event): what, cpu, timestamp_ns, then a 24-byte union.
const PROC_EVENT_LEN: usize = 40;
const NLMSG_DONE: u16 = 3;
const NLMSG_OVERRUN: u16 = 4;
const PROC_EVENT_NONE: u32 = 0;
const PROC_EVENT_FORK: u32 = 1;
const PROC_EVENT_EXEC: u32 = 2;
const PROC_EVENT_UID: u32 = 4;
const PROC_EVENT_COMM: u32 = 0x200;
const PROC_EVENT_EXIT: u32 = 0x8000_0000;

struct Tracked {
    start: u64,
    name: String,
    pidfd: Option<OwnedFd>,
}

enum Control {
    Commands(BTreeSet<String>),
}

pub struct ProcessWatch {
    control: Sender<Control>,
    wake: OwnedFd,
}

impl Processes for ProcessWatch {
    fn set_commands(&mut self, commands: &BTreeSet<String>) {
        let _ = self.control.send(Control::Commands(commands.clone()));
        wake(self.wake.as_raw_fd());
    }
}

fn wake(fd: RawFd) {
    let one: u64 = 1;
    // SAFETY: writes 8 bytes from a live u64 to an eventfd we own.
    unsafe { libc::write(fd, (&one as *const u64).cast(), 8) };
}

impl ProcessWatch {
    pub fn new(force_poll: bool, events: Events) -> Result<ProcessWatch, String> {
        // SAFETY: eventfd has no memory-safety preconditions; the result is checked.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if fd < 0 {
            return Err(format!("eventfd: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: fd is a fresh descriptor that nothing else owns.
        let wake_fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: dup has no memory-safety preconditions; the result is checked.
        let thread_fd = unsafe { libc::dup(fd) };
        if thread_fd < 0 {
            return Err(format!("dup: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: thread_fd is a fresh descriptor that nothing else owns.
        let thread_wake = unsafe { OwnedFd::from_raw_fd(thread_fd) };
        let (control, receiver) = mpsc::channel();
        std::thread::Builder::new()
            .name("processes".into())
            .spawn(move || Watcher::new(force_poll, events, receiver, thread_wake).run())
            .map_err(|error| error.to_string())?;
        Ok(ProcessWatch { control, wake: wake_fd })
    }
}

struct Watcher {
    events: Events,
    control: Receiver<Control>,
    wake: OwnedFd,
    commands: BTreeSet<String>,
    processes: HashMap<i32, Tracked>,
    socket: Option<OwnedFd>,
    acknowledged: bool,
    ack_token: u32,
    ack_deadline: Option<Instant>,
    next_scan: Instant,
    mode: &'static str,
    changes: u64,
    events_seen: u64,
    stream_recoveries: u64,
    last_recovery_log: Option<Instant>,
    reported: Option<(BTreeSet<String>, &'static str)>,
    error: Option<String>,
    uid: u32,
}

/// Read a small /proc file relative to an open process directory.
fn read_at(dir: RawFd, file: &CStr, capacity: usize) -> Option<Vec<u8>> {
    // SAFETY: file is NUL-terminated; dir is an open directory descriptor.
    let fd = unsafe { libc::openat(dir, file.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: fd is a fresh descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut buffer = vec![0u8; capacity];
    let length = loop {
        // SAFETY: reads at most capacity - 1 bytes into a buffer of capacity bytes.
        let length = unsafe { libc::read(fd.as_raw_fd(), buffer.as_mut_ptr().cast(), capacity - 1) };
        if length < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        break length;
    };
    if length <= 0 || length as usize == capacity - 1 {
        return None;
    }
    buffer.truncate(length as usize);
    Some(buffer)
}

fn pidfd_open(pid: i32) -> Option<OwnedFd> {
    // SAFETY: pidfd_open has no memory-safety preconditions; the result is checked.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as RawFd;
    // SAFETY: on success fd is a fresh descriptor that nothing else owns.
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}

/// True once every thread of the process has exited.
fn pidfd_exited(fd: &OwnedFd) -> bool {
    let mut pfd = libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 };
    // SAFETY: polls one valid pollfd without blocking.
    unsafe { libc::poll(&mut pfd, 1, 0) != 0 }
}

/// The start time (field 22) from /proc/PID/stat, or None for a dead process.
/// The name may contain spaces and parentheses, so parse from the last ')'.
fn parse_stat(stat: &[u8], pid: i32) -> Option<u64> {
    let tail = &stat[stat.iter().rposition(|byte| *byte == b')')?..];
    if tail.len() < 4 || tail[1] != b' ' || tail[3] != b' ' {
        return None;
    }
    match tail[2] {
        b'X' | b'x' => return None,
        b'Z' => {
            // A zombie leader can still have executing threads. A process pidfd
            // becomes readable only when the entire thread group has exited.
            if pidfd_open(pid).is_none_or(|fd| pidfd_exited(&fd)) {
                return None;
            }
        }
        _ => {}
    }
    // Field 3 is the state; starttime is field 22.
    let fields = std::str::from_utf8(&tail[4..]).ok()?;
    fields.split(' ').nth(22 - 4)?.trim_end_matches('\n').parse().ok()
}

impl Watcher {
    fn new(force_poll: bool, events: Events, control: Receiver<Control>, wake: OwnedFd) -> Watcher {
        let mut watcher = Watcher {
            events,
            control,
            wake,
            commands: BTreeSet::new(),
            processes: HashMap::new(),
            socket: None,
            acknowledged: false,
            ack_token: rand_u32(),
            ack_deadline: None,
            next_scan: Instant::now() + RECONCILE,
            mode: "connecting",
            changes: 0,
            events_seen: 0,
            stream_recoveries: 0,
            last_recovery_log: None,
            reported: None,
            error: None,
            // SAFETY: getuid cannot fail.
            uid: unsafe { libc::getuid() },
        };
        if force_poll {
            watcher.fallback("--poll requested");
        } else if let Err(error) = watcher.connect() {
            watcher.fallback(&error);
        }
        watcher
    }

    fn connect(&mut self) -> Result<(), String> {
        let last_error = || std::io::Error::last_os_error().to_string();
        // SAFETY: socket has no memory-safety preconditions; the result is checked.
        let fd = unsafe {
            libc::socket(libc::AF_NETLINK, libc::SOCK_DGRAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, NETLINK_CONNECTOR)
        };
        if fd < 0 {
            return Err(last_error());
        }
        // SAFETY: fd is a fresh descriptor that nothing else owns.
        let socket = unsafe { OwnedFd::from_raw_fd(fd) };
        let receive_buffer: libc::c_int = 1024 * 1024;
        // SAFETY: passes a live c_int of the declared size.
        unsafe {
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF, (&receive_buffer as *const libc::c_int).cast(),
                             size_of::<libc::c_int>() as libc::socklen_t)
        };
        // SAFETY: sockaddr_nl is plain data; all-zero is a valid value.
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as u16;
        address.nl_groups = CN_IDX_PROC;
        // SAFETY: binds to a live sockaddr_nl of the declared size.
        let bound = unsafe {
            libc::bind(fd, (&address as *const libc::sockaddr_nl).cast(), size_of::<libc::sockaddr_nl>() as libc::socklen_t)
        };
        if bound < 0 {
            return Err(last_error());
        }
        self.socket = Some(socket);
        if !self.subscription(PROC_CN_MCAST_LISTEN) {
            let error = last_error();
            self.socket = None;
            return Err(error);
        }
        self.ack_deadline = Some(Instant::now() + ACK_TIMEOUT);
        self.next_scan = Instant::now() + RECONCILE;
        Ok(())
    }

    fn subscription(&self, operation: u32) -> bool {
        let Some(socket) = &self.socket else { return false };
        let mut packet = [0u8; NLMSG_HDRLEN + CN_MSG_LEN + 4];
        let length = packet.len() as u32;
        packet[0..4].copy_from_slice(&length.to_ne_bytes());
        packet[4..6].copy_from_slice(&NLMSG_DONE.to_ne_bytes());
        let connector = NLMSG_HDRLEN;
        packet[connector..connector + 4].copy_from_slice(&CN_IDX_PROC.to_ne_bytes());
        packet[connector + 4..connector + 8].copy_from_slice(&CN_VAL_PROC.to_ne_bytes());
        packet[connector + 12..connector + 16].copy_from_slice(&self.ack_token.to_ne_bytes());
        packet[connector + 16..connector + 18].copy_from_slice(&4u16.to_ne_bytes());
        packet[connector + CN_MSG_LEN..].copy_from_slice(&operation.to_ne_bytes());
        // SAFETY: sockaddr_nl is plain data; all-zero is a valid value.
        let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        kernel.nl_family = libc::AF_NETLINK as u16;
        // SAFETY: sends a live buffer to a live address, both of the declared sizes.
        let sent = unsafe {
            libc::sendto(socket.as_raw_fd(), packet.as_ptr().cast(), packet.len(), 0,
                         (&kernel as *const libc::sockaddr_nl).cast(), size_of::<libc::sockaddr_nl>() as libc::socklen_t)
        };
        sent == packet.len() as isize
    }

    fn close_socket(&mut self) {
        if self.socket.is_some() {
            self.subscription(PROC_CN_MCAST_IGNORE);
            self.socket = None;
        }
        self.ack_deadline = None;
    }

    fn fallback(&mut self, reason: &str) {
        eprintln!("omapower: process events unavailable ({reason}); polling /proc every {} ms", FALLBACK.as_millis());
        self.close_socket();
        self.mode = "polling";
        self.next_scan = Instant::now() + FALLBACK;
        self.scan("fallback");
    }

    /// Does this /proc entry belong to a watched command you started? Opens the
    /// directory once so the UID, comm and start time describe one incarnation.
    fn process_info(&self, pid: i32) -> Option<(String, u64)> {
        let path = std::ffi::CString::new(format!("/proc/{pid}")).ok()?;
        // SAFETY: path is NUL-terminated; the result is checked.
        let dir = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if dir < 0 {
            return None;
        }
        // SAFETY: dir is a fresh descriptor that nothing else owns.
        let dir = unsafe { OwnedFd::from_raw_fd(dir) };
        // SAFETY: stat is plain data; fstat fills it in.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fstat on an open descriptor into a live stat.
        if unsafe { libc::fstat(dir.as_raw_fd(), &mut st) } != 0 {
            return None;
        }
        // Only the final newline in comm is a delimiter, not whitespace.
        let mut comm = read_at(dir.as_raw_fd(), c"comm", 32)?;
        if comm.last() == Some(&b'\n') {
            comm.pop();
        }
        let name = String::from_utf8(comm).ok()?;
        // Check the cheap name match before reading the login UID.
        if !self.commands.contains(&name) || !self.started_by_user(dir.as_raw_fd(), &st) {
            return None;
        }
        let stat = read_at(dir.as_raw_fd(), c"stat", 4096)?;
        drop(dir);
        Some((name, parse_stat(&stat, pid)?))
    }

    /// Commands you start, including through sudo: either owned by you, or
    /// carrying your login UID. The kernel keeps the login UID across sudo/su,
    /// while other users' and system processes have a different or unset one.
    fn started_by_user(&self, dir: RawFd, st: &libc::stat) -> bool {
        if st.st_uid == self.uid {
            return true;
        }
        read_at(dir, c"loginuid", 16)
            .and_then(|text| String::from_utf8(text).ok())
            .and_then(|text| text.trim_end_matches('\n').parse::<u64>().ok())
            .is_some_and(|loginuid| loginuid == u64::from(self.uid))
    }

    fn remove_process(&mut self, pid: i32, source: &str) {
        if let Some(process) = self.processes.remove(&pid) {
            debug(format_args!("process unmatched: pid={pid} name={} source={source}", process.name));
            self.changes += 1;
        }
    }

    fn refresh_pid(&mut self, pid: i32, source: &str) {
        if pid <= 0 || self.commands.is_empty() {
            return;
        }
        let info = self.process_info(pid);
        if source.starts_with("event:") {
            debug(format_args!("process event: pid={pid} source={source} matched={}", if info.is_some() { "yes" } else { "no" }));
        }
        let Some((name, start)) = info else {
            self.remove_process(pid, source);
            return;
        };
        if self.processes.get(&pid).is_some_and(|old| old.start == start && old.name == name) {
            return;
        }
        let pidfd = pidfd_open(pid);
        if let Some(fd) = &pidfd {
            // A PID can exit or be recycled between reading /proc and pidfd_open().
            if pidfd_exited(fd) || self.process_info(pid) != Some((name.clone(), start)) {
                self.remove_process(pid, source);
                return;
            }
        }
        debug(format_args!(
            "process matched: pid={pid} name={name} start={start} source={source} pidfd={}",
            if pidfd.is_some() { "yes" } else { "no" }
        ));
        self.processes.insert(pid, Tracked { start, name, pidfd });
        self.changes += 1;
    }

    fn scan(&mut self, source: &str) {
        if self.commands.is_empty() {
            if !self.processes.is_empty() {
                self.changes += self.processes.len() as u64;
                debug(format_args!("process matches cleared: source={source}"));
                self.processes.clear();
            }
            return;
        }
        let entries = match std::fs::read_dir("/proc") {
            Ok(entries) => entries,
            Err(error) => {
                self.error = Some(format!("open /proc: {error}"));
                return;
            }
        };
        let mut seen = HashSet::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.error = Some(format!("read /proc: {error}"));
                    return;
                }
            };
            let Some(pid) = entry.file_name().to_str().and_then(|name| name.parse::<i32>().ok()).filter(|pid| *pid > 0)
            else {
                continue;
            };
            seen.insert(pid);
            self.refresh_pid(pid, source);
        }
        let gone: Vec<i32> = self.processes.keys().filter(|pid| !seen.contains(pid)).copied().collect();
        for pid in gone {
            debug(format_args!("process unmatched: pid={pid} source={source}"));
            self.processes.remove(&pid);
            self.changes += 1;
        }
    }

    fn rescan(&mut self) {
        let before = self.changes;
        let events = self.mode == "events";
        self.scan(if events { "recovery" } else { "poll" });
        if events && self.changes != before {
            eprintln!(
                "omapower: recovery scan corrected {} process match changes; events received={}",
                self.changes - before,
                self.events_seen
            );
        }
        self.next_scan = Instant::now() + if events { RECONCILE } else { FALLBACK };
    }

    /// Process connector messages are best-effort. Never trust user-sent
    /// netlink messages, truncated packets, or a thread's comm as the
    /// leader's name.
    fn handle_event(&mut self, event: &[u8]) {
        let word = |offset: usize| u32::from_ne_bytes(event[offset..offset + 4].try_into().expect("4 bytes"));
        let pid = |offset: usize| word(offset) as i32;
        let what = word(0);
        if what != PROC_EVENT_NONE {
            self.events_seen += 1;
        }
        const DATA: usize = 16;
        match what {
            PROC_EVENT_NONE => {
                let error = word(DATA);
                if error != 0 {
                    self.fallback(&std::io::Error::from_raw_os_error(error as i32).to_string());
                } else if !self.acknowledged {
                    self.acknowledged = true;
                    self.ack_deadline = None;
                    self.mode = "events";
                    eprintln!("omapower: process events subscribed; recovery scan every {} ms", RECONCILE.as_millis());
                }
            }
            PROC_EVENT_FORK if pid(DATA + 8) == pid(DATA + 12) => self.refresh_pid(pid(DATA + 12), "event:fork"),
            PROC_EVENT_EXEC => self.refresh_pid(pid(DATA + 4), "event:exec"),
            PROC_EVENT_UID => self.refresh_pid(pid(DATA + 4), "event:uid"),
            PROC_EVENT_COMM if pid(DATA) == pid(DATA + 4) => self.refresh_pid(pid(DATA + 4), "event:comm"),
            // Refresh rather than blindly remove: a queued exit may belong to an
            // older PID incarnation. pidfds also report exits if netlink drops one.
            PROC_EVENT_EXIT => self.refresh_pid(pid(DATA + 4), "event:exit"),
            _ => {}
        }
    }

    /// Returns true when a full snapshot is needed to recover an incomplete stream.
    fn process_packet(&mut self, bytes: &[u8]) -> bool {
        let mut recover = false;
        let mut offset = 0;
        while bytes.len() - offset >= NLMSG_HDRLEN {
            let field = |at: usize| u32::from_ne_bytes(bytes[at..at + 4].try_into().expect("4 bytes"));
            let length = field(offset) as usize;
            let kind = u16::from_ne_bytes(bytes[offset + 4..offset + 6].try_into().expect("2 bytes"));
            if length < NLMSG_HDRLEN || length > bytes.len() - offset {
                return true;
            }
            let step = (length + 3) & !3;
            if step > bytes.len() - offset {
                return true;
            }
            let payload = &bytes[offset + NLMSG_HDRLEN..offset + length];
            offset += step;
            if kind == NLMSG_OVERRUN {
                recover = true;
                continue;
            }
            if kind != NLMSG_DONE {
                continue;
            }
            if payload.len() < CN_MSG_LEN {
                recover = true;
                continue;
            }
            let word = |at: usize| u32::from_ne_bytes(payload[at..at + 4].try_into().expect("4 bytes"));
            if word(0) != CN_IDX_PROC || word(4) != CN_VAL_PROC {
                continue;
            }
            let ack = word(12);
            let data_length = u16::from_ne_bytes(payload[16..18].try_into().expect("2 bytes")) as usize;
            if data_length < PROC_EVENT_LEN || data_length > payload.len() - CN_MSG_LEN {
                recover = true;
                continue;
            }
            let event = &payload[CN_MSG_LEN..CN_MSG_LEN + PROC_EVENT_LEN];
            let what = u32::from_ne_bytes(event[0..4].try_into().expect("4 bytes"));
            // Acknowledgements are multicast too. Do not accept another client's
            // subscription acknowledgement as proof that ours succeeded.
            if what == PROC_EVENT_NONE && ack != self.ack_token.wrapping_add(1) {
                continue;
            }
            self.handle_event(event);
            if what == PROC_EVENT_NONE && u32::from_ne_bytes(event[16..20].try_into().expect("4 bytes")) != 0 {
                return true;
            }
            if self.socket.is_none() {
                return false;
            }
        }
        recover || offset != bytes.len()
    }

    fn socket_ready(&mut self, revents: i16) {
        let mut recover = revents & libc::POLLERR != 0;
        if revents & (libc::POLLHUP | libc::POLLNVAL) != 0 {
            self.fallback("process event socket disconnected");
            return;
        }
        let mut buffer = vec![0u8; 65536];
        for _ in 0..256 {
            let Some(socket) = &self.socket else { return };
            // SAFETY: sockaddr_nl is plain data; all-zero is a valid value.
            let mut sender: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
            let mut iov = libc::iovec { iov_base: buffer.as_mut_ptr().cast(), iov_len: buffer.len() };
            // SAFETY: msghdr is plain data; all-zero is a valid value.
            let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
            message.msg_name = (&mut sender as *mut libc::sockaddr_nl).cast();
            message.msg_namelen = size_of::<libc::sockaddr_nl>() as u32;
            message.msg_iov = &mut iov;
            message.msg_iovlen = 1;
            // SAFETY: message points at live buffers of the declared sizes.
            let length = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_DONTWAIT) };
            if length < 0 {
                let error = std::io::Error::last_os_error();
                match error.raw_os_error() {
                    Some(libc::EINTR) => continue,
                    Some(libc::ENOBUFS) => {
                        debug(format_args!("process stream: receive buffer overflow"));
                        recover = true;
                        continue;
                    }
                    Some(libc::EAGAIN) => break,
                    _ => {
                        self.fallback(&error.to_string());
                        return;
                    }
                }
            }
            if length == 0 {
                self.fallback("process event socket closed");
                return;
            }
            if (message.msg_namelen as usize) < size_of::<libc::sockaddr_nl>()
                || sender.nl_family != libc::AF_NETLINK as u16
                || sender.nl_pid != 0
            {
                continue;
            }
            if message.msg_flags & libc::MSG_TRUNC != 0 {
                debug(format_args!("process stream: truncated datagram"));
                recover = true;
                continue;
            }
            if self.process_packet(&buffer[..length as usize]) {
                debug(format_args!("process stream: overrun or malformed packet"));
                recover = true;
            }
            if self.socket.is_none() {
                return;
            }
        }
        if recover {
            self.stream_recoveries += 1;
            let now = Instant::now();
            if self.last_recovery_log.is_none_or(|last| now - last >= Duration::from_secs(5)) {
                eprintln!(
                    "omapower: process stream loss/error; immediate recovery scan (total={})",
                    self.stream_recoveries
                );
                self.last_recovery_log = Some(now);
            }
            self.scan("stream-recovery");
        }
    }

    fn report(&mut self) -> bool {
        if let Some(error) = self.error.take() {
            let _ = self.events.send(Event::Fatal(error));
            return false;
        }
        let active: BTreeSet<String> = self.processes.values().map(|process| process.name.clone()).collect();
        let current = Some((active, self.mode));
        if self.reported != current {
            self.reported = current.clone();
            let (active, mode) = current.expect("just set");
            if self.events.send(Event::Processes { active, mode }).is_err() {
                return false;
            }
        }
        true
    }

    fn run(mut self) {
        // Subscribe before the snapshot; queued events reconcile startup races.
        self.scan("startup");
        loop {
            while let Ok(control) = self.control.try_recv() {
                match control {
                    Control::Commands(commands) => {
                        self.commands = commands;
                        self.scan("config");
                    }
                }
            }
            if !self.report() {
                return;
            }
            let now = Instant::now();
            let deadline = [Some(self.next_scan), self.ack_deadline].into_iter().flatten().min().expect("next_scan");
            let timeout = deadline.saturating_duration_since(now).as_millis().min(i32::MAX as u128) as i32;
            let mut fds = vec![libc::pollfd { fd: self.wake.as_raw_fd(), events: libc::POLLIN, revents: 0 }];
            if let Some(socket) = &self.socket {
                fds.push(libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLIN, revents: 0 });
            }
            let first_pidfd = fds.len();
            let mut pids = Vec::new();
            for (pid, process) in &self.processes {
                if let Some(fd) = &process.pidfd {
                    fds.push(libc::pollfd { fd: fd.as_raw_fd(), events: libc::POLLIN, revents: 0 });
                    pids.push(*pid);
                }
            }
            // SAFETY: fds is a live array of fds.len() pollfds.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                self.error = Some(format!("poll: {error}"));
                continue;
            }
            if fds[0].revents != 0 {
                let mut counter = 0u64;
                // SAFETY: reads 8 bytes into a live u64 from our eventfd.
                unsafe { libc::read(self.wake.as_raw_fd(), (&mut counter as *mut u64).cast(), 8) };
            }
            if self.socket.is_some() && fds[1].revents != 0 {
                self.socket_ready(fds[1].revents);
            }
            for (index, pid) in pids.into_iter().enumerate() {
                if fds[first_pidfd + index].revents != 0 {
                    self.remove_process(pid, "pidfd");
                }
            }
            let now = Instant::now();
            if self.ack_deadline.is_some_and(|deadline| deadline <= now) {
                self.fallback("subscription acknowledgement timed out");
            }
            if self.next_scan <= now {
                self.rescan();
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.close_socket();
    }
}

fn rand_u32() -> u32 {
    let mut value = 0u32;
    // SAFETY: getrandom writes at most 4 bytes into a live u32.
    let filled = unsafe { libc::getrandom((&mut value as *mut u32).cast(), 4, libc::GRND_NONBLOCK) };
    if filled == 4 {
        value
    } else {
        std::process::id() ^ (Instant::now().elapsed().subsec_nanos())
    }
}

/// Per-PID tracing, enabled like GLib's: G_MESSAGES_DEBUG=omapower (or all).
fn debug(message: std::fmt::Arguments) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| {
        std::env::var("G_MESSAGES_DEBUG")
            .is_ok_and(|value| value.split([' ', ',']).any(|domain| domain == "omapower" || domain == "all"))
    });
    if enabled {
        eprintln!("omapower-debug: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_start_time() {
        let mut stat = String::from("123 (a) b) c) S");
        for field in 4..=21 {
            stat.push_str(&format!(" {field}"));
        }
        stat.push_str(" 987654 23 24\n");
        assert_eq!(parse_stat(stat.as_bytes(), 123), Some(987654));
        assert_eq!(parse_stat(b"123 (dead) X 1 2", 123), None);
        assert_eq!(parse_stat(b"123 (short) S 1 2", 123), None);
        assert_eq!(parse_stat(b"garbage", 123), None);
    }

    fn test_watcher(force_poll: bool) -> (Watcher, Receiver<Event>) {
        let (events, receiver) = mpsc::channel();
        let (_control, control_receiver) = mpsc::channel();
        // SAFETY: eventfd has no memory-safety preconditions; the result is checked.
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) };
        assert!(fd >= 0);
        // SAFETY: fd is a fresh descriptor that nothing else owns.
        let wake = unsafe { OwnedFd::from_raw_fd(fd) };
        (Watcher::new(force_poll, events, control_receiver, wake), receiver)
    }

    fn packet(kind: u16, ack: u32, length: u16, what: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; NLMSG_HDRLEN + CN_MSG_LEN + PROC_EVENT_LEN];
        let total = bytes.len() as u32;
        bytes[0..4].copy_from_slice(&total.to_ne_bytes());
        bytes[4..6].copy_from_slice(&kind.to_ne_bytes());
        let connector = NLMSG_HDRLEN;
        bytes[connector..connector + 4].copy_from_slice(&CN_IDX_PROC.to_ne_bytes());
        bytes[connector + 4..connector + 8].copy_from_slice(&CN_VAL_PROC.to_ne_bytes());
        bytes[connector + 12..connector + 16].copy_from_slice(&ack.to_ne_bytes());
        bytes[connector + 16..connector + 18].copy_from_slice(&length.to_ne_bytes());
        bytes[connector + CN_MSG_LEN..connector + CN_MSG_LEN + 4].copy_from_slice(&what.to_ne_bytes());
        bytes
    }

    #[test]
    fn packets_and_acknowledgements() {
        let (mut watcher, _receiver) = test_watcher(true);
        // Pretend a subscription is pending, without a socket.
        watcher.mode = "connecting";
        let token = watcher.ack_token;
        let event_length = PROC_EVENT_LEN as u16;
        // Another listener's acknowledgement doesn't count as ours.
        assert!(!watcher.process_packet(&packet(NLMSG_DONE, token, event_length, PROC_EVENT_NONE)));
        assert!(!watcher.acknowledged);
        assert!(!watcher.process_packet(&packet(NLMSG_DONE, token.wrapping_add(1), event_length, PROC_EVENT_NONE)));
        assert!(watcher.acknowledged);
        assert_eq!(watcher.mode, "events");
        // Oversized payload lengths, overruns and truncated packets all ask for
        // a recovery snapshot.
        assert!(watcher.process_packet(&packet(NLMSG_DONE, 0, 65535, PROC_EVENT_EXEC)));
        let overrun = packet(NLMSG_OVERRUN, 0, event_length, PROC_EVENT_EXEC);
        assert!(watcher.process_packet(&overrun));
        let whole = packet(NLMSG_DONE, 0, event_length, PROC_EVENT_EXEC);
        for size in 1..whole.len() {
            assert!(watcher.process_packet(&whole[..size]), "size {size}");
        }
        // Messages for other connector users are ignored.
        let mut other = whole.clone();
        other[NLMSG_HDRLEN..NLMSG_HDRLEN + 4].copy_from_slice(&7u32.to_ne_bytes());
        assert!(!watcher.process_packet(&other));
    }

    #[test]
    fn finds_and_forgets_this_process() {
        let (mut watcher, receiver) = test_watcher(true);
        let comm = std::fs::read_to_string("/proc/thread-self/comm").unwrap();
        let name = comm.trim_end_matches('\n').to_string();
        // Thread names differ from the leader's: only the leader's comm counts.
        let leader = std::fs::read_to_string("/proc/self/comm").unwrap().trim_end_matches('\n').to_string();
        watcher.commands = BTreeSet::from([leader.clone()]);
        watcher.scan("test");
        // SAFETY: getpid cannot fail.
        let pid = unsafe { libc::getpid() };
        let start = watcher.processes.get(&pid).expect("this process matches").start;
        assert!(start > 0);
        // A stale PID incarnation is replaced.
        watcher.processes.get_mut(&pid).unwrap().start = 0;
        watcher.refresh_pid(pid, "test");
        assert_eq!(watcher.processes[&pid].start, start);
        // A queued old exit must not delete a live PID.
        let mut event = [0u8; PROC_EVENT_LEN];
        event[0..4].copy_from_slice(&PROC_EVENT_EXIT.to_ne_bytes());
        event[16..20].copy_from_slice(&pid.to_ne_bytes());
        event[20..24].copy_from_slice(&pid.to_ne_bytes());
        watcher.handle_event(&event);
        assert!(watcher.processes.contains_key(&pid));
        assert!(watcher.report());
        match receiver.try_recv() {
            Ok(Event::Processes { active, mode }) => {
                assert_eq!(active, BTreeSet::from([leader.clone()]));
                assert_eq!(mode, "polling");
            }
            other => panic!("unexpected {other:?}"),
        }
        watcher.commands = BTreeSet::from([format!("{name}x")]);
        watcher.scan("test");
        assert!(watcher.processes.is_empty());
    }
}
