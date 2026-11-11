//! The configuration file: `{"commands": [...], "enabled": true}`.
//!
//! Every platform reads the same format. Unknown members are preserved on
//! edit, including exact number tokens, so other tools can share the file.

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Compiler drivers, GCC's cc1/cc1plus workers, and heavy apps whose process
/// name is the same on every system. Every name is at most 15 bytes.
const COMMON_COMMANDS: &[&str] = &[
    "rustc", "cargo", "gcc", "g++", "cc", "c++", "cc1", "cc1plus", "clang", "clang++", "zig", "go",
    "javac", "swiftc", "swift-frontend", "gfortran", "flang", "nvcc", "ghc", "7z", "blender",
    "ffmpeg", "obs",
];
#[cfg(windows)]
const PLATFORM_COMMANDS: &[&str] = &[
    "cl", "msbuild", "7zG", "AfterFX", "HandBrake", "Photoshop", "Resolve", "WinRAR", "obs64",
];
#[cfg(target_os = "macos")]
const PLATFORM_COMMANDS: &[&str] = &["xcodebuild", "swift-build", "Blender", "HandBrake", "Resolve"];
#[cfg(not(any(windows, target_os = "macos")))]
const PLATFORM_COMMANDS: &[&str] = &["7zz", "HandBrakeCLI", "ghb", "resolve"];

/// Process names are compared with Linux `comm` names (at most 15 bytes), so the
/// same limit applies everywhere and a configuration stays portable.
pub const MAX_NAME_BYTES: usize = 15;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub commands: BTreeSet<String>,
    pub enabled: bool,
}

pub fn default_commands() -> BTreeSet<String> {
    COMMON_COMMANDS.iter().chain(PLATFORM_COMMANDS).map(|name| name.to_string()).collect()
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && !name.contains(['/', '\n', '\0'])
}

const DIR_NAME: &str = "brrr";
const LEGACY_DIR_NAME: &str = "omapower";
/// Copied once from the omapower directory when the brrr file is not there yet.
const MIGRATED_FILES: &[&str] =
    &["config.json", "windows-power-restore.json", "macos-power-restore.json"];

/// `$XDG_CONFIG_HOME/brrr` when set (on every platform), otherwise the
/// platform's per-user configuration directory.
pub fn config_dir() -> PathBuf {
    user_config_dir(DIR_NAME)
}

fn legacy_config_dir() -> PathBuf {
    user_config_dir(LEGACY_DIR_NAME)
}

fn user_config_dir(name: &str) -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir).join(name);
    }
    #[cfg(windows)]
    if let Some(dir) = std::env::var_os("APPDATA") {
        return PathBuf::from(dir).join(name);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support").join(name)
    } else {
        home.join(".config").join(name)
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

/// Copy the previous omapower configuration and recovery journals when the
/// brrr copies do not exist yet. An existing brrr file is left alone. This
/// runs on every operating system; a journal that was never written is skipped.
pub fn migrate() {
    if let Err(error) = migrate_tree(&legacy_config_dir(), &config_dir()) {
        eprintln!("brrr: could not copy the previous configuration: {error}");
    }
}

fn migrate_tree(from_dir: &Path, to_dir: &Path) -> io::Result<()> {
    for name in MIGRATED_FILES {
        copy_if_absent(&from_dir.join(name), &to_dir.join(name))?;
    }
    Ok(())
}

fn copy_if_absent(from: &Path, to: &Path) -> io::Result<()> {
    if !from.is_file() || to.exists() {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut source = File::open(from)?;
    let mut dest = match File::options().write(true).create_new(true).open(to) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error),
    };
    io::copy(&mut source, &mut dest)?;
    dest.sync_all()?;
    eprintln!("brrr: copied {} to {}", from.display(), to.display());
    Ok(())
}

/// Parse a configuration document. serde_json rejects comments, trailing
/// commas, NaN, lone surrogates, invalid UTF-8 and excessive nesting.
fn parse(bytes: &[u8]) -> Result<Map<String, Value>, String> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err("the configuration must be a JSON object".into()),
        Err(error) => Err(format!("invalid JSON configuration ({error})")),
    }
}

