# Design

## Context

See `proposal.md` for the motivation and `specs/session-state/spec.md` for the rules. The parts of the current code that shape the approach:

- `main.rs` builds everything in order: `cli::resolve_root` (canonical root) → `Player::open` → `library::scan` → `App::new(tracks, player, FsStore, …)` → `terminal::run`. `terminal::run` ends the loop on quit or on a termination signal, calls `App::shutdown` (which stops audio) and then restores the terminal. Emergency exits (second signal, watchdog) end the process without returning.
- `Playback::new` always starts at `Volume::MAX` with shuffle and repeat off. Shuffle and repeat live in `Queue` (`set_shuffle`, `cycle_repeat`); there is no setter for repeat or volume. Tracks start only through `start_current(paused)`, always at `Duration::ZERO`; `Audio::load(path, start, paused)` can already open a track at a position, paused.
- `App` reaches files only through the `PlaylistStore` trait (`FsStore` in production, a tempfile store or `no_store()` in tests). `App::new` is called by hundreds of tests.
- `Visualizer::on` and `LyricsPane::on` start `true`; `open_named(name)` opens a playlist in the main pane.
- `bounded_read::read_limited` is the reader for small untrusted files (regular files only, no blocking on a FIFO, size cap). `playlist/store.rs` has the atomic temp-file + rename write, with stale-temp cleanup, for playlists.
- No serialisation crate is in the tree. Paths can be non-UTF-8 on Unix.

## Goals / Non-Goals

**Goals:**
- The state file can never make kasset open a path the scan or the playlist listing did not produce, and can never panic, stall or flood the screen.
- Saving costs nothing noticeable: no work while nothing changes, one small file write at most every 30 s otherwise.
- Tests never touch the real state folder.

**Non-Goals:**
- Saving list selections, filters, scroll positions, focus beyond the restored playlist, or the shuffled order itself (a resumed shuffle builds a new order starting at the resumed track).
- Locking between concurrent kasset processes beyond read-merge-write.
- A configuration file. The state file is written by kasset and not meant for hand editing; it is plain text, but non-ASCII is percent-encoded.
- Tying the resume point to a track's identity: it is keyed by relative path only, so a file replaced by a different song with the same name resumes at the old position (clamped as in D5).

## Decisions

### D1. Two modules: pure `state.rs`, I/O in `state/store.rs`

`state.rs` holds the model and the file format, with no I/O, like `lyrics.rs` and `library/infer.rs`:

```
pub struct Settings { volume: Volume, shuffle: bool, repeat: Repeat, visualizer: bool, lyrics: bool }
pub struct Resume { track: RelPath, item: Option<usize>, position: Duration,
                    source: SavedSource /* Library | Playlist(String) */, open: Option<String> }
pub struct Session { settings: Settings, resume: Option<Resume> }   // what one run saves
pub struct RootKey(Vec<u8>)  // the canonical root's bytes (D3 encoding source), compared exactly
pub struct StateFile { settings: Option<Settings>, folders: Vec<(RootKey, FolderEntry)> }
pub fn parse(bytes: &[u8]) -> Parsed  // Ok(StateFile) | Newer | Invalid
pub fn render(file: &StateFile) -> Vec<u8>   // never more than MAX_STATE_BYTES (D3)
pub fn merge(file: StateFile, root: &RootKey, session: &Session, keep: Option<&Resume>) -> StateFile
```

`state/store.rs` has `trait StateStore { fn load(&mut self) -> Loaded; fn save(&mut self, session: &Session, keep: Option<&Resume>) -> Result<Saved, String>; }` with `Saved::{Written, Skipped}`, implemented by `FsStateStore` (real file) and `NoState` (loads nothing, saves nothing). `Loaded` carries the settings, this folder's resume point, and whether the file was `Newer`. Two pure helpers make the wiring testable without touching `dirs`: `location(state: Option<PathBuf>, local: Option<PathBuf>) -> Option<PathBuf>` (D2) and, in `main.rs`, `make_store(no_state: bool, location: Option<PathBuf>, root: &Path) -> Box<dyn StateStore>` (D8). `App` owns a `Box<dyn StateStore>` like it owns its `PlaylistStore`. `App::new` keeps its signature and uses `NoState`, so existing tests are unchanged and never touch the home folder; `main` installs the real store with `App::restore(store)`.

*Alternative:* load/save in `main.rs` around `terminal::run`. Rejected: the periodic save needs the running app, and a store trait keeps it testable with a fake that records saves or fails.

