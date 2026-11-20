# Tasks

Every group ends green: `cargo test` (the whole suite), `cargo clippy --all-targets -- -D warnings` and `openspec validate persist-session-state --strict` pass before the group is handed off for review, and the code stays warning-free on non-unix targets (unix-only code and tests behind `#[cfg(unix)]`). Each group updates the `//!` docs of the modules it touches **and its own line in `CLAUDE.md`'s Architecture section** in that group, not later.

Test hermeticity, for every group: no test calls `dirs` or builds a store from the real per-user location. `FsStateStore` is always built from an explicit path in a `tempfile` folder, `location` and `make_store` are pure, `App::new` keeps using `NoState`, and existing test helpers never call `App::restore`.

## 1. Pure state model and file format (new `src/state.rs`)

- [ ] 1.1 Add `src/state.rs` (declared from `main.rs`) with a `//!` doc stating its role and invariants: pure, no I/O, the file is untrusted, parsing never panics or stalls, a resume point only names a path relative to the root, and `render` never exceeds the read cap. Implement `Settings`, `Resume`, `SavedSource`, `Session`, `RootKey` (canonical root bytes, compared exactly), `RelPath`, `StateFile`, `parse`, `render` and `merge(file, root, session, keep)` as in D1, D3, D6 and D7: header and `FORMAT_VERSION`, percent-encoding, folders most recently used first, `MAX_FOLDERS` 20, `MAX_STATE_BYTES` 64 KiB, forgiving per-line parsing. `RelPath` is built from a relative path's `Normal` components and validated on parse. Add the `CLAUDE.md` line for `state.rs`. Verify with unit tests:
  - `render` then `parse` gives back the same `StateFile`, including non-ASCII names, spaces, `%`, newlines and control characters in paths and playlist names, and (unix) non-UTF-8 path bytes;
  - spec *Damaged file* (random bytes, an empty file, no header → `Invalid`); spec *Invalid value ignored* (volume 250 ignored, `repeat all` kept); spec *Volume between steps* (63 → 60); unknown keys and bad escapes ignored line by line; a duplicate key keeps the first value;
  - CRLF line endings parse like LF; settings keys after the first `folder` line are ignored; a second entry for the same root is ignored, and `merge` leaves exactly one entry for the current root, first;
  - spec *Downgrade*: `kasset-state 2` → `Newer`; `kasset-state 0` and `kasset-state x` → `Invalid`;
  - spec *Path outside the library*: `track ../../etc/passwd`, `/etc/passwd`, `a//b`, `a/./b` and an encoded `%2F` inside a component are all rejected;
  - `merge`: other folders keep their order; the 21st folder drops the oldest (spec *Oldest folder forgotten*); the settings are replaced; with `keep` and a session without a track, the kept track, item, position and source are written, and with a current track `keep` is ignored;
  - spec *Long paths*: 20 entries with 4096-byte non-ASCII roots and tracks render to at most `MAX_STATE_BYTES`, current folder first, and `parse` of the output gives back exactly the entries written; a single entry that cannot fit is written without its track, and one whose `folder` line alone cannot fit is left out, with the settings still written;
  - a 64 KiB file of very long lines, 100 000 `folder` lines and a 40-digit number neither panic nor stall, and keep at most `MAX_FOLDERS` folders.

## 2. State file I/O (new `src/state/store.rs`)

- [ ] 2.1 Add the `StateStore` trait (`load`, `save(session, keep) -> Result<Saved, String>`), `FsStateStore`, `NoState` and the pure `location(state, local)` as in D1, D2 and D7. Loading goes through `bounded_read::read_limited`: regular files only, capped. Saving is read-merge-write: temp file `.state.tmp-<pid>-<random>` created with `create_new` (unix mode `0600`, and `0700` for a newly created folder), `sync_all`, `rename`. A name collision is retried once with a new random part, stale `.state.tmp-*` leftovers (older than 10 min) are swept, and the temp file is removed on failure. A `Newer` file gives `Ok(Saved::Skipped)`. Update the `CLAUDE.md` `state` line. Verify with tests on a `tempfile` folder:
  - `location`: the state dir wins, then the local data dir, each with `kasset/state` appended; `(None, None)` gives `None` (spec *No state folder known*);
  - save then load gives back the session; a missing file loads as empty; spec *Damaged file*: a file of random bytes loads as empty and the next `save` replaces it with one that parses;
  - spec *Owner-only permissions* (unix): the file mode is `0600` and the folder created is `0700`;
  - spec *Two instances* (sequential merge): two stores for different roots on the same file each save once, and both folders are in the file;
  - spec *Downgrade*: with a `kasset-state 2` file, `save` returns `Ok(Skipped)` and the file is byte-for-byte unchanged, both when the load already saw it and when it appears after the load;
  - spec *Not a regular file* (unix): a FIFO at the state path loads as empty without blocking;
  - spec *State folder not writable*: the state folder is a regular file → `save` returns an error naming the path, and no `.state.tmp-*` file is left anywhere;
  - a `.state.tmp-*` leftover with an old mtime (set with `File::set_modified`, std since Rust 1.75, so no new crate) is removed by the next save and a fresh one is kept; an existing `kasset` folder with mode `0755` keeps its mode;
  - spec *Symlinked state file* (unix): a symlink at the state path is replaced by a regular file and its target is unchanged;
  - `NoState` loads nothing and its `save` creates no file.

