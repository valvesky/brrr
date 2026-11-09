<div align="center">

<img src="assets/logo.png" width="240" alt="Omapower logo">

<sub>Art by <a href="https://twitter.com/monky_sm/status/1744264103558779075">@monky_sm</a></sub>

# Omapower

**Full power while you compile. Back to normal when you're done.**

[![Omarchy plugin](https://img.shields.io/badge/Omarchy-shell_plugin-d4f57a?style=flat&labelColor=222723)](https://omarchy.org)

</div>

Compiling on a laptop in balanced or power saver mode can be painfully slow.
Omapower notices when you start a build and switches your machine to the performance profile.
When the build finishes, you're back on whatever profile you had before. You don't have to think about it.

## Install

```sh
omarchy plugin add https://github.com/valvesky/omapower.git --enable
```

That's it. It works right away. There's nothing to build and nothing else to install.

## What you get

- Performance mode while a compiler is running, and only then.
- Your usual profile back as soon as the build is done.
- A bar widget that shows what's going on, with a switch to turn it off and a list of the commands it watches.

Out of the box it watches the usual compilers: Rust and Cargo, GCC, Clang, Zig, Go, Java, Swift, Fortran, CUDA and GHC. You can add your own from the widget or [from the terminal](docs/guide.md#choose-commands).

It doesn't need root. And if you change the profile yourself in the middle of a build, Omapower leaves it alone until that build is over.

## Want to know more?

The [guide](docs/guide.md) covers [picking commands](docs/guide.md#choose-commands), [how it works](docs/guide.md#how-it-works), [running it without the Omarchy shell](docs/guide.md#standalone-mode-without-the-shell), [debugging](docs/guide.md#diagnostics) and [running the tests](docs/guide.md#tests).

## License

[MIT](LICENSE). **The art at the top is by [@monky_sm](https://twitter.com/monky_sm/status/1744264103558779075) and isn't covered by the MIT license.**
