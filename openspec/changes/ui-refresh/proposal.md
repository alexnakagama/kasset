# Proposal

## Why

The kasset screen shows one full-width list at a time. `Tab` swaps the whole list between the library and the playlists, so the user cannot see the playlists while browsing the library. They also cannot tell which side they are on without reading the title. The key bar lists every key and is cut off at the terminal's width, so keys at its end never show on normal-width terminals. The now-playing box takes 6 rows of a 24-row terminal. The UI should feel like a modern TUI player: both sides visible at once, help available on demand, and more room for the list.

## What Changes

- **Two-pane layout.** A sidebar on the left lists a "Library" entry, then the saved playlists with their track counts. A main pane on the right shows the tracks of whichever entry was chosen: the library or one playlist. One pane has keyboard focus at a time, and its border is highlighted. On terminals narrower than 70 columns, only the focused pane is shown, at full width (today's single-list behaviour).
- **BREAKING (keys).** `Tab` now moves focus between the sidebar and the main pane; it no longer swaps the list. `Enter` on a sidebar entry shows it in the main pane and focuses the main pane. `Esc` in a playlist with no filter moves focus to the sidebar and leaves the playlist on screen; today it goes back to the playlists list. `R` and `D` act on the playlist selected in the sidebar.
- The playlists are listed when kasset starts, so the sidebar is filled from the first frame. Today they are listed the first time the user presses `Tab`.
- **Help overlay.** `?` opens a scrollable popup listing every key, grouped by topic; any other key closes it. The bottom bar shows only the most useful keys for the current focus and mode, and always ends with `? help`, which is never cut off.
- **Compact now-playing area.** Three rows instead of six:
  - a separator line titled "Now playing";
  - the state, title, artist and album, with the shuffle, repeat and volume indicators at the right;
  - a thin progress bar (`━━━━╸────`) between the elapsed and total times.
- **One colour scheme.** Each colour stands for one thing: playing track, selection, focused border, dimmed text, error. The colours are defined in one place and taken from the terminal's 16-colour palette, so they follow the user's terminal theme.
- The key list is defined once and used to build `kasset --help`, the help overlay and the bottom bar, so they can no longer disagree. README keys tables updated.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `player-tui`:
  - Screen layout: two panes, a compact now-playing area, and a key bar ending in `? help`.
  - New requirements for pane focus, narrow terminals and the help overlay.
  - The progress bar scenario is reworded.
- `playlists`:
  - Switch between library and playlists: now a sidebar plus focus.
  - Open a playlist: `Enter` in the sidebar.
  - Rename and delete: act on the sidebar selection.
  - The "No folder" scenario: the empty sidebar text.
  - Key help: per focus, plus the overlay.

## Impact

- `src/app.rs`:
  - The `side` / `open` state becomes a focus (sidebar or main) plus what the main pane shows (library or the open playlist).
  - The sidebar list gains a fixed "Library" row.
  - New `Mode::Help` with a scroll offset.
  - The playlists are listed in `App::new`.
  - Updated `Tab`, `Enter`, `Esc`, `R` and `D` handling.
  - The module doc changes, and many key-sequence tests change: the `Tab` and `Esc` semantics are different.
- `src/ui.rs`: new layout (sidebar, main pane, narrow fallback), focus styling, compact now-playing area with a custom progress line instead of `Gauge`, help overlay, shorter key bar. Most `TestBackend` tests get new expected output.
- New `src/keys.rs`: the single key table. New `src/theme.rs`: style roles.
- `src/cli.rs`: `--help` text built from the key table.
- `README.md`: keys tables and the description of the views.
- Specs: `player-tui`, `playlists`. CLAUDE.md architecture notes for `app.rs` and `ui.rs`.
- No new dependencies; ratatui and crossterm already provide everything.
- Out of scope:
  - mouse support;
  - an artist/album browser;
  - a visible "up next" queue;
  - a visualizer;
  - user-configurable colours or themes, and `NO_COLOR`;
  - remembering focus or the open playlist between runs;
  - showing a playlist's tracks while moving through the sidebar, before `Enter` (see design).
