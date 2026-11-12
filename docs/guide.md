# brrr guide

[← Back to the README](../README.md)

brrr is the sound your fans make. It switches the machine to its performance mode while something
heavy is running — a game, a zip, a video export, a compile — and puts the previous mode back when
that work finishes.

It is one program, built separately for each operating system. The rules are shared: the
configuration file, which process names count, and when to boost, hold back or let go. Each system
uses its own native pieces to watch processes, switch the power mode and show an icon.

## Get started

### Windows

Download `brrr-windows-setup.exe` from the
[releases](https://github.com/valvesky/brrr/releases) and run it. One setup file covers x64 and
ARM PCs. It installs for your account only, under `%LOCALAPPDATA%\Programs\brrr`, so it needs
no administrator rights. It adds brrr to the Start menu and, unless you untick it, starts
brrr whenever you sign in. Its icon appears in the notification area (you may have to drag it
out of the overflow). *Start at login* in its menu turns the sign-in start on or off later.

brrr isn't code-signed yet, so Microsoft Defender SmartScreen may warn about it:

- **Edge says the file isn't commonly downloaded:** open the download's **…** menu, choose
  **Keep**, then **Show more** › **Keep anyway**.
- **Edge blocks it with no Keep option:** download it in PowerShell instead, with
  `curl.exe -LO https://github.com/valvesky/brrr/releases/latest/download/brrr-windows-setup.exe`.
- **Windows says *Windows protected your PC* when you run it:** choose **More info** ›
  **Run anyway**.

Every release is built from this repository's source by the
[Release workflow](https://github.com/valvesky/brrr/actions/workflows/release.yml).

Prefer no installer? `brrr-windows-x86_64.exe` and `brrr-windows-aarch64.exe` are the same
program on their own: put one somewhere it can stay and run it, and use *Start at login* (or
`brrr --install`).

It switches the *Power mode* you find under Settings › System › Power & battery, the same setting
as the slider, and needs no administrator rights. Windows 10 (1809 or later) and Windows 11 are
supported. The icon follows the taskbar's light or dark theme and shows the current mode.

To remove it, uninstall *brrr* from Settings › Apps. The uninstaller quits it first, so it can
put back the power mode, and removes the sign-in entry. Your app list stays in
`%APPDATA%\brrr`. Without the installer: turn off *Start at login*, choose *Quit* and delete
the file.

Upgrading from omapower uses the same installer entry. The setup asks the old tray icon to quit
so it can put the power mode back, then installs brrr. A sign-in entry named Omapower is removed.

### Linux

brrr needs [power-profiles-daemon](https://gitlab.freedesktop.org/upower/power-profiles-daemon)
or tuned-ppd, which provides the same interface. Most desktop distributions include one of them;
`powerprofilesctl` tells you whether it runs.

Download `brrr-linux-x86_64.tar.gz` or `brrr-linux-aarch64.tar.gz` from the
[releases](https://github.com/valvesky/brrr/releases). The binary is statically linked, so it
runs on any distribution. Put it on your `PATH` as `brrr`, then:

```sh
brrr --install   # show the tray icon at every login, from now on (and add its app icon)
brrr --tray &    # and right now
```

The tray icon is a StatusNotifierItem: KDE Plasma, waybar, Xfce, the Omarchy shell and GNOME with
the AppIndicator extension all show it, drawn and themed by the desktop itself. Its menu has the
switch, the watched apps and *Add app…*, which asks with `zenity` or `kdialog` when one is
installed (otherwise it opens the configuration file). `brrr --uninstall` removes the login
item. If an `omapower.desktop` login item is still there, the first run replaces it.

**Without a tray**, run brrr as a systemd user service instead. Create
`~/.config/systemd/user/brrr.service`:

```ini
[Unit]
Description=brrr: full power while something heavy is running
After=graphical-session.target

[Service]
ExecStart=/absolute/path/to/brrr
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now brrr
```

Without arguments brrr runs headless, so a unit that starts the `brrr` binary with no arguments
still has no icon. If you wrote an `omapower.service` yourself, point ExecStart at `brrr`. To
stop, run `systemctl --user disable --now brrr`, remove the unit file and run
`systemctl --user daemon-reload`.

### macOS

Download `brrr-macos.dmg` from the [releases](https://github.com/valvesky/brrr/releases),
open it and drag *brrr* onto *Applications*, then open it from there. The app isn't notarized,
so the first time macOS asks: right-click it, choose *Open*, then *Open* again (or allow it under
System Settings › Privacy & Security). It runs as a menu bar icon, without a Dock icon. The app
is universal: one download for Apple Silicon and Intel Macs.

macOS has no setting an app can change without administrator rights: Low Power Mode and High
Power Mode are switched with `pmset`, which needs root. Choose *Allow Power Mode Switching…* from
the menu and enter your password once. brrr installs a sudoers rule
(`/etc/sudoers.d/brrr`) that allows only these exact commands, for your user only:

```
/usr/bin/pmset -b lowpowermode [01], /usr/bin/pmset -c lowpowermode [01],
/usr/bin/pmset -b powermode [012],   /usr/bin/pmset -c powermode [012]
```

*Start at Login* (or `brrr --install`, which also installs the rule) adds a LaunchAgent,
`~/Library/LaunchAgents/com.brrr.agent.plist`. `brrr --uninstall` removes both, asking for
your password again.

On macOS, boosting means turning off Low Power Mode for the current power source (battery or
charger) while a heavy app is running and, on MacBook Pros that have it, turning on High Power
Mode. If neither applies, your Mac is already at full power and brrr has nothing to change. An
existing `/etc/sudoers.d/omapower` rule still allows the same `pmset` commands; a new install
writes `/etc/sudoers.d/brrr` with that same rule. `brrr --uninstall` removes both files.
If Start at Login still uses `com.omapower.agent`, the first run moves it to `com.brrr.agent`.

### Omarchy

```sh
omarchy plugin add https://github.com/valvesky/brrr.git --enable
```

That's all. After Omarchy's confirmation, automatic boost starts: the plugin ships prebuilt static
binaries for x86_64 and aarch64, so there is nothing to build and no packages to install. The
widget's icons come from the bar's Nerd Font.

On other architectures, run `make` in `~/.config/omarchy/plugins/raw.brrr` (it needs
[Rust](https://rustup.rs)), then restart the Omarchy shell: the plugin uses that build when no
prebuilt binary fits.

The plugin's background service runs one brrr for as long as the Omarchy shell runs, however
many bars show the widget. The widget shows whether automatic boost is on, its status and the
watched apps. Its switch turns automatic boost on or off; the setting is saved in your
configuration and kept across restarts. Plugin updates replace and restart brrr.

If you installed an early version that used `setup.sh`, the plugin retires that systemd user
service once on first start (stopping it, removing its unit and `~/.local/share/omapower`). Your
app list is kept, and the first run of brrr copies it into the new configuration folder.

Remove the plugin with `omarchy plugin remove raw.brrr`; brrr stops with it.

## Choose commands

By default, brrr watches common heavy apps and the usual compilers. On every system: `7z`,
`ffmpeg`, `blender`, `obs`, plus `rustc`, `cargo`, `gcc`, `g++`, `cc`, `c++`, `cc1`, `cc1plus`,
`clang`, `clang++`, `zig`, `go`, `javac`, `swiftc`, `swift-frontend`, `gfortran`, `flang`, `nvcc`
and `ghc`. GCC's `cc1` and `cc1plus` workers are included so compilation stays covered
independently of the driver.

Each system adds the process names used there:

- **Windows:** `7zG`, `WinRAR`, `HandBrake`, `obs64`, `Resolve`, `AfterFX` (After Effects),
  `Photoshop`, and the compilers `cl` and `msbuild`.
- **macOS:** `HandBrake`, `Resolve`, `Blender` (the app's capitalized name, as well as `blender`),
  and the compilers `xcodebuild` and `swift-build`.
- **Linux:** `7zz`, `ghb` (HandBrake), `HandBrakeCLI` and `resolve`.

Adobe Premiere Pro's process name (`Adobe Premiere Pro`) is 18 bytes, past the 15-byte limit, so
it cannot be watched. After Effects and Photoshop use the shorter names above.

Defaults apply only when the configuration file is missing; existing lists (including empty
lists) are left unchanged.

Add or remove names from the icon's menu or the Omarchy widget, or from the command line:

```sh
brrr --add NAME
brrr --remove NAME
brrr --disable   # or --enable
```

(In Omarchy the binary is `~/.config/omarchy/plugins/raw.brrr/bin/brrr`.)

Names are process names, not paths or command-line arguments: 1–15 bytes, without slashes or
newlines. On Linux they must match the process's `comm` name exactly; on macOS its name; on Windows
the program's file name without `.exe`, ignoring case (`msbuild` matches `MSBuild.exe`).

The list lives in `config.json`:

| System  | Location |
| ------- | -------- |
| Linux   | `~/.config/brrr/config.json` |
| macOS   | `~/Library/Application Support/brrr/config.json` |
| Windows | `%APPDATA%\brrr\config.json` |

If `XDG_CONFIG_HOME` is set, `$XDG_CONFIG_HOME/brrr/config.json` is used on every system.

The first time brrr runs, if the old omapower folder has a `config.json` and the brrr one does
not, brrr copies it. The same one-time copy applies to `windows-power-restore.json` and
`macos-power-restore.json`, on every system, and only when the new file is missing. Nothing you
already have under brrr is overwritten. A copied list keeps the names you chose; the extra default
apps are used only when there was no configuration file. The old folder is left in place.

```json
{"commands": ["rustc", "cargo"], "enabled": true}
```

`enabled` is optional and defaults to `true`; the switch sets it. Changes are picked up
immediately, without polling. While the file is invalid, brrr watches nothing (releasing any
boost), the icon reports the error and edits are refused; fix the file by hand and it recovers
without a restart. brrr serializes its own edits and keeps any other members of the file
exactly as written, but external editors aren't locked out.

## How it works

brrr watches the apps you start, however you start them (a game, HandBrake, `7z`, a compile).
While at least one runs and automatic boost is on, it switches to the performance mode;
when the last one exits, it puts back what was there before. The power mode is system-wide, so
while brrr boosts, the whole machine runs at full power.

A manual change that undoes the boost is respected until the matching processes finish. If the
system refuses the performance mode (some machines and virtual machines have none), brrr
doesn't retry until the matching processes finish, and the icon says so. Only one brrr runs per
user: whichever starts second waits briefly, then exits (status 75), and the Omarchy widget reports
that another instance is running.

### Linux

brrr sees processes in its PID namespace that are owned by your user, plus processes running
as another user (such as root through `sudo`) that carry your login UID, which the kernel keeps
across `sudo` and `su`. Other users' processes and system services don't match. It uses kernel
process events when available, backed by a recovery scan every five seconds to repair missed
events, and watches matched processes through pidfds. Reported stream errors or overflows trigger
an immediate recovery scan. When events aren't available it scans every 500 ms instead;
`brrr --poll` forces that mode. Very short-lived processes may still be missed. You don't need
root or extra capabilities.

It asks power-profiles-daemon for a *performance hold*. Holds belong to brrr's system D-Bus
connection, so the daemon drops them whenever brrr exits, however it exits. Other
applications' holds and system policy can also affect the effective profile. brrr handles
daemon restarts and retries failed releases; after an unrecoverable error it exits, and the
plugin service (or systemd) restarts it within five seconds.

Under the Omarchy shell, brrr runs with `--supervised`: it reports its state as JSON lines on
stdout, and exits when its stdin pipe closes, so it never outlives the shell. The plugin service
writes the latest state to `$XDG_RUNTIME_DIR/brrr-status.json` for the widgets. Any other bar
can use the same protocol.

For a command you launch yourself, `powerprofilesctl` offers a simpler alternative that acquires
the hold before the command starts:

```sh
powerprofilesctl launch --profile performance -- your-heavy-app
```

### Windows

brrr checks the process list once a second and counts the programs running in your own logon
session, including ones you started as administrator. It switches the *Power mode* overlay to
*Best performance* (on the Balanced plan, which nearly every Windows 10 and 11 PC uses). On another
plan it switches to High performance if that plan exists, and does nothing on High or Ultimate
performance, which already are. It watches the effective power mode, so a change you make
yourself is noticed.

Unlike power-profiles-daemon's holds, a Windows power mode stays as it was set if a program
crashes. So while it boosts, brrr writes what it has to put back to `windows-power-restore.json` next to the
configuration, and the next start restores it if the mode is still the one brrr set.

### macOS

brrr checks the process list once a second and counts your own processes. It reads the
current power source and mode with `pmset -g` and changes only the setting for the current power
source, through the sudoers rule above. Like on Windows, it keeps a `macos-power-restore.json` next to the configuration while it boosts,
so a crash is repaired at the next start.

## Diagnostics

brrr logs to stderr: process event subscription or fallback, changes to detected commands and
boost state, boost requests and results, power service changes and manual releases. Quiet
recovery scans produce no output; scans that repair process matches report the correction. A
correction means the snapshot disagreed with tracking, not proof of a lost kernel event: process
exits and renames can race with a scan. Stream-loss notices are rate-limited to one per five
seconds, with a cumulative recovery count.

Under the Omarchy shell, these lines appear in the shell's journal at debug level; in the systemd
service, in the service's:

```sh
journalctl --user -t omarchy-shell -b -f | grep brrr   # plugin
journalctl --user -u brrr -b -f                        # systemd service
```

On Windows and macOS, quit the icon and run the binary from a terminal (`brrr --tray`, or
`/Applications/brrr.app/Contents/MacOS/brrr`) to see the same lines.

A detected command with no boost request points to suppression or an unavailable power service; a
request without a result points to trouble with the service. `suppressed:true` means a manual
change is being respected until all matching processes finish. `rejected:true` means the system
refused the performance mode (the reason is logged as `performance hold rejected:`); brrr tries
again once all matching processes have finished.

For per-process event and match tracing on Linux, stop the running brrr (disable the plugin, or
stop the service) and run it in the foreground:

```sh
G_MESSAGES_DEBUG=brrr brrr
```

Verbose tracing includes process IDs, matching names, event sources, and pidfd availability, but
not command-line arguments. It can be noisy while lots of processes start and exit; leave it off for normal use.

## Build and test

brrr is a single Rust package (Rust 1.89 or newer). `src/core` holds everything shared: the
configuration, the boost state machine, the configuration watcher, the single-instance lock and
the command line. `src/linux`, `src/windows` and `src/macos` each hold that system's process
watcher, power backend and status icon. Building on a system builds that system's binary:

```sh
cargo build --release      # target/release/brrr
```

`scripts/build-release.sh` builds the release packages, each on its own system:

- `linux`: static (musl) Linux binaries, used by the Omarchy plugin too.
- `windows`: the Windows executable for this machine's architecture.
- `windows-installer`: `brrr-windows-setup.exe` from both Windows executables, with
  [Inno Setup](https://jrsoftware.org/isinfo.php) 6.3 or newer. The script is
  `installer/windows/brrr.iss`.
- `macos`: a universal app bundle with its icon, and the `brrr-macos.dmg` disk image.

`make test` runs the Rust unit tests, the command-line and JSON compatibility tests, the widget's
JavaScript tests and the isolated D-Bus integration tests on Linux, including a headless Quickshell
run of the real plugin service and widget. These require `jq`, Node.js, `dbus-run-session`, a C
compiler (for a small test helper), Quickshell and Python with PyGObject (`python-gobject` on Arch;
override `PYTHON` if needed). The integration tests use a mock power-profiles-daemon; check the
widget's look in Omarchy separately. On Windows and macOS, `cargo test` runs the unit tests.

### Prebuilt binaries

`bin/brrr` picks `bin/brrr-x86_64` or `bin/brrr-aarch64` for the machine. The
[Binaries workflow](../.github/workflows/binaries.yml) rebuilds and tests both whenever the
sources change on `master`, checks that Windows and macOS still build, and commits the Linux
binaries for the Omarchy plugin. The [Release workflow](../.github/workflows/release.yml) builds
every platform on that platform when a `v*` tag is pushed, builds the Windows installer from both
Windows binaries, and attaches the results to a GitHub release.

The app icons are Font Awesome's bolt (CC BY 4.0), like the tray icons, drawn as SVG in
`assets/icon`: `brrr.svg` is the flat icon for Windows and Linux, and `brrr-macos.svg` the
glass one for macOS. The picture at the top of the README, `assets/logo.png`, is separate art by
[@monky_sm](https://twitter.com/monky_sm/status/1744264103558779075) and is not covered by the MIT
license or by the Font Awesome license. After changing an app icon, render it again:

```sh
resvg -w 1024 -h 1024 assets/icon/brrr.svg assets/icon/brrr-1024.png
resvg -w 1024 -h 1024 assets/icon/brrr-macos.svg assets/icon/brrr-macos-1024.png
for size in 16 24 32 48 64 128 256; do resvg -w $size -h $size assets/icon/brrr.svg icon-$size.png; done
magick icon-256.png icon-128.png icon-64.png icon-48.png icon-32.png icon-24.png icon-16.png assets/icon/brrr.ico
```
