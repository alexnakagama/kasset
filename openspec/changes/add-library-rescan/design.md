# Design

## Context

See proposal.md (Why) and the `library-rescan` spec for behaviour. Current
state that shapes the approach:

- `main.rs` runs `library::scan(&root)` once (walk with `discover`, tags with
  `read_track_in`, `sort_tracks`) and hands the sorted `Vec<Track>` to
  `App::new`, which moves it into `Playback`. A track's **library index** is
  its position in that vector, and the library order *is* the vector order.
- Library indices are held by: `Queue` items (`(0..len)` for the library
  source, so item position == library index there), `up_next` /
  `queued_current`, `Playlist` lines (`Kind::Entry(Some(index))`, resolved by
  `Resolver` at load time) and `OpenPlaylist::items`, `Catalog` albums and
  `BrowseLevel::tracks`, `App::search_keys[index]`, the library `ListState`
  (rows are library indices), and `Picker` tracks. `Source::Artist(a)` /
  `Album(b)` hold catalog indices.
- `Queue::replace_items(tracks)` already keeps the current track current
  (same item, else first occurrence) and detaches the queue when the track
  is no longer listed. `App::reload(name)` → `install` → `replace_open` /
  `follow_reload` already re-read a playlist and make the open view and the
  playing order follow it.
- `App` has no I/O except through injected stores (`PlaylistStore`,
  `StateStore`), so tests use fakes. It only knows the root as a display
  string. There are no worker threads yet; `panic_guard::quietly` marks a
  **thread-local** flag, so a panic on another thread would reach the
  terminal's panic hook and print over the UI unless that thread runs
  inside `quietly` too.
- Keys: `r` is repeat, `R` rename; `u` is unused in every mode. Letters with
  Ctrl/Alt are deliberately not handled.

## Goals / Non-Goals

**Goals:**
- Library indices stay valid for the whole run, so a rescan does not have
  to find and rewrite every holder (D1).
- The UI thread never reads tags or walks folders for a rescan (D4).
- `queue.rs` and the existing queue/playback tests stay unchanged; tests that
  build `App`/`Playback` from a sorted track list behave exactly as today.
- Rescanned input gets the same untrusted-input treatment as the startup
  scan, by reusing the same functions.

**Non-Goals:**
- Folder watching, periodic rescans, rescanning on focus.
- Re-reading lyrics of the playing track after a retag (the lyrics pane is
  keyed by path and reloads on the next track).
- Restoring a `pending` saved resume point that a rescan now makes
  findable; the pending point is still kept for the next start as today.
- Freeing the memory of removed tracks during the run.
- Changing the startup scan, the state file or playlist files.

## Decisions

### D1. Append-only track table plus a library order

`Playback` keeps `tracks: Vec<Track>` **append-only** for the run: a rescan
replaces a track in place when its file changed, pushes new files at the
end, and marks tracks whose files are gone (`present: Vec<bool>`). A
separate `order: Vec<usize>` lists the **present** tracks in library order
(`library::sort_tracks`' key, applied to indices). A file that reappears at
a known path takes back its old index.

At startup the order is `0..len` (the scan already sorted the vector), so
every existing test and the startup behaviour are unchanged.

*Alternative: rebuild a fresh sorted vector and remap every holder.* About
ten holders across `Playback` and `App` (queue items, `up_next`,
`queued_current`, playlists, browse stacks, the catalog sources, selections,
the resolver, search keys, the picker), each with its own edge cases;
missing one silently plays or marks the wrong track. Rejected. The cost of
D1 is that removed tracks stay in memory until restart, bounded by the
number of distinct paths seen in one run.

### D2. The library list goes through the order

The library `ListState`'s rows become **positions in `order`** (like the open
playlist, whose rows are positions in its items). `Playback` gains
`library_order()`; the library source's queue items are `order` (not
`0..len`) after a rescan, so `Playback::play(track)` selects the item
position of `track` in the items (linear search, once per key press).
`resume` / `resume_queued` / the library `play` reset the queue to `order`.
The "Library" count uses `order.len()`; the library marks `current()` by
index as today.

### D3. File stamps for the incremental scan

`discover` already reads each file's metadata (following links) for the
hard-link check; it also returns a `TrackStamp { len, modified:
Option<SystemTime>, id: Option<(dev, ino)> }` (`id` on unix only) parallel to
the paths. `Library` carries the stamps, `main` hands them to `App`
together with the rescanner (D4); `App` keeps `stamps: Vec<Option<TrackStamp>>`
indexed like the track table. `None` (e.g. tracks given to `App::new` in
tests) means "unknown": the next rescan reads that file again. A file is
unchanged only when its stamp equals the stored one (all three fields).

Stamps live beside the table, not in `Track`: `Track`'s equality is what
decides whether a reread track counts as "changed" (spec: only what is
shown or sorted), and a new mtime alone must not count.

### D4. A `Rescanner` trait and a worker thread

