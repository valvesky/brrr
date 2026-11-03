# Omapower guide

[← Back to the README](../README.md)

## Get started

You'll need Linux, power-profiles-daemon, a system D-Bus, GLib/GIO and JSON-GLib. To build, install a C compiler, Linux headers, make and pkg-config. The optional widget also needs Omarchy/Quickshell and Font Awesome 7 Free Solid.

Build with `make` and keep the repository at a stable path. Create `~/.config/systemd/user/omapower.service`, replacing the path with the absolute path to your built binary:

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

For the optional widget, link this repository's `plugin` directory to `~/.config/omarchy/plugins/raw.omapower` (or under `$XDG_CONFIG_HOME` if set), then run `omarchy plugin enable raw.omapower`. Click the widget to manage commands or start and stop the service. The toggle doesn't change whether the service starts at login.

After rebuilding, restart the service and reload the shell/plugin. To uninstall, run `systemctl --user disable --now omapower` and remove the service file and plugin link.

## Choose commands

Omapower watches `rustc` and `cargo` by default. Add or remove names in the widget, or use:

```sh
omapower --add NAME
omapower --remove NAME
```

Names must match Linux process `comm` names exactly: 1–15 bytes, without slashes or newlines. They are not paths or command-line arguments. The list lives in `$XDG_CONFIG_HOME/omapower/config.json` (normally `~/.config/omapower/config.json`):

```json
{"commands": ["rustc", "cargo"]}
```

Changes are picked up automatically. An invalid file stops the service and prevents widget edits; fix it manually and check `journalctl --user -u omapower`. Omapower serializes its own edits, but external editors aren't locked out.

## How it works

Omapower watches processes owned by your user in its visible PID namespace. It uses kernel process events when available, backed by a recovery scan every five seconds to repair missed events. Reported stream errors or overflows trigger an immediate recovery scan. When events aren't available it scans every 500 ms instead; `omapower --poll` forces that mode. Very short-lived processes may still be missed. You don't need root or extra capabilities.

A manual profile change that releases Omapower's hold is respected until the matching processes finish. Other applications' holds and system policy can also affect the effective profile. The service handles daemon restarts and retries failed releases; systemd restarts it after an unrecoverable error.

For a command you launch yourself, `powerprofilesctl` offers a simpler alternative that acquires the hold before the command starts:

```sh
powerprofilesctl launch --profile performance -- cargo build
```

## Diagnostics

The service logs event subscription/fallback, changes to detected commands and hold state, hold requests/results, daemon owner changes, and `ProfileReleased` signals. Quiet recovery scans produce no log output; scans that repair process matches report the correction. A correction means the snapshot disagreed with tracking, not proof of a lost kernel event: process exits and renames can race with a scan. Stream-loss notices are rate-limited to one per five seconds, with a cumulative recovery count.

```sh
journalctl --user -u omapower -b -f
```

A detected command with no hold request points to suppression or an unavailable daemon; a request without an acquisition result points to D-Bus/daemon trouble. `suppressed:true` means an external release is being respected until all matching processes finish; the release signal does not identify whether a person or another application caused it.

For detailed per-PID event and match tracing, stop the service and run the binary in the foreground (from this repository):

```sh
systemctl --user stop omapower
G_MESSAGES_DEBUG=omapower ./omapower
# Reproduce the issue, then press Ctrl+C and restore the service:
systemctl --user start omapower
```

Verbose tracing includes process IDs, matching names, event sources, and pidfd availability, but not command-line arguments. It can be noisy during builds; leave it disabled for normal use. Events still cannot guarantee detection of commands that exit before inspection.

## Tests

Run `make test` for the C, CLI, JavaScript and isolated D-Bus integration tests. These require `jq`, Node.js, `dbus-run-session` and Python with PyGObject (`python-gobject` on Arch; override `PYTHON` if needed). The tests use mock power-profile and systemd services; check the widget in Omarchy separately.
