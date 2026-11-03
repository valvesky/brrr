#!/bin/bash
# First-run setup. Only the fixed package-install command may run as root.
set -euo pipefail

[[ $EUID -ne 0 ]] || { echo 'Run Omapower setup as your normal user.' >&2; exit 1; }
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
data=${XDG_DATA_HOME:-$HOME/.local/share}/omapower
config=${XDG_CONFIG_HOME:-$HOME/.config}
unit=$config/systemd/user/omapower.service

missing_packages() {
  local package
  for package in base-devel linux-api-headers glib2 power-profiles-daemon; do
    /usr/bin/pacman -Q "$package" >/dev/null 2>&1 || printf '%s\n' "$package"
  done
}

case ${1:-} in
  --check)
    # Setup is currently supported on Arch/Omarchy only.
    supported=false
    [[ -x /usr/bin/pacman ]] && supported=true
    ready=false
    [[ -x $data/omapower && -f $unit && -f $data/setup-complete ]] && ready=true
    packages=()
    if $supported; then mapfile -t packages < <(missing_packages); fi
    printf '{"ready":%s,"supported":%s,"missing":[' "$ready" "$supported"
    separator=''
    for package in "${packages[@]}"; do printf '%s"%s"' "$separator" "$package"; separator=,; done
    printf ']}\n'
    ;;
  --install-deps)
    [[ -x /usr/bin/pacman ]] || { echo 'Automatic dependency installation requires Arch Linux.' >&2; exit 1; }
    mapfile -t packages < <(missing_packages)
    if (( ${#packages[@]} )); then
      # pkexec supplies the system authentication UI (fingerprint/password).
      # Never elevate a script, build, shell, or path from the checkout.
      /usr/bin/pkexec /usr/bin/pacman -S --needed --noconfirm "${packages[@]}"
    fi
    ;;
  --setup)
    [[ -x /usr/bin/pacman ]] || { echo 'Automatic setup requires Arch Linux.' >&2; exit 1; }
    [[ -z $(missing_packages) ]] || { echo 'Install the missing dependencies first.' >&2; exit 1; }
    # Serialize setup attempts from multiple widget instances.
    mkdir -p -- "$data" "$(dirname -- "$unit")"
    exec 9>"$data/setup.lock"
    flock -n 9 || { echo 'Another setup is already running.' >&2; exit 1; }
    binary_tmp=$(mktemp "$data/.omapower.XXXXXX")
    unit_tmp=$(mktemp "$(dirname -- "$unit")/.omapower.XXXXXX")
    trap 'rm -f -- "$binary_tmp" "$unit_tmp"' EXIT
    # Build outside the checkout: omarchy-shell reloads the plugin when its
    # directory changes, which kills this script mid-setup.
    OMAPOWER_OUT=$binary_tmp make -C "$root" -B omapower
    chmod 755 -- "$binary_tmp"
    # Quote systemd ExecStart, including literal percent specifiers.
    executable=$data/omapower
    [[ $executable != *$'\n'* && $executable != *$'\r'* ]] || exit 1
    executable=${executable//\\/\\\\}
    executable=${executable//\"/\\\"}
    executable=${executable//%/%%}
    executable=${executable//\$/\$\$}
    printf '[Unit]\nDescription=Omapower process-based power profile holds\nAfter=graphical-session.target\n\n[Service]\nExecStart="%s"\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n' "$executable" > "$unit_tmp"
    chmod 644 "$unit_tmp"
    rm -f -- "$data/setup-complete"
    mv -f -- "$binary_tmp" "$data/omapower"
    mv -f -- "$unit_tmp" "$unit"
    systemctl --user daemon-reload
    systemctl --user enable omapower.service
    systemctl --user restart omapower.service
    touch -- "$data/setup-complete"
    ;;
  *) echo 'Usage: bash setup.sh --check|--install-deps|--setup' >&2; exit 2 ;;
esac
