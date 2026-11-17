# Tasks

Every group ends green: `cargo test` (the whole suite) and `cargo clippy --all-targets -- -D warnings` pass before the group is handed off for review.

## 1. One key table (`src/keys.rs`, `src/cli.rs`)

- [x] 1.1 Add `src/keys.rs` with a `//!` doc, `GROUPS` (D7) covering every key in the new spec, including `?`, `Tab`/Shift+Tab as "move focus", and `Enter`/`Esc`/`R`/`D` in the sidebar sense. Add `BarContext`, `keys::bar(context)` and `keys::help_line_count()`. Verify with unit tests:
  - every `BarContext` has a non-empty bar;
  - no bar label is wider than 24 columns;
  - the Sidebar bar contains `Enter`, `R` and `D` labels;
  - the Playlist bar contains `d` and `J/K` labels;
  - `help_line_count()` equals the number of lines the overlay draws for `GROUPS`.
- [x] 1.2 Build the `--help` text with `keys::help_text()` and pass it as `after_help` in `src/cli.rs`, removing `KEYBINDINGS`. Update the existing `help_includes_keybindings` test: it asserts the old `Tab` wording. Add a test that the output lists `Tab`, `a / A`, `d`, `J / K`, `R`, `D` and `?` (spec *Help lists playlist keys*). Verify with `cargo test`, clippy, and by checking `cargo run -- --help` by eye.

## 2. Focus model and sidebar state (`src/app.rs`, minimal `src/ui.rs`)

- [ ] 2.1 Replace `Side` with `Focus { Sidebar, Main }` and derive `view()` from `(focus, open)` (D1). Add `focus()`, `main_view()`, and the per-pane `sidebar_list()` / `main_list()` returning `ListView`. Make `playlist_list` the sidebar list with a "Library" row at value 0 that is never filtered out. Add `selected_playlist()` / `sidebar_value_of()` and route every indexing site listed in D2 through them. Make the minimal `ui.rs` change so the current single-list rendering skips value 0 and indexes playlists correctly. Verify with:
  - unit tests on the sidebar list: the filter keeps "Library"; a non-empty filter selects the first matching playlist; `g` and `k` reach "Library"; `G` reaches the last playlist; the count title counts playlists only;
  - the whole suite green.
- [ ] 2.2 Add `App::notify` with the precedence of D4 and route every `pending` write through it. List the playlists in `App::new`. Verify with key-free tests:
  - *Playlists listed on start*;
  - *No folder* (only "Library", nothing written, `Playlists` still absent);
  - *Nothing played yet* at the state level (no source marked while no track is loaded);
  - the scan notice and a listing error are joined (listing error forced by making `Playlists` a regular file);
  - an info notice raised after an error in the same key press does not hide the error.

  Measure `App::new` on 100 000 generated tracks with no playlists. If it adds more than 100 ms, move the listing to the first `tick` (D4). Record the measurement in the PR description.
- [ ] 2.3 Implement `focus_sidebar` / `focus_main` and the key semantics of D3:
  - `Tab` and `BackTab` toggle focus;
  - `Enter` on "Library", on the shown playlist, and on another playlist (a failed load leaves focus and `open` unchanged);
  - `Esc` per focus, with `Esc` from a playlist selecting it in the sidebar;
  - `R`/`D` on the selected playlist only, doing nothing on "Library".

  Verify with key-sequence tests on a temp-dir store for: *Tab moves focus* (also with `BackTab`), *Keys go to the focused pane*, *Focus on start*, *Open playlists view*, *Back to library*, *Return to open playlist*, *Listing refreshed*, *Open playlist removed meanwhile*, *Filter the sidebar* (then `Enter` opens the matching playlist, not the library), *Open*, *Esc from a playlist*, *Reopen the shown playlist*, *Delete the shown playlist*, and `R`/`D` on "Library" doing nothing.
