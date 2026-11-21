# Design

## Context

See proposal.md (Why) and the `library-browsing` spec for behaviour. Current
state that shapes the approach:

- Tracks are scanned once at start and never change during a run
  (`Playback::tracks()`). Library indices are stable for the whole run.
- `App` drives the main pane from `(focus, open)`. `open: Option<OpenPlaylist>`
  is the playlist shown, else the library. `View` (Library / Playlists /
  Playlist) selects the `ListState` that keys act on. Each `ListState` holds
  `rows` (values), `selected` and the query.
- The sidebar's `ListState` uses value 0 for "Library" and `i + 1` for
  `playlists[i]`. This is used by `sidebar_value_of`, `selected_playlist`,
  `refilter_sidebar`, `apply_filter` (the first match skips "Library"),
  `SidebarPick`, `ui::render_sidebar` (separator after "Library") and many
  tests.
- `Playback` keeps a `Source` (Library / Playlist(name) / Detached) only as
  the play order's identity. The items are passed in by `App`
  (`play_source`, `resume`).
- The state file's `source` value is `library` or `playlist <name>`. An
  unknown value fails `parse_source` and is ignored, so the default
  (library) applies.
- `Track` has `title, artist, album, number, duration` and no album artist
  or disc.

## Goals / Non-Goals

**Goals:**
- Grouping is pure, deterministic and built once, so the UI only indexes
  into it.
- Album and artist play orders reuse the playlist machinery (`Queue` items
  plus a `Source` identity) without changing `queue.rs`.
- Older kasset versions keep working with a state file written by this one.

**Non-Goals:**
- Restoring the browse location (which artist or album was shown) on start.
- Genre, year or composer browsing, cover art, or editing tags.
- Inferring the album artist or the disc number from paths (disc folders
  stay skipped as today, and the disc number comes only from tags).
- Listing a guest artist under their own name when they only appear on
  someone else's album.

## Decisions

### D1. A pure `browse` module builds a `Catalog` once

`src/browse.rs` (no I/O): `Catalog::build(tracks: &[Track]) -> Catalog` with

- `albums: Vec<Album { title, artist: usize, tracks: Vec<usize> }>`, sorted
  by (artist key, title key). `tracks` are library indices in album order:
  disc (missing = 1), then number (missing after numbered), then title
  lower-cased, then path.
- `artists: Vec<Artist { name, albums: Vec<usize>, track_count }>`, sorted by
  key. `albums` are sorted by title key.
- Keys are `to_lowercase()` of the sanitised text, like the existing search
  keys. Display text is the first spelling in library order.
- `artist_tracks(a) -> Vec<usize>`: the artist's albums' tracks concatenated.
  It is computed when opened or played, not stored.
- Lower-cased search keys for artists (name) and albums (title + artist).

`App::new` builds it from `playback.tracks()` and keeps it. The play
order identity is the catalog index, which is valid for the whole run.

*Alternatives:* computing groups on every key press (simpler, but O(n) per
frame on big libraries), or grouping by folder (wrong for tagged libraries).

### D2. Album artist and disc in `Track`

`Track` gains `album_artist: Option<String>` (tag `ItemKey::AlbumArtist`,
first tag that has it, primary first, through `sanitize`, read with
`Tag::get_string`, since it has no `Accessor` method and so needs a
small helper next to `tag_text`) and
`disc: Option<u32>` (`Accessor::disk`, > 0). The grouping artist is
`album_artist.as_deref().unwrap_or(&artist)`. Neither is inferred.
Inferring them would mean that a folder named `Downloads` overrides tagged
artists for grouping. `sort_tracks` inserts disc between album and number
(the `music-library` delta). `test_support::track` and every `Track { … }`
literal get `album_artist: None, disc: None`.

*Alternative:* using the folder artist as the inferred album artist. This was
rejected for the reason above. It also doesn't help tagged libraries, where
the album-artist tag is the usual way to mark compilations.

### D3. Sidebar fixed rows: values 0, 1, 2, then playlists from 3

