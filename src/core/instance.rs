//! One running instance per user, however it was started.

use std::fs::File;
use std::path::PathBuf;
use std::time::Duration;

/// Exit status when another instance holds the lock (EX_TEMPFAIL).
pub const EXIT_BUSY: i32 = 75;

pub fn runtime_dir() -> PathBuf {
    #[cfg(target_os = "linux")]
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os("LOCALAPPDATA") {
        let dir = PathBuf::from(dir).join("omapower");
        let _ = std::fs::create_dir_all(&dir);
        return dir;
    }
    std::env::temp_dir()
}

/// Wait briefly, so a restart can replace an instance that is still
/// releasing its hold. The lock lasts as long as the returned file is open.
pub fn acquire() -> Option<File> {
    let path = runtime_dir().join("omapower.lock");
    let file = match File::options().create(true).truncate(false).write(true).open(&path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("omapower: lock: {error}");
            return None;
        }
    };
    for _ in 0..50 {
        if file.try_lock().is_ok() {
            return Some(file);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}
