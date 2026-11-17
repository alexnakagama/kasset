# Design

## Context

See `proposal.md` for the motivation, and `specs/player-tui/spec.md` and `specs/playlists/spec.md` for the required behaviour. This is the current state that shapes the approach.

- **`App` (`src/app.rs`)** keeps three things:
  - `side: Side` (Library / Playlists);
  - `open: Option<OpenPlaylist>`;
  - one `ListState` (rows, selection, filter) for each list: `library`, `playlist_list`, and `open.list`.

  `App::view()` returns which of the three lists is on screen, and every key handler dispatches on it. `list()` / `list_mut()` pick the `ListState` from `(side, open)`, and the public `visible()`, `selected()` and `query()` read the list on screen through `list()`. The playlists are listed lazily: `refresh_playlists` runs when `Tab` enters the playlists side, when a playlist is closed, and after rename, delete or a vanished file. It never runs after `a`/`A`, "New playlist…" or `d`/`J`/`K`, which was fine while the list was hidden during those edits. The `Resolver` (`OnceCell`) is built by the first listing or the first picker (`a`/`A`), so users who never touch playlists don't pay for building it. Status messages go through one `pending: Option<Notice>`, so the last writer wins.
- **`ui::render` (`src/ui.rs`)** draws a fixed vertical stack: the list (`Min(3)`), a bordered now-playing block (`NOW_PLAYING_HEIGHT = 6`, a ratatui `Gauge` with a centred "1:05 / 3:30" label), a status line, and a key bar. The key bar comes from eight hand-written `KEYS_*` strings (three for the views, five for the prompts and search) and is cut at the width. Popups (picker, name input, delete confirmation) are centred over the list area. `ui::View` keeps a single scroll offset and the `page_size` that the event loop feeds back with `App::set_page_size`.
- **Keys documented in three places.** Besides the `KEYS_*` bar strings, `cli.rs` has its own hand-written `KEYBINDINGS` text, and the README has two key tables. All three are kept in sync by hand. The CLAUDE.md rule "update README, `--help` and the help bar together" exists because of that.
- **Rename.** `rename_playlist` silently drops the open copy if it is the renamed playlist. A comment says this is "not reachable from the keys", because `R` only works in the playlists list. With a sidebar next to the open playlist, it becomes reachable. `install(playlist)` only replaces the open copy when the names are equal, so it cannot follow a rename by itself.
- **Constraints:**
  - All text drawn must be sanitised (playlist names through `shown_name`, tags already sanitised).
  - clippy pedantic, MSRV 1.89, no new dependencies.
  - Every UI behaviour is tested as key sequences against `App` and as `TestBackend` frames.

## Goals / Non-Goals

**Goals:**
- Keep `App` the single owner of UI state, and keep rendering read-only. The two panes are two views of state that already exists (`library`, `playlist_list`, `open`), not a new copy of it.
- Keep every existing playlist guarantee: saves, stamp checks, read-only reasons, forwarding edits to the play order. The change is confined to *which list keys act on* and *how it is drawn*.
- One key table feeding `--help`, the help overlay and the bottom bar.

**Non-Goals:**
- Showing a playlist's contents while the sidebar selection moves (preview). That would mean a disk read for every arrow key on the UI thread, and the README already warns that a stalled network mount freezes the UI. Only `Enter` loads a playlist.
- Mouse input, user-configurable themes and `NO_COLOR`. The palette uses named ANSI colours only, so it follows the terminal theme.
- Generating the README tables from the key table. The README stays hand-written, and task 2.7 cross-checks it.

## Decisions

### D1. Focus model replaces `Side`
`Side` becomes `Focus { Sidebar, Main }`. What the main pane shows is derived: `open.is_some()` means the playlist, otherwise the library. A separate `main_shows_playlist` flag would only duplicate that. This works because `open` is now set *only* by `Enter` on a playlist in the sidebar. It is cleared by:
- `Enter` on "Library";
- deleting the shown playlist;
- its file disappearing (`playlist_gone`);
- a failed reload after renaming it (D5).

