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
- Playlists saved as `.m3u8` files inside your music folder, playable on
  their own and readable by other players

## Install

### Prebuilt binary

Download the archive for your platform from the
[latest release](https://github.com/alexnakagama/kasset/releases/latest),
extract it and put `kasset` (`kasset.exe` on Windows) somewhere on your `PATH`:

| Platform | Archive |
|---|---|
| Linux x86_64 | `kasset-v<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Linux ARM64 | `kasset-v<version>-aarch64-unknown-linux-gnu.tar.gz` |
| macOS Apple Silicon | `kasset-v<version>-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `kasset-v<version>-x86_64-apple-darwin.tar.gz` |
| Windows x86_64 | `kasset-v<version>-x86_64-pc-windows-msvc.zip` |

For example, on Linux x86_64:

```sh
tar -xzf kasset-v0.1.0-x86_64-unknown-linux-gnu.tar.gz
install -m 755 kasset-v0.1.0-x86_64-unknown-linux-gnu/kasset ~/.local/bin/
```

On Windows, extract the zip and put `kasset.exe` in a folder on your `PATH`.

Each release also has a `SHA256SUMS` file to check a download against:

```sh
sha256sum --check --ignore-missing SHA256SUMS          # Linux
shasum -a 256 --check --ignore-missing SHA256SUMS      # macOS
```

On Windows, compare the output of `Get-FileHash kasset-*.zip` (PowerShell)
with the matching line in `SHA256SUMS`.

- **Linux:** the binary needs the ALSA runtime library (`libasound.so.2`),
  which nearly every desktop system already has (`libasound2` /
  `libasound2t64` on Debian and Ubuntu, `alsa-lib` on Fedora and Arch). It
  needs glibc 2.35 or newer. PulseAudio and PipeWire systems work through
  their ALSA compatibility layer.
- **Windows:** the binary is statically linked against the C runtime, so it
  needs no Visual C++ Redistributable.
- **macOS:** the binary is not signed, so Gatekeeper blocks it the first time.
  Clear the quarantine flag once: `xattr -d com.apple.quarantine kasset`.

### From source

Requirements:

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

macOS and Windows need no extra system packages. Then, from a checkout:

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

### Playlists

| Key                    | Action                                   |
| ---------------------- | ---------------------------------------- |
| `Tab`                  | switch between the library and the playlists; each side keeps its own selection and filter |
| `a`                    | in the library: add the selected track to a playlist |
| `A`                    | in the library: add every listed track (after any search) to a playlist |
| `Enter`                | in the playlists list: open the playlist; in a playlist: play from the selected track |
| `Esc`                  | in a playlist: clear the filter, then go back to the playlists list |
| `d`                    | in a playlist: remove the selected entry (only that occurrence) |
| `J` / `K`              | in a playlist: move the selected entry down / up (not while filtered) |
| `R`                    | in the playlists list: rename the selected playlist (changing only the letter case is allowed) |
| `D`                    | in the playlists list: delete the selected playlist after a `y` confirmation; any other key cancels |

`a` and `A` open a picker: choose a playlist, or "New playlist…" to type a
name. Tracks already in the playlist are skipped and the status bar says how
many. Names may not contain `/ \ : * ? " < > |` or control characters, may
not start with `.`, are at most 100 characters (and 200 bytes) long and must
differ from existing playlist names ignoring case.

Every edit is saved at once. Deleting a playlist removes only its `.m3u8`
file (for a symlinked playlist, only the link), never a song.

Playing a track from an open playlist makes that playlist the play order:
next, previous, automatic advance, shuffle and repeat stay within it. Adding
tracks to the playlist that is playing extends the play order at once, and
so do removing and moving its entries. Removing the track that is playing
lets it finish, then goes on with the entry that followed it. Renaming or
deleting the playing playlist does not interrupt playback.
Playing a track from the library makes the whole library the play order
again. The playing marker shows in an open playlist only when it is the one
playing.

## Playlists on disk

Playlists are stored as UTF-8 `<name>.m3u8` files in a `Playlists` folder
directly inside the music folder (for example `~/Music/Playlists/Running.m3u8`).
The folder is created when the first playlist is saved; other files in it
(`.m3u`, `.pls`, …) are ignored.

Entries are written as paths relative to the playlist file with `/` as
separator (`../Band/Album/01 Song.flac`), so the music folder can be moved or
copied elsewhere — or opened from another machine — and its playlists still
work, in kasset and in other players such as mpv or VLC. Absolute paths and
Windows-style `\` separators in playlists written by other programs are read
too.

Every change is saved immediately and atomically: the file on disk is always
either the old or the new complete playlist. kasset never overwrites a
playlist another program changed since kasset read it; it reloads the
playlist instead and says so. Entries that do not match a track in the
library (a moved file, a hand-written line) are kept in place, never dropped,
and reported when the playlist is opened. Comments, a byte order mark,
Windows line endings, a missing final newline and the file's permission bits
are kept; a playlist that is a symbolic link stays a link and its target is
updated, as long as the target is inside `Playlists/` too. A playlist linking
to a file outside `Playlists/` (one planted in a downloaded music folder, say)
can be played, renamed and deleted but is read-only, so kasset never writes
through it.

This is the only place kasset writes to: nothing outside `Playlists/` is ever
modified, and only when you create, edit, rename or delete a playlist. Saving
writes a dot-prefixed temporary file next to the playlist and renames it over
the original.

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
- No album art, streaming sources, configuration file, media keys / MPRIS or
  saved state between runs (other than playlists). Only `.m3u8` playlists in
  the `Playlists` folder are read; `.m3u` and `.pls` files are not.
- Playlists that are not valid UTF-8, or that are over 16 MiB or 100 000
  entries, can be opened and played but are read-only: entries cannot be
  added, removed or moved (the file can still be renamed or deleted).
- Saving a playlist replaces its file: permission bits are kept and the owner
  and group are restored when allowed, but extended attributes and ACLs are
  not kept, and other hard links to the old file no longer share its content.
- A playlist entry that points to a hard link of a library file (rather than
  the path the library lists) is shown as not found in the library; it is
  kept in the file.
- On filesystems without hard links (FAT, exFAT, some network mounts),
  creating or renaming a playlist checks that the name is free and then
  renames into place; another program creating the same file in between
  could be overwritten.
- Changes made by another program within the same second and keeping the
  same file size may go unnoticed on filesystems with coarse timestamps.
- Playlist files are read and written on the UI thread, like audio files, so
  a stalled network mount can freeze the interface until the I/O returns.
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

### Releasing

1. Bump `version` in `Cargo.toml` and merge it to `master`.
2. Do a dry run of the release on `master`: Actions → Release → Run workflow,
   or `gh workflow run Release --ref master` (follow it with `gh run watch`).
   It tests, builds and packages every platform without publishing; the
   archives can be downloaded from the run page to try them.
3. Once it is green, push a tag matching the version:

   ```sh
   git tag v0.2.0 && git push origin v0.2.0
   ```

The `Release` workflow (`.github/workflows/release.yml`) then checks that the
tag matches `Cargo.toml`, tests and builds every platform again, and publishes
a GitHub Release with the archives and `SHA256SUMS`. A tag with a suffix
(`v0.2.0-rc.1`) is published as a pre-release. Pull requests that change
`Cargo.toml`, `Cargo.lock`, `.cargo/` or the workflow get the dry run
automatically. If a tagged build fails nothing is published: fix it, then
move the tag with `git tag -d v0.2.0 && git push origin :refs/tags/v0.2.0`
and push it again.

## License

MIT; see [LICENSE](LICENSE).
