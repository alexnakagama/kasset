# Design

## Context

See proposal.md (Why) and the `play-queue` spec for behaviour. Current state
that shapes the approach:

- `Queue` (`queue.rs`) is the pure play order of one source: items (library
  indices), a cursor, shuffle and repeat. It supports live edits and a
  *detached* state. CLAUDE.md requires existing queue and playback tests to
  keep passing unchanged when play-order logic is generalised.
- `Playback` (`playback.rs`) owns `queue: Queue` and `source: Source`.
  `current()` is `queue.current()`, and `current_item()` is
  `queue.current_item()`. `next()`, `track_ended()`, `record_failure()` and
  `prev()` all move `queue` and then call `start_current`, which loads
  `current()`. `play`, `play_source` and `resume` reset `queue` and `source`.
- `App` (`app.rs`) shows one thing in the main pane: the library, the open
  playlist (`open`) or a browse stack (`browse`). `open` and `browse` are
  never both set, and only `show_library`, `show_playlist` and `show_browse`
  set them. The sidebar has `FIXED_ROWS` (3) fixed rows: "Library" (0),
  "Artists" (1) and "Albums" (2). Playlist `i` has value `i + FIXED_ROWS`,
  mapped only through `SidebarEntry::of_value` / `value()`.
- Markers: `sidebar_playing()` maps the `Source` to a sidebar value.
  `browse_row_playing()` and the playlist view use `source` and
  `current_item()`. The library marks `current()`.
- State file (`state.rs`): per folder, `track`, `item`, `position-ms`,
  `source` and `open` lines. Unknown keys are ignored, and
  `render` keeps the file within `MAX_STATE_BYTES` (64 KiB) by dropping the
  current folder's resume point and then `open` when it does not fit.

## Goals / Non-Goals

**Goals:**
- The queue sits on top of the play order. `queue.rs` and its tests do not
  change, and the existing playback tests keep passing unchanged.
- One definition of "the current track" (`Playback::current()`) covers queued
  tracks, so the now-playing line, lyrics, the visualizer and the library
  marker need no queue-specific code.
- The state file stays readable by older versions (no `FORMAT_VERSION`
  bump).

**Non-Goals:**
- A history of played tracks (`p` does not walk back through the queue).
- Queueing from the sidebar (a whole playlist with `e` on its row), or
  dragging with the mouse.
- Saving queued tracks the scan did not find (unlike an unresumable resume
  point, they are dropped).
- Showing the playing queued track inside the queue view. Once it plays it
  has left the queue, and the sidebar "Queue" marker shows that it is from
  the queue.

## Decisions

### D1. The up-next list lives in `Playback`, beside `Queue`

`Playback` gains:

- `up_next: Vec<usize>`: library indices in play order, at most
  `MAX_UP_NEXT` (1000). A `Vec` is enough at that size (front removal is
  `O(n)` on at most 1000 elements), and edits at any position (`d`, `J`/`K`,
  play now) are simple.
- `queued_current: Option<usize>`: the library index of the queued track
  that is current, if one is (named apart from `Queue`, the play order).
- `up_next_taken: u64`, counting tracks taken from the front of `up_next`
  (to play them), so the queue view can keep its selection on the same entry
  (D6).
- `up_next_generation: u64`, bumped on every change to `up_next` (including
  advancing), so `App` knows when to refilter the queue view without
  comparing lists.

`queue` keeps the play order's position the whole time a queued track plays:
its cursor stays on the track played before the queue took over.

*Alternatives:* putting the queue inside `Queue` mixes two orders (shuffled
source and FIFO up-next) in one cursor, and would change `queue.rs` and its
tests. Making the queue a `Source` (option B in explore) loses "carry on
where I was", which is the reason for the feature.

### D2. `current()` covers queued tracks; source position is separate

- `current()` is `queued_current.or(queue.current())`. `start_current`, `duration`,
  `current_track`, `record_failure`'s message and `resume` use it unchanged.
- `current_item()` returns `None` while `queued_current` is `Some`, so every
  "row of the play order" marker (playlist entries, album and artist rows,
  browsed tracks) turns off without code changes at its call sites.
- New `playing_queued() -> bool`, and `source_current() -> Option<usize>` /
  `source_item() -> Option<usize>` (the `queue` cursor, for saving the session
  while a queued track plays).
- Every other caller of `current()` / `current_item()` in `app.rs`
  (`session`, `browse_row_playing`, `sidebar_playing`, lyrics `loaded_for`,
  `settle_pending`) is audited in task 3.5, which records the result.

