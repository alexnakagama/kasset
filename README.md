# kasset

A fast, keyboard-driven terminal music player for the music you already have
in a local folder. Type `kasset`, browse your library and play it without
leaving the terminal.

- Recursive scan of a folder for mp3, flac, ogg/oga (Vorbis), wav and m4a/aac
- Title, artist, album, track number and duration read from tags, with the
  file name as fallback
- Play, pause, stop, next/previous, seek, volume, automatic advance
- Shuffle and repeat (off / all / one)
- Live search with `/` over title, artist and album

## Requirements

- Rust 1.89 or newer (`rustup` recommended)
- On Linux, the ALSA development headers and `pkg-config`, needed to build the
  audio backend:

  ```sh
  # Debian / Ubuntu
  sudo apt install libasound2-dev pkg-config
  # Fedora
  sudo dnf install alsa-lib-devel pkgconf-pkg-config
  # Arch
  sudo pacman -S alsa-lib pkgconf
  ```

  PulseAudio and PipeWire systems work through their ALSA compatibility layer.

macOS and Windows need no extra system packages.

## Install

```sh
cargo install --locked --path .
```

`--locked` builds with the exact dependency versions in `Cargo.lock`; plain
`cargo install --path .` also works but may pick newer compatible versions.
This builds an optimised binary and places `kasset` in `~/.cargo/bin`, which
`rustup` adds to your `PATH`. Check it with:

```sh
kasset --version
```

To remove it: `cargo uninstall kasset`.

## Usage

```sh
kasset [DIR]
```

- `kasset ~/Downloads/albums` opens that folder.
- `kasset` with no argument opens your music directory (on Linux the
  `XDG_MUSIC_DIR` entry in `~/.config/user-dirs.dirs`, set with
  `xdg-user-dirs-update`; else `~/Music`) and, if there is none, the current
  directory.
- `kasset --help` prints usage and the key summary; `kasset --version` prints
  the version.

If the folder does not exist, `kasset` exits with an error naming it. If no
audio output device is available, it prints
`No audio output device available` and exits with a non-zero status. A folder
without supported files opens normally and shows `No music found in <folder>`.

Subfolders that cannot be read, broken links and symlink loops are skipped;
the status bar says how many entries were left out.

## Keys

| Key                    | Action                                   |
| ---------------------- | ---------------------------------------- |
| `Enter`                | play the selected track                  |
| `Space`                | pause / resume                           |
| `s`                    | stop                                     |
| `n` / `p`              | next / previous track (`p` restarts the track after 3 s) |
| `←` / `→`              | seek −5 s / +5 s                         |
| `+` (or `=`) / `-`     | volume up / down in 5 % steps            |
| `z`                    | toggle shuffle                           |
| `r`                    | cycle repeat: off → all → one            |
| `/`                    | search; `Enter` keeps the filter, `Esc` clears it |
| `Esc`                  | clear an active filter                   |
| `↑` / `↓`, `k` / `j`   | move the selection                       |
| `PgUp` / `PgDn`        | move the selection by a page             |
| `g` / `Home`, `G` / `End` | first / last track                    |
| `q`, `Ctrl+C`          | quit                                     |

While searching, typed characters go to the query, so letter keys do not act
as commands until you press `Enter` or `Esc`.

Shuffle plays every track once, in random order, before any repeats; the
current track stays current when you turn it on. The search filter only
changes the visible list, not the play order.

A track that cannot be opened or decoded is reported in the status bar and
skipped, even with repeat one. If every track fails in a row, playback stops
and the last error stays visible.

## Terminal notes

- Standard output must be a terminal; `kasset > file` is refused. Standard
  input may be redirected — keys are then read from the controlling terminal
  (`/dev/tty`).
- The terminal is restored on quit, on error, on panic and on `SIGINT`,
  `SIGTERM` or `SIGHUP`. A **second** signal of the same kind is an emergency
  exit for when `kasset` is stuck (for example on a stalled network mount): it
  ends the process at once without restoring the terminal. Run `reset` if the
  shell is left in a bad state afterwards.
- While the UI is up, messages that audio libraries print to standard error
  are discarded so they cannot draw over the screen. When standard error is
  redirected to a file, they are kept there.

## Known limitations

- **Opus (`.opus`) is not supported**; such files are not listed. The decoder
  used (symphonia) has no Opus support.
- No playlists (m3u), album art, streaming sources, configuration file, media
  keys / MPRIS or saved state between runs.
- No gapless playback, crossfade or equaliser.
- The library is scanned on every start; very large libraries take longer to
  open. The scan stops at 64 folder levels and 2 million entries.
- Seeking may be unavailable for some files or formats; the failure is shown
  in the status bar.
- If the output device disappears while playing (e.g. a USB DAC is unplugged),
  the error is shown but the device is not reopened; restart `kasset`.
- Opening and seeking files happens on the UI thread, so a stalled network
  mount (NFS, sshfs) can freeze the interface until the I/O returns.
- Terminals that do not answer the cursor-position query (DSR) fail at
  startup.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

Two tests need real audio hardware and are ignored by default:

```sh
cargo test -- --ignored                          # needs an output device
KASSET_SAMPLE=/path/to/song.mp3 cargo test -- --ignored   # also plays a sample
```