`App::view()` keeps its meaning, "the list keys act on", which keeps the key-handler changes small:
- `(Sidebar, _)` → `View::Playlists`;
- `(Main, None)` → `View::Library`;
- `(Main, Some)` → `View::Playlist`.

`list()` / `list_mut()` follow `view()` exactly as today, so key handling is unchanged. Rendering, though, now draws both panes in every frame, and `visible()` / `selected()` / `query()` only describe the focused one. New read-only accessors for rendering:
- `focus()`;
- `main_view() -> MainView { Library, Playlist }`;
- `sidebar_list()` and `main_list()`, each returning a `ListView<'_> { rows: &[usize], selected: Option<usize>, query: &str }` borrowed from the matching `ListState`.

The ui functions (`count_title`, `render_tracks`, the sidebar renderer) take a `ListView` instead of calling `app.visible()` / `app.selected()` / `app.query()`. The old focused-list accessors stay for key-handling tests.
- *Alternative*: keep `Side` and add a `focus` flag on top. Rejected: two booleans encoding three states invites impossible combinations, such as focus on a side that is not drawn.

### D2. The sidebar's "Library" row lives in the sidebar `ListState`
`playlist_list` becomes the sidebar list. Its row values are `0` for "Library" and `i + 1` for `playlists[i]`. `refilter` always keeps value `0`, so the "Library" row is never filtered out (spec: *Filter the sidebar*). The `count_title` "N of M match" text counts playlists only.

Helpers `selected_playlist() -> Option<&PlaylistInfo>` and `sidebar_value_of(name) -> Option<usize>` hide the `+ 1` offset. Every site that indexes `playlists` through `playlist_list` today must go through them or account for the offset:
- `refresh_playlists`: its `keep` lookup and its `select_value`;
- `activate`, for `View::Playlists`;
- `refilter_current` and `clear_filter`, for the sidebar: the length becomes `playlists.len() + 1` and the matcher always accepts value 0;
- `count_title`, which counts playlists, not rows;
- `start_rename` and `start_delete`;
- `ui::render_playlists`, which becomes the sidebar renderer.

`R` and `D` on the "Library" row do nothing. Because the "Library" row is always listed, the sidebar is never empty. The "No playlists yet…" and "No playlists match '…'" texts are drawn as a dim hint *below* the "Library" row, not instead of the list.

**Selection after filtering.** `apply_filter` sets `selected = 0`. In the sidebar, that would select "Library" after every keystroke of a search, so `/run`, `Enter`, `Enter` would open the library. For the sidebar, `apply_filter` instead selects row 1, the first matching playlist, when one exists, and row 0 otherwise.
- *Alternative*: a separate `library_selected: bool` beside an unchanged playlist list. Rejected: moving with `k` from the first playlist up to "Library", and `g`/`Home`, `PgUp` across both, would all need special cases. One list of rows gets them for free.

### D3. Keys
- **Focus changes go through two functions.**
  - `focus_sidebar(select: Option<&str>)` sets the focus, calls `refresh_playlists(select)` (spec: *listed again whenever the sidebar gains focus*), and so keeps the sidebar selection unless `select` names a listed playlist.
  - `focus_main()` sets the focus and, if a playlist is shown, runs `check_open()`. If the file is gone, `playlist_gone` clears `open`, so the main pane shows the library with the error notice (spec: *Open playlist removed meanwhile*).
- **`Tab`**, and **`BackTab`** (Shift+Tab, which many terminals send for the same key with Shift), toggle focus through those two functions. `Tab` keeps the sidebar selection.
- **`Enter`** in the sidebar:
  - On "Library": clear `open` and focus Main. The library `ListState` was never touched, so its filter and selection survive.
  - On the playlist already shown: `check_open()` (reload only if changed, which keeps filter and selection via `install`), then focus Main.
  - On another playlist: `open_named` as today, then focus Main. A failed load leaves `open` and the focus unchanged and reports the error.
