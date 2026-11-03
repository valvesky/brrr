<div align="center">

<img src="assets/logo.png" width="240" alt="Omapower logo">

# Omapower

**Performance when you need it. Your usual profile when you don't.**

[![Omarchy plugin](https://img.shields.io/badge/Omarchy-shell_plugin-d4f57a?style=flat&labelColor=222723)](https://omarchy.org)

</div>

## Install

```sh
omarchy plugin add https://github.com/valvesky/omapower.git --enable
```

Requires Omarchy with Quickshell plugin support. Click the widget and choose **Set up Omapower** to build and enable the daemon. Missing system packages require administrator authentication; the build and user service do not.

See the [setup guide](docs/guide.md#get-started) for details and manual installation.

## About

Omapower gives demanding commands a little extra power, without leaving your machine in performance mode all day.

- **Automatic performance** — holds the performance profile while a configured command is running.
- **Your usual profile afterward** — releases its hold when the work is done.
- **Optional bar widget** — see your profile, edit watched commands, and start or stop the service.

Watches `rustc` and `cargo` by default. Choose your own commands in the widget or with `omapower --add NAME`.

No root required. A manual profile change that releases Omapower's hold is respected until the matching commands finish.

See the guide for [configuration](docs/guide.md#choose-commands), [how it works](docs/guide.md#how-it-works), [diagnostics](docs/guide.md#diagnostics), and [tests](docs/guide.md#tests).