```
trait Rescanner {
    /// Starts a rescan against the known tracks; Err if one cannot start.
    fn start(&mut self, known: Vec<Known>) -> Result<(), String>;
    /// The finished rescan, once (None while running or idle).
    fn poll(&mut self) -> Option<Result<Rescan, String>>;
    fn running(&self) -> bool;
}
Known  = { path: PathBuf, stamp: Option<TrackStamp> }     // index = position
Rescan = { found: Vec<Found>, skipped: usize, truncated: bool }
Found  = Same(index) | Reread(index, Track, TrackStamp) | New(Track, TrackStamp)
```

- `ThreadRescanner::new(root: PathBuf)` (in `library/rescan.rs`) spawns one
  `std::thread` per rescan with the `known` snapshot and an `mpsc` channel.
  The whole worker body runs inside `panic_guard::quietly`, so even a panic
  outside the tag reader is silent and becomes `Err("rescan failed")`; a
  disconnected channel is reported the same way. The thread is never
  joined: quitting drops the receiver and the process exit ends the thread
  (spec: quit does not wait). `std::thread::Builder::spawn` failure →
  `Err` from `start`.
- The worker calls the pure-ish `library::rescan(root, &known)`: `discover`
  (same rules, limits and hard-link dedupe as startup), then per found path
  an exact `PathBuf` lookup in `known` (both walks start from the same root
  value), and `read_track_in(root, path)` only for `Reread` / `New`.
- `App::new` installs `NoRescan` (`start` → `Err("rescanning is not
  available")`); `main` sets `ThreadRescanner` with
  `App::set_rescanner(rescanner, stamps)`, mirroring `App::restore`'s
  injected store. App tests use a `FakeRescanner` that returns a scripted
  `Rescan` on the next `poll`, so no test waits on a thread; the real
  rescanner is tested on a `tempfile` folder with the in-memory WAV
  fixtures, polling with a bounded wait.

*Alternative: scan synchronously on the key press.* Freezes the UI for the
whole walk (seconds on large or network libraries). Rejected.
*Alternative: a long-lived worker owning its own cache.* Two copies of the
truth; the snapshot is cheap (paths and stamps). Rejected.

### D5. Applying a rescan

`App::tick` polls the rescanner at its start (after `settle_pending`, before
the pending notice is taken), so the summary notice is shown in the same
tick and `playback.tick` already runs on the updated play order. On
`Ok(rescan)`:

1. **Safety check** (spec "A failed rescan keeps the library"): if it found
   no files while `order` is non-empty → error notice, nothing applied.
   `Err` → error notice "Cannot rescan the library: <sanitised reason>".
