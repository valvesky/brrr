//! Watch the configuration file without polling. The file's directory is
//! watched (atomic replacements change the file's identity), and so is the
//! directory above it, so a deleted and recreated directory is noticed.
//! A symlinked file's target directory is watched too.

use super::engine::{Event, Events};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};

pub struct ConfigWatch {
    watcher: RecommendedWatcher,
    path: PathBuf,
    watched: Vec<PathBuf>,
}

/// Directories whose changes may affect the file at `path`.
fn directories(path: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Some(dir) = path.parent() {
        result.push(dir.to_path_buf());
        if let Some(parent) = dir.parent() {
            result.push(parent.to_path_buf());
        }
    }
    if let Ok(target) = std::fs::canonicalize(path) {
        if let Some(dir) = target.parent().filter(|dir| !result.iter().any(|known| known == dir)) {
            result.push(dir.to_path_buf());
        }
    }
    result
}

impl ConfigWatch {
    pub fn new(path: PathBuf, events: Events) -> Result<ConfigWatch, String> {
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        std::fs::create_dir_all(&dir).map_err(|error| format!("create config directory: {error}"))?;
        let file = path.clone();
        let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
            let relevant = match &result {
                Ok(event) => event.need_rescan()
                    || event.paths.iter().any(|changed| {
                        changed == &file || changed == &dir || changed.parent() == Some(dir.as_path())
                            || std::fs::canonicalize(&file).is_ok_and(|target| &target == changed)
                    }),
                Err(_) => true,
            };
            if relevant {
                let _ = events.send(Event::ConfigTouched);
            }
        })
        .map_err(|error| format!("watch configuration: {error}"))?;
        let mut watch = ConfigWatch { watcher, path, watched: Vec::new() };
        watch.rearm();
        if watch.watched.is_empty() {
            return Err("cannot watch the configuration directory".into());
        }
        Ok(watch)
    }

    /// Re-establish watches: a replaced directory needs a new watch.
    pub fn rearm(&mut self) {
        for dir in self.watched.drain(..) {
            let _ = self.watcher.unwatch(&dir);
        }
        for dir in directories(&self.path) {
            if self.watcher.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
                self.watched.push(dir);
            }
        }
    }
}
