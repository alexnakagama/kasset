# Design

## Context

See proposal.md for motivation and specs/playlists/spec.md for behaviour. Current state that shapes the approach:

- The library root is canonicalized by `cli::resolve_root`; track paths are `root` joined with what `walkdir` yields (`follow_links(true)`), so they are lexically under the canonical root but not themselves canonicalized. Discovery deduplicates by `(dev, ino)`.
- `Queue` is a permutation of `0..len` over the whole library, with `pos` into it; shuffle reshuffles that permutation. With `Repeat::One`, `next(Ended)` returns `current()`. `Playback` owns the `Queue` and the tracks.
- `App` keeps `visible: Vec<usize>` of **library indices**, a `Mode` (Normal / Search), and transient notices. It does no I/O and is fully tested with `SilentAudio`.
- All I/O happens on the UI thread; there is no background worker.
- Project hardening style: bounded inputs (scan entry caps, query length), sanitised display text, `unsafe` forbidden, clippy pedantic.

## Goals / Non-Goals

**Goals:**
- Keep `App` free of filesystem calls: it talks to a small storage trait so key-sequence tests run against a temp-dir store.
- One play-order abstraction for library and playlists, so shuffle/repeat/failure-skip logic is not duplicated.
- Never lose or corrupt user data in a playlist file: unmatched entries, comments, duplicates, BOM, line endings and non-UTF-8 content survive.

**Non-Goals:**
- Watching the `Playlists` folder for changes (conflicts are detected at save time; the playlists side is refreshed when entered).
- Canonicalizing every library path (too slow for large libraries).
- Editing non-UTF-8 playlists or `.m3u` (legacy encoding) support.

## Decisions

### D1. File model: raw lines with their endings
`Playlist { name, path, bom: bool, lines: Vec<Line>, editable: Result<(), ReadOnlyReason>, stamp: FileStamp }`. Each `Line { bytes: Vec<u8>, ending: Ending }` with `Ending::{Lf, CrLf, None}` (only the last line may be `None`), plus a parsed kind: `Entry { resolved: Option<usize> /* library index */ }` or `Other` (comments, `#EXTM3U`, `#EXTINF`, blank). Serialising writes BOM + each line's bytes + its ending, so an unchanged playlist round-trips byte-identically (spec: *File formatting preserved*). New lines use the first line's ending (`Lf` for a new file); appending after a last line with `Ending::None` first gives that line the file's ending. New entries are written with an `#EXTINF:<secs>,<artist> - <title>` line before them; on remove, an `#EXTINF` immediately preceding the entry is removed with it, and on move it travels with it.

If the content (after the BOM) is not valid UTF-8, entries are still parsed for playback (on Unix the bytes are used as an `OsStr`; on Windows lossily) but `editable` is `Err(NotUtf8)`, and every mutating operation returns that error without touching the file (spec: *Playlists that cannot be edited safely*).
- *Alternative*: store only matched library indices and regenerate the file. Rejected: loses hand edits and other players' entries.
- *Alternative*: `String` lines with lossy decoding. Rejected: saving would corrupt Latin-1 files.