2. `Playback::apply_rescan(found) -> RescanSummary { added, removed, changed,
   pruned }`: replaces / appends tracks, recomputes `present` and `order`,
   prunes `up_next` of removed tracks (bumping `up_next_generation` only if
   it changed; `pruned` lists the old `up_next` positions removed, for the
   queue view's selection, D9), and, for `Source::Library` / `Detached`,
   replaces the play order's items with `order` / the present items through
   the private helper below. `queued_current` and the current track are
   never touched (spec: playback continues).

   *Helper `replace_play_order(items)`.* `Queue::replace_items` alone
   detaches a current track that is not in `items` at `next_slot` = its
   old slot, which does **not** account for removed items before it: delete
   the playing track together with the four tracks before it and the next
   track played would skip four. So when the queue's current track is
   attached and missing from `items`, the helper first inserts it into
   `items` right after the nearest preceding item of the *old* items that is
   still in `items` (at 0 if none), calls `replace_items`, then
   `remove_item` at that position: `remove_item` detaches the queue at the
   removed item's successor, which is exactly the first remaining track that
   followed it. Already detached, or current still listed: plain
   `replace_items`.
   `added` counts new indices and reappeared ones, `removed` tracks that
   were present and are not, `changed` `Reread` tracks whose `Track`
   differs.
3. `App` rebuilds what derives from tracks: `search_keys` (entries for
   reread/new indices), `stamps`, `Catalog::build` over the present tracks
   only, and drops the `Resolver` (`OnceCell::take`) so the next use
   rebuilds it from present tracks only.
4. Artist/album play order (D6), browse stacks (D7), playlists (D8),
   selections (D9).
5. One notice through `App::notify` (spec "Rescan summary"), joined with the
   startup scan's "skipped" / "too many entries" wording. `main.rs::scan_notice`
   moves to `app.rs` as `scan_notice(skipped, truncated) -> Option<Notice>`,
   used by `main` for the startup scan and by `App` for a rescan. It does
   **not** move into `library`: `Notice` belongs to `playback`, and
   `library` sits below it in the layering, so `library` only reports the
   plain counts (`skipped`, `truncated`) and never builds notices.

`Catalog::build` and `Resolver::new` gain a way to skip non-present tracks
(an index filter that keeps each track's table index, so catalog and
resolver indices stay valid for the whole table); their existing callers
pass "all present".

### D6. Artist and album play orders follow by name

Before rebuilding the catalog, `App` records the playing source's names
(`Artist(a)` → artist key; `Album(b)` → artist key + title key). After, it
looks them up with `find_artist` / `find_album`: found → the source gets the
new catalog index and `queue.replace_items` gets its new tracks; not found →
the source becomes `Detached` with its items minus removed tracks (spec:
"continues with its remaining tracks, no longer tied to it"). A new
`Playback::follow_rescan(source, items)` does both through the
`replace_play_order` helper (D5), without touching the current track.

### D7. Browse stacks follow by name

Each level of `artists_nav` / `albums_nav` is rebuilt from the bottom up:
the bottom list always exists; an `ArtistAlbums(a)` / `ArtistTracks(a)` /
`Album(b)` level is mapped through the old catalog's names to the new
catalog; the first level that no longer exists is popped together with
every level above it (spec: back to the nearest level that still exists).
Surviving levels keep their filter (re-applied) and selection (D9). The
group key comparison is the catalog's existing lower-case key.

### D8. Playlists are reloaded, not rewritten

After the resolver is dropped, `refresh_playlists` (keeping the sidebar's
selection) re-lists them with the new resolver. The playing playlist, open
or not, is loaded once for its items and passed to
`Playback::follow_rescan(Source::Playlist(name), items)` (D5 helper) *first*,
so a removed current track keeps the right successor (the existing
`follow_reload` / `Edit::Replace` path would not). Then the open playlist is
reloaded through the existing `reload(name)`; its `follow_reload` finds the
play order already equal to the items and does nothing. This re-reads the small
`.m3u8` files from disk, which is what sidebar focus does already, and never
writes. A playlist that vanished meanwhile goes through `playlist_gone` as
today.

### D9. Selections keep the same track

Because indices are stable (D1), keeping a selection is a lookup by value:

- library: the selected track's index before → its new position in `order`;
  if removed, the same row clamped (`select_row`);
- open playlist: the selected entry's track index, nearest occurrence to its
  old position (positions shift when earlier entries start matching);
- browse track levels: by track index; group levels by the group's key;
- the queue view: `sync_queue_view` refilters when `up_next_generation`
  changed but keeps the *row*, so `App` first records the selected `up_next`
  position `p` and afterwards selects `p - (pruned entries before p)`
  (the same entry if it was kept, else the entry that took its place),
  clamped;
- the sidebar: by name, as `refresh_playlists` does.

Filters are re-applied with the new search keys before selecting.

### D10. Key, status and modes

- `u` in Normal mode, any focus and view (rejected: `F5` — kasset uses no
  function keys and some terminals or multiplexers take them; `Ctrl-R` —
  Ctrl letters are reserved by design; `r`/`R` are taken). Listed in the
  `keys::GROUPS` general group next to `v` / `l`, **not** in the key bar
  (no room, and it is an occasional action). README keys table updated.
- While `rescanner.running()`, the status line shows "Scanning the
  library…" whenever no transient notice is shown, the way `stop_reason`
  is shown (`ui.rs` asks `App::scanning()`). Priority on the status line:
  a transient notice, then "Scanning the library…", then `stop_reason`.
  Scanning wins over the stop reason because it is short-lived and says
  something is still happening; the stop reason comes back as soon as the
  scan ends (it stays until playback starts again). The 250 ms frame tick bounds
  how late a finished rescan is applied; no terminal loop change.
- A rescan may be applied in any mode. Prompts that hold library indices
  stay valid (D1); `add_to` skips tracks for which `Playback::is_present` is false, so a
  picker opened before the rescan never writes an entry for a removed file. `Mode::Help`
  and confirmations are unaffected.

## Risks / Trade-offs

- [A file rewritten with the same size within the filesystem's timestamp
  granularity is seen as unchanged] → the unix `(dev, ino)` in the stamp
  catches replace-by-rename (what tag editors usually do); the user can
  touch the file. Accepted.
- [An unmounted drive makes the folder look empty] → the empty-result
  guard keeps the library (D5). A *partly* readable folder (one
  subfolder unmounted) does remove those tracks; a later rescan brings them
  back under the same indices, but they have already left the queue.
- [A truncated rescan (too many entries) removes tracks past the limit] →
  same limit as startup, reported the same way; the startup library would
  have been truncated identically.
- [Memory grows with paths removed during a long run] → bounded by distinct
  paths seen; a removed `Track` is a few hundred bytes. Accepted.
- [A panic on the worker thread printing over the UI] → whole body inside
  `panic_guard::quietly` (D4), tested with a panicking fake work function.
- [The library list's rows changing meaning (index → order position)
  breaks code that indexes tracks with a row value] → done in one task
  with the UI, guarded by tests that rescan into a non-identity order and
  check every view's rows and markers.

## Migration Plan

None: no state, playlist or CLI changes. Rollback is reverting the change.