There is one constant, `FIXED_ROWS = 3` (Library, Artists, Albums), and
playlist `i` is value `i + FIXED_ROWS`. All existing arithmetic goes through
`selected_playlist`, `sidebar_value_of` and one new `playlist_of_value`
helper, so no raw `+ 1` remains. `refilter_sidebar` keeps values
`< FIXED_ROWS`. `apply_filter` selects the first row at or after
`FIXED_ROWS` when there is one. `SidebarPick` gains `Artists` and `Albums`.
`ui::render_sidebar` draws the separator after the third fixed row. The
sidebar offset keeps counting playlist rows only, and the fixed rows and
separator stay on top. In a short pane the fixed rows are drawn first, top
down, and cut from the bottom. The separator is drawn only when at least one
row remains below it for playlists, and the playlist page size is what is
left. The hard-coded sites to change in `render_sidebar` are the
`draw_row(…, 0, LIBRARY_ROW, …)` call, `value.checked_sub(1)`, and the
`rest.height >= 2` separator check.

### D4. Main pane state: `open` stays, `browse` is added, at most one is set

Instead of rewriting every `self.open` use into a new enum, `App` gets
`browse: Option<BrowseRoot>` (`Artists` or `Albums`) plus two persistent
stacks: `artists_nav: Vec<Level>` (bottom is `Level::Artists`) and
`albums_nav: Vec<Level>` (bottom is `Level::Albums`). Each `Level` owns its
`ListState`:

- `Artists`: rows are artist indices.
- `ArtistAlbums(a)`: value 0 is "All tracks", `i + 1` is
  `artists[a].albums[i]`.
- `Albums`: rows are album indices.
- `AlbumTracks(b)` and `ArtistTracks(a, items)`: rows are positions in the
  items. The items are library indices, so `Enter` passes the position
  straight to `play_source`, as for playlists.

Invariant: `open.is_some()` and `browse.is_some()` are never both true.
`show_library`, `show_playlist` and `show_browse` are the only places that
set them, so `Enter` on "Library" or on a playlist clears `browse` (today
`show_selected` only clears `open`). `View` gains `Groups` (artist, album or "All tracks" rows) and
`Tracks` (browsed track list). `list()`, `list_mut()`, `refilter_current`,
`clear_filter` and `selected_track` gain the browse arm (the top of the
active stack). `MainView` (which is `Copy + Eq`) gains `Browse(BrowseKind)`, a small
copyable descriptor (`Artists`, `ArtistAlbums(a)`, `Albums`, `Album(b)`,
`ArtistTracks(a)`). The UI reads the level's `ListView` and items through
accessors, never through `&Level`. The stacks survive leaving and re-entering, which
gives "Back to the same place". `Enter` on "Artists" while it is already
shown only focuses the main pane.

*Alternative:* replacing `open` with `enum Main { Library, Playlist(..),
Browse(..) }`. This is cleaner, but it touches roughly 100 sites in a
6,900-line file and the playlist tests. The invariant plus three setters
gets the same safety.

### D5. Going back

`Esc`: if a filter is active, clear it. Else, if browsing with a stack
deeper than 1, pop it (the "Going back" requirement). Else, if browsing or a
playlist is shown, focus the sidebar with that entry selected. `Backspace`
in Normal mode with the main pane focused pops when the depth is > 1,
whether or not a filter is active, else does nothing. Backspace in Search
and NameInput modes keeps editing text, since those modes handle keys
before `normal_key`. Popping drops the deeper level with its filter.
It reselects the opened entry because the parent level's `ListState` was
never touched.

`focus_sidebar` and `refresh_playlists` select a playlist by name only
(`select: Option<&str>`). They change to take `Option<SidebarPick>`, so
`Esc` can select "Artists" or "Albums" through `restore_sidebar`.

Filtering an artist's albums: `apply_filter` selects the first row after
"All tracks" when there is one, like the sidebar skips its fixed rows.

### D6. Play sources `Artist(usize)` and `Album(usize)`

