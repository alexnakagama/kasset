# Proposal

## Why

kasset forgets everything when it quits. Every start is at 100% volume, with shuffle and repeat off, both panes shown and nothing playing, so the user re-applies the same settings and goes looking for the track they were in the middle of. The README already lists "no saved state between runs (other than playlists)" as a known limitation. This is the most visible gap in daily use.

## What Changes

- kasset keeps a small **state file** in the user's per-user state folder (not in the music folder), and reads it at start and writes it while running and at quit.
- **Settings shared by every library folder** are remembered: volume, shuffle, repeat mode, and whether the visualizer and the lyrics pane are on.
- **A resume point per library folder** is remembered: the current track, its position, where it was played from (the library or a playlist), and the playlist shown in the main pane. When kasset opens that folder again, the track is loaded **paused** at that position (it never starts playing on its own), and the playlist is shown again if it still exists. Up to a fixed number of folders are kept, most recently used first.
- The saved state is treated as **untrusted input**: it can only select tracks the scan found and playlists the listing found, never open another path. A missing, unreadable, damaged or newer-format file means starting with the defaults. A newer-format file is not overwritten.
- Saving never interrupts playback or quitting. A failed save is reported once in the status line while running, and on standard error after the screen is restored at quit; the exit status is unchanged.
- A new **`--no-state`** option starts with the defaults and neither reads nor writes the state file (private sessions, troubleshooting).
- **BREAKING (behaviour)**: kasset no longer always starts at 100% volume with shuffle/repeat off and both panes on, and no longer always starts with nothing loaded. The first run, and every `--no-state` run, still starts with those defaults.

## Capabilities

### New Capabilities
- `session-state`: what kasset remembers between runs, where it keeps it, when it is saved and restored, how a damaged or foreign state file is handled, and the `--no-state` option.

### Modified Capabilities
- `audio-playback`: *Volume control* starts at the saved volume (100% when there is none).
- `audio-visualizer`: *Toggle the visualizer* starts from the saved setting and the setting lasts across runs.
- `lyrics`: *Toggle the lyrics pane* starts from the saved setting and lasts across runs; *No lyrics* shows "Nothing playing" only when nothing was resumed.
- `player-tui`: *kasset command* gains `--no-state`; *Pane focus* starts on the restored playlist when one was open.

## Impact

- New `src/state.rs` (pure: the state model, parsing and writing the file format, merging) and `src/state/store.rs` (file I/O: location, bounded read, atomic write). No new dependencies: the format is a small hand-written line format, read through `bounded_read`.
- `src/playback.rs`: set volume/shuffle/repeat directly, and resume a track paused at a position from a given source.
- `src/app.rs`: snapshot and restore of the session; a periodic save from `App::tick`; the final snapshot in `App::shutdown`, written by `App::save_final`; restoring the shown playlist and focus; keeping a resume point that could not be resumed.
- `src/main.rs`, `src/cli.rs`: the `--no-state` flag, choosing the store, and writing the final save (and reporting a failure) after the terminal is restored. `src/terminal.rs` keeps its interface.
- `README.md` (new "Saved state" section, Known limitations), `CLAUDE.md` (architecture line for `state.rs`; the "kasset must never write outside `Playlists/`" rule becomes "never writes inside the library folder outside `Playlists/`; the only other file it writes is its own state file").
- Files: one new file per user, e.g. `~/.local/state/kasset/state` on Linux. Nothing in the music folder changes.