- [ ] 2.4 Make rename follow the shown playlist with `replace_open` and `follow_reload` (D5). Verify with key-sequence tests:
  - *Rename the shown playlist*: a `d` after the rename saves to `Morning run.m3u8`, and `Running.m3u8` is not recreated;
  - renaming the playing playlist keeps playback and the play order;
  - a forced load failure after the rename clears `open` and shows the error.
- [ ] 2.5 Add `update_info` and call it after add, create, `d`/`J`/`K` saves and reloads (D10). Verify with key-sequence tests:
  - *Sidebar kept current*: the new playlist is listed with 1 track, and the count drops after `d`;
  - the sidebar selection stays on the same playlist after an insert before it;
  - a failed save leaves the sidebar entry unchanged.
- [ ] 2.6 Port the existing app tests that encode the old `Tab` and `Esc` semantics by updating the shared helpers (`select_playlist`, `open`) instead of each test. Keep every assertion about storage, play order and notices unchanged. Verify that:
  - `cargo test` passes;
  - `git diff --stat` shows no change in `src/queue.rs`, `src/playback.rs`, `src/playlist.rs` or `src/playlist/`;
  - `cargo test -- queue:: playback:: playlist::` passes.
- [ ] 2.7 Update the documentation:
  - the `app.rs` `//!` doc: focus model, sidebar rows, startup listing, notices;
  - the README keys tables: `Tab`/Shift+Tab, `Enter`, `Esc`, `R`, `D`, with the breaking `Tab`/`Esc` change noted;
  - in CLAUDE.md, the `app.rs` paragraph, and the "When adding keys…" rule, which now points to `src/keys.rs` + README.

  Verify by reading the README tables side by side with `keys::GROUPS`, and that clippy is clean.

## 3. Help overlay state (`src/app.rs`, `src/terminal.rs`)

- [ ] 3.1 Add `Mode::Help { scroll }`, `?` in Normal mode, `help_key` (scroll keys with PgUp/PgDn by `help_rows`; Ctrl+C quits; any other key closes and is consumed) and `set_help_rows`, which also re-clamps `scroll` (D6). Feed `help_rows` from `ui::View` in `terminal::run`, next to `set_page_size`. Verify with key-sequence tests:
  - *Open and close help*: `q` closes without quitting;
  - *Not while typing*: `?` in Search mode is added to the query;
  - scrolling stops at the last page, and `k` moves back at once after over-pressing `j`;
  - a larger `set_help_rows` re-clamps `scroll`;
  - `Enter` while the overlay is open neither plays nor opens anything.
- [ ] 3.2 Add `?` to the README keys table. The key table already has it from task 1.1. Verify by reading the README table.

## 4. Theme and two-pane layout (`src/theme.rs`, `src/ui.rs`)

- [ ] 4.1 Add `src/theme.rs` with a `//!` doc and the style roles from D8. Move every colour and emphasis literal and `Stylize` shorthand out of `ui.rs`. Verify that `grep -nE "Color::|Modifier::|\.(bold|italic|dark_gray|red|green|cyan|yellow)\(\)" src/ui.rs` finds nothing outside `#[cfg(test)]`, and that the whole suite passes.
- [ ] 4.2 Render the body as sidebar + main pane at a width of at least 70 columns, and only the focused pane below that, using `sidebar_list()` / `main_list()`. Draw focused and unfocused borders and selections per D8. Keep separate `sidebar_offset` / `main_offset` in `ui::View`, with `page_size` taken from the focused pane. Centre popups over the body. Verify with `TestBackend` tests:
  - *Both panes visible* at 100×30;
  - *Narrow terminal* at 60×24, before and after `Tab`;
  - *Widening the terminal*: the same `View` drawn at 60, then 100 columns;
  - the focused border has the focused style and the other border does not;
  - the selected row is reversed only in the focused pane, and underlined in the other;
  - both panes keep their own scroll position when focus moves;
  - the picker popup is drawn inside the body.