`playback::Source` gains `Artist(usize)` and `Album(usize)` (catalog
indices). `Playback` only stores and compares them. `edit_playlist`,
`playlist_renamed` and `playlist_deleted` match on `Source::Playlist` only,
so they ignore the new variants. `sidebar_playing` maps them to values 1 and
2. Playing markers in browse lists compare the row's index with the source.
An artist row is marked for `Artist(a)` and for `Album(b)` when
`albums[b].artist == a`. In a track list, the marker is shown only when the
list's own album or artist is the source and the position is
`current_item()`. The sidebar marks "Albums" for any album source, even one
reached through Artists, because the source does not record the route.

### D7. `a` on group rows passes album or artist order to the picker

`Picker::tracks` is documented as "in library order". It becomes "in the
order to add". `a` on an album row passes `album.tracks`, on an artist or
"All tracks" row it passes `artist_tracks(a)`, and `A` in a track list
passes the listed items. `add_to` goes through `Playlist::add`, which
already keeps the given order while skipping duplicates.

### D8. State file: two new `source` values, version unchanged

- `source artist <enc-name>`
- `source album <enc-artist> <enc-title>` (one space between two
  percent-encoded fields; encoded values never contain a space)

`SavedSource` gains `Artist(String)` and `Album { artist, title }`. The names
must be non-empty UTF-8 of at most `MAX_GROUP_NAME_BYTES` (1024 = 4 ×
`MAX_TEXT_CHARS`, so any sanitised tag text fits). They are not path
components, so `/` is allowed. Two 1024-byte names, encoded at up to 3×
each, still fit well inside the 64 KiB `render` budget, and `render`
already drops a resume point that does not fit. They are only compared with catalog keys
(lower-cased) and never used as paths. `render` writes `library` for an
invalid name, as it does for playlists. Older kasset fails `parse_source` on
these values and ignores the line, so the source defaults to library and
`FORMAT_VERSION` stays 1. `App::session` maps `Source::Artist(a)` and
`Source::Album(b)` to names, with `item = current_item()`. `resume_source`
looks the names up by key, uses the saved item if it is still the track,
else the first match, else the library.

### D9. Keys and bar contexts

`BarContext` gains `BrowseTop` (artist or album list: Enter open, a add,
Esc sidebar), `BrowseGroups` (an artist's albums: Enter open, a add, Esc
back) and `BrowseTracks` (Enter play, a/A add, Esc back). `bar_context`
picks one from `MainView` and the stack depth. `GROUPS` gains a "Browse"
group: `Esc / Backspace` back one level, plus `a` on a group row. These
existing entries also change:
- The `Enter play` and `a/A add` bars gain `BrowseTracks`.
- The sidebar `Enter open` bar gains `BrowseTop` and `BrowseGroups`.
- The `Esc` entries' texts ("in a playlist: clear the filter, then focus
  the sidebar", "clear an active filter") cover browsing too, and their
  bars gain the new contexts with the right label.

The bar order invariant (each view's own keys before the transport keys)
and the `bar(Library)[..2]` order test are kept. The key table, `--help`
and the README keys tables change in the same group as the keys
themselves.

## Risks / Trade-offs

- [Sidebar value shift breaks many existing tests that select playlists by
  raw value] → Tests go through the helpers (`select_playlist`), so update
  the helpers and grep for literal `+ 1` and `value 0` uses first. Old
  tests must otherwise pass unchanged.
- [Large libraries: building the catalog adds start-up time] → One sort
  of n keys, the same order as `sort_tracks`. Measure it alongside the
  existing `startup_listing_cost` test style.
- [Album artist missing from tags in poorly tagged compilations splits them
  by track artist] → Documented in the README. Users can tag album
  artist. Inferring it was rejected (D2).
- [A real album titled "Unknown" merges with untagged tracks of the same
  artist] → Accepted. It is rare and harmless.
- [Catalog indices in `Source` are only valid within one run] → They are
  never persisted. The state stores names (D8).

## Migration Plan

No data migration is needed. State files from 0.6.0 parse unchanged. Files
written by this version stay readable by 0.6.0, which falls back to the
library source. Release as a minor version.
