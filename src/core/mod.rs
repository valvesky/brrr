//! Shared by every platform: configuration, the hold state machine, the
//! configuration watcher, the single-instance lock and the command line.

pub mod config;
pub mod config_watch;
pub mod engine;
pub mod instance;

use config::Edit;

/// The command line every platform accepts.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Edit(Edit),
    /// Run the watcher. `ui` selects the platform's tray/menu bar icon.
    Run { supervised: bool, poll: bool, ui: bool },
    Install,
    Uninstall,
    Help,
}

pub const USAGE: &str = "\
Usage: omapower [--tray | --headless] [--poll]
       omapower --supervised [--poll]
       omapower --add NAME | --remove NAME | --enable | --disable
       omapower --install | --uninstall

  --tray        show the status icon (the default on Windows and macOS)
  --headless    run without an icon (the default on Linux, for systemd)
  --supervised  report state as JSON lines on stdout; exit when stdin closes
  --poll        scan processes on a timer instead of using kernel events
  --install     start at login (and, on macOS, allow the power mode switch)
  --uninstall   undo --install";

pub fn parse(args: &[String], ui_by_default: bool) -> Option<Command> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--add", name] => return Some(Command::Edit(Edit::Add(name.to_string()))),
        ["--remove", name] => return Some(Command::Edit(Edit::Remove(name.to_string()))),
        ["--enable"] => return Some(Command::Edit(Edit::Enable(true))),
        ["--disable"] => return Some(Command::Edit(Edit::Enable(false))),
        ["--install"] => return Some(Command::Install),
        ["--uninstall"] => return Some(Command::Uninstall),
        ["--help" | "-h"] => return Some(Command::Help),
        _ => {}
    }
    let (mut supervised, mut poll, mut tray, mut headless) = (false, false, false, false);
    for arg in args {
        let flag = match arg {
            "--supervised" => &mut supervised,
            "--poll" => &mut poll,
            "--tray" => &mut tray,
            "--headless" => &mut headless,
            _ => return None,
        };
        if *flag {
            return None;
        }
        *flag = true;
    }
    if [supervised, tray, headless].iter().filter(|set| **set).count() > 1 {
        return None;
    }
    Some(Command::Run { supervised, poll, ui: tray || (ui_by_default && !supervised && !headless) })
}

/// Run a configuration edit from the command line; returns the exit status.
pub fn run_edit(edit: &Edit) -> i32 {
    match config::edit(edit) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("omapower: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn command_line() {
        assert_eq!(parse(&args(&[]), false), Some(Command::Run { supervised: false, poll: false, ui: false }));
        assert_eq!(parse(&args(&[]), true), Some(Command::Run { supervised: false, poll: false, ui: true }));
        assert_eq!(
            parse(&args(&["--supervised", "--poll"]), true),
            Some(Command::Run { supervised: true, poll: true, ui: false })
        );
        assert_eq!(parse(&args(&["--add", "make"]), false), Some(Command::Edit(Edit::Add("make".into()))));
        assert_eq!(parse(&args(&["--poll", "--poll"]), false), None);
        assert_eq!(parse(&args(&["--enable", "extra"]), false), None);
        assert_eq!(parse(&args(&["--tray", "--supervised"]), false), None);
        assert_eq!(parse(&args(&["--add"]), false), None);
    }
}

/// The `--supervised` frontend: one JSON line per state change on stdout.
/// Returns false once stdout is closed, so the engine exits.
pub fn supervised_frontend() -> engine::Frontend {
    use std::io::Write;
    let mut last = String::new();
    Box::new(move |state: &engine::State| {
        let json = state.to_json();
        if json == last {
            return true;
        }
        last = json;
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "{last}").and_then(|()| stdout.flush()).is_ok()
    })
}

/// Supervised mode: the supervisor holds our stdin. EOF means it is gone.
pub fn watch_stdin(events: engine::Events) {
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buffer = [0u8; 256];
        let mut stdin = std::io::stdin();
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        eprintln!("omapower: supervisor closed the pipe; exiting");
        let _ = events.send(engine::Event::Stop);
    });
}
