<div align="center">

<img src="assets/logo.png" width="240" alt="brrr logo">

<sub>Art by <a href="https://twitter.com/monky_sm/status/1744264103558779075">@monky_sm</a></sub>

# brrr

**Full power when you open something heavy. Quiet again when you close it.**

[![Windows](https://img.shields.io/badge/Windows-10_%26_11-d4f57a?style=flat&labelColor=222723)](docs/guide.md#windows)
[![Linux](https://img.shields.io/badge/Linux-any_tray-d4f57a?style=flat&labelColor=222723)](docs/guide.md#linux)
[![macOS](https://img.shields.io/badge/macOS-12+-d4f57a?style=flat&labelColor=222723)](docs/guide.md#macos)
[![Omarchy plugin](https://img.shields.io/badge/Omarchy-shell_plugin-d4f57a?style=flat&labelColor=222723)](https://omarchy.org)

</div>

brrr is the sound your fans make. Open a game, zip a big folder, export a video, or start a compile, and brrr switches your machine to its performance mode. Close it, and you're back on the mode you had before. You don't have to think about it.

## Install

**Windows 10 and 11**: download `brrr-windows-setup.exe` from the
[releases](https://github.com/valvesky/brrr/releases) and run it. It needs no administrator
rights, and it starts brrr when you sign in unless you untick that.

**Linux** (any desktop with a tray: KDE Plasma, waybar, GNOME with AppIndicator support, …):
download `brrr-linux-x86_64.tar.gz` (or `aarch64`) from the
[releases](https://github.com/valvesky/brrr/releases), put `brrr` on your `PATH`, then run
`brrr --install` to show it in your tray at every login. It needs power-profiles-daemon (or tuned-ppd),
which most distributions include.

**macOS 12 or later**: download `brrr-macos.dmg`, open it and drag *brrr* to Applications.
Open it, choose *Allow Power Mode Switching…* from its menu bar icon and enter your password once.

**Omarchy**

```sh
omarchy plugin add https://github.com/valvesky/brrr.git --enable
```

## What you get

- Performance mode while a heavy app is running, and only then.
- Your usual mode back as soon as that app closes.
- A status icon that looks like it belongs on each system: a notification area icon with a native
  menu on Windows, an SF Symbol in the macOS menu bar, a tray icon your Linux desktop draws itself,
  and a bar widget in Omarchy. Each has a switch to turn it off and the list of apps it watches.

Out of the box it watches common heavy apps — archives, video, 3D, screen recording — and the usual
compilers. You can add your own from the menu or [from the terminal](docs/guide.md#choose-commands).

If you change the power mode yourself while something heavy is still open, brrr leaves it alone until
that work is over.

## Want to know more?

The [guide](docs/guide.md) covers [each platform](docs/guide.md#get-started),
[picking apps](docs/guide.md#choose-commands), [how it works](docs/guide.md#how-it-works),
[debugging](docs/guide.md#diagnostics) and [building and testing](docs/guide.md#build-and-test).

## License

[MIT](LICENSE). **The art at the top is by [@monky_sm](https://twitter.com/monky_sm/status/1744264103558779075) and isn't covered by the MIT license.**