### D2. Location

`dirs::state_dir()` (Linux: `$XDG_STATE_HOME` or `~/.local/state`) else `dirs::data_local_dir()` (macOS: `~/Library/Application Support`, Windows: `%LOCALAPPDATA%`), joined with `kasset/state`. `dirs` is already a dependency and ignores relative XDG values. When neither is known, `main` uses `NoState` (spec *No state folder known*). The folder is created on first save (`create_dir_all`; on Unix the parents (e.g. `~/.local/state`) are created with a plain `create_dir_all`, then a newly created `kasset` folder alone is made with `DirBuilder` mode `0700`, while an existing one is left as it is, since the file's own `0600` is what protects it; on Windows the per-user folder's ACL is what keeps it private, and the mode calls are `#[cfg(unix)]`).

### D3. A versioned line format, no new dependencies

```
kasset-state 1
volume 60
shuffle on
repeat all
visualizer off
lyrics on
folder /home/alex/M%C3%BAsica
track Band/LP/01%20Song.flac
item 3
position-ms 135000
source playlist Running
open Running
folder /mnt/other
…
```

- Line 1 is the header with the format version. A missing or malformed header → `Invalid` (defaults, the file is replaced on save). A version above `FORMAT_VERSION` (1) → `Newer` (defaults, and saving is disabled for the run: spec *Newer state files are kept*).
- Settings lines come before the first `folder`; each `folder` line starts an entry, and the lines after it belong to it. Folders are written most recently used first, at most `MAX_FOLDERS` (20).
- Values are percent-encoded: every byte outside `0x21..=0x7E`, and `%`, is written as `%XX`, so any path or name fits on one line and the file stays printable. On Unix, paths are encoded from their raw bytes (`OsStrExt`); on Windows from their UTF-8 form, and a path that is not valid Unicode is simply not saved (no resume for it).
- Parsing is line by line and forgiving: an unknown key, a bad value (volume above 100, an unknown repeat word, bad percent escapes) or a duplicate key is ignored on its own (the first value wins). A trailing `\r` is stripped (a file saved with CRLF still reads). Settings keys after the first `folder` line are ignored. A second `folder` entry for a root already seen is ignored (the first wins), and `merge` removes every entry for the current root before putting the new one first. The volume is rounded down to a multiple of 5. Reading is capped at `MAX_STATE_BYTES` (64 KiB) through `bounded_read`; a truncated read drops its last (possibly cut) line, and at most `MAX_FOLDERS` entries are kept. Scanning is linear and every number is parsed with checked arithmetic, so a crafted file can't panic or stall.
- **Writing respects the read cap.** Percent-encoding can triple a path, so 20 long non-ASCII entries can exceed 64 KiB. `render` adds folders most recently used first and stops before the output would pass `MAX_STATE_BYTES`; if even the current folder alone does not fit (only possible with very long paths, e.g. Windows verbatim paths of up to ~32 K characters), it is written without its track and source, and if its `folder` line alone does not fit, the folder is left out and only the settings are written. What is written is therefore always read back whole (spec *Long paths*).
- **Windows.** The root key is the raw canonical root from `resolve_root` (the verbatim `\\?\C:\…` form), stable across runs; it is never shown on screen (the UI shows `display_path(root)`, as today). A root or a track path that is not valid Unicode is silently not saved (no resume point for that folder or track).

*Alternatives:* `serde` + `toml`/`serde_json`. Rejected: three or more new crates (and MSRV checks) for about ten keys, and neither format can hold a non-UTF-8 path without an encoding of our own anyway.

### D4. The resume point only ever selects what the scan and the listing found

- The track is stored **relative to the root**, as its `Normal` components joined with `/` (also on Windows). On restore, a relative path with an empty, `.` or `..` component, a separator inside a decoded component, or a root/prefix is rejected. A valid one is looked up in a map from each scanned track's relative path (`strip_prefix(root)`) to its library index, built once at restore. Only a hit is used, so the file can't name a path the scan didn't find (spec *Path outside the library*).
- The root key is the canonical root from `cli::resolve_root`, encoded as above, compared byte for byte.
- Playlist names (`source playlist`, `open`) are used only if the sidebar listing has a playlist with exactly that name; then the playlist is opened through the existing `PlaylistStore`. Text from the file only reaches the screen through `text::sanitize` (e.g. in "Cannot resume …").

### D5. Restoring: settings first, then the resume point, paused