## 3. Playback setters and paused resume (`src/playback.rs`, `src/queue.rs`)

- [ ] 3.1 Add `Queue::set_repeat`, and `Playback::set_volume` (public), `set_shuffle`, `set_repeat` and `Playback::resume(source, items, item, position)` as in D5. Resume loads paused at the position: zero when a tag duration is known and the position is at or past it, and a retry at zero after any `SeekFailed` but not after other errors. It never skips to another track. On failure it leaves nothing current, `Stopped`, with the notice "Cannot resume …" (sanitised). Existing queue and playback tests must pass unchanged. Update the `CLAUDE.md` `playback.rs` line. Verify with `FakeAudio` tests:
  - spec *Resumed paused*: the state is `Paused`, the audio was loaded paused at 2:15, and `toggle_pause` plays from there;
  - spec *Position past the end*: a saved position past the tag duration loads at zero;
  - spec *Position cannot be reached*: a `SeekFailed` with `past_end` and one without both lead to one retry at zero, which succeeds;
  - spec *Track cannot be opened*, two cases: (a) a `SeekFailed` at the position, then a failure at zero → exactly two loads; (b) a plain open/decode error → exactly one load, no retry. Both end with nothing current, `Stopped`, one error notice, and no other track loaded;
  - resume with shuffle on keeps the resumed track current, and next/previous then follow a shuffled order of the source's items;
  - the setters change what `volume()`, `shuffle()` and `repeat()` report, and the volume reaches the audio.

## 4. Snapshot, restore and saving in the app (`src/app.rs`)

- [ ] 4.1 Give `App` a `Box<dyn StateStore>` (`NoState` from `App::new`, so existing tests are unchanged), and add the following (D4–D6):
  - `App::restore(store)`: settings first, then the shown playlist with main-pane focus (independent of the source), then the resume point, only through the scanned-track map and the playlist listing; `pending_resume` is kept until a track becomes current; a `Newer` load gives one `Info` notice and stops saving for the run;
  - `App::session()`, with `Detached` saved as the library;
  - the periodic save from `App::tick`: `SAVE_INTERVAL` 30 s, only when the session changed, passing `keep`, with the first failure notified once until a save succeeds;
  - `App::shutdown()`, which takes the final session before stopping the audio and writes nothing;
  - `App::save_final() -> Option<String>`, which writes it.

  Update the `CLAUDE.md` `app.rs` line. Verify with key-sequence tests using a recording/failing fake store and the tempfile playlist store:
  - spec *Settings restored*, *First run* and *Settings shared across folders* (one fake file, two apps on different roots), and restoring with the visualizer/lyrics off shows no notice;
  - spec *Separate folders keep separate resume points*: two apps on different roots save into one in-memory file, and a third app on the first root resumes its track;
  - spec *Resumed in its playlist*, *Track gone* ("Running" still shown, no error), *Playlist gone*, *Track no longer in the playlist* and *Source and shown playlist differ*; the saved `item` is used when it still points to the track, else the first matching entry;
  - spec *Path outside the library* at app level: a fake store naming `../../etc/passwd`, or any track the scan did not find, makes `FakeAudio` record no `load` at all;
  - spec *Track cannot be opened* at app level: the status line shows "Cannot resume …", and nothing is current;
  - spec *Nothing to resume*: a first run in a folder whose user shows "Running" and plays nothing → `save_final` writes no track but keeps `open`, and a new app on the same file has nothing current and shows "Running";
  - spec *Unresumable point is kept* and *Playing replaces the resume point*: after a failed resume, `save_final` writes the kept track and position; after the user plays another track, it writes that one;
  - spec *Lyrics: Resumed track*: after `restore` and a `tick`, the lyrics pane has the resumed track's lyrics, not "Nothing playing";
  - with `NoState`, `tick` never builds a session (a counting fake confirms no snapshot work);
  - spec *Nothing changed*: ticks over 5 minutes while paused save nothing; spec *Saved while running*: while playing, a save happens at 30 s and not before, holding the position at that time;
  - spec *Repeated failures are not repeated*: a store that always fails gives one error notice over several intervals, and another after a success then a failure;
  - spec *Downgrade*: a `Newer` store gives one `Info` notice at restore, no save attempts from `tick`, and `save_final` returns `None`;
  - spec *Saved at quit*: `shutdown` keeps the position from before the stop, writes nothing, and `save_final` then saves it; with a failing store, `save_final` returns the error; `save_final` without `shutdown` does nothing.