### D3. One advance rule for next, track end and failures

A private `advance(cause: Advance) -> bool` replaces the direct
`queue.next(cause)` calls in `next`, `track_ended` and `record_failure`.
It returns whether a track is now current to start:

1. `cause == Ended`, repeat one, and `queued_current` is `Some`: keep it
   and replay it.
2. `cause == Ended`, repeat one, a play order track is current and the play
   order is not detached: `queue.next(Ended)` (it returns the same track, so
   it is replayed). Repeat one holds over the queue: the queue waits for `n`
   (spec *Shuffle and repeat with the queue*). A *detached* track cannot be
   replayed (`Queue::next` moves every cause on while detached), so it falls
   through to step 3, which keeps `removed_playing_entry_with_repeat_one`
   and the other existing tests unchanged.
3. Else, if `up_next` is not empty: `queued_current = Some(up_next.remove(0))`,
   bump `up_next_taken` and the generation.
4. Else: `queue.next(cause)` from the play order's position (nothing current
   → its first item, as today). **Only when it returns `Some`** is
   `queued_current` set to `None`. On `None` (end of the play order with
   repeat off) nothing changes, so the queued track that just played stays
   current: `n` says "Already at the last track" and keeps playing it, and
   at a natural end `track_ended` stops with it still current, so Space
   replays it, as for the last track of a play order today.

`Manual` and `Failed` never take steps 1–2, as `Queue` already does for
them.

Failures (`record_failure`): a failed queued track already left `up_next`.
It is reported (`Cannot play '<file>'…`), and `failures` is incremented only
when `queued_current.is_none() && !queue.is_detached()`, so queued failures
never use up the play order's attempts. Then:

```
more = if !up_next.is_empty() { advance(Failed) }   // step 3
       else { failures < queue.len() && advance(Failed) }
```

The "(no playable tracks, stopped)" suffix is added only when `up_next` is
empty and `failures >= queue.len()` and a play order track failed, so a
failing queued track with an empty play order (e.g. every playlist entry
removed meanwhile) moves on to the next queued track instead of stopping
with a wrong suffix. Queued tracks are finite and each is tried once, so a
run of bad queued files cannot loop. `MAX_ATTEMPTS_PER_CALL` still bounds
one call, and the `retry` path in `tick` re-enters `start_current`, which
loads `current()` (the queued track when one is current).

`prev()` with `queued_current = Some(q)`: restart `q` if past
`PREV_RESTART_AFTER` and not stopped. Otherwise, if `queue.current()` is
`Some`, set `queued_current = None` and start the play order's track from
zero. If the play order has nothing current, `q` is restarted instead. `q`
is never put back in `up_next`.

`stop()` keeps `queued_current`, so Space (which starts `current()` when
stopped) replays the same queued track. `toggle_pause()` when stopped with
nothing current and a non-empty `up_next` calls `advance(Manual)` and starts
the first queued track, so a queue restored with nothing to resume plays on
Space as well as `n`.

### D4. Starting a new play order keeps the queue

`play`, `play_source` and `resume` set `queued_current = None` and leave `up_next`
alone. `Enter` on a queue row calls `play_queued(pos)`: it removes
`up_next[pos]`, sets `queued_current` to it, and starts it. The play order's cursor
is unchanged, so after it the queue and then the play order continue as in
D3.

Queue operations on `Playback` (all bump `up_next_generation` and validate
indices, returning `false` and changing nothing when invalid):
`enqueue(tracks, front) -> usize` (number added: the first ones that fit
under `MAX_UP_NEXT`), `remove_queued(pos)`, `move_queued(from, to)`,
`play_queued(pos)`, `clear_queued()`, `up_next() -> &[usize]`, and
`restore_up_next(tracks)` (start only, capped).

### D5. "Queue" is the fourth fixed sidebar row (value 3)

`FIXED_ROWS` becomes 4, with `SidebarEntry::Queue` at value 3 (after
"Albums") and playlist `i` at `i + 4`. "Artists" and "Albums" keep their
values, so only playlist values move. Every mapping already goes through
`of_value` / `value()`. `SidebarPick` gains `Queue`. The sidebar filter keeps
all fixed rows, as today. The UI draws the row as "Queue" with
`count_title`-style count, and the separator still goes under the last
fixed row.

*Alternative:* "Queue" right under "Library" (value 1) reads well, but would
renumber "Artists" and "Albums" and touch many more tests and helpers for no
behavioural gain.

### D6. The queue view is a third main-pane state

