//! Explicit, one-time authorization and the per-user launch agent.

use std::ffi::CStr;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const LABEL: &str = "com.omapower.agent";

pub fn report(result: Result<(), String>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("omapower: {error}");
            1
        }
    }
}

pub fn output(command: &mut Command) -> Result<String, String> {
    let output = command
        .output()
        .map_err(|error| format!("{}: {error}", command.get_program().to_string_lossy()))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(command_error(&output))
    }
}

fn command_error(output: &Output) -> String {
    let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if error.is_empty() {
        format!("command exited with {}", output.status)
    } else {
        error
    }
}

fn valid_user(user: &str) -> bool {
    !user.is_empty()
        && user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}

/// Read the account database rather than trusting USER, which is inherited
/// from the launching environment and need not identify the current user.
fn username() -> Result<String, String> {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let mut buffer = vec![0u8; 16384];
    loop {
        let mut passwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: All output pointers and the scratch buffer are valid; the
        // returned strings are read before that buffer is resized or dropped.
        let error = unsafe {
            libc::getpwuid_r(
                uid,
                passwd.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if error == libc::ERANGE && buffer.len() < 1_048_576 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if error != 0 || result.is_null() {
            return Err("cannot determine the current account name".into());
        }
        // SAFETY: getpwuid_r succeeded and returned a non-null initialized entry.
        let passwd = unsafe { passwd.assume_init() };
        if passwd.pw_name.is_null() {
            return Err("the current account has no name".into());
        }
        // SAFETY: pw_name points to a terminated string in the live scratch buffer.
        let user = unsafe { CStr::from_ptr(passwd.pw_name) }
            .to_str()
            .map_err(|_| "the account name is not UTF-8")?
            .to_string();
        if !valid_user(&user) {
            return Err(
                "the account name must contain only A–Z, a–z, 0–9, dot, underscore or hyphen"
                    .into(),
            );
        }
        return Ok(user);
    }
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

fn applescript_string(text: &str) -> String {
    format!(
        "\"{}\"",
        text.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', "\\n")
            .replace('\r', "\\r")
    )
}

fn rule(user: &str) -> Result<String, String> {
    if !valid_user(user) {
        return Err("invalid account name for sudoers".into());
    }
    Ok(format!(
        "{user} ALL=(root) NOPASSWD: /usr/bin/pmset -b lowpowermode [01], /usr/bin/pmset -c lowpowermode [01], /usr/bin/pmset -b powermode [012], /usr/bin/pmset -c powermode [012]"
    ))
}

fn authorization_script(user: &str) -> Result<String, String> {
    let rule = shell_quote(&rule(user)?);
    // The temporary file lives in the root-owned destination directory, and
    // its dotted name is ignored by sudo until validation and atomic rename.
    Ok(format!(
        "set -eu; /bin/mkdir -p /etc/sudoers.d; temporary=$(/usr/bin/mktemp /etc/sudoers.d/.omapower.XXXXXX); trap '/bin/rm -f \"$temporary\"' EXIT; /usr/bin/printf '%s\\n' {rule} > \"$temporary\"; /usr/sbin/chown root:wheel \"$temporary\"; /bin/chmod 0440 \"$temporary\"; /usr/sbin/visudo -cf \"$temporary\"; /bin/mv -f \"$temporary\" /etc/sudoers.d/omapower; trap - EXIT"
    ))
}

fn administrator(script: &str) -> Result<(), String> {
    let expression = format!(
        "do shell script {} with administrator privileges",
        applescript_string(script)
    );
    output(Command::new("/usr/bin/osascript").args(["-e", &expression])).map(|_| ())
}

pub fn allow_power_switching() -> Result<(), String> {
    administrator(&authorization_script(&username()?)?)?;
    eprintln!("omapower: installed /etc/sudoers.d/omapower (root:wheel, 0440)");
    Ok(())
}

fn agent_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents/com.omapower.agent.plist"))
}

pub fn login_enabled() -> bool {
    agent_path().is_ok_and(|path| path.exists())
}

fn xml(text: &str) -> Result<String, String> {
    if text.chars().any(|ch| !matches!(ch, '\t' | '\n' | '\r' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}')) {
        return Err("the executable path contains a character XML cannot represent".into());
    }
    Ok(text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}

fn plist(executable: &str) -> Result<String, String> {
    let executable = xml(executable)?;
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>Label</key><string>{LABEL}</string>\n\
         <key>ProgramArguments</key><array><string>{executable}</string></array>\n\
         <key>RunAtLoad</key><true/>\n\
         <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>\n\
         <key>ProcessType</key><string>Interactive</string>\n\
         </dict></plist>\n"
    ))
}

fn domain() -> String {
    // SAFETY: getuid has no preconditions.
    format!("gui/{}", unsafe { libc::getuid() })
}

pub fn set_login(enabled: bool) -> Result<(), String> {
    let path = agent_path()?;
    if enabled {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let contents = plist(
            executable
                .to_str()
                .ok_or("the executable path is not UTF-8")?,
        )?;
        super::write_atomic(&path, contents.as_bytes())?;
        let result = Command::new("/bin/launchctl")
            .args(["bootstrap", &domain()])
            .arg(&path)
            .output()
            .map_err(|error| format!("launchctl bootstrap: {error}"))?;
        if !result.status.success() {
            // launchctl commonly reports an already loaded job as EIO, so
            // inspect the job as well as accepting its explicit diagnostic.
            let message = command_error(&result);
            let loaded = Command::new("/bin/launchctl")
                .args(["print", &format!("{}/{LABEL}", domain())])
                .output()
                .is_ok_and(|output| output.status.success());
            if !loaded && !message.to_ascii_lowercase().contains("already loaded") {
                return Err(format!("launchctl bootstrap: {message}"));
            }
        }
        eprintln!("omapower: enabled Start at Login ({})", path.display());
        Ok(())
    } else {
        // Removing the plist comes first: bootout may send SIGTERM to this
        // very process when the toggle is used from the launch agent's menu.
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("remove {}: {error}", path.display())),
        }
        eprintln!("omapower: disabled Start at Login ({})", path.display());
        // Check whether there is a job first: an absent job is already stopped.
        let service = format!("{}/{LABEL}", domain());
        let loaded = Command::new("/bin/launchctl")
            .args(["print", &service])
            .output()
            .map_err(|error| format!("launchctl print: {error}"))?
            .status
            .success();
        if loaded {
            output(Command::new("/bin/launchctl").args(["bootout", &service])).map(|_| ())
        } else {
            Ok(())
        }
    }
}

