# Proposal

## Why

kasset shows what is playing and how the music sounds (the spectrum), but not the words. Many music folders already carry lyrics: `.lrc` files downloaded next to the songs, or lyrics embedded in FLAC/OGG/M4A tags by taggers such as Picard or beets. Showing them, scrolling in time with the song, makes kasset a place to listen *along*, without leaving the terminal.

## What Changes

- New **lyrics pane**: a third column on the right of the main pane, titled "Lyrics", showing the lyrics of the current track. It never takes keyboard focus; `Tab` keeps moving between the sidebar and the main pane.
- **Synced lyrics**: for timed (LRC) lyrics, the current line is highlighted and kept in the middle of the pane, earlier lines are dimmed, and the pane follows seeking. The highlight changes when the line's time is reached, not on the next regular refresh.
- **Unsynced lyrics** (no timestamps) are shown as plain text that scrolls through the pane in proportion to the track's progress.
- **Sources**, read when a track becomes current, never during the library scan:
  1. `<same name>.lrc` next to the audio file (extension in any case);
  2. otherwise lyrics in the file's tags (Vorbis comments, MP4 `©lyr`, ID3v2 `USLT`), parsed the same way.
- **LRC parsing**: several timestamps per line, out-of-order lines, the `[offset:]` header, word-level timing (stripped), other header tags (ignored).
- **`l` key** shows or hides the pane (on at start), like `v` for the spectrum. Listed in `--help`, the `?` overlay and the README; not in the key bar.
- The pane is shown only on terminals at least **100 columns** wide; narrower terminals keep today's layout, and `l` then explains why nothing appears.
- Lyrics are untrusted input: size-capped, read without blocking on FIFOs or devices, sanitised before display, never written.

Out of scope: synced lyrics in ID3v2 `SYLT` frames, downloading lyrics, editing lyrics, word-by-word (karaoke) highlighting.

## Capabilities

### New Capabilities
- `lyrics`: finding, parsing and showing a track's lyrics in the lyrics pane, in time with playback, and the `l` toggle.

### Modified Capabilities
- `player-tui`: the *Screen layout* requirement gains the lyrics pane as a third column on wide terminals.

## Impact

- New `src/lyrics.rs` (pure LRC parsing and line lookup) and lyrics loading (sidecar file + tags).
- `src/library.rs`: tag reading is reused to fetch lyrics tags for one file.
- `src/playlist/store.rs`: the bounded, FIFO-safe file reader is moved to a shared module so lyrics loading reuses it.
- `src/app.rs`: lyrics state, the `l` key, a "next line" deadline.
- `src/ui.rs`, `src/theme.rs`: three-column body, lyrics rendering and wrapping.
- `src/terminal.rs`: the frame deadline also honours the next lyric line.
- `src/keys.rs`, `README.md`, `CLAUDE.md`: the `l` key and the new module.
- No new dependencies.