`App` gains `queue_shown: bool` and `queue_list: ListState` (rows =
positions in `playback.up_next()`). `show_queue()` sets it and clears `open`
and `browse`. `show_library`, `show_playlist` and `show_browse` clear it.
`main_view()` debug-asserts that at most one of `open`, `browse` and
`queue_shown` is set. `MainView::Queue` and `View::Queue` join the enums,
with arms in `list`, `list_mut`, `refilter_current`, `clear_filter` and
`selected_track` (the row's library index).

`App::sync_queue_view()` compares `up_next_generation` with the last one
seen and, on a change, refilters `queue_list`. It runs at the **start** of
`handle_key` (so a queue-view key never acts on positions from before an
`n`, `p` or track end) and at the end of `handle_key` and `tick`. Positions
are only valid for one generation. The selection follows the entry: when
`up_next_taken` grew by `k`, the selected position moves up by `k`
(saturating, then clamped), so auto-advance does not make it jump. After
App's own edits the selection is set explicitly (`d`: same row, clamped;
`J`/`K`: the moved entry). The filter uses the library's search keys.

Keys in `View::Queue`: `Enter` → `play_queued`, `d` → `remove_queued`,
`J`/`K` → `move_queued` (with a filter active they change nothing and say
`Clear the filter (Esc) to reorder`, as `move_selected` does for
playlists), `a`/`A` → the picker (as in the library), `Esc` → clear the
filter, else `focus_sidebar(Some(SidebarPick::Queue))`. `Backspace` does
nothing here, as in the library.

Wiring that the new row and view touch (most are compile-forced by
exhaustive matches): `escape`'s pick (`queue_shown` → `SidebarPick::Queue`),
`sidebar_pick` / `pick_value` / `restore_sidebar`, `show_selected` (`Enter`
on "Queue" → `show_queue`), `refilter_sidebar`, `ui::sidebar_playlist_rows`
and `ui::FIXED_ROW_NAMES`.

### D7. `e` / `E` and the clear prompt

`e` (end) and `E` (front) act in `View::Library`, `View::Playlist` and
`View::Tracks` (selected track) and in `View::Groups`
(`selected_group_tracks`, the same tracks `a` adds). They do nothing in
`View::Queue` and in the sidebar. Notices (`Info`, sanitised titles):

- one track: `Queued '<title>'` / `Playing next: '<title>'`
- several: `Queued N tracks` / `N tracks play next`
- cut by the limit: `Queued K of N tracks; the queue is full (1000)`
- nothing fits: `The queue is full (1000 tracks)`

`D` on the "Queue" sidebar row (only if the queue is not empty) opens a new
`Mode::ConfirmClearQueue` ("Clear the queue (N tracks)? y/n"). `y` calls
`clear_queued` (`Info` notice `Cleared the queue (N tracks)`, none if
the queue emptied itself meanwhile), and any other key cancels and is
consumed (`Info` notice `The queue was not cleared`), like
`ConfirmDelete`. A separate mode keeps the playlist-delete code untouched.
It gets its own popup in `ui.rs` (drawn like `render_confirm_delete`, text
"Clear the queue (N tracks)? y/n") and its own `BarContext::ConfirmClearQueue`
(`y clear`, `any other key cancel`; in `ALL`, not normal, so no `? help`).
`R` on the "Queue" row does nothing.

### D8. Markers

- `sidebar_playing()`: `Some(Queue value)` when `playback.playing_queued()`.
  Otherwise unchanged.
- `browse_row_playing()`: `false` for every row while `playing_queued()`
  (album and artist rows check `source` only, so this needs an explicit early
  return). Track rows already turn off through `current_item()`.
- Playlist view: through `current_item()` (D2). Library: `current()`,
  unchanged.
- The queue view marks no row.

### D9. State format: `queue` and `queued-track` lines

In a folder entry:

- `queue <relpath>`, repeated, in queue order. Each is parsed on its own
  (an invalid one is skipped), at most `MAX_UP_NEXT` are kept, and further
  ones are ignored. `FolderEntry` gains `queue: Vec<RelPath>`.
- `queued-track <relpath>` (first valid wins, only kept when the entry has a
  `track`). `Resume` gains `queued: Option<RelPath>` (`None` by default). With it, `track`,
  `item` and `source` describe the play order's track the queued track was
  played after, and `position-ms` is the queued track's position.

Older versions ignore both keys and do not fail. They resume `track` at
`position-ms`, which was the queued track's position: an acceptable
fallback (past the end → 0:00, per the existing rule; otherwise a slightly
odd position), so `FORMAT_VERSION` stays 1.