## 5. Command line, wiring and docs (`src/cli.rs`, `src/main.rs`, README, CLAUDE.md)

- [ ] 5.1 Add `--no-state` to `Cli` (D8), and the pure `make_store(no_state, location, root)` in `main.rs`. `main` always calls `App::restore(make_store(…))` before `terminal::run`, then passes its result to `finish(app, result)` (D6). `finish` calls `App::save_final()` for every outcome, prints `kasset: cannot save state to <path>: <reason>` on failure, and returns the result unchanged. `terminal.rs` is unchanged. Verify with tests:
  - `finish` with a recording fake store saves exactly once for `Ok(Exit::Quit)`, `Ok(Exit::Signal(15))` and `Err(_)`, and returns each unchanged; with a failing store it still returns the same result;
  - `--no-state` parses and is listed in `--help` (spec *Listed in the help*);
  - `make_store(true, Some(path), root)` and `make_store(false, None, root)` give a store whose save creates no file. `make_store(false, Some(path), root)` gives one that writes `path` (spec *Private session*, *No state folder known*);
  - spec *State kept outside the music folder*: after a save through `make_store` with the state path in one temp folder and the root in another, the root folder's listing is unchanged.
- [ ] 5.2 Update `README.md`:
  - add a "Saved state" section covering what is saved, the per-folder resume, the location on each OS, `--no-state`, how to reset by deleting the file, that a symlinked file is replaced (symlink the folder instead), and the privacy note;
  - add `--no-state` to the usage line;
  - in "Known limitations", remove "saved state between runs", and add that the shuffled order is not restored and that the resume point follows the file name.

  Update `CLAUDE.md`:
  - the data flow in `main.rs` (`App::restore`, `save_final` after `terminal::run`);
  - the write rule: "never writes inside the library folder outside `Playlists/`; the only other file written is the per-user state file". This means rewording the end of the playlist line in `CLAUDE.md` ("kasset must never write outside `Playlists/`"), and the README "Playlists on disk" sentence "This is the only place kasset writes to: nothing outside `Playlists/` is ever modified, …" so it says this is the only place in the music folder kasset writes to, and that the only other file it writes is its saved state (see Saved state);
  - add `session-state` to the current specs list.

  Verify by reading both rendered sections.

## 6. Integration check

- [ ] 6.1 Run a scripted check with the release build, a pty harness answering DSR, `XDG_STATE_HOME` set to a temp folder, and a generated fixture library of WAV files in a temp folder. Each step has a pass/fail assertion:
  - play a track, set the volume to 60%, turn shuffle on, set repeat to "all", hide the visualizer, wait 5 s, and quit with `q`;
  - start again: the screen shows vol 60%, shuffle on, repeat all and no spectrum strip; the track is current and paused, and its position is within 1 s of where it was at quit; `Space` makes the position advance;
  - `--no-state`: the screen shows vol 100%, and `cmp` of the state file before and after the run passes;
  - `stat -c %a` of the file prints `600`;
  - play a track for 5 s and send SIGTERM: on restart the track is paused within 1 s of where it was (the save lands within the 2 s signal grace period);
  - with random bytes in the file: the start shows the defaults and no error, and after quitting the file parses (header `kasset-state 1`);
  - with the state folder replaced by a regular file (no `chmod`): quitting prints `kasset: cannot save state to …` on stderr, and `$?` is `0`.

  Then run `cargo +1.89 build` (no new dependencies, so the MSRV is unchanged) and `openspec validate persist-session-state --strict`.

## Workflow follow-up

- Hand each group to the reviewer and fix its findings before starting the next.
- Sync the deltas into `openspec/specs/` (new `session-state`; `audio-playback`, `audio-visualizer`, `lyrics`, `player-tui`) and archive the change once it is reviewed.
