<div align="center">

<img src="assets/logo.svg" alt="" width="96">

# LyricsLens

**Synced lyrics over any window.**

LyricsLens reads what your player is playing, finds the lyrics on
[LRCLIB](https://lrclib.net), and draws the line being sung on a layer above
everything else — including a window in fullscreen.

[![CI](https://github.com/byjuliocesa/lyricslens/actions/workflows/ci.yml/badge.svg)](https://github.com/byjuliocesa/lyricslens/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/byjuliocesa/lyricslens?label=release)](https://github.com/byjuliocesa/lyricslens/releases/latest)
[![License](https://img.shields.io/github/license/byjuliocesa/lyricslens)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-stable-b7410e)](https://www.rust-lang.org)

</div>

<p align="center">
  <img src="assets/demo.gif" alt="LyricsLens showing the line being sung over the desktop" width="800">
</p>

---

## Install

Into `~/.local`, no sudo, nothing outside your home directory:

```bash
curl -fsSL https://raw.githubusercontent.com/byjuliocesa/lyricslens/main/install.sh | sh
```

To read it first — and you should:

```bash
curl -fsSL https://raw.githubusercontent.com/byjuliocesa/lyricslens/main/install.sh -o install.sh
less install.sh && sh install.sh
```

To remove it:

```bash
curl -fsSL https://raw.githubusercontent.com/byjuliocesa/lyricslens/main/install.sh | sh -s -- --remove
```

<details>
<summary>Debian, Ubuntu and Mint</summary>

From the repository, so it updates with the rest of the system —
[the instructions are here](https://byjuliocesa.github.io/lyricslens/packages/).

Or download the `.deb` from [the latest release](https://github.com/byjuliocesa/lyricslens/releases/latest) and:

```bash
sudo apt install ./lyricslens-*-x86_64.deb
```

`apt install ./file.deb` resolves the libraries it needs. `dpkg -i` does not:
it fails with unmet dependencies and leaves the package half-installed.

</details>

<details>
<summary>Fedora and openSUSE</summary>

```bash
sudo dnf install ./lyricslens-*-x86_64.rpm
```

</details>

<details>
<summary>On Arch, from the PKGBUILD</summary>

```bash
curl -fsSLO https://raw.githubusercontent.com/byjuliocesa/lyricslens/main/packaging/PKGBUILD
makepkg -si
```

</details>

<details>
<summary>From source</summary>

```bash
cargo install --path .
```

</details>

## What it needs to run

| | At least |
|---|---|
| glibc | 2.39 |
| GTK | 4.12 |
| libadwaita | 1.5 |

Which means Ubuntu 24.04, Debian 13, Fedora 40, openSUSE Tumbleweed or Arch, and
anything newer. gtk4-layer-shell is built into the published binary, so there
is nothing to install for it.

Older than that, build from source: it works against anything that can build
GTK 4.12, whatever glibc is underneath. `install.sh` reads the exact glibc
each release needs from its `MINIMUM_GLIBC` file and stops before downloading
a binary this machine could not run.

Building from source needs the development packages. On Arch: `gtk4
libadwaita gtk4-layer-shell`. On Debian and Ubuntu: `libgtk-4-dev
libadwaita-1-dev`, and gtk4-layer-shell from its own source on Ubuntu before
25.10, which does not package it.

## Use

```bash
lyricslens
```

The overlay appears near the bottom of the screen and follows the song. It runs
in the background and gives the terminal straight back. `lls` is the same
program under a shorter name.

Launching it again while a copy is running brings that one to the front instead
of starting another.

It puts an icon in the status bar, where a left click shows and hides it and
the menu holds the rest, quitting included. Desktops without a status bar can
use `lyricslens --quit` or the preferences window.

| Command | What it does |
| --- | --- |
| `lyricslens`, or `lls` | runs the overlay and gives the terminal back |
| `lyricslens --foreground` | keeps the terminal, for watching the log |
| `lyricslens --settings` | opens the preferences window |
| `lyricslens --toggle` | hides the overlay, or brings it back |
| `lyricslens --position` | enters the mode where you drag it somewhere else |
| `lyricslens --upgrade` | installs the newest release over this one |
| | the preferences window says when there is one, once a day at most |
| `lyricslens --uninstall` | removes it from `~/.local` |
| `lyricslens --paths` | prints where the settings and the cached lyrics live |
| `lyricslens --quit` | closes the overlay that is running |
| `lyricslens --help` | the whole list |

Tab completes these options in bash, zsh and fish, for both names. The
installer adds one line to `~/.zshrc` for zsh, marked so `--remove` takes it
out again.

### A key to hide it

Wayland gives an ordinary program no way to claim a key combination, so the
binding belongs to your compositor. On Hyprland and Sway it is asked for
directly: type the combination under *Keys* and it works straight away.
Nothing is written to your configuration, so there is nothing to undo — the
compositor forgets the binding when it restarts, and this asks again when it
starts.

Anywhere else, the preferences window shows the line to paste into your own
configuration, in the language your compositor reads:

```conf
bind = SUPER, L, exec, lyricslens --toggle
bind = SUPER SHIFT, L, exec, lyricslens --position
```

### Moving it

`--position` turns on a mode where the overlay takes your clicks instead of
letting them through: drag it where you want, then press the key again. Where
you left it is saved.

A layer surface belongs to one screen and cannot be dragged to another, so the
screen is a choice in the preferences rather than a drag across the edge.

### How it looks

The preferences window holds the font and its size and weight, the text
colour, which edge the words line up with, how wide the overlay gets before a
line wraps, how round the strip's corners are, the shadow that keeps it
readable over a bright window, how dark the strip behind the line is,
the line just sung above and how many are shown coming up underneath, a
karaoke mode that fills the line as the song moves through it, and whether the
overlay clears while the player is paused. Every one of them shows on
the overlay straight away.

The same window has the switch that lets you drag the overlay, the screen it
lives on, and whether it starts with your session.

<p align="center">
  <img src="assets/settings.gif" alt="Changing the preferences and the overlay updating straight away" width="800">
</p>

### When the words are wrong

Open the preferences and look under *Lyrics for this track*. The artist and
title come filled in from what is playing; correct them, search, and pick from
everything the service has under that name. The choice is kept for that
recording, so the song comes back right next time.

## What it needs from a player

LyricsLens follows the position the player reports, and some players report
none. Firefox is the notable case: it answers with zero forever, so there is
nothing to synchronise against, and the overlay says so on screen rather than
guessing. Players that do report it — Spotify, mpv with `mpv-mpris`, Rhythmbox,
Amberol — work.

If the player names a local file, an `.lrc` of the same name next to it is used
before anything is fetched — so a tagged collection works with no network at
all, and a file you corrected by hand is the one that plays.

With more than one player open, pick the one you mean in the preferences, or
for a single run:

```bash
LYRICSLENS_PLAYER=spotify lyricslens
```

## Desktops

The overlay is a `wlr-layer-shell` surface, which is what lets it stay above a
fullscreen window. Where that protocol is missing it falls back to an ordinary
X11 window.

| Desktop | Overlay | Above fullscreen |
| --- | --- | --- |
| Hyprland | tested | yes |
| Sway, KWin, COSMIC, niri, Wayfire | expected to work | yes |
| GNOME on Wayland | tested, fallback | yes |
| X11, any window manager | fallback | no |

Hyprland and GNOME on Wayland have actually been used. On GNOME the fallback
window stays above a fullscreen video, lets clicks through to what is
underneath, and can be dragged; more than one screen there is untested. The
rest of the table is what the protocol implies, not a report from use.

## How it works

```
MPRIS (D-Bus) ──▶ track ──▶ LRCLIB ──▶ cache on disk
                              │
                    clock ────┘
                      │
                   overlay
```

Two decisions carry the rest, and both came from measurement:

- **The clock anchors on the edge, not on the reading.** Players report the
  position rounded down to the second, so a reading alone carries up to a
  second of error. The instant the integer flips is the one moment the real
  position is known. Measured: the estimate stays within 8ms of the song.
- **On Wayland the compositor owns the window.** Staying on top, choosing a
  place, claiming a key — a program cannot do any of that by itself. It all
  goes through the compositor.

## Language

The preferences window, the tray menu and what the overlay says follow your
session's `LANG`: English, Portuguese and Spanish are built in.

To use another language, or to fix a word, put a file named after the language
in `~/.config/lyricslens/locale/`, such as `fr.toml`, and restart the program.
Each line pairs the English sentence with its translation; `locale/es.toml`
in this repository has every sentence there is. Anything the file leaves out
stays in English, and a regional file such as `pt_br.toml` only needs the words
that differ from `pt.toml`.

## Where it keeps things

| Path | What |
| --- | --- |
| `~/.config/lyricslens/settings.toml` | preferences, readable and editable by hand |
| `~/.cache/lyricslens/` | one LRC file per recording, so a song already played works offline |
| `~/.local/state/lyricslens/lyricslens.log` | what happened, for a bug report; rolls over at 1 MiB |

`lyricslens --paths` prints all three.

## Contributing

Bug reports and pull requests are welcome — [CONTRIBUTING.md](CONTRIBUTING.md)
says how to build it and what a good change looks like.

## License

[MIT](LICENSE)