`session()`: while `playing_queued()` and the play order has a current track,
`track`, `item` and `source` come from `source_current()` / `source_item()`,
and `queued` is the `queued_current` track's `RelPath`. If the play order has nothing
current, the queued track is saved as a plain library resume point (`source
library`, item = its library index, no `queued`). The queue is saved as the
`RelPath`s of `up_next` (an entry without a `RelPath`, e.g. a non-Unicode
path off Unix, is skipped).

`render`: the entry is written as resume lines, `open`, then the `queue`
lines. Every folder, current or older, in order: full; else the longest
prefix of its queue that fits (cumulative line lengths, one pass), which is
the empty queue at worst. Only the current folder then falls back further to
the existing "no resume", "no open" steps. An older folder whose entry
without a queue does not fit is skipped, as today. So a long queue in
another folder is cut, never dropped wholesale, by an unrelated save. `merge`'s `keep` applies to the
resume point only. The queue always comes from the session.

`restore`: after `open`, the queue is mapped to library indices through a
`HashMap<RelPath, usize>` built once from the scanned tracks (not a linear
search per entry: up to 1000 × library size), then `restore_up_next`. Then
the resume point: the play order is resolved exactly as today from
`track` / `item` / `source`. With `queued` found, `Playback::resume` loads the
queued track at the position and the play order's cursor at the resolved
item (a new `queued: Option<usize>` argument). With `queued` not found, the
play order's track is resumed at zero. With the play order's track not found
but `queued` found, the library becomes the play order with nothing current,
and the queued track is loaded. A resume point that did not resume stays
`pending` as today (including its `queued`). When loading the queued track
fails, `resume`'s existing failure rule applies (nothing current, the play
order's list kept with no cursor), so the play order's cursor is lost for
this run and `n` starts the play order from its first item. This is
accepted: it is rare, the notice says so, and `pending` keeps the full point
for the next start.

The queue is restored independently of the resume point (before it in
code). Queued tracks the scan does not find are dropped, even when the whole
library is on an unmounted drive. Unlike the resume point they are not kept
as `pending`. See Non-Goals.

### D10. Keys and help

`keys::GROUPS` gains a "Queue" group: `e / E` (add to the end of the queue /
play next), and `Enter`, `d`, `J / K`, `Esc` in the queue view, plus `D` on
"Queue" in the sidebar (clear). `BarContext::Queue` shows `Enter play`,
`d remove`, `J/K move`, `Esc sidebar`. `Library`, `Playlist` and
`BrowseTracks` gain `e/E queue` before `Space pause` (the existing
"each view's own labels precede `Space pause`" test covers it).
`BrowseTop`/`BrowseGroups` do not show it in the bar (space), but the help
lists it. The existing action texts that name only playlists are widened:
`d` ("in a playlist or the queue: remove …"), `J / K` ("in a playlist or
the queue: move …"), `Esc` ("in a playlist, the queue or the artist / album
list …"), and `D` ("in the sidebar: delete the selected playlist, or clear
the queue on \"Queue\" (asks y/n)"). README keys tables change in the same
task.

## Risks / Trade-offs

- [Some `current()` caller assumes "current = play order's item"] → D2's
  audit task lists every caller. Tests cover the markers, the session saved
  while a queued track plays, lyrics loading for a queued track, and
  playlist edits (remove, reorder, reload) while a queued track plays.
- [Playlist values move from 3 to 4 and tests that hard-code them break] →
  tests should use `SidebarEntry::value`. Tests that hard-code values are
  changed only for the sidebar row change, and the hand-off says so.
- [Large queue makes the periodic autosave compare costlier] → `session()`
  builds at most 1000 `RelPath`s every 30 s, which is negligible. The
  `HashMap` is built only at restore.
- [Repeat one hides the queue] → this is intended (spec), and `n` moves on.
  The bar and help do not need to explain it.
- [The state file grows with long queues] → the 64 KiB cap with
  queue-first truncation (D9) keeps the resume point safe.
- [Downgrade loses queues] → an older kasset that saves drops the `queue`
  and `queued-track` lines of every folder (its parser ignores unknown keys).
  Accepted: the resume points survive and nothing fails.
- [Positions in the queue view go stale between a key and a tick] →
  `sync_queue_view` runs at the start of every key (D6).

## Migration Plan

No migration. Existing state files have no `queue` lines and load with an
empty queue. Files written by this version still work with older versions
(D9). Release as a minor version after the change is archived.
