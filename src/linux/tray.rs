//! A StatusNotifierItem tray icon. The bar or desktop draws the icon and the
//! menu itself (waybar, KDE Plasma, GNOME with AppIndicator support, …), so
//! it looks native wherever it shows up.

use crate::core::config::{self, Edit};
use crate::core::engine::{Event, Events, Profile, State};
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub struct Tray {
    pub state: State,
    events: Events,
}

impl Tray {
    pub fn new(events: Events) -> Tray {
        Tray { state: State::default(), events }
    }
}

/// Freedesktop icon names (Adwaita, Breeze, Papirus, …) for each profile.
fn icon_name(profile: Profile) -> &'static str {
    match profile {
        Profile::Performance => "power-profile-performance-symbolic",
        Profile::PowerSaver => "power-profile-power-saver-symbolic",
        Profile::Balanced | Profile::Unknown => "power-profile-balanced-symbolic",
    }
}

/// Our own glyphs, for hosts whose icon theme lacks the names above.
fn pixmaps(profile: Profile) -> Vec<ksni::Icon> {
    let sources: [&[u8]; 3] = match profile {
        Profile::Performance => [
            include_bytes!("../../assets/tray/bolt-white-24.png"),
            include_bytes!("../../assets/tray/bolt-white-32.png"),
            include_bytes!("../../assets/tray/bolt-white-48.png"),
        ],
        Profile::PowerSaver => [
            include_bytes!("../../assets/tray/recycle-white-24.png"),
            include_bytes!("../../assets/tray/recycle-white-32.png"),
            include_bytes!("../../assets/tray/recycle-white-48.png"),
        ],
        Profile::Balanced | Profile::Unknown => [
            include_bytes!("../../assets/tray/yin-yang-white-24.png"),
            include_bytes!("../../assets/tray/yin-yang-white-32.png"),
            include_bytes!("../../assets/tray/yin-yang-white-48.png"),
        ],
    };
    sources.iter().filter_map(|png| decode_argb(png)).collect()
}

/// Decode an RGBA PNG into the ARGB32 (network byte order) SNI pixmaps use.
fn decode_argb(png: &[u8]) -> Option<ksni::Icon> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut buffer).ok()?;
    if frame.color_type != png::ColorType::Rgba || frame.bit_depth != png::BitDepth::Eight {
        return None;
    }
    let data = buffer[..frame.buffer_size()]
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[3], pixel[0], pixel[1], pixel[2]])
        .collect();
    Some(ksni::Icon { width: frame.width as i32, height: frame.height as i32, data })
}

/// Run a configuration edit off the D-Bus thread; report failures.
fn edit(change: Edit) {
    std::thread::spawn(move || {
        if let Err(error) = config::edit(&change) {
            eprintln!("brrr: {error}");
            notify(&format!("Couldn't change the configuration: {error}"));
        }
    });
}

fn notify(message: &str) {
    let _ = Command::new("notify-send")
        .args(["--app-name=brrr", "brrr", message])
        .stdin(Stdio::null())
        .spawn();
}

/// Ask for a command name with the desktop's own dialog tool.
fn ask_for_command() {
    std::thread::spawn(|| {
        let prompt = "App to watch (its process name, for example ffmpeg):";
        let attempts: [(&str, Vec<&str>); 3] = [
            ("zenity", vec!["--entry", "--title=brrr", "--text", prompt]),
            ("kdialog", vec!["--title", "brrr", "--inputbox", prompt]),
            ("qarma", vec!["--entry", "--title=brrr", "--text", prompt]),
        ];
        for (program, args) in attempts {
            let Ok(output) = Command::new(program).args(&args).stdin(Stdio::null()).output() else { continue };
            if !output.status.success() {
                return; // cancelled
            }
            let name = String::from_utf8_lossy(&output.stdout).trim_end_matches('\n').to_string();
            if config::valid_name(&name) {
                edit(Edit::Add(name));
            } else {
                notify("Command names are 1–15 bytes, without slashes.");
            }
            return;
        }
        // No dialog tool: let the person edit the list by hand.
        open_config();
    });
}

fn open_config() {
    let path = config::config_path();
    if !path.exists() {
        if let Ok(current) = config::load(&path) {
            let _ = config::edit(&Edit::Enable(current.enabled));
        }
    }
    let _ = Command::new("xdg-open").arg(path).stdin(Stdio::null()).spawn();
}

fn config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"))
}

fn data_home() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
}

pub fn autostart_path() -> PathBuf {
    config_home().join("autostart/brrr.desktop")
}

fn legacy_autostart_path() -> PathBuf {
    config_home().join("autostart/omapower.desktop")
}

fn legacy_icon_path() -> PathBuf {
    data_home().join("icons/hicolor/scalable/apps/omapower.svg")
}

pub fn autostart_enabled() -> bool {
    autostart_path().exists() || legacy_autostart_path().exists()
}

/// The flat app icon, in the user's icon theme, so desktops show it for the
/// login entry (GNOME Tweaks, KDE's Autostart settings, …).
pub fn icon_path() -> PathBuf {
    data_home().join("icons/hicolor/scalable/apps/brrr.svg")
}

