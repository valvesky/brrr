<div align="center">

<img src="assets/logo.png" width="240" alt="Omapower logo">

<sub>Art by <a href="https://twitter.com/monky_sm/status/1744264103558779075">@monky_sm</a></sub>

# Omapower

**Full power while you compile. Back to normal when you're done.**

[![Linux](https://img.shields.io/badge/Linux-any_tray-d4f57a?style=flat&labelColor=222723)](docs/guide.md#linux)
[![Windows](https://img.shields.io/badge/Windows-10_%26_11-d4f57a?style=flat&labelColor=222723)](docs/guide.md#windows)
[![macOS](https://img.shields.io/badge/macOS-12+-d4f57a?style=flat&labelColor=222723)](docs/guide.md#macos)
[![Omarchy plugin](https://img.shields.io/badge/Omarchy-shell_plugin-d4f57a?style=flat&labelColor=222723)](https://omarchy.org)

</div>

Compiling on a laptop in balanced or power saver mode can be painfully slow.
Omapower notices when you start a build and switches your machine to its performance mode.
When the build finishes, you're back on whatever mode you had before. You don't have to think about it.

## Install

**Omarchy**

```sh
omarchy plugin add https://github.com/valvesky/omapower.git --enable
```

**Linux** (any desktop with a tray: KDE Plasma, waybar, GNOME with AppIndicator support, …):
download `omapower-linux-x86_64.tar.gz` (or `aarch64`) from the
[releases](https://github.com/valvesky/omapower/releases), put `omapower` on your `PATH`, then run
`omapower --install` to show it in your tray at every login. It needs power-profiles-daemon (or tuned-ppd),
which most distributions include.

**Windows 10 and 11**: download `omapower-windows-x86_64.exe` (or `aarch64`) and run it. Choose
*Start at login* from its icon in the notification area.

**macOS 12 or later**: download `omapower-macos.zip`, move *Omapower.app* to Applications and open
it. Choose *Allow Power Mode Switching…* from its menu bar icon and enter your password once.

## What you get

- Performance mode while a compiler is running, and only then.
- Your usual mode back as soon as the build is done.
- A status icon that looks like it belongs on each system: a notification area icon with a native
  menu on Windows, an SF Symbol in the macOS menu bar, a tray icon your Linux desktop draws itself,
  and a bar widget in Omarchy. Each has a switch to turn it off and the list of commands it watches.

Out of the box it watches the usual compilers: Rust and Cargo, GCC, Clang, Zig, Go, Java, Swift,
Fortran, CUDA and GHC, plus MSVC and MSBuild on Windows and Xcode on macOS. You can add your own
from the menu or [from the terminal](docs/guide.md#choose-commands).

If you change the power mode yourself in the middle of a build, Omapower leaves it alone until
that build is over.

## Want to know more?

The [guide](docs/guide.md) covers [each platform](docs/guide.md#get-started),
[picking commands](docs/guide.md#choose-commands), [how it works](docs/guide.md#how-it-works),
[debugging](docs/guide.md#diagnostics) and [building and testing](docs/guide.md#build-and-test).

## License

[MIT](LICENSE). **The art at the top is by [@monky_sm](https://twitter.com/monky_sm/status/1744264103558779075) and isn't covered by the MIT license.**
