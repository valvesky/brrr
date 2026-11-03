#!/bin/sh
# The legacy systemd unit from setup.sh is retired once; anything else is kept.
set -eu
script=$(realpath "$(dirname "$0")/../bin/retire-legacy-service")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin"
cat > "$tmp/bin/systemctl" <<'EOF'
#!/bin/sh
echo "$*" >> "$CALLS"
[ ! -e "$FAIL" ]
EOF
chmod 755 "$tmp/bin/systemctl"
export PATH="$tmp/bin:$PATH" HOME="$tmp/home" CALLS="$tmp/calls" FAIL="$tmp/fail"
# Characters setup.sh escaped for systemd: \ " % $
export XDG_CONFIG_HOME="$tmp/home/.config" XDG_DATA_HOME="$tmp/home/sh\\a\"r%e\$d"
data="$XDG_DATA_HOME/omapower"
escaped="$tmp/home/sh\\\\a\\\"r%%e\$\$d/omapower/omapower"
unit="$XDG_CONFIG_HOME/systemd/user/omapower.service"
config="$XDG_CONFIG_HOME/omapower/config.json"

# Nothing installed: no systemd calls.
"$script"
[ ! -e "$CALLS" ]

# A hand-written standalone unit is not ours to remove.
mkdir -p "${unit%/*}" "$data" "${config%/*}"
printf '[Service]\nExecStart=/opt/omapower/omapower\n' > "$unit"
"$script"
[ -f "$unit" ] && [ ! -e "$CALLS" ]

# The setup.sh unit: systemd unreachable keeps everything for a retry.
printf '[Service]\nExecStart="%s"\n' "$data/omapower" > "$unit"
"$script"
[ ! -e "$CALLS" ] # unescaped: not what setup.sh wrote
printf '[Service]\nExecStart="%s"\n' "$escaped" > "$unit"
: > "$data/omapower"; : > "$data/setup-complete"; : > "$data/setup.lock"
printf '{"commands":["make"]}' > "$config"
touch "$FAIL"
if "$script" 2>/dev/null; then exit 1; fi
[ -f "$unit" ] && [ -f "$data/omapower" ]

# Success: stopped, removed, reloaded once; the command list stays.
rm -f "$FAIL" "$CALLS"
"$script" 2>/dev/null
[ "$(cat "$CALLS")" = "$(printf -- '--user disable --now omapower.service\n--user daemon-reload')" ]
[ ! -e "$unit" ] && [ ! -e "$data" ]
[ "$(cat "$config")" = '{"commands":["make"]}' ]

# Second run is a no-op.
rm -f "$CALLS"
"$script"
[ ! -e "$CALLS" ]
printf 'legacy service retirement tests passed\n'