fn read_config(object: &Map<String, Value>) -> Result<Config, String> {
    let enabled = match object.get("enabled") {
        None => true,
        Some(Value::Bool(enabled)) => *enabled,
        Some(_) => return Err("enabled must be true or false".into()),
    };
    let Some(Value::Array(items)) = object.get("commands") else {
        return Err("commands must be an array of process names".into());
    };
    let mut commands = BTreeSet::new();
    for item in items {
        let Value::String(name) = item else { return Err("commands must contain strings".into()) };
        if !valid_name(name) {
            return Err(format!(
                "process names must be 1–{MAX_NAME_BYTES} bytes without NULs, slashes or newlines"
            ));
        }
        commands.insert(name.clone());
    }
    Ok(Config { commands, enabled })
}

/// Load the configuration. A missing file means the defaults, enabled.
pub fn load(path: &Path) -> Result<Config, String> {
    match fs::read(path) {
        Ok(bytes) => read_config(&parse(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(Config { commands: default_commands(), enabled: true })
        }
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Edit {
    Add(String),
    Remove(String),
    Enable(bool),
}

/// Follow a symlinked configuration file so an edit updates its target
/// instead of replacing the link.
fn resolve(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = File::create(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Apply one edit under the configuration lock. An invalid configuration is
/// never rewritten: the person has to fix it by hand.
pub fn edit(edit: &Edit) -> Result<(), String> {
    if let Edit::Add(name) | Edit::Remove(name) = edit {
        if !valid_name(name) {
            return Err("invalid command name".into());
        }
    }
    let dir = config_dir();
    fs::create_dir_all(&dir).map_err(|error| format!("config directory: {error}"))?;
    let lock = File::create(dir.join("config.lock")).map_err(|error| format!("config lock: {error}"))?;
    lock.lock().map_err(|error| format!("config lock: {error}"))?;

    let path = resolve(&config_path());
    let (members, mut commands) = match fs::read(&path) {
        Ok(bytes) => {
            let commands = read_config(&parse(&bytes)?)?.commands;
            let members = serde_json::from_slice::<Members>(&bytes).map_err(|error| error.to_string())?;
            (members.0, commands)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (Vec::new(), default_commands()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    match edit {
        Edit::Add(name) => {
            commands.insert(name.clone());
        }
        Edit::Remove(name) => {
            commands.remove(name);
        }
        Edit::Enable(_) => {}
    }
    // Commands first, then "enabled", then every other member exactly as written.
    let mut text = format!("{{\"commands\":{}", json_names(&commands));
    if let Edit::Enable(enabled) = edit {
        text.push_str(if *enabled { ",\"enabled\":true" } else { ",\"enabled\":false" });
    }
    for (name, value) in &members {
        if name == "commands" || (name == "enabled" && matches!(edit, Edit::Enable(_))) {
            continue;
        }
        text.push(',');
        text.push_str(&serde_json::to_string(name).expect("strings always encode"));
        text.push(':');
        text.push_str(value.get());
    }
    text.push('}');
    write_atomically(&path, text.as_bytes()).map_err(|error| format!("{}: {error}", path.display()))
}

/// An object's members with their raw text, so numbers like 1e400 or
/// 0.1234567890123456789012345 survive an edit unchanged. A repeated name
/// keeps its first position and its last value, as when reading.
struct Members(Vec<(String, Box<RawValue>)>);

impl<'de> Deserialize<'de> for Members {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Members, D::Error> {
        struct MembersVisitor;
        impl<'de> Visitor<'de> for MembersVisitor {
            type Value = Members;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Members, A::Error> {
                let mut members: Vec<(String, Box<RawValue>)> = Vec::new();
                while let Some((name, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    match members.iter_mut().find(|(known, _)| *known == name) {
                        Some(member) => member.1 = value,
                        None => members.push((name, value)),
                    }
                }
                Ok(Members(members))
            }
        }
        deserializer.deserialize_map(MembersVisitor)
    }
}

/// Encode names as a sorted JSON array.
pub fn json_names<'a>(names: impl IntoIterator<Item = &'a String>) -> String {
    let mut sorted: Vec<&String> = names.into_iter().collect();
    sorted.sort();
    serde_json::to_string(&sorted).expect("strings always encode")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(contents: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("brrr-config-{}-{}", std::process::id(), rand()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.json");
        if !contents.is_empty() {
            fs::write(&file, contents).unwrap();
        }
        (dir, file)
    }

    fn rand() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn missing_file_means_defaults() {
        let (dir, file) = temp_file("");
        let config = load(&file).unwrap();
        assert!(config.enabled);
        assert_eq!(config.commands, default_commands());
        assert!(config.commands.iter().all(|name| valid_name(name)));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn empty_and_disabled_lists_are_kept() {
        let (dir, file) = temp_file(r#"{"commands":[],"enabled":false}"#);
        assert_eq!(load(&file).unwrap(), Config { commands: BTreeSet::new(), enabled: false });
        fs::write(&file, r#"{"commands":["make","make"]}"#).unwrap();
        assert_eq!(load(&file).unwrap().commands, BTreeSet::from(["make".to_string()]));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_documents_are_errors() {
        for document in [
            "", "null", "[]", "{}", r#"{"commands":false}"#, r#"{"commands":[1]}"#,
            r#"{"commands":[""]}"#, r#"{"commands":["a/b"]}"#, r#"{"commands":["1234567890123456"]}"#,
            r#"{"commands":[],"enabled":"yes"}"#, r#"{"commands":[],}"#, r#"{"commands":["\ud800"]}"#,
        ] {
            let (dir, file) = temp_file("x");
            fs::write(&file, document).unwrap();
            assert!(load(&file).is_err(), "{document}");
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn names() {
        assert!(valid_name("swift-frontend"));
        assert!(valid_name("HandBrakeCLI"));
        assert!(valid_name("a;echo hacked"));
        assert!(!valid_name("123456789012345x"));
        assert!(!valid_name("éééééééé"));
        assert!(!valid_name("bin/rustc"));
        // Premiere's process name is 18 bytes, past the portable limit.
        assert!(!valid_name("Adobe Premiere Pro"));
    }

    #[test]
    fn defaults_fit_the_process_name_limit() {
        let commands = default_commands();
        assert!(commands.iter().all(|name| valid_name(name)));
        for name in ["rustc", "cargo", "gcc", "ffmpeg", "blender", "7z", "obs"] {
            assert!(commands.contains(name), "{name}");
        }
        #[cfg(windows)]
        for name in ["7zG", "WinRAR", "HandBrake", "obs64", "Resolve", "AfterFX", "Photoshop", "cl"] {
            assert!(commands.contains(name), "{name}");
        }
        #[cfg(target_os = "macos")]
        for name in ["HandBrake", "Resolve", "Blender", "xcodebuild", "swift-build"] {
            assert!(commands.contains(name), "{name}");
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        for name in ["7zz", "ghb", "HandBrakeCLI", "resolve"] {
            assert!(commands.contains(name), "{name}");
        }
    }

    #[test]
    fn migration_copies_missing_files_and_does_not_overwrite() {
        let root = std::env::temp_dir().join(format!("brrr-migrate-{}-{}", std::process::id(), rand()));
        let old = root.join("omapower");
        let new = root.join("brrr");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("config.json"), b"{\"commands\":[\"make\"]}").unwrap();
        fs::write(old.join("windows-power-restore.json"), b"win").unwrap();
        fs::write(old.join("macos-power-restore.json"), b"mac").unwrap();
        fs::write(old.join("notes.txt"), b"skip").unwrap();
        migrate_tree(&old, &new).unwrap();
        assert_eq!(fs::read(new.join("config.json")).unwrap(), b"{\"commands\":[\"make\"]}");
        assert_eq!(fs::read(new.join("windows-power-restore.json")).unwrap(), b"win");
        assert_eq!(fs::read(new.join("macos-power-restore.json")).unwrap(), b"mac");
        assert!(!new.join("notes.txt").exists());
        fs::write(new.join("config.json"), b"kept").unwrap();
        fs::write(old.join("config.json"), b"newer-old").unwrap();
        fs::remove_file(old.join("windows-power-restore.json")).unwrap();
        migrate_tree(&old, &new).unwrap();
        assert_eq!(fs::read(new.join("config.json")).unwrap(), b"kept");
        assert_eq!(fs::read(new.join("windows-power-restore.json")).unwrap(), b"win");
        fs::remove_dir_all(root).unwrap();
    }
}