### D2. Path resolution and matching
Each entry is resolved against the playlist's folder (absolute used as-is, relative joined) and lexically normalised (`.`/`..` removed, no filesystem access). Matching against a `HashMap<PathBuf, usize>` built once from library track paths tries, in order:
1. the literal entry (so `AC\DC - Song.mp3` on Unix matches);
2. on non-Windows only, the entry with `\` read as separator (Windows-written playlists);
3. for entries still unmatched, `canonicalize` of the candidate compared with lazily canonicalized track paths (cost proportional to misses).
Written entries are relative to the `Playlists` folder (common prefix, then `..` segments), joined with `/`; components themselves are written literally. Entries that cannot be made relative (different drive on Windows) are written absolute. Implemented locally, no new dependency.
- *Alternative*: absolute paths. Rejected: the user wants playlists inside the music folder; relative paths make the folder portable.

### D3. Storage boundary
`trait PlaylistStore { fn list(&self) -> Result<Vec<PlaylistInfo>>; fn load(&self, name) -> Result<Playlist>; fn save(&self, &mut Playlist) -> Result<SaveOutcome>; fn create(..); fn rename(..); fn delete(..) }`. `FsStore` implements it over `<root>/Playlists`; tests use `FsStore` on a `tempfile::TempDir`, so no mock is needed. `App<A: Audio>` gains a `Box<dyn PlaylistStore>` passed from `main.rs`.

### D4. Saving, creating, renaming
- **Target**: the playlist path, or, if it is a symbolic link, its resolved target (`canonicalize`), so the link survives (spec: *Saving keeps file identity*).
- **Save**: in the target's folder, create `.<name>.m3u8.tmp-<pid>` with `create_new`, write, copy the target's permission bits onto it, `sync_all`, then `rename` over the target. Ownership is not preserved (documented; kasset normally owns the files it edits). On any error the temp file is removed.
- **Conflict check**: `FileStamp = (len, mtime)` captured on load; before saving, the target is stat'ed and a different stamp returns `SaveOutcome::ChangedOnDisk`, upon which `App` reloads and reports (spec: *External changes*). After a successful save the stamp is **refreshed from the written file's metadata**, so consecutive edits do not report a false conflict.
- **No-clobber create and rename**: write the new file under a temp name, then `fs::hard_link(temp, final)` (fails with `AlreadyExists` if a file appeared meanwhile) and remove the temp; rename is `hard_link(old, new)` + `remove_file(old)`. Where hard links are unsupported (FAT/exFAT, some network filesystems) fall back to an existence check + `rename`; the race between check and rename is a documented residual risk. A case-only rename goes through a temp name with `rename`, since the case-insensitive filesystems that need it make `hard_link` fail on "existing" names.
- **Delete**: `remove_file` on the playlist path itself (removes a link, never its target or any audio file).
- *Alternative*: `renameat2(RENAME_NOREPLACE)`. Rejected: Linux-only and needs `unsafe`/extra rustix features; `hard_link` is portable std.

### D5. Bounds and names
Files over 16 MiB are read only up to that size and files with more than 100 000 entries are read up to the limit; either way the playlist is marked read-only (`editable = Err(TooLarge)`), because saving a truncated read would drop data. Adds that would push a playlist past 100 000 entries are refused as a whole. Names: the spec rules, at most 200 bytes UTF-8 so that `.<name>.m3u8.tmp-<pid>` (≤ 1 + 200 + 5 + 5 + 10 = 221 bytes) fits within the common 255-byte `NAME_MAX`; Windows reserved names (`CON`, `NUL`, `COM1`…) are refused so the folder stays usable there. Displayed names go through `text::sanitize`.

### D6. Generalise the play order
`Queue` becomes an order over **items**: `items: Vec<usize>` (library indices, duplicates allowed) and `order: Vec<usize>`, a permutation of `0..items.len()`; `select` and `current_item` work with item positions, `current()` still returns a library index. `Queue::new(len)` keeps its meaning (items = `0..len`).

Detached state, for a removed playing entry: `detached: Option<Detached { track: usize /* library index still playing */, next_slot: usize /* index into order */ }>`. While detached:
- `current()` returns `detached.track` (it is no longer in `items`).
- `next(cause)` for **any** cause, including `Ended` with `Repeat::One`, goes to `order[next_slot]` if it exists; if `next_slot == order.len()` it behaves as at the end of the order (repeat off: `None`; otherwise wrap to slot 0). Detachment clears.
- `prev()` goes to `order[next_slot - 1]` if `next_slot > 0`; otherwise as at the start (wrap with repeat on, `None` with repeat off). The 3-second restart rule in `Playback::prev` applies first, unchanged.
- `toggle_shuffle` clears `next_slot` to the start of the new order; the detached track keeps playing.
- Further edits adjust `next_slot` like any position.

Edit operations:
- `remove_item(i)`: drop `i` from `items` and `order`, renumber; if `i` was current, enter the detached state with `next_slot` = the old `pos`, i.e. the slot that now holds the entry that followed it **in play order** (the shuffled order when shuffle is on, not the playlist-order successor). `prev()` while detached likewise uses the preceding slot of the play order.
- `move_item(from, to)`: reorder `items`; in linear mode `order` stays identity and `pos` follows the current item; in shuffle mode the permutation is relabelled so the shuffled sequence is unchanged.
- `append_items(tracks, rng)`: linear mode appends; shuffle mode inserts each new item at a uniformly random slot after the current position (spec: *Add to playing playlist*).
- `replace_items(tracks, rng)` for a reload: the playing entry is located by preferring the same item position if it still holds the same library track, else the first occurrence of that track; if absent, enter the detached state at the old position clamped. With shuffle on, reshuffle with the current item first.

`Playback` gets `play_source(source: Source, items: Vec<usize>, start: usize)` and forwards edits. `Source` is `Library`, `Playlist(name)` or `Detached` and is what the UI compares to decide the playing marker. `App` forwards to `Playback` **every** change to the playlist that is the source, whatever triggered it: `d`/`J`/`K`, `a`/`A` from the library, and reloads after an external change. Rename updates the source name; delete turns it into `Detached`.
- *Alternative*: a second queue type for playlists. Rejected: duplicates shuffle/repeat/failure logic and its tests.

### D7. App views and modes
`App` holds `library: ListState` and `playlists_side: PlaylistsSide` where `PlaylistsSide` is `List(ListState)` or `Open { playlist, list: ListState, back: ListState }`; `ListState` is rows + selection + query. `Tab` swaps which side is shown, so both sides keep their state (spec: *Switch between library and playlists*). Entering the playlists side refreshes the listing; an open playlist whose stamp changed is reloaded, and one that no longer exists falls back to the list with a notice. Modes are extended with `Picker { tracks, selected }`, `NameInput { purpose: Create(tracks) | Rename(name), text }` and `ConfirmDelete(name)`. Search keys for playlist rows reuse the per-track library keys. Name input reuses the search-input character rules plus the name rules, with the rejection reason shown inline. Read-only playlists appear in the picker marked as such.

### D8. UI
Picker and prompts are a centred `Clear` + bordered popup over the list. The list title shows the view: `kasset · <root>`, `Playlists`, `Playlist · Running` (plus `read-only` when applicable). Key help strings per view/mode. `TestBackend` render tests for the picker and the empty-playlists message.

### D9. Testing write failures
Tests must not rely on `chmod`, which root ignores (CI containers often run as root). Failures are forced in ways that hold for every user: make `Playlists` a regular file (create fails with `NotADirectory`), or pre-create a directory at the deterministic temp path `.<name>.m3u8.tmp-<pid>` (`create_new` fails). Permission-preservation and symlink tests are `#[cfg(unix)]`.

