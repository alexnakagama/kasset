# Proposal

## Why

`kasset` can only play the whole library (optionally filtered by search) in one fixed or shuffled order, and nothing survives between runs. Users want to group songs into named playlists they can come back to, edit, and play on their own. The README lists "No playlists (m3u) … saved state between runs" as a known limitation; this change removes the playlist part of it.

## What Changes

- Named playlists stored as `.m3u8` files in a `Playlists/` folder inside the library root (e.g. `~/Music/Playlists/Running.m3u8`), with entries written as paths relative to the playlist file, so playlists move with the music folder and open in other players.
- Add songs from the library: `a` adds the selected track, `A` adds every listed track (so search + `A` adds a whole album or artist). A picker popup chooses the target playlist or creates a new one.
- A Playlists view (`Tab` switches between Library and Playlists) listing the saved playlists; `Enter` opens one, showing its tracks.
- Playing from an open playlist limits play order (next/previous, shuffle, repeat, automatic advance) to that playlist.
- Edit a playlist: `d` removes the selected entry, `J` / `K` move it down / up, `R` renames a playlist, `D` deletes one after a `y/n` confirmation.
- Every edit is saved immediately and atomically. Entries kasset cannot match to the scanned library (hand edits, files moved away) are preserved on save and reported, never silently dropped. File formatting (BOM, CRLF, final newline), permissions and symlinks are kept. Playlists that are not valid UTF-8 or exceed the size limits open and play but are read-only.
- Edits to the playlist that is playing (including tracks added from the library) apply to the play order at once; removing the playing track lets it finish and continues with the next entry.
- Failures to write (read-only folder, permissions) or external modification of the file are reported in the status bar; the edit is not applied.
- README and `kasset --help` keybinding summary updated; "No playlists" removed from known limitations.

## Capabilities

### New Capabilities
- `playlists`: storing, listing, creating, renaming and deleting named playlists in the music folder, adding/removing/reordering their entries, the keys and views for doing so, and playing a playlist as its own play order.

### Modified Capabilities
<!-- none. The existing capabilities (music-library, audio-playback, player-tui) are still only in the unarchived change `add-music-player-tui`, so `openspec/specs/` is empty and there is nothing to MODIFY yet. Their behaviour is unchanged when no playlist is involved; how play order behaves inside a playlist is specified in `playlists`. -->

## Impact

- New module `src/playlist.rs` (m3u8 parse/write, path resolution, file operations).
- `src/queue.rs` / `src/playback.rs`: the play order works over an arbitrary list of library tracks (possibly with duplicates) instead of always `0..len`, and can be edited while playing.
- `src/app.rs`: views (library, playlists list, one playlist), new modes (playlist picker, name entry, delete confirmation) and keys.
- `src/ui.rs`: picker popup, playlists view, per-view key help. `src/cli.rs`: help text. `src/main.rs`: passes the playlists folder to `App`.
- No new dependencies (`tempfile` stays dev-only; atomic save uses a temp file in the same folder plus `rename`).
- kasset now **writes** into the music folder (only under `Playlists/`, and only when the user edits a playlist).
- Out of scope: reading or editing `.m3u` / `.pls` files or playlists elsewhere in the library, smart/auto playlists, undo, playlist import/export commands, remembering the last opened playlist between runs.
