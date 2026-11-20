# kasset

A fast, keyboard-driven terminal music player for the music you already have
in a local folder. Type `kasset`, browse your library and play it without
leaving the terminal.

- Recursive scan of a folder for mp3, flac, ogg/oga (Vorbis), wav and m4a/aac
- Title, artist, album, track number and duration read from tags; when a tag
  is missing, it is taken from the file name (`01 - Artist - Title`) and from
  `Artist/Album/` folders, with video labels such as "(Official Audio)"
  removed from the title
- Play, pause, stop, next/previous, seek, volume, automatic advance
- Shuffle and repeat (off / all / one)
- Live search with `/` over title, artist and album
- Playlists saved as `.m3u8` files inside your music folder, playable on
  their own and readable by other players
- Lyrics of the current track beside the track list, following the song
  line by line when they are timed (`.lrc` files or lyrics tags)
- Remembers your settings and, for each music folder, the track and
  position you stopped at

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
kasset [--no-state] [DIR]
```

- `kasset ~/Downloads/albums` opens that folder.
- `kasset` with no argument opens your music directory (on Linux the
  `XDG_MUSIC_DIR` entry in `~/.config/user-dirs.dirs`, set with
  `xdg-user-dirs-update`; else `~/Music`) and, if there is none, the current
  directory.
- `kasset --no-state` starts with the default settings and nothing resumed,
  and neither reads nor writes the saved state (see [Saved state](#saved-state)).
- `kasset --help` prints usage and the key summary; `kasset --version` prints
  the version.

If the folder does not exist, `kasset` exits with an error naming it. If no
audio output device is available, it prints
`No audio output device available` and exits with a non-zero status. A folder
without supported files opens normally and shows `No music found in <folder>`.

Subfolders that cannot be read, broken links and symlink loops are skipped;
the status bar says how many entries were left out.

## The screen

- **Sidebar** (left): "Library", then your playlists with their track counts.
  Read-only playlists are drawn dimmed. While a track is loaded, the entry
  that is the play order carries the playing marker (`▶`, `‖` or `■`).
- **Main pane** (right): the tracks of the entry chosen in the sidebar, the
  whole library or one playlist, with title, artist, album and length.
- One pane has the keyboard focus: its border is highlighted and its
  selected row is drawn in reverse video; the other pane's selected row is
  only bold and underlined. `Tab` moves the focus.
- On terminals narrower than 70 columns only the focused pane is shown, at
  full width; keys work the same, and widening the terminal brings the
  other pane back.
- **Spectrum strip**: on terminals at least 24 rows tall, a 6-row strip
  across the full width, between the panes and the now-playing area, shows
  the frequency spectrum of the track that is playing: one bar per band,
  from the bass (about 40 Hz) on the left to the treble (16 kHz) on the
  right, each from silence (about −60 dB) to full height (0 dB). The bars
  are measured before the volume, so turning it down does not shrink
  them; they jump up at once and fall back over about a second, and fall to
  nothing when playback is paused or stopped. `v` hides or shows the strip
  (it is on at start); on a shorter terminal it is not drawn, and it comes
  back when the terminal is tall enough. The screen redraws about 30 times
  a second only while the bars move.
- Below the panes: the now-playing area, a status line for messages, and a
  bar with the main keys for what has the focus. The now-playing area is
  two lines: the play state, title, artist and album, with shuffle, repeat
  and volume at the right end; then the elapsed time, a progress bar and the
  track length (`-:--` and an empty bar when the length is unknown). On a
  narrow terminal the title is cut first; the indicators keep their place.
- The key bar ends with `? help` whenever no prompt or search is open; `?`
  opens the list of every key, which scrolls when it does not fit.

## Lyrics

On terminals at least 100 columns wide, a **Lyrics** pane to the right of the
main pane shows the lyrics of the current track. It is on at start; `l` hides
or shows it. On a narrower terminal it is not drawn (`l` then says so in the
status line), and it comes back when the terminal is wide enough. It never
takes the keyboard focus.

Lyrics are looked up when a track becomes current, in this order:

1. A `.lrc` file next to the audio file with the same name, the extension in
   any case: `Band/Album/01 Song.flac` uses `Band/Album/01 Song.lrc` (or
   `.LRC`, `.Lrc`, …).
2. Otherwise, lyrics stored in the file's tags: a Vorbis comment `LYRICS` or
   `UNSYNCEDLYRICS` (FLAC, Ogg), MP4 `©lyr` (M4A), or ID3v2 `USLT` (MP3).

A `.lrc` file that can be read always wins, even if it holds no lyrics: an
**empty `.lrc` file hides the lyrics in the tags**. One that cannot be read
(no permission, a directory, a named pipe) is ignored and the tags are used.

Timed lyrics (`[mm:ss.xx]` stamps, as in LRC files) highlight the line being
sung, keep it in the middle of the pane and follow seeking; an `[offset:±ms]`
header shifts every line (a positive offset shows lines earlier), and
word-level `<mm:ss.xx>` timings are ignored. Lyrics without timestamps are
shown as plain text that scrolls with the track's progress. Lyrics are read
again only when the track changes or the pane is shown again, so a `.lrc`
edited while its track plays is picked up the next time.

Lyrics are treated as untrusted: files are read up to 1 MiB, control
characters and escape sequences are removed, and kasset never creates,
changes or deletes `.lrc` files or tags. Synced lyrics in ID3v2 `SYLT` frames
are not read.

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
| `Esc`                  | clear an active filter (see also Playlists below) |
| `↑` / `↓`, `k` / `j`   | move the selection                       |
| `PgUp` / `PgDn`        | move the selection by a page             |
| `g` / `Home`, `G` / `End` | first / last row                      |
| `v`                    | show / hide the visualizer               |
| `l`                    | show / hide the lyrics                   |
| `?`                    | show every key; `↑`/`↓`, `k`/`j`, `PgUp`/`PgDn`, `g`/`Home`, `G`/`End` scroll it, any other key closes it |
| `q`, `Ctrl+C`          | quit                                     |

While searching, typed characters go to the query, so letter keys do not act
as commands until you press `Enter` or `Esc`.

### Playlists

| Key                    | Action                                   |
| ---------------------- | ---------------------------------------- |
| `Tab`, `Shift+Tab`     | move focus between the sidebar and the main pane; each list keeps its own selection and filter |
| `a`                    | in the library: add the selected track to a playlist |
| `A`                    | in the library: add every listed track (after any search) to a playlist |
| `Enter`                | in the sidebar: show the selected entry ("Library" or a playlist) in the main pane and focus it; in a playlist: play from the selected track |
| `Esc`                  | in a playlist: clear the filter, then focus the sidebar (the playlist stays shown); in the sidebar: clear the filter |
| `d`                    | in a playlist: remove the selected entry (only that occurrence) |
| `J` / `K`              | in a playlist: move the selected entry down / up (not while filtered) |
| `R`                    | in the sidebar: rename the selected playlist (changing only the letter case is allowed) |
| `D`                    | in the sidebar: delete the selected playlist after a `y` confirmation; any other key cancels |

The sidebar lists "Library" first, then your playlists with their track
counts; they are listed when kasset starts and again whenever the sidebar
gets the focus. `R` and `D` do nothing on "Library".

**Changed in this version:** `Tab` used to swap the whole list between the
library and the playlists; it now moves the focus between the two panes.
`Esc` in a playlist used to go back to the list of playlists; it now focuses
the sidebar and leaves the playlist shown.

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

This is the only place in the music folder kasset writes to: nothing else
in it is ever modified, and `Playlists/` only when you create, edit, rename or
delete a playlist. Saving writes a dot-prefixed temporary file next to the
playlist and renames it over the original. The only other file kasset writes
is its saved state, in your per-user state folder (see
[Saved state](#saved-state)).

Shuffle plays every track once, in random order, before any repeats; the
current track stays current when you turn it on. The search filter only
changes the visible list, not the play order.

A track that cannot be opened or decoded is reported in the status bar and
skipped, even with repeat one. If every track fails in a row, playback stops
and the last error stays visible.

## Saved state

kasset remembers between runs:

- **Settings**, shared by every music folder: the volume, shuffle, the repeat
  mode, and whether the visualizer and the lyrics pane are shown.
- **For each music folder** (the last 20 opened, identified by their full
  resolved path): the current track and its position, whether it was played
  from the library or from a playlist, and which playlist the main pane
  shows.

When you open a folder again, its track is loaded **paused** at the saved
position, never playing on its own: `Space` continues it. Next and previous
follow the same play order (the library or the playlist) as before, and the
playlist that was shown is shown again. A track the scan no longer finds is
not resumed, and a playlist that is gone, or no longer holds the track, is
replaced by the library. If the saved position can't be reached (the file
became shorter, or can't seek), the track starts paused at 0:00. A resume
point that can't be used (the track's drive isn't mounted, say) is kept,
and used on a later start once the track is found again, until you play
something else in that folder.

The state is saved when you quit (`q`, `Ctrl+C`) and when a `SIGINT`,
`SIGTERM` or `SIGHUP` stops kasset, as long as the save completes within the
2-second grace period after the signal (see [Terminal notes](#terminal-notes);
the previous save is then kept intact), and, while it runs, at most every 30
seconds when something changed, so after a crash you resume at most about 30
seconds earlier. A failed save never stops the music or changes the exit
status: it is shown once in the status line, or printed after the terminal is
restored when quitting.

The file is:

| Platform | Location |
|---|---|
| Linux | `$XDG_STATE_HOME/kasset/state` (default `~/.local/state/kasset/state`) |
| macOS | `~/Library/Application Support/kasset/state` |
| Windows | `%LOCALAPPDATA%\kasset\state` |

- **`--no-state`** runs without it: default settings, nothing resumed, and
  the file is neither read nor written.
- **To reset**, delete the file (kasset starts with the defaults and writes
  a new one). A damaged file is also ignored and replaced with a valid one.
- The file is replaced on every save (written next to it, then renamed). A
  **symbolic link at `state` is replaced** by a regular file, never written
  through; to keep the state elsewhere (a dotfiles repository, say),
  symlink the `kasset` folder instead.
- A state file from a newer kasset is left untouched: the older version
  starts with the defaults and says so once.
- **Privacy:** the file lists the music folders you opened and the track you
  were last playing in each. On Linux and macOS it is readable by you only
  (mode `0600`, in a folder created with mode `0700`). Use `--no-state` for
  a session that leaves no trace, or delete the file.

## Terminal notes

- Standard output must be a terminal; `kasset > file` is refused. Standard
  input may be redirected — keys are then read from the controlling terminal
  (`/dev/tty`).
- The terminal is restored on quit, on error, on panic and on `SIGINT`,
  `SIGTERM` or `SIGHUP`. A **second** signal of the same kind is an emergency
  exit for when `kasset` is stuck (for example on a stalled network mount): it
  ends the process at once without restoring the terminal. If `kasset` has not
  quit 2 seconds after the first signal, it ends the same way on its own, so
  closing the terminal window always stops the music. Run `reset` if the
  shell is left in a bad state afterwards.
- The borders, the progress bar (`━ ╸ ─`), the playing markers (`▶ ‖ ■`)
  and the ellipsis (`…`) are Unicode characters of "ambiguous" East Asian
  width. Terminals set up to draw those as double width (an option in some
  CJK setups) will misalign the screen; use the default single-width
  setting.
- While the UI is up, messages that audio libraries print to standard error
  are discarded so they cannot draw over the screen. When standard error is
  redirected to a file, they are kept there.

## Known limitations

- **Opus (`.opus`) is not supported**; such files are not listed. The decoder
  used (symphonia) has no Opus support.
- No album art, streaming sources, configuration file or media keys / MPRIS.
  Only `.m3u8` playlists in the `Playlists` folder are read; `.m3u` and
  `.pls` files are not.
- With shuffle on, the resumed track is the same, but the rest of the
  shuffled order is new: the order itself is not saved.
- The resume point follows the file name (its path inside the music
  folder): a renamed or moved track is not resumed, and a different song
  saved under the same name is resumed at the old position (or at 0:00 if
  that is past its end).
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
a GitHub Release with the archives and `SHA256SUMS`, as an official release
marked latest. Pull requests that change
`Cargo.toml`, `Cargo.lock`, `.cargo/` or the workflow get the dry run
automatically. If a tagged build fails nothing is published: fix it, then
move the tag with `git tag -d v0.2.0 && git push origin :refs/tags/v0.2.0`
and push it again.

## License

MIT; see [LICENSE](LICENSE).