`App::restore(store)` runs once, after `App::new` and before `terminal::run`:

1. `store.load()`. `Newer` and `Invalid` give no settings and no resume point.
2. Settings: new `Playback::set_volume`, `set_shuffle` and `set_repeat` (`Queue` gets `set_repeat`); `visualizer.on` and `lyrics.on` are set without a notice.
3. Shown playlist: if `open` names a listed playlist, `open_named` it and focus the main pane. This is independent of the track and of the play source (spec *Track gone*, *Source and shown playlist differ*).
4. Resume: find the track (D4). The play source is the named source playlist when it is listed and, once loaded, holds the track: the saved `item` if it still points to that track, else the first entry that does. Otherwise it is the library. A source playlist that is not the shown one is loaded through the store only to get its items; the main pane is not changed.
5. New `Playback::resume(source, items, item, position)`: resets the queue to `items` from `source` and selects `item` (with shuffle on, `select` builds an order starting at it). Then `audio.load(path, start, paused = true)`, where `start` is the saved position, or zero when a tag duration is known and the position is at or past it. With no tag duration, only the retry below protects the resume. If the load fails with any `SeekFailed` (past the end, seeking unsupported, a decoder seek error), it is retried once at zero, because the file itself is playable (spec *Position cannot be reached*). Any other load error (open or decode) is not retried, so a broken file is loaded once. Any other failure leaves the queue with nothing current and the state `Stopped`, and gives the notice "Cannot resume <sanitised title>: <reason>" (spec *Track cannot be opened*). Unlike `start_current`, a failed resume never skips to the next track: resuming must never start a different track on its own.

### D6. What is saved, and when

`App::session()` builds a `Session` from the live state:
- settings: from `Playback` and the two panes;
- resume: the current track, as a relative path, with `current_item()`, the position (`Playback::position()`, which is zero while stopped) in whole milliseconds, the source (`Source::Detached` is saved as the library), and the open playlist's name. With no current track, the resume point has no track but still keeps `open`.

**Keeping an unresumable point.** `App` keeps the loaded resume point as `pending_resume` until a track becomes current during the run (by resuming it, or by the user playing anything); then it is dropped. While it is kept, every save passes it as `keep`, and `merge` uses its track, item, position and source when the session has no current track. A missing drive or a transient failure therefore doesn't erase the resume point, but playing something does replace it (spec *Unresumable point is kept*, *Playing replaces the resume point*).

**Periodic save.** When the store is `NoState` or the file was `Newer`, `tick` skips this entirely and never builds a `session()`; hundreds of existing tests drive `tick` with `NoState`. Otherwise `App::tick(now)`, at most every `SAVE_INTERVAL` (30 s), compares `session()` with the last session saved (or loaded) and saves only if they differ. While paused or stopped nothing changes, so nothing is written (spec *Nothing changed*). While playing, the position changes, so the file is rewritten about twice a minute. That is cheap (a few hundred bytes). Each write ends with `sync_all` before the rename, so the atomicity the spec promises holds after a power loss too. On a slow home folder (NFS) that can hold one frame for a moment every 30 s; this is accepted, like the playlist saves that already `fsync` on the UI thread.

**Final save.** `App::shutdown()` (called by `terminal::run` before the screen is restored, as today) takes the final `Session` **before** it stops the audio (stopping resets the position to zero), keeps it, then stops. Nothing is written yet, so a slow disk cannot hold the screen. `terminal::run` keeps its `Result<Exit>` signature. After it returns, `main` passes the result to a small `finish(app, result) -> Result<Exit>`. For every outcome (`Ok(Exit::Quit)`, `Ok(Exit::Signal(_))` or `Err`), `finish` calls `App::save_final() -> Option<String>`, which writes the kept session (or does nothing if `shutdown` never ran or the store is `NoState`). It prints `kasset: {message}` when that fails, and returns the result unchanged. `FsStateStore` builds the message itself (`cannot save state to <display_path(file)>: <reason>`), since only it knows the path, and `NoState` never fails. Keeping this in one function lets a unit test check every exit path. The exit status is unchanged. Emergency exits don't save. The signal watchdog (`SIGNAL_GRACE`, 2 s after the first signal) is not cancelled when `run` returns, so after a signal the final save is best-effort: it races that timer, and a watchdog exit during it leaves the last periodic save, because the write is atomic. A hang-up from a closed terminal that the loop never notices saves nothing, as the spec allows.