const ICON: &[u8] = include_bytes!("../../assets/icon/brrr.svg");

fn remove(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {error}", path.display())),
        _ => Ok(()),
    }
}

fn write(path: &std::path::Path, contents: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    }
    std::fs::write(path, contents).map_err(|error| format!("{}: {error}", path.display()))
}

/// Start the tray at login through the XDG autostart directory.
pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let path = autostart_path();
    if !enabled {
        return remove(&path)
            .and(remove(&icon_path()))
            .and(remove(&legacy_autostart_path()))
            .and(remove(&legacy_icon_path()));
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    // Desktop entry Exec quoting: escape \ " ` $ inside double quotes.
    let quoted: String = exe.to_string_lossy().chars().flat_map(|c| {
        if matches!(c, '\\' | '"' | '`' | '$') { vec!['\\', c] } else { vec![c] }
    }).collect();
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=brrr\nComment=Full power when something heavy is open\n\
         Exec=\"{quoted}\" --tray\nIcon=brrr\nTerminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    );
    // The icon is a nicety: a login item without it still works.
    if let Err(error) = write(&icon_path(), ICON) {
        eprintln!("brrr: icon: {error}");
    }
    write(&path, entry.as_bytes())?;
    // Drop the pre-rename login item so the next login starts brrr only.
    let _ = remove(&legacy_autostart_path());
    let _ = remove(&legacy_icon_path());
    Ok(())
}

/// If the previous omapower login item is still installed, replace it with one
/// that starts this binary. An existing brrr login item is left as it is.
pub fn migrate_autostart() {
    if !legacy_autostart_path().exists() && !legacy_icon_path().exists() {
        return;
    }
    if autostart_path().exists() {
        let _ = remove(&legacy_autostart_path());
        let _ = remove(&legacy_icon_path());
        return;
    }
    if legacy_autostart_path().exists() {
        if let Err(error) = set_autostart(true) {
            eprintln!("brrr: could not move the login item to the new name: {error}");
        }
    } else {
        let _ = remove(&legacy_icon_path());
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "brrr".into()
    }

    fn title(&self) -> String {
        "brrr".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::Hardware
    }

    fn icon_name(&self) -> String {
        icon_name(self.state.profile).into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        pixmaps(self.state.profile)
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "brrr".into(),
            description: self.state.summary(),
            icon_name: icon_name(self.state.profile).into(),
            icon_pixmap: Vec::new(),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let state = &self.state;
        let mut commands: Vec<MenuItem<Self>> = state
            .commands
            .iter()
            .map(|name| {
                let label = if state.active.contains(name) { format!("{name} (running)") } else { name.clone() };
                let target = name.clone();
                SubMenu {
                    label,
                    submenu: vec![
                        StandardItem {
                            label: "Stop Watching".into(),
                            icon_name: "list-remove-symbolic".into(),
                            activate: Box::new(move |_: &mut Self| edit(Edit::Remove(target.clone()))),
                            ..Default::default()
                        }
                        .into(),
                    ],
                    ..Default::default()
                }
                .into()
            })
            .collect();
        if !commands.is_empty() {
            commands.push(MenuItem::Separator);
        }
        commands.push(
            StandardItem {
                label: "Add app…".into(),
                icon_name: "list-add-symbolic".into(),
                enabled: state.config_error.is_none(),
                activate: Box::new(|_: &mut Self| ask_for_command()),
                ..Default::default()
            }
            .into(),
        );
        let enabled = state.enabled;
        vec![
            StandardItem { label: state.summary(), enabled: false, ..Default::default() }.into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Automatic Boost".into(),
                checked: enabled,
                enabled: state.config_error.is_none(),
                activate: Box::new(move |_: &mut Self| edit(Edit::Enable(!enabled))),
                ..Default::default()
            }
            .into(),
            SubMenu {
                label: "Watched apps".into(),
                enabled: state.config_error.is_none(),
                submenu: commands,
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open Configuration File".into(),
                icon_name: "document-edit-symbolic".into(),
                activate: Box::new(|_: &mut Self| open_config()),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Start at Login".into(),
                checked: autostart_enabled(),
                activate: Box::new(|_: &mut Self| {
                    if let Err(error) = set_autostart(!autostart_enabled()) {
                        notify(&format!("Couldn't change the login item: {error}"));
                    }
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit-symbolic".into(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.events.send(Event::Stop);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_decode() {
        for profile in [Profile::Performance, Profile::Balanced, Profile::PowerSaver] {
            let icons = pixmaps(profile);
            assert_eq!(icons.len(), 3);
            for icon in icons {
                assert_eq!(icon.data.len(), (icon.width * icon.height * 4) as usize);
                // Glyphs are white on transparent: some pixels opaque, some not.
                assert!(icon.data.chunks(4).any(|pixel| pixel[0] == 255));
                assert!(icon.data.chunks(4).any(|pixel| pixel[0] == 0));
            }
        }
    }
}
