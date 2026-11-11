//! brrr: full power while something heavy is running.
//!
//! The shared logic lives in `core`. Each operating system has its own
//! module with a native process watcher, power backend and status icon, and
//! is built into its own binary.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod core;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as platform;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
use macos as platform;

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
compile_error!("brrr supports Linux, Windows and macOS");

use crate::core::Command;

fn main() {
    platform::prepare_console();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let status = match core::parse(&args, platform::UI_BY_DEFAULT) {
        None => {
            eprintln!("{}", core::USAGE);
            2
        }
        Some(Command::Help) => {
            println!("{}", core::USAGE);
            0
        }
        Some(Command::Edit(edit)) => {
            core::config::migrate();
            core::run_edit(&edit)
        }
        Some(Command::Install) => platform::install(),
        Some(Command::Uninstall) => platform::uninstall(),
        Some(Command::Run { supervised, poll, ui }) => {
            core::config::migrate();
            match core::instance::acquire() {
                // Keep the lock file open for the life of the process.
                Some(_lock) => platform::run(supervised, poll, ui),
                None => {
                    eprintln!("brrr: another brrr instance is already running");
                    platform::already_running(ui);
                    core::instance::EXIT_BUSY
                }
            }
        }
    };
    std::process::exit(status);
}