- **`Esc`**:
  - Main + playlist: clear the filter, else `focus_sidebar(Some(shown name))`. `open` is kept, and the shown playlist becomes the sidebar selection, as the old `close_playlist` did.
  - Main + library: clear the filter, as today.
  - Sidebar: clear the filter only.
- **`a` / `A`** stay library-only (`View::Library`). **`d` / `J` / `K`** stay playlist-only (`View::Playlist`).
- **`R` / `D`** stay on `View::Playlists`, which is now the sidebar.
- **`?`** in Normal mode opens the help overlay (D6). It is not a search character in Normal mode, and in Search mode it is still typed into the query.

### D4. Startup listing, the resolver cost, and notices
`App::new` calls `refresh_playlists(None)` so the sidebar is filled on the first frame. This builds the `Resolver` at startup: a `HashMap` of all track paths. That cost used to be paid only by users who opened the playlists side or the picker. It is linear in the library size and small next to the library scan, which already reads every file's tags. Task 2.2 measures `App::new` on a generated 100 000-track list. If it adds more than 100 ms, the listing moves to the first `tick` after the first frame. That needs no spec change: the first frame shows an empty sidebar for one frame.

**Notices.** Every write to `pending` goes through one new helper, `App::notify(notice)`:
- With nothing pending, the notice is stored.
- An error replaces a pending info notice.
- An info notice never replaces a pending error.
- Two notices of the same level are joined with `; `.

So a listing error at startup does not hide the library-scan notice passed to `App::new`, which reports skipped entries; the two are joined. Likewise, "Renamed X to Y" cannot hide a listing or reload error raised in the same key press. `playback.take_notice()` in `tick` keeps its current precedence.
- *Alternative*: list lazily on the first sidebar focus, and show "Press Tab to load playlists" until then. Rejected: the sidebar showing nothing until a key is pressed defeats the point of the change.

### D5. Rename of the shown playlist follows the new name
`rename_playlist` no longer drops `open`. `install` cannot be reused here, because it only replaces `open` when the names are equal, and after the rename `open` still carries the old name.

On a successful `store.rename(old, new)`, when `open.name() == old`:
1. Load the playlist with `store.load(new)`. The new path and stamp therefore come from disk and are never patched in memory.
2. Call a new `replace_open(playlist)`. It swaps `open.playlist` and `open.items`, refilters `open.list` and restores the selected entry with `select_value(keep)`. It shares that body with `install`.
3. Forward to the play order with `follow_reload`, which is already done for reloads.

If the load fails, `open` is cleared and the load error is reported with `notify` (D4). This is a change: today `open` is cleared silently. The task 2.4 test checks that a `d` after the rename saves to `<new>.m3u8` and that `<old>.m3u8` is not recreated.

`delete_playlist` already clears `open`; with D1, that means the main pane shows the library (spec: *Delete the shown playlist*). The focus stays on the sidebar.

### D10. The sidebar stays current after the app's own edits
With the sidebar always visible, a stale entry is a visible bug. For example, a playlist created through "New playlist…" would not be listed, and a count would not drop after `d`. A full `refresh_playlists` after each edit would re-read every playlist file on every key press.

Instead, a helper `update_info(&Playlist)` writes the saved playlist's `PlaylistInfo` (track count, read-only reason) in place. It inserts the entry, sorted case-insensitively like the store's listing, when the playlist is new. Then it refilters the sidebar and keeps the selection with `select_value` on the previously selected name. It is called after:
- a successful add (`a`/`A`) or create;
- a successful `d`/`J`/`K` save;
- a reload in `install`.

Rename and delete keep calling `refresh_playlists`, because they change the set of files.

### D6. Help overlay: `Mode::Help { scroll: usize }`
Handled by `help_key`:
- Up/Down, `k`/`j`, PgUp/PgDn, Home/End, `g`/`G` change `scroll`;
- Ctrl+C quits (already handled before mode dispatch);
- every other key, including `?`, `Esc` and `q`, sets `Mode::Normal` and is consumed.