## Risks / Trade-offs

- [Lexical matching misses tracks reached through a symlink differently than the playlist path] → canonicalize fallback for unmatched entries only; worst case the entry is reported as unmatched and preserved, never lost.
- [An entry pointing at a hard link of a library file is reported unmatched: discovery keeps one path per `(dev, ino)` and canonicalize does not merge hard links] → accepted; the entry is preserved and counted, and plays if the user adds the library's path instead.
- [On Unix, a legal filename containing `\` that does not exist literally may match a different file through the `\`→`/` fallback] → literal match is tried first, so it only happens when the literal file is absent; accepted.
- [mtime granularity (1–2 s on some filesystems) can miss an external change made within the same second] → also compare length; accepted residual risk, documented.
- [No-clobber create/rename falls back to check-then-rename where hard links are unsupported] → tiny race window on FAT/exFAT and some network mounts; documented.
- [Saving replaces the inode: ownership and extended attributes are not preserved; permission bits are] → documented.
- [Saving on the UI thread on a slow network mount can freeze the UI] → same as existing open/seek behaviour; files are small. Documented in README known limitations.
- [Writing into the user's music folder may surprise users or sync tools] → only under `Playlists/` (or a symlink target the user set up), only on explicit edits; temp files are dot-prefixed and removed on failure.
- [Queue generalisation risks regressions in existing playback] → all existing queue/playback tests must pass unchanged against `Queue::new(len)` before playlist tests are added.

## Migration Plan

Additive: no existing files are read differently; users without a `Playlists` folder see no change until they create a playlist. Rollback is reverting the change; playlist files remain valid `.m3u8` usable by other players.

Note: the capabilities from `add-music-player-tui` are not archived yet, so this change only ADDs a new `playlists` capability. Archive `add-music-player-tui` before archiving this one.

## Open Questions

- Should `A` with no filter (whole library, possibly thousands of tracks) ask for confirmation? Default: no, the status message reports the count; can be added later without changing the design.
