#!/bin/sh
# Build the prebuilt helper for the current architecture into bin/.
# Used by CI on Ubuntu 24.04 so binaries need only glibc 2.39 and GLib 2.80,
# well below any current Omarchy (Arch) system. No -march: generic CPUs.
set -eu
cd "$(dirname -- "$0")/.."
arch=$(uname -m)
out=bin/omapower-$arch
glib='-DGLIB_VERSION_MIN_REQUIRED=GLIB_VERSION_2_80 -DGLIB_VERSION_MAX_ALLOWED=GLIB_VERSION_2_80'
OMAPOWER_OUT=$out make -B omapower CFLAGS="-O2 -Wall -Wextra -Wpedantic -Werror $glib"
strip "$out"
chmod 755 "$out"
# Argument errors exit 2 before touching the session: a cheap smoke test.
status=0
"$out" --bogus 2>/dev/null || status=$?
[ "$status" -eq 2 ]
echo "built $out"
