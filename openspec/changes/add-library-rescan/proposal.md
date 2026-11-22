# Proposal

## Why

kasset scans the music folder once, at startup. Music copied in, deleted or
retagged while it runs stays invisible (or stale) until the user quits and
starts it again, which also interrupts what is playing. A rescan on demand
removes that restart. It is not trivial today because every list, the play
order, the up-next queue and the open playlist hold **library indices** into
one sorted track list, and a fresh scan would shift them all.

## What Changes

- A new key, `u` ("update library"), rescans the library folder in the
  **background** while the UI keeps running and playback continues. The
  status line says "Scanning the library…" until it ends. When the scan
  finishes, a notice sums it up ("Library updated: 12 added, 3 removed,
  5 changed", or "Library unchanged (340 tracks)"), with the same
  "skipped" / "too many entries" remarks as the startup scan.
- The rescan is **incremental**: a file whose path, size and modification
  time are unchanged keeps the metadata read before; only new and changed
  files have their tags read again.
- After a rescan, everything shown follows the new library:
  - the library list, its counts and its search show new and retagged
    tracks in library order and no longer list removed ones;
  - "Artists" and "Albums" are rebuilt; an open artist or album level stays
    open if that artist or album still exists (found by name), else the
    view goes back to the nearest level that does;
  - playlists are matched again, so entries for files that appeared now
    match and entries for removed files become unmatched (playlist files
    are never rewritten by a rescan);
  - every list keeps its selection on the same track (or entry) when it is
    still listed.
- Playback follows the rescan without interruption:
  - the track playing keeps playing, even if its file was removed;
  - the play order (library, playlist, artist or album) takes in the new
    tracks and drops the removed ones, keeping its current position; when the
    playing album or artist no longer exists, the play order continues
    detached;
  - removed tracks leave the up-next queue.
- Pressing `u` while a rescan is running does not start another one (a
  notice says one is running). A rescan that cannot read the folder, or
  that finds **no** music at all where there was some (e.g. an unmounted
  drive), changes nothing and shows an error.
- No folder watching and no automatic rescans: the user decides when.

## Capabilities

### New Capabilities
- `library-rescan`: rescanning the library folder while kasset runs: the
  trigger key, the background and incremental scan, its notices and safety
  rules, and how lists, browse views, playlists, the play order and the
  up-next queue follow the new library.

### Modified Capabilities
<!-- None: the startup scan, ordering, metadata and every existing view keep
     their requirements; the new capability states how they follow a rescan. -->

## Impact

- `src/library.rs`: discovery also returns each file's stamp (size,
  modification time, file id); sorting can order a set of indices; a new
  `library/rescan.rs` computes what changed against the known tracks and
  reads tags only for new or changed files, on a worker thread.
- `src/playback.rs`: the track table becomes append-only (indices never
  change during a run) with a separate library order of present tracks;
  `Playback` applies a rescan (append, replace in place, mark removed),
  updates the play order through `Queue::replace_items` and prunes
  `up_next`. `queue.rs` is unchanged.
- `src/app.rs`: a `Rescanner` (trait, like `PlaylistStore` / `StateStore`)
  owned by `App`, the `u` key, polling in `tick`, and rebuilding search
  keys, the catalog, the resolver, browse stacks, playlists and selections.
  The library list's rows become positions in the library order.
- `src/ui.rs` (library rows through the order, "Library" count),
  `src/keys.rs` (new entry), `src/main.rs` (wires the real rescanner and
  initial stamps), `README.md` keys table, `CLAUDE.md` architecture notes.
- No new dependencies (a `std::thread` and an `mpsc` channel); MSRV
  unchanged. The saved state format does not change.