- [ ] 4.3 Render the sidebar rows (playing marker only while a track is loaded, name elided at the end, right-aligned count, `DIM` read-only names, the "Library" row and separator, the empty and no-match hints below "Library") and the main pane titles (library root elided at the start, playlist name with read-only suffix). Verify with `TestBackend` tests:
  - *Playlist playing*, *Different source* and *Nothing played yet* (sidebar markers);
  - *No folder* (the hint shows below "Library");
  - a 300-character playlist name is elided without overflowing the sidebar;
  - a playlist name with control characters is drawn sanitised.
- [ ] 4.4 Update the `ui.rs` `//!` doc, the CLAUDE.md `ui.rs` paragraph and module list (add `keys.rs` and `theme.rs`), and the README description of the screen (two panes, the narrow fallback, ambiguous-width glyphs under Terminal notes). Verify that clippy is clean and the whole suite passes.

## 5. Now-playing area, key bar and overlay rendering (`src/ui.rs`)

- [ ] 5.1 Replace the 6-row now-playing block with the 3-row area of D8: a top-rule block, a state/title/artist/album line with right-aligned `shuffle · repeat · vol` (the right part has priority on narrow widths), and a custom progress line instead of `Gauge`. Verify with `TestBackend` tests:
  - *Track playing*: "1:05" and "3:30" around a bar with about 31% `━` cells;
  - *Indicators*;
  - no track: "select a track and press Enter" and `-:--`;
  - unknown duration: an empty bar;
  - an 80-column frame with a 200-character title keeps the indicators intact.
- [ ] 5.2 Draw the key bar from `keys::bar(context)`, cutting at whole labels and right-aligning `? help` only in the Normal-mode contexts (D7). Verify with `TestBackend` tests: *Help hint always visible* at 40 columns, *Playlist view help*, *Sidebar help*, *No help hint in prompts*, and no `? help` while searching.
- [ ] 5.3 Render the help overlay from `keys::GROUPS` with scrolling, truncating lines and never wrapping them, and record `help_rows` in `ui::View`. Verify with `TestBackend` tests: *Help does not fit* (at 80×15 the first rows show, and after `j` the view moves one line), the full overlay at 100×50 shows every group title, the overlay's scrollable height equals `keys::help_line_count()`, and a 40-column overlay truncates lines with `…`.
- [ ] 5.4 Update the README's now-playing and progress bar wording and the remaining doc comments that mention `NOW_PLAYING_HEIGHT` or the `Gauge`. Verify that `grep -rn "Gauge\|NOW_PLAYING_HEIGHT" src README.md` returns only code that is actually used, and that the whole suite and clippy pass.

## 6. Integration

- [ ] 6.1 Run `cargo clippy --all-targets -- -D warnings`, `cargo test` and `cargo +1.89 build`. Verify that all three succeed. Also verify that no dependency was added (`git diff master -- Cargo.toml Cargo.lock` is empty).
- [ ] 6.2 Run kasset in a real terminal or in a pty with the pyte harness, on a library with playlists. Check, with screenshots or a screen dump:
  - both panes at 120 columns and the fallback at 60;
  - `Tab`/`Enter`/`Esc` flows, rename and delete from the sidebar;
  - the `?` overlay;
  - the compact now-playing area while playing;
  - resizing while playing.

  Verify the terminal is restored after `q`.
- [ ] 6.3 Cross-check every scenario in `specs/player-tui/spec.md` and `specs/playlists/spec.md` of this change against a named test, or against step 6.2 for the visual ones. Verify by listing the mapping in the PR description, with no scenario left unmapped.

## Workflow follow-up

- Groups 1–3 change `--help` and the keys together, so they land in one PR or branch. Group 1 alone makes `--help` describe keys that groups 2–3 implement.
- After each task group, hand the group off for review, as with the previous changes, and fix the findings before starting the next group.
- Sync the delta specs and archive the change once all groups are reviewed, then bump the minor version and note the `Tab`/`Esc` key changes in the release notes.