**Failures while running.** The first failed periodic save calls `App::notify` with an error notice. Later failures are silent until a save succeeds again (spec *Repeated failures are not repeated*).

**Newer file.** A `Newer` load gives one `Info` notice at start ("Saved state is from a newer kasset; it is not changed this run") and makes the app stop saving for the run: `tick` never compares or saves, and `save_final` does nothing. The store also answers `Ok(Saved::Skipped)` to any save while its file is `Newer`, including one found newer at save time, so a downgrade is never reported as a failure (spec *Downgrade*).

### D7. Writing: read, merge, replace atomically

`FsStateStore::save(session)`:
1. Read the current file again (bounded, as in D3). `Newer` → `Ok(Skipped)` with nothing written (D6). `Invalid` or missing → start from an empty `StateFile`, so a damaged file is replaced by a valid one.
2. `merge`: replace the settings; remove every entry for this root and put the new one first (with `keep` filling in the track, D6); keep the other entries in order; cut to `MAX_FOLDERS`. Another process's entries saved meanwhile therefore survive (spec *Two instances*; this is a sequential merge, and a true same-instant race is accepted below).
3. Write `render(...)` to `.state.tmp-<pid>-<random>` (a random `u64` in hex from `rand`, already a dependency) in the same folder with `create_new` (on Unix with mode `0600`), `sync_all`, then `rename` over `state`. A rename replaces a symlink at `state` instead of writing through it (spec *Symlinked state file*; README says to symlink the folder, not the file, if needed). The random part means two processes never share a temp name, even in containers whose pid is always the same, so one can never remove or rename the other's half-written file. If the name exists anyway, creation is retried once with a new random part. Leftovers of crashed or killed runs are swept at save time: regular files named `.state.tmp-*` in the folder that are older than `STALE_TEMP_AGE` (10 min, as in `playlist/store.rs`) are removed, best effort. On failure the temp file is removed. On Windows, `rename` replaces an existing file; if another program holds `state` open (antivirus, backup), the rename fails and is reported like any save failure.

On non-unix targets `bounded_read::open_regular` checks the path with `metadata` (which follows a symlink) before opening; that is accepted for reading the state file, since reading through a link is harmless and the size cap still applies.

The ~40-line write helper is written for the state store rather than shared with `playlist/store.rs`. Sharing it would mean refactoring the playlist save path (owner/permission copying, stamps), which is well tested and not part of this change.

*Alternative:* a lock file to serialise concurrent saves. Rejected: two kasset processes saving within the same few milliseconds is very unlikely, and losing one resume point in that case is harmless.

### D8. `--no-state`

`Cli` gets `#[arg(long)] no_state: bool`. `make_store(no_state, location, root)` returns `NoState` when the flag is set or no location is known, else `FsStateStore` for that file and root; `main` always calls `App::restore(make_store(…))`, so `--no-state` reads and writes nothing (spec *Private session*). `cli.rs`'s `--help` test checks the flag is listed.

Tests never call `dirs`: `FsStateStore` is always built from an explicit path (a `tempfile` folder), `location` and `make_store` are pure, and `App::new` keeps using `NoState`.

## Risks / Trade-offs

- [Resuming opens a file at startup, on the UI thread, so a stalled network mount delays the first frame] → The same is true of pressing Enter on that track today; it happens once and only for the saved track.
- [The state file shows which tracks were played, and in which folders] → It lives in a per-user folder with mode `0600` (Unix) and holds at most 20 folders; `--no-state` avoids it entirely. This is documented in the README.
- [The shuffled order is not restored] → The resumed track is the same; the rest of the order is new. Accepted (non-goal).
- [Two processes saving at the same instant can drop one folder's resume point] → Accepted (D7).
- [Behaviour change: users relying on starting at full volume with both panes on] → The defaults stay for the first run and with `--no-state`; the README explains the state file and how to reset it (delete it).
- [A kept resume point for a track that was deleted for good stays in the file until the user plays something in that folder] → Intended (it protects against unmounted drives); it is a few hundred bytes, and playing anything replaces it.
- [A replaced symlink surprises users who keep `state` in a dotfiles repo] → Documented; symlinking the folder works.
- [A new write location: kasset used to write only inside `Playlists/`] → The project rule is narrowed and stated in CLAUDE.md: never write inside the library folder outside `Playlists/`; the only other file written is the state file in the per-user state folder (D2, D7).

## Migration Plan

None needed: with no state file, kasset behaves exactly as before. Rolling back to an older version ignores the file. To reset, the user deletes the file.