The overlay is centred over the whole screen above the status line, at most 64 columns wide. It draws the key table grouped as Library, Sidebar & playlists, Playback, Navigation & search, Help & quit. The bar lists its labels in table order, so this order puts each view's own keys before the shared ones, which matters because the bar is cut at the width. Lines are truncated with `…`, never wrapped, so the number of lines does not depend on the width. `keys::help_line_count()` is the single count, and both the renderer and `App` use it.

`scroll` is clamped in `App` to `help_line_count() - help_rows` (saturating). `help_rows` is the overlay's visible height, reported back by the event loop exactly like `page_size`: `ui::View` records it while the overlay is drawn and `terminal::run` calls `app.set_help_rows`. Without that, `j` at the bottom would keep growing an invisible offset and `k` would appear dead.

Two more rules:
- `set_help_rows` re-clamps `scroll` too, so a resize that makes the overlay taller never leaves the view beyond the end.
- PgUp/PgDn move by `help_rows`. `help_rows` keeps its last value between openings and is at least 1.

### D7. One key table: new `src/keys.rs`
`keys.rs` holds `pub const GROUPS: &[Group]`. A `Group` has a title and entries. Each entry has `keys` (e.g. `"J / K"`, ASCII so `--help` aligns), `action` (the long text), and `bar: &[Bar]`. A `Bar` is a short label such as `"J/K move"` plus the contexts whose bar shows it. A prompt's labels sit on the entry that opens the prompt: the picker and "New playlist…" labels on `a / A`, the rename labels on `R`, the confirmation labels on `D`, the search labels on `/`, and the overlay labels on `?`. Within an entry, labels are listed in the order they are drawn.

`BarContext` is one of:
- Library, Playlist, Sidebar — Normal mode, by `view()`;
- Search, Picker, Name (create), Rename, ConfirmDelete, Help.

Uses:
- `cli.rs` builds `after_help` from `GROUPS` at runtime with `keys::help_text()`. clap's `after_help` accepts a `String`.
- The overlay renders `GROUPS` directly.
- `ui::key_help` becomes `keys::bar(context) -> Vec<&'static str>`, joined with ` · `.

The bar draws the labels left to right, stops before a label that would not fit, and right-aligns `? help` in the three Normal-mode contexts (Library, Playlist, Sidebar). It is left out in Search, Picker, Name, Rename, ConfirmDelete and Help, where `?` is typed, cancels the prompt, or closes the overlay.

Tests check that:
- every `BarContext` has a non-empty bar;
- `help_text()` contains each group title and the playlist keys (spec: *Help lists playlist keys*);
- no bar label is wider than 24 columns.
- *Alternative*: keep the hand-written strings. Rejected: three copies, plus the README, must be kept in agreement by hand, and the new focus semantics would change all of them at once.

### D8. Layout and theme
- **New `src/theme.rs`** holds `const` style roles:
  - `PLAYING`: green + bold;
  - `SELECTED_FOCUSED`: reversed;
  - `SELECTED_UNFOCUSED`: bold and underlined, no reverse, so the inactive pane's cursor is visible but clearly secondary, and not told apart from the playing row by colour alone;
  - `BORDER_FOCUSED`: cyan, with a bold title;
  - `BORDER_UNFOCUSED`: default colour;
  - `DIM`: dark gray, for hints and read-only names;
  - `ERROR`: red + bold;
  - `PAUSED`: yellow;
  - `PROGRESS`: cyan.

  Every `Color::` / `Modifier::` literal in `ui.rs` moves there, and so does every `Stylize` shorthand used for colour or emphasis (`.dark_gray()`, `.bold()`, `.italic()`). The module starts with a `//!` doc. The unfocused border is deliberately not `DarkGray`, which is invisible on some dark themes such as Solarized.
- **Vertical layout:** `[body Min(3), now-playing Length(3), status Length(1), keys Length(1)]`. That is 5 fixed rows instead of 8.
- **Body.**
  - When `width >= 70` (`TWO_PANE_MIN_WIDTH`): `[sidebar Length(clamp(width / 4, 22, 32)), main Fill(1)]`.
  - Otherwise only the focused pane, using the whole body width.

  Popups are centred over the body.
