# Proposal

## Why

The play order in kasset is always a whole source: the library, a playlist,
an album or an artist. There is no way to say "play this song next, then
carry on with what I was listening to" without building a playlist for it.
An up-next queue is the most common feature that source-based players are
missing. kasset's play order is already a list of library indices with a
source identity, so a queue can sit on top of it without changing how
sources play.

## What Changes

- A new **up-next queue**: an ordered list of library tracks (duplicates
  allowed, at most 1000) that plays **before** the source continues. When a
  track ends, or on `n`, the first queued track plays and leaves the queue.
  Once the queue is empty, the source continues with the track after the one
  it was on. The source itself (library, playlist, album or artist) never
  changes because of the queue.
- `e` adds to the end of the queue and `E` adds to its front ("play next").
  Both act on the selected track in the library, a playlist or a browsed
  track list, and on all tracks of a selected artist, album or "All tracks"
  row (in the order `a` would add them). A notice says what was queued.
- A fourth fixed sidebar entry, **Queue**, under "Albums" and above the
  playlists, shows the number of tracks waiting. `Enter` on it lists the
  queue in the main pane with the library's columns. There, `Enter` plays
  that entry now (taking it out of the queue), `d` removes it, `J`/`K` move
  it, `a`/`A` add to a playlist as in the library, `/` filters, and `Esc`
  goes back to the sidebar. `D` on the "Queue" sidebar entry clears the queue
  after a y/n confirmation.
- Queued tracks play in the order queued: shuffle only applies to the
  source. Repeat one replays the current track (queued or not) when it
  ends, so the queue waits for `n`. Repeat
  all and repeat off never put a played track back in the queue.
- `p` while a queued track is playing restarts it after 3 seconds, as
  usual. Before that it goes back to the source's track, the one that was
  playing before the queue took over. There is no history of played queued
  tracks.
- Playing a track from the library, a playlist or a browse view replaces the
  source as today and **keeps** the queue.
- While a queued track plays, the sidebar marks "Queue" as playing, and no
  source row (playlist entry, album or artist row) has the playing marker.
  The library still marks the playing track.
- The queue is saved per library folder with the session and restored on
  start. Tracks the scan no longer finds are dropped, even if the folder is
  only temporarily missing (e.g. an unmounted drive); unlike the resume
  point, the queue is not kept for a later start. A queued track that
  was playing at quit is resumed paused as before, and the source continues
  after it. Older kasset versions ignore the new state lines. The format
  version stays 1.

## Capabilities

### New Capabilities
- `play-queue`: the up-next queue. Covers adding (`e`/`E`), playing order
  and how it interacts with next, previous, automatic advance, shuffle,
  repeat and failures, the queue view and its edits, clearing, markers,
  limits and keys.

### Modified Capabilities
- `player-tui`: the screen layout's sidebar lists "Queue" after "Albums".
- `playlists`: the sidebar listing includes the fixed "Queue" entry, and the
  sidebar playing marker covers a queued track.
- `session-state`: the queue is remembered per library folder, including
  whether the resumed track was a queued one.
- `audio-playback`: next and automatic advance take the first queued track
  before the next track of the play order (except at a track's end with
  repeat one).
- `library-browsing`: no browse row and neither "Artists" nor "Albums" in
  the sidebar has the playing marker while a queued track is current.

## Impact

- `src/playback.rs`: `Playback` gains the up-next list and the
  "playing a queued track" state (`queued_current`). `next`, `prev`, `tick` (track end,
  failures), `start_current`, `current`, `current_item`, `play`,
  `play_source` and `resume` take them into account. New queue operations
  (enqueue at end and front, remove, move, play now, clear). `queue.rs` is
  unchanged.
- `src/app.rs`: `SidebarEntry::Queue` / `SidebarPick::Queue` with
  `FIXED_ROWS` = 4 (playlists move from value 3 to 4), the queue main view
  with its own `ListState`, `e`/`E`/`D` keys and the clear prompt (`Mode::ConfirmClearQueue`), markers, notices, `session()`
  and `restore`.
- `src/state.rs`: `FolderEntry` gains the queue (`queue <relpath>` lines)
  and `Resume` gains a queued-track flag. `render` drops queue lines from the
  end before dropping anything else when a folder does not fit.
- `src/ui.rs` (sidebar row, queue view, empty text, clear popup),
  `src/keys.rs` (new group, `Queue` and `ConfirmClearQueue` bar contexts,
  widened `d` / `J / K` / `Esc` / `D` texts), `README.md` keys tables, `CLAUDE.md` architecture
  notes.
- No new dependencies.
