# Proposal

## Why

The library is one flat list sorted by artist, so finding and playing one
album or everything by one artist means scrolling or searching and then
playing the whole library from there. kasset already knows each track's artist
and album, from tags or inferred from paths, so it can offer the usual
"Artists → Albums → Tracks" browsing that every music player has.

## What Changes

- The sidebar gets two fixed entries under "Library": **Artists** and
  **Albums**, above the separator and the playlists.
- `Enter` on "Artists" lists every artist in the main pane (with album and
  track counts). `Enter` on an artist lists its albums, headed by an
  "All tracks" row. `Enter` on an album lists its tracks in album order.
  "Albums" lists every album (title, artist, track count) and goes straight to
  its tracks. `Backspace` (or `Esc` once any filter is cleared) goes back up one level,
  and from the top level `Esc` moves focus to the sidebar. Each level keeps
  its own filter and selection, and going back reselects the entry you came
  from.
- Playing a track inside an album, or an artist's "All tracks", makes **that
  album or artist the play order**: next, previous, auto-advance, shuffle and
  repeat stay within it, like a playlist. The playing marker shows on the
  album/artist rows and on the sidebar entry ("Artists" or "Albums").
- `/` search works at every level (artist names; album title or artist;
  tracks as in the library). `a` adds the selected track, or the whole
  selected album or artist, to a playlist; `A` adds every listed track.
- Albums are grouped by **album artist**: the album-artist tag when present,
  else the track artist. So a compilation stays one album (for example under
  "Various Artists"), and each track still shows its own artist. Tracks are
  ordered within an album by **disc number** then track number. The disc
  number is read from tags too and is also used in the library order.
- Album and artist play sources are saved with the session and restored on
  start, falling back to the library like a deleted playlist. Older kasset
  versions read such a state file as a library source. The browse location
  shown in the main pane is not restored. On start the main pane shows the
  library (or the saved playlist), as today.

## Capabilities

### New Capabilities
- `library-browsing`: browsing the library by artist and album (grouping
  rules, drill-down navigation, per-level filter and selection, playing an
  album or artist as the play order, playing markers, keys).

### Modified Capabilities
- `music-library`: track metadata gains album artist and disc number, and the
  library order sorts by disc number before track number.
- `player-tui`: the screen layout's sidebar lists "Library", "Artists",
  "Albums", then the playlists.
- `playlists`: the sidebar listing, its filtering (the fixed entries always
  stay) and the sidebar playing marker cover the new fixed entries and the
  album and artist play orders.
- `session-state`: the saved play source can be an artist or an album, and
  is restored or replaced by the library on start.

## Impact

- New pure module `src/browse.rs` (catalog of artists and albums built once
  from the scanned tracks).
- `src/library.rs`: `Track` gains `album_artist` and `disc`. Read from tags
  (`ItemKey::AlbumArtist`, disc accessor), with `sort_tracks` updated. Every
  `Track { … }` literal (tests, `test_support::track`) gets the new fields.
- `src/playback.rs`: `Source` gains `Artist(usize)` and `Album(usize)`
  variants.
- `src/app.rs`: the sidebar's fixed rows (values 0–2, playlists from 3), a
  browse location stack in the main pane, key handling, `session()` and
  `restore`.
- `src/state.rs`: `SavedSource` gains `Artist(String)` and
  `Album { artist, album }`, encoded as new `source` values. The format
  version stays 1.
- `src/ui.rs`, `src/keys.rs` (new bar contexts and entries), `README.md`
  keys tables, `CLAUDE.md` architecture notes.
- No new dependencies.