- **Sidebar rows:** a playing marker, the name (elided at the end), and the track count right-aligned. The marker is the play-state marker (`▶ ‖ ■`), as on today's playlist rows, on the entry that is the play order, and only while a track is loaded (`playback.current_track().is_some()`). At startup, nothing is marked. A read-only playlist's name is drawn `DIM`. The sidebar has no header row. Its title is ` kasset ` on the left and the playlist count on the right. A `─` separator is drawn under "Library" when there is room.
- **Main pane:**
  - Library: titled ` Library · <root> `, with the root elided at the start as today.
  - Playlist: titled ` <name> ` or ` <name> · read-only `.
  - Both show the match count on the right and use the existing track table.
- **`ui::View`** gains a second offset: `sidebar_offset` and `main_offset`. Both panes are now drawn in the same frame and must scroll independently. `page_size` is taken from the focused pane, so PgUp/PgDn match what the user is looking at.
- **Now-playing area:** a `Block` with only the top border, titled ` Now playing `, and two lines inside.
  - Line 1: state marker and title (bold), then ` — artist · album` (elided at the end). Right-aligned: `shuffle on|off · repeat off|all|one · vol N%`. When the width is short, the right part is drawn first and the left part gets what is left, at least the state and the start of the title.
  - Line 2: `elapsed ━━━━╸──── total`, drawn by a small function: `filled = round(ratio × width)` cells of `━`, a `╸` head when partially filled, then `─`. It replaces `Gauge`, whose centred label hides the bar on narrow terminals. `progress_ratio` keeps its clamping.
- Plain words are used for the shuffle and repeat indicators instead of emoji or arrows. Emoji widths vary between terminals and would break ratatui's width computations.

### D9. What stays the same
These keep their current code paths: picker, name input, delete confirmation, the search line in the status row, playback, `Queue` and the playlist store. Beyond the `ui.rs` call sites and the `View`/`Focus` dispatch, these change:
- `notify` (D4);
- rename following the shown playlist (D5);
- `update_info` after edits (D10).

## Risks / Trade-offs

- [Many existing app tests encode the old `Tab` and `Esc` semantics (`switch_side`, "Esc back to the list")] → Port them deliberately in task 2.6: each old scenario maps to a new-spec scenario. Tests whose assertions are about playlist storage or playback keep their assertions and only change the key prelude, through updated helpers (`select_playlist`, `open`). Queue and playback tests must not change at all.
- [Startup now always builds the resolver and reads every playlist] → Bounded by the existing `LIST_BYTE_BUDGET`. Measured in task 2.2. Above 100 ms, the listing moves to the first tick (D4).
- [Box-drawing and marker glyphs (`━ ╸ ─ ▶ ‖ ■ …`) are East-Asian "ambiguous" width and may be double-width in some CJK terminal setups] → Same class as the borders ratatui already draws, and the current UI already uses `▶ ‖ ■ …`. Documented under Terminal notes in the README. No ASCII fallback.
- [A user whose muscle memory is `Esc` → playlists list] → `Esc` still leads to the playlists, now by focusing the sidebar. Listed as a breaking key change in the README and release notes.
- [At exactly 70–79 columns the main table gets about 46 columns] → The title, artist and album columns already use `Fill` ratios and elide. Below 70 columns the single-pane fallback applies.
- [Groups land one at a time. After group 2 the sidebar rows are offset by one, but the full sidebar renderer only arrives in group 4] → Task 2.1 makes the minimal `ui.rs` change (index through `sidebar_list()` and skip value 0) so the whole test suite stays green after every group.
- [`?` help scroll needs feedback from the renderer] → Handled like `page_size`, which is already fed back. Until the first frame `help_rows` is 1, so the first scroll is clamped conservatively and corrected on the next frame.

## Migration Plan

No persisted state or file format changes. Users only see the new layout and key semantics. Rollback is reverting the change. Release as a minor version bump, with the key changes called out in the release notes.
