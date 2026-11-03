# Omapower guide

[← Back to the README](../README.md)

## Get started

```sh
omarchy plugin add https://github.com/valvesky/omapower.git --enable
```

That's all. After Omarchy's confirmation, automatic boost starts: the plugin ships
prebuilt helpers for x86_64 and aarch64, so there is nothing to build and no
packages to install on a current Omarchy system (it needs GLib and
power-profiles-daemon, which Omarchy includes). The widget also needs
Font Awesome 7 Free Solid.

The plugin's background service runs one helper for as long as the Omarchy shell
runs, however many bars show the widget. The widget shows whether automatic
boost is on, its status and the watched commands. Its switch turns automatic
boost on or off; the setting is saved in your configuration and kept across
restarts. Plugin updates replace and restart the helper.

If you installed an earlier version that used `setup.sh`, the plugin retires
that systemd user service once on first start (stopping it, removing its unit
and `~/.local/share/omapower`). Your command list is kept.

### Standalone mode (without the shell)

To boost while the Omarchy shell isn't running, or on another desktop, run the
helper as a systemd user service instead. Build it with `make` (needs a C
compiler, Linux headers, make, pkg-config and GLib/GIO development files), keep
the repository at a stable path, and create
`~/.config/systemd/user/omapower.service`:

```ini
[Unit]
Description=Omapower process-based power profile holds
After=graphical-session.target

[Service]
ExecStart=/absolute/path/to/omapower/omapower
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now omapower
```

Only one helper runs per user: whichever starts second waits briefly, then
exits, and the widget reports that another instance is running. Restart the
service after rebuilding.

### Uninstall

Remove the plugin with `omarchy plugin remove raw.omapower`; the helper stops
with it. Your command list remains in `$XDG_CONFIG_HOME/omapower`. For
standalone mode, run `systemctl --user disable --now omapower`, remove the unit
file and run `systemctl --user daemon-reload`.

## Choose commands

By default, Omapower watches `rustc`, `cargo`, `gcc`, `g++`, `cc`, `c++`, `cc1`, `cc1plus`, `clang`, `clang++`, `zig`, `go`, `javac`, `swiftc`, `swift-frontend`, `gfortran`, `flang`, `nvcc`, `ghc`. GCC's `cc1` and `cc1plus` workers are included so compilation stays covered independently of the driver. Defaults apply only when the configuration file is missing; existing command lists (including empty lists) are left unchanged.

Add or remove names in the widget, or use the helper from the plugin directory
(or your own build in standalone mode):

```sh
~/.config/omarchy/plugins/raw.omapower/bin/omapower --add NAME
~/.config/omarchy/plugins/raw.omapower/bin/omapower --remove NAME
~/.config/omarchy/plugins/raw.omapower/bin/omapower --disable   # or --enable
```

Names must match Linux process `comm` names exactly: 1–15 bytes, without slashes or newlines. They are not paths or command-line arguments. The list lives in `$XDG_CONFIG_HOME/omapower/config.json` (normally `~/.config/omapower/config.json`):

```json
{"commands": ["rustc", "cargo"], "enabled": true}
```

`enabled` is optional and defaults to `true`; the widget's switch sets it.
Changes are picked up automatically. While the file is invalid, Omapower
watches nothing (releasing any hold), the widget reports the error and refuses
edits; fix the file by hand and it recovers without a restart. Omapower
serializes its own edits, but external editors aren't locked out.

## How it works

Omapower watches processes owned by your user in its visible PID namespace. It uses kernel process events when available, backed by a recovery scan every five seconds to repair missed events. Reported stream errors or overflows trigger an immediate recovery scan. When events aren't available it scans every 500 ms instead; `omapower --poll` forces that mode. Very short-lived processes may still be missed. You don't need root or extra capabilities.

A manual profile change that releases Omapower's hold is respected until the matching processes finish. Other applications' holds and system policy can also affect the effective profile. The helper handles power-profiles-daemon restarts and retries failed releases; after an unrecoverable error the plugin service (or systemd, in standalone mode) restarts it within five seconds.

Holds belong to the helper's system D-Bus connection, so power-profiles-daemon drops them whenever the helper exits, however it exits. Under the shell, the helper runs with `--supervised`: it reports its state as JSON lines on stdout, and exits when its stdin pipe closes, so it never outlives the shell. The plugin service writes the latest state to `$XDG_RUNTIME_DIR/omapower-status.json` for the widgets.

For a command you launch yourself, `powerprofilesctl` offers a simpler alternative that acquires the hold before the command starts:

```sh
powerprofilesctl launch --profile performance -- cargo build
```

## Diagnostics

The helper logs event subscription/fallback, changes to detected commands and hold state, hold requests/results, daemon owner changes, and `ProfileReleased` signals. Quiet recovery scans produce no log output; scans that repair process matches report the correction. A correction means the snapshot disagreed with tracking, not proof of a lost kernel event: process exits and renames can race with a scan. Stream-loss notices are rate-limited to one per five seconds, with a cumulative recovery count.

Under the shell, these lines appear in the shell's journal at debug level; in standalone mode, in the service's:

```sh
journalctl --user -t omarchy-shell -b -f | grep Omapower   # plugin
journalctl --user -u omapower -b -f                        # standalone
```

A detected command with no hold request points to suppression or an unavailable daemon; a request without an acquisition result points to D-Bus/daemon trouble. `suppressed:true` means an external release is being respected until all matching processes finish; the release signal does not identify whether a person or another application caused it.

For detailed per-PID event and match tracing, stop the running helper (disable the plugin, or stop the standalone service) and run the binary in the foreground:

```sh
G_MESSAGES_DEBUG=omapower ./omapower
```

Verbose tracing includes process IDs, matching names, event sources, and pidfd availability, but not command-line arguments. It can be noisy during builds; leave it disabled for normal use. Events still cannot guarantee detection of commands that exit before inspection.

## Tests

The helper uses a unity build: `omapower.c` includes `config.c` and `process-watch.c`, with shared declarations in `omapower.h`. Compile only `omapower.c`; `make` tracks all included sources and the header for rebuilds.

Run `make test` for the C, CLI, JavaScript and isolated D-Bus integration tests, including a headless Quickshell run of the real plugin service and widget. These require `jq`, Node.js, `dbus-run-session`, Quickshell and Python with PyGObject (`python-gobject` on Arch; override `PYTHON` if needed). The tests use a mock power-profiles-daemon; check the widget's look in Omarchy separately.

## Prebuilt binaries

`bin/omapower` picks `bin/omapower-x86_64` or `bin/omapower-aarch64` for the machine. The [Binaries workflow](../.github/workflows/binaries.yml) rebuilds both with [`scripts/build-release.sh`](../scripts/build-release.sh) whenever the C sources change on `master`, and commits them. Builds run on Ubuntu 24.04 without CPU-specific flags and target the GLib 2.80 API, so the helpers run on any system with glibc 2.34 and GLib 2.80 or newer.