pub fn install() -> Result<(), String> {
    allow_power_switching()?;
    set_login(true)
}

pub fn uninstall() -> Result<(), String> {
    // Complete both independent removals even if either fails.
    let agent = set_login(false);
    if let Err(error) = &agent {
        eprintln!("omapower: {error}");
    }
    let rule = administrator("/bin/rm -f /etc/sudoers.d/omapower");
    match &rule {
        Ok(()) => eprintln!("omapower: removed /etc/sudoers.d/omapower"),
        Err(error) => eprintln!("omapower: remove sudoers rule: {error}"),
    }
    agent.and(rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_validation_and_restricted_rule() {
        for name in ["raw", "some.user-1_2", "a"] {
            let rule = rule(name).unwrap();
            assert_eq!(rule.matches("/usr/bin/pmset").count(), 4);
            assert!(!rule.contains("ALL=NOPASSWD"));
        }
        for name in ["", "a b", "root\nALL", "$(id)", "é", "a:root", "x\\y"] {
            assert!(authorization_script(name).is_err());
        }
        let script = authorization_script("raw").unwrap();
        assert!(script.find("visudo -cf").unwrap() < script.find("/bin/mv -f").unwrap());
        assert!(script.contains("chmod 0440"));
        assert!(script.contains("root:wheel"));
    }

    #[test]
    fn shell_and_applescript_quoting_are_separate_layers() {
        assert_eq!(shell_quote("a'b;$x"), "'a'\"'\"'b;$x'");
        assert_eq!(applescript_string("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn agent_escapes_executable_and_has_restart_policy() {
        let text = plist("/Users/A & B/<build>/omapower\"").unwrap();
        assert!(text.contains("/Users/A &amp; B/&lt;build&gt;/omapower&quot;"));
        assert!(text.contains("<key>SuccessfulExit</key><false/>"));
        assert!(text.contains("<key>ProcessType</key><string>Interactive</string>"));
        assert!(plist("bad\0path").is_err());
    }
}
