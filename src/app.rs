//! UI state and the mapping from keys to actions. No terminal I/O happens
//! here, and files are only reached through a [`PlaylistStore`], so
//! everything is testable with a fake audio output and a temporary folder.
//!
//! The screen has two panes: a sidebar and a main pane. Exactly one has
//! keyboard focus ([`Focus`]); `Tab` / Shift+Tab move it. Keys act on the
//! focused pane only ([`App::view`]).
//!
//! - The sidebar lists [`FIXED_ROWS`] fixed rows, "Library", "Artists" and
//!   "Albums" (values 0, 1, 2), then the saved playlists (value
//!   `i + FIXED_ROWS` for `playlists[i]`). Values are only ever turned into
//!   entries through [`SidebarEntry`]. The fixed rows are never filtered
//!   out, so the sidebar is never empty. `Enter` shows the selected entry in
//!   the main pane and focuses it.
//! - The main pane shows the open playlist, a browse stack, or else the
//!   library ([`MainView`]). `open` (the playlist) and `browse` are never
//!   both set; only `show_library`, `show_playlist` and `show_browse` set
//!   them. Only `Enter` on a sidebar playlist opens a playlist; it is closed
//!   by `Enter` on another sidebar entry, by deleting it, and when its file
//!   disappears.
//! - "Artists" and "Albums" each have a browse stack of levels
//!   ([`BrowseKind`]) over the [`Catalog`] built once in [`App::new`]: the
//!   artist list → an artist's albums ("All tracks" first) → tracks, and the
//!   album list → tracks. `Enter` on a group row pushes a level; `Backspace`
//!   pops one (never the bottom level), and `Esc` does too once the filter
//!   is cleared; at the bottom level `Esc` focuses the sidebar on the entry.
//!   The stacks are kept while something else is shown, so `Enter` on
//!   "Artists" comes back to the same place. Catalog indices are only valid
//!   for the run and are never saved.
//!
//! Each list (library, sidebar, open playlist, every browse level) keeps
//! its own filter and selection; going back leaves the parent level as it
//! was, with the opened entry selected. The playlists are listed in
//! [`App::new`], so the sidebar is filled on the first frame, again whenever
//! the sidebar gains focus, and after rename and delete. The app's own
//! edits (add, create, `d`/`J`/`K`, reloads) update the edited playlist's
//! sidebar entry in place.
//!
//! `?` opens the help overlay ([`Mode::Help`]). Its scroll is bounded by the
//! number of help lines the last frame showed ([`App::set_help_rows`], fed
//! by the event loop like the list page size), so scrolling stops at the
//! last page. Any key other than a scroll key closes it and does nothing
//! else.
//!
//! The spectrum visualizer is on at start (or as saved); `v` (Normal mode,
//! either pane) turns it off or on, with a status message when the terminal
//! is too short for the strip, where the change cannot be seen. Whether the
//! strip fits and how many bars it has are fed back by the event loop
//! ([`App::set_spectrum_layout`]). [`App::tick`] measures the spectrum only
//! while the strip is shown, and passes samples only while playing with
//! audio arriving (the tap's count advanced within [`STALE_AFTER`]), so a
//! paused, stopped or stalled track lets the bars fall. While the strip is
//! not shown, no FFT runs and the levels are reset.
//!
//! The lyrics pane ([`LyricsPane`]) is on at start (or as saved); `l`
//! (Normal mode, either pane) turns it off or on, with a status message
//! when the terminal is too narrow for it, like `v`. Whether it fits is fed
//! back by the event loop ([`App::set_lyrics_layout`]). It never takes focus.
//! While it is shown, [`App::tick`] reads the current track's lyrics once
//! each time the current track's *path* changes (so seeking, pausing or
//! replaying the same file never reads again); while it is not shown nothing
//! is read and the cache is dropped, so showing it again always reads afresh.
//! [`App::lyrics_deadline`] tells the event loop when the highlighted synced
//! line changes next, or asks for an immediate frame while a read is due
//! (track changed, pane shown or widened), so the pane is never left blank
//! for a whole frame.
//!
//! Status messages raised during one key press go through [`App::notify`]:
//! an error is never hidden by an info message raised after it, and two
//! messages of the same level are joined.
//!
//! Every playlist change (add, remove, move, rename, delete) goes to disk
//! before the next key is handled. Edits are made on a copy and only shown
//! once saved, so a failed or conflicting save leaves the screen as it was.
//! Changes to the playlist that is playing are forwarded to the play order.
//!
//! The session (settings, the current track and position, its play source
//! and the shown playlist) is saved through a [`StateStore`]. [`App::new`]
//! uses [`NoState`], which saves nothing; [`App::restore`] installs the real
//! store and continues the saved session once at start: the settings, the
//! shown playlist (focused), then the track, loaded **paused**. The state
//! file is untrusted: it only ever selects a track the scan found (matched by
//! its path relative to the root) and playlists the listing found (by exact
//! name). A resume point that could not be resumed is kept and saved again
//! until a track becomes current. [`App::tick`] saves at most every
//! [`SAVE_INTERVAL`], only when the session changed, and reports only the
//! first of a run of failed saves. [`App::shutdown`] takes the final session
//! before it stops the audio and writes nothing; [`App::save_final`] writes
//! it after the terminal is restored. With a store that persists nothing, or
//! after a newer state file was seen, no session is ever built or saved.

use std::cell::OnceCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::browse::Catalog;
use crate::keys;
use crate::library::Track;
use crate::lyrics::{self, Lyrics};
use crate::playback::{Edit, Level, Notice, PlayState, Playback, Source};
use crate::player::Audio;
use crate::playlist::{
    AddOutcome, Playlist, PlaylistInfo, PlaylistStore, Resolver, SaveOutcome, StoreError,
    validate_name,
};
use crate::state::store::{NoState, Saved, StateStore};
use crate::state::{FolderEntry, MAX_POSITION, RelPath, Resume, SavedSource, Session, Settings};
use crate::text::{is_invisible_format, sanitize};
use crate::ui::{LYRICS_MIN_WIDTH, SPECTRUM_MIN_HEIGHT};
use crate::visualizer::{FFT_SIZE, Spectrum};

/// How long a transient status message stays on screen.
pub const STATUS_TTL: Duration = Duration::from_secs(5);

/// Longest search query or playlist name input accepted, in characters.
pub const MAX_QUERY_CHARS: usize = 128;

/// What keys currently do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Navigation and transport keys.
    Normal,
    /// Typing a search query.
    Search,
    /// Choosing the playlist to add tracks to.
    Picker(Picker),
    /// Typing a playlist name.
    NameInput(NameInput),
    /// Asking whether to delete the named playlist: `y` deletes, any other
    /// key cancels.
    ConfirmDelete(String),
    /// The help overlay, listing [`keys::help_lines`] from line `scroll`.
    /// Scroll keys move it; any other key closes it and does nothing else.
    Help {
        /// First help line shown, at most
        /// `keys::help_line_count() - help_rows` (see [`App::set_help_rows`]).
        scroll: usize,
    },
}

/// The "add to playlist" popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    /// Library indices of the tracks to add, in library order.
    tracks: Vec<usize>,
    /// The playlists offered, as listed when the picker opened.
    choices: Vec<PlaylistInfo>,
    /// 0 is "New playlist…", `i + 1` is `choices[i]`.
    selected: usize,
}

impl Picker {
    /// How many tracks would be added.
    #[must_use]
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// The existing playlists offered (after the "New playlist…" row).
    #[must_use]
    pub fn choices(&self) -> &[PlaylistInfo] {
        &self.choices
    }

    /// Selected row: 0 is "New playlist…", `i + 1` is `choices()[i]`.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }
}

/// What a typed name is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamePurpose {
    /// A new playlist holding these library tracks.
    Create(Vec<usize>),
    /// A new name for the playlist with this name.
    Rename(String),
}

/// The playlist name prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameInput {
    purpose: NamePurpose,
    /// The name as typed (only printable, visible characters).
    text: String,
    /// Why the last submitted name was refused, shown until the text changes.
    error: Option<String>,
}

impl NameInput {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    #[must_use]
    pub const fn purpose(&self) -> &NamePurpose {
        &self.purpose
    }
}

/// Which list keys act on: the focused pane, and what it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// The main pane, showing the library.
    Library,
    /// The sidebar (its fixed rows and the playlists).
    Playlists,
    /// The main pane, showing the open playlist.
    Playlist,
    /// The main pane, browsing a list of artists or albums (an artist's
    /// albums included, with its "All tracks" row).
    Groups,
    /// The main pane, browsing the tracks of an album or of an artist.
    Tracks,
}

/// The pane with keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Main,
}

/// What the main pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainView {
    Library,
    /// The open playlist ([`App::open_playlist`]).
    Playlist,
    /// A browse level ([`App::browse_tracks`] holds a track level's tracks).
    Browse(BrowseKind),
}

/// Which sidebar entry a browse stack starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseRoot {
    /// The artist list and what is opened from it.
    Artists,
    /// The album list and what is opened from it.
    Albums,
}

/// One browse level, by catalog index ([`App::catalog`]; valid for the run
/// only). Rows of each level's list ([`App::main_list`]) are:
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseKind {
    /// Every artist: rows are artist indices.
    Artists,
    /// An artist's albums: row value 0 is "All tracks" ([`ALL_TRACKS_VALUE`]),
    /// `i + 1` is the artist's `albums()[i]` (see [`artist_album_of_value`]).
    ArtistAlbums(usize),
    /// Every album: rows are album indices.
    Albums,
    /// An album's tracks: rows are positions in [`App::browse_tracks`].
    Album(usize),
    /// All of an artist's tracks, album by album: rows are positions in
    /// [`App::browse_tracks`].
    ArtistTracks(usize),
}

impl BrowseKind {
    /// Whether the level lists tracks (else artists or albums).
    #[must_use]
    pub const fn is_tracks(self) -> bool {
        matches!(self, Self::Album(_) | Self::ArtistTracks(_))
    }
}

/// Row value of "All tracks" in an artist's albums.
pub const ALL_TRACKS_VALUE: usize = 0;

/// The position in an artist's `albums()` of row value `value` of its
/// albums list; `None` for "All tracks".
#[must_use]
pub const fn artist_album_of_value(value: usize) -> Option<usize> {
    value.checked_sub(ALL_TRACKS_VALUE + 1)
}

/// How many fixed rows the sidebar has above the playlists: "Library",
/// "Artists" and "Albums" (D3 of add-artist-album-browsing).
pub const FIXED_ROWS: usize = 3;

/// One sidebar row, named from its value ([`App::sidebar_list`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarEntry {
    /// Value 0.
    Library,
    /// Value 1.
    Artists,
    /// Value 2.
    Albums,
    /// Value `i + FIXED_ROWS`: [`App::playlists`]`[i]`.
    Playlist(usize),
}

impl SidebarEntry {
    /// The entry with sidebar value `value`. The only place that maps a
    /// value to a playlist index.
    #[must_use]
    pub const fn of_value(value: usize) -> Self {
        match value {
            0 => Self::Library,
            1 => Self::Artists,
            2 => Self::Albums,
            _ => Self::Playlist(value - FIXED_ROWS),
        }
    }

    /// The sidebar value of this entry; the only place that maps a playlist
    /// index to a value. `None` for an index too large to have one.
    #[must_use]
    pub const fn value(self) -> Option<usize> {
        match self {
            Self::Library => Some(0),
            Self::Artists => Some(1),
            Self::Albums => Some(2),
            Self::Playlist(i) => i.checked_add(FIXED_ROWS),
        }
    }
}

/// One list as drawn: its rows, the selected row and its filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListView<'a> {
    /// Values of the rows listed, in order: library indices, sidebar values
    /// (see [`SidebarEntry`]), positions in [`OpenPlaylist::items`], or the
    /// rows of a browse level (see [`BrowseKind`]).
    pub rows: &'a [usize],
    /// Index into `rows` of the selection, if anything is listed.
    pub selected: Option<usize>,
    /// The search query (empty when no filter is active).
    pub query: &'a str,
}

/// The sidebar entry selected, kept across a change of its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SidebarPick {
    Library,
    Artists,
    Albums,
    Playlist(String),
}

impl From<BrowseRoot> for SidebarPick {
    fn from(root: BrowseRoot) -> Self {
        match root {
            BrowseRoot::Artists => Self::Artists,
            BrowseRoot::Albums => Self::Albums,
        }
    }
}

/// Rows, selection and filter of one list. Rows are values naming entries of
/// the list's full contents: library indices, sidebar values (never index
/// `playlists` with a raw row value: go through [`SidebarEntry`]), positions
/// in the open playlist's items, or a browse level's rows ([`BrowseKind`]).
#[derive(Debug, Clone, Default)]
struct ListState {
    /// Positions of the rows shown, in list order.
    rows: Vec<usize>,
    /// Index into `rows` of the selected row (0 when the list is empty).
    selected: usize,
    /// The search query as typed (only printable, visible characters).
    query: String,
    /// `query` lower-cased, the form matched against search keys.
    query_lower: String,
}

impl ListState {
    /// The whole list of `len` entries, unfiltered, first row selected.
    fn all(len: usize) -> Self {
        Self {
            rows: (0..len).collect(),
            ..Self::default()
        }
    }

    fn selected_row(&self) -> Option<usize> {
        (!self.rows.is_empty()).then_some(self.selected)
    }

    /// Position (in the full list) of the selected row.
    fn selected_value(&self) -> Option<usize> {
        self.rows.get(self.selected).copied()
    }

    /// Selects `row`, clamped to the list.
    fn select_row(&mut self, row: usize) {
        self.selected = row.min(self.rows.len().saturating_sub(1));
    }

    /// Selects the row showing `value` if it is listed, else keeps the
    /// current row (clamped).
    fn select_value(&mut self, value: Option<usize>) {
        let row = value
            .and_then(|v| self.rows.iter().position(|&r| r == v))
            .unwrap_or(self.selected);
        self.select_row(row);
    }

    /// Recomputes the rows of a list of `len` entries for the current query;
    /// `matches(position, query_lower)` says whether an entry matches. The
    /// selection is left to the caller.
    fn refilter(&mut self, len: usize, matches: impl Fn(usize, &str) -> bool) {
        self.query_lower = self.query.to_lowercase();
        let query = self.query_lower.as_str();
        self.rows = if query.is_empty() {
            (0..len).collect()
        } else {
            (0..len).filter(|&i| matches(i, query)).collect()
        };
    }

    /// Drops the filter over a list of `len` entries, keeping the selected
    /// entry selected.
    fn clear_filter(&mut self, len: usize) {
        let keep = self.selected_value();
        self.query.clear();
        self.query_lower.clear();
        self.rows = (0..len).collect();
        // Without a filter, row and position coincide.
        self.select_row(keep.unwrap_or(0));
    }

    fn view(&self) -> ListView<'_> {
        ListView {
            rows: &self.rows,
            selected: self.selected_row(),
            query: &self.query,
        }
    }
}

/// The playlist shown in the main pane.
#[derive(Debug)]
pub struct OpenPlaylist {
    playlist: Playlist,
    /// Library indices of its matched entries, in file order (duplicates
    /// kept); the play order's items when it plays.
    items: Vec<usize>,
    list: ListState,
}

impl OpenPlaylist {
    #[must_use]
    pub fn name(&self) -> &str {
        self.playlist.name()
    }

    /// Library indices of the listed entries, in playlist order.
    #[must_use]
    pub fn items(&self) -> &[usize] {
        &self.items
    }

    #[must_use]
    pub const fn playlist(&self) -> &Playlist {
        &self.playlist
    }
}

/// One level of a browse stack: what it lists and its own filter and
/// selection. Its parent level is never touched while it is shown, so
/// going back reselects the entry that was opened.
#[derive(Debug)]
struct BrowseLevel {
    kind: BrowseKind,
    /// A track level's tracks (library indices, in album or artist order):
    /// the play order's items when it plays. Empty for artist and album
    /// lists.
    tracks: Vec<usize>,
    list: ListState,
}

impl BrowseLevel {
    /// `kind` listed whole from `catalog`, first row selected. A track
    /// level's tracks are copied out of the catalog here, once.
    fn new(kind: BrowseKind, catalog: &Catalog) -> Self {
        let tracks = match kind {
            BrowseKind::Album(b) => catalog.album(b).map(|a| a.tracks().to_vec()),
            BrowseKind::ArtistTracks(a) => Some(catalog.artist_tracks(a)),
            BrowseKind::Artists | BrowseKind::ArtistAlbums(_) | BrowseKind::Albums => None,
        }
        .unwrap_or_default();
        let mut level = Self {
            kind,
            tracks,
            list: ListState::default(),
        };
        level.list = ListState::all(level.len(catalog));
        level
    }

    /// How many entries the level lists unfiltered.
    fn len(&self, catalog: &Catalog) -> usize {
        match self.kind {
            BrowseKind::Artists => catalog.artists().len(),
            BrowseKind::ArtistAlbums(a) => catalog
                .artist(a)
                // Its albums and the "All tracks" row.
                .map_or(0, |artist| artist.albums().len() + 1),
            BrowseKind::Albums => catalog.albums().len(),
            BrowseKind::Album(_) | BrowseKind::ArtistTracks(_) => self.tracks.len(),
        }
    }

    /// Recomputes the rows for the query (`keys`: the library's search
    /// keys): artists by name, albums by title or artist, tracks as in the
    /// library. "All tracks" always stays. The selection is not moved.
    fn refilter(&mut self, catalog: &Catalog, keys: &[String]) {
        let len = self.len(catalog);
        let tracks = &self.tracks;
        let album_matches = |b: usize, q: &str| {
            catalog
                .album(b)
                .is_some_and(|al| al.search_key().contains(q))
        };
        match self.kind {
            BrowseKind::Artists => self.list.refilter(len, |a, q| {
                catalog
                    .artist(a)
                    .is_some_and(|artist| artist.search_key().contains(q))
            }),
            BrowseKind::ArtistAlbums(a) => {
                let albums = catalog.artist(a).map_or(&[][..], |artist| artist.albums());
                self.list.refilter(len, |value, q| {
                    artist_album_of_value(value)
                        .is_none_or(|i| albums.get(i).is_some_and(|&b| album_matches(b, q)))
                });
            }
            BrowseKind::Albums => self.list.refilter(len, album_matches),
            BrowseKind::Album(_) | BrowseKind::ArtistTracks(_) => {
                self.list.refilter(len, |i, q| {
                    tracks
                        .get(i)
                        .and_then(|&t| keys.get(t))
                        .is_some_and(|k| k.contains(q))
                });
            }
        }
    }

    /// The level opened by `Enter` on the selected row, if it opens one.
    fn child(&self, catalog: &Catalog) -> Option<BrowseKind> {
        let value = self.list.selected_value()?;
        match self.kind {
            BrowseKind::Artists => catalog
                .artist(value)
                .map(|_| BrowseKind::ArtistAlbums(value)),
            BrowseKind::ArtistAlbums(a) => match artist_album_of_value(value) {
                None => Some(BrowseKind::ArtistTracks(a)),
                Some(i) => catalog
                    .artist(a)?
                    .albums()
                    .get(i)
                    .map(|&b| BrowseKind::Album(b)),
            },
            BrowseKind::Albums => catalog.album(value).map(|_| BrowseKind::Album(value)),
            BrowseKind::Album(_) | BrowseKind::ArtistTracks(_) => None,
        }
    }
}

/// Everything the UI shows and the user can change.
#[derive(Debug)]
pub struct App<A: Audio> {
    playback: Playback<A>,
    store: Box<dyn PlaylistStore>,
    /// Built lazily, in practice by the playlist listing in [`App::new`].
    resolver: OnceCell<Resolver>,
    /// Library root, ready for display.
    root: String,
    /// Per track, lower-cased "title artist album" for case-insensitive search.
    search_keys: Vec<String>,
    focus: Focus,
    /// Rows are library indices.
    library: ListState,
    /// The playlists as last listed (and updated by the app's own edits),
    /// sorted by name ignoring case.
    playlists: Vec<PlaylistInfo>,
    /// The sidebar: values as in [`SidebarEntry`] (the fixed rows, then
    /// `playlists[i]` at `i + FIXED_ROWS`).
    playlist_list: ListState,
    /// The playlist shown in the main pane, if any. Never set together with
    /// `browse`; with neither, the library is shown. Only
    /// [`App::show_library`], [`App::show_playlist`] and [`App::show_browse`]
    /// set the two.
    open: Option<OpenPlaylist>,
    /// The artists and albums, grouped once from the tracks.
    catalog: Catalog,
    /// The browse stack shown in the main pane, if any.
    browse: Option<BrowseRoot>,
    /// What "Artists" shows: its bottom level is always the artist list, and
    /// it is kept while something else is shown, so `Enter` on "Artists"
    /// comes back to the same place.
    artists_nav: Vec<BrowseLevel>,
    /// What "Albums" shows, like `artists_nav`: the album list at the bottom.
    albums_nav: Vec<BrowseLevel>,
    mode: Mode,
    /// Transient message and when it expires.
    status: Option<(Notice, Instant)>,
    /// Message raised by a key press, stamped and shown on the next tick.
    pending: Option<Notice>,
    /// Whether `pending` was raised by the key press being handled (or by
    /// startup), so a new notice is combined with it rather than replacing it.
    pending_fresh: bool,
    /// Rows the list showed in the last frame (`PageUp`/`PageDown` step).
    page_size: usize,
    /// Help lines the overlay showed when last drawn (at least 1); bounds
    /// `Mode::Help`'s scroll and is its `PageUp`/`PageDown` step. Kept
    /// between openings.
    help_rows: usize,
    visualizer: Visualizer,
    lyrics: LyricsPane,
    quit: bool,
    /// The saved session: where it is read from and written to.
    saved: SavedState,
}

/// How often, at most, [`App::tick`] saves the session while it changes.
pub const SAVE_INTERVAL: Duration = Duration::from_secs(30);

/// Saving the session (D4–D6 of the `persist-session-state` design).
#[derive(Debug)]
struct SavedState {
    /// [`NoState`] until [`App::restore`] installs the real store.
    store: Box<dyn StateStore>,
    /// The library root the tracks were scanned from (set by
    /// [`App::restore`]); resume points are relative to it.
    root: Option<PathBuf>,
    /// Whether anything is ever saved this run: false with a store that
    /// persists nothing and after a newer state file was seen.
    enabled: bool,
    /// The resume point loaded at start, kept while no track has been
    /// current this run, so a track that could not be resumed (a drive not
    /// mounted) is saved again rather than forgotten.
    pending: Option<Resume>,
    /// The session last saved, or as restored; a periodic save happens only
    /// when the session differs from it.
    last: Option<Session>,
    /// When the next periodic check is due; set by the first tick.
    next_check: Option<Instant>,
    /// A save failed and was reported; later failures stay quiet until a
    /// save succeeds.
    failing: bool,
    /// The session taken by [`App::shutdown`], written by [`App::save_final`].
    last_words: Option<Session>,
    /// Sessions built, so tests can confirm none is built when nothing is
    /// saved.
    #[cfg(test)]
    built: std::cell::Cell<usize>,
}

impl SavedState {
    fn none() -> Self {
        Self {
            store: Box::new(NoState),
            root: None,
            enabled: false,
            pending: None,
            last: None,
            next_check: None,
            failing: false,
            last_words: None,
            #[cfg(test)]
            built: std::cell::Cell::new(0),
        }
    }
}

/// How long the tap may go without new audio while playing before the
/// visualizer treats it as stalled and lets the bars fall. The tap fills in
/// bursts of one device callback, so this leaves room for large buffers.
const STALE_AFTER: Duration = Duration::from_millis(250);

/// The spectrum visualizer's state. Only [`App::tick`] measures, and only
/// while the strip is shown, so a hidden strip costs no FFT.
#[derive(Debug)]
struct Visualizer {
    /// Whether the user wants it (`v`); on at start unless restored off.
    on: bool,
    spectrum: Spectrum,
    /// Whether the last frame was tall enough for the strip (fed back by the
    /// event loop, recorded even while `on` is false).
    fits: bool,
    /// Bars the last frame drew (0 when the strip was not drawn).
    bars: usize,
    /// Reused for [`Playback::recent_samples`], so ticks do not allocate.
    samples: Vec<f32>,
    /// The tap's last `(generation, written)` and when it was first seen, to
    /// notice a tap that stopped filling while the state is still `Playing`.
    /// The generation tells a new load's tap from the old one even when
    /// their counts happen to match.
    last_written: Option<((u64, u64), Instant)>,
}

impl Visualizer {
    fn new() -> Self {
        Self {
            on: true,
            spectrum: Spectrum::new(),
            fits: false,
            bars: 0,
            samples: Vec::with_capacity(FFT_SIZE),
            last_written: None,
        }
    }

    /// Whether the strip is drawn, so the spectrum must be kept up to date.
    const fn shown(&self) -> bool {
        self.on && self.fits && self.bars > 0
    }
}

/// Shortest wait [`App::lyrics_deadline`] asks for, so a position just short
/// of a line's time can never make the event loop spin.
pub const LYRICS_MIN_WAIT: Duration = Duration::from_millis(10);

/// The lyrics pane's state. Lyrics are read only by [`App::tick`], only while
/// the pane is shown, and once per current track path: seeking, pausing,
/// stopping and replaying the same file reuse the cache; only another path,
/// or showing the pane again, reads afresh.
#[derive(Debug)]
struct LyricsPane {
    /// Whether the user wants it (`l`); on at start unless restored off.
    on: bool,
    /// Whether the last frame was wide enough for the pane (fed back by the
    /// event loop, recorded even while `on` is false).
    fits: bool,
    /// The track path `lyrics` were read for; `None` when nothing was read
    /// since the pane was last shown.
    loaded_for: Option<PathBuf>,
    /// The lyrics of `loaded_for`; `None` when it has none.
    lyrics: Option<Lyrics>,
    /// Counts lyrics loads, so the UI can tell when `lyrics` was replaced.
    generation: u64,
}

impl LyricsPane {
    const fn new() -> Self {
        Self {
            on: true,
            fits: false,
            loaded_for: None,
            lyrics: None,
            generation: 0,
        }
    }

    /// Whether the pane is drawn, so lyrics must be kept for the current track.
    const fn shown(&self) -> bool {
        self.on && self.fits
    }

    /// Drops the cache while the pane is not shown, so showing it again
    /// reads the lyrics afresh (picking up a `.lrc` edited meanwhile).
    fn forget_if_hidden(&mut self) {
        if !self.shown() {
            self.loaded_for = None;
            self.lyrics = None;
        }
    }
}

/// The lyrics pane's accessors, read by the UI and the event loop.
impl<A: Audio> App<A> {
    /// Records whether the last frame was wide enough for the lyrics pane,
    /// like [`App::set_spectrum_layout`]. While it does not fit, the cached
    /// lyrics are dropped.
    pub fn set_lyrics_layout(&mut self, fits: bool) {
        self.lyrics.fits = fits;
        self.lyrics.forget_if_hidden();
    }

    /// Whether the lyrics pane is on (`l`), whether or not it fits.
    #[must_use]
    pub const fn lyrics_on(&self) -> bool {
        self.lyrics.on
    }

    /// Whether the lyrics pane is on and the last frame was wide enough.
    #[must_use]
    pub const fn lyrics_shown(&self) -> bool {
        self.lyrics.shown()
    }

    /// Whether the lyrics of the current track have been looked up: false
    /// with no current track, and until the first tick after the track
    /// changed or the pane was shown. Until then the pane shows nothing, so
    /// "No lyrics" never flashes before lyrics that are about to load.
    #[must_use]
    pub fn lyrics_ready(&self) -> bool {
        match (&self.lyrics.loaded_for, self.playback.current_track()) {
            (Some(loaded), Some(track)) => *loaded == track.path,
            _ => false,
        }
    }

    /// The current track's lyrics, once looked up ([`App::lyrics_ready`]);
    /// `None` when it has none or they are not looked up yet.
    #[must_use]
    pub fn lyrics(&self) -> Option<&Lyrics> {
        if self.lyrics_ready() {
            self.lyrics.lyrics.as_ref()
        } else {
            None
        }
    }

    /// Changes each time lyrics are read, so the UI can keep their wrapped
    /// rows until the next read.
    #[must_use]
    pub const fn lyrics_generation(&self) -> u64 {
        self.lyrics.generation
    }

    /// When the lyrics pane needs the next frame, as seen at `now`. While the
    /// pane is shown and the current track's lyrics are not looked up yet
    /// (the track just changed, or the pane was just shown or widened), that
    /// is `now`, so the next tick reads them instead of leaving the pane
    /// blank for a whole frame; the read marks them looked up, so this
    /// happens once per track. Otherwise it is the time until the next
    /// synced line starts, at least [`LYRICS_MIN_WAIT`], and `None` unless a
    /// track is playing and its lyrics are synced with a line still to come.
    #[must_use]
    pub fn lyrics_deadline(&self, now: Instant) -> Option<Instant> {
        if !self.lyrics_shown() {
            return None;
        }
        if self.playback.current_track().is_some() && !self.lyrics_ready() {
            return Some(now);
        }
        if self.playback.state() != PlayState::Playing {
            return None;
        }
        let position = self.playback.position();
        let next = self.lyrics()?.next_change(position)?;
        let wait = next.saturating_sub(position).max(LYRICS_MIN_WAIT);
        now.checked_add(wait)
    }

    /// Sets whether the lyrics pane is on, without a notice (test fixtures).
    #[cfg(test)]
    pub fn set_lyrics_on(&mut self, on: bool) {
        self.lyrics.on = on;
        self.lyrics.forget_if_hidden();
    }
}

impl<A: Audio> App<A> {
    /// The app over `tracks` (already sorted), playing through `audio`, with
    /// playlists kept in `store`. `root` is the library folder as shown to
    /// the user; `notice`, if any, is shown in the status area on the first
    /// frame, joined with a playlist listing error if there is one. The
    /// playlists are listed here, so the sidebar is filled on the first frame.
    pub fn new(
        tracks: Vec<Track>,
        audio: A,
        store: Box<dyn PlaylistStore>,
        root: String,
        notice: Option<Notice>,
    ) -> Self {
        let search_keys = tracks
            .iter()
            .map(|t| format!("{}\n{}\n{}", t.title, t.artist, t.album).to_lowercase())
            .collect();
        let library = ListState::all(tracks.len());
        let catalog = Catalog::build(&tracks);
        let artists_nav = vec![BrowseLevel::new(BrowseKind::Artists, &catalog)];
        let albums_nav = vec![BrowseLevel::new(BrowseKind::Albums, &catalog)];
        let mut app = Self {
            playback: Playback::new(tracks, audio),
            store,
            resolver: OnceCell::new(),
            root,
            search_keys,
            focus: Focus::Main,
            library,
            playlists: Vec::new(),
            playlist_list: ListState::default(),
            open: None,
            catalog,
            browse: None,
            artists_nav,
            albums_nav,
            mode: Mode::Normal,
            status: None,
            pending: notice,
            pending_fresh: true,
            page_size: 1,
            help_rows: 1,
            visualizer: Visualizer::new(),
            lyrics: LyricsPane::new(),
            quit: false,
            saved: SavedState::none(),
        };
        app.refresh_playlists(None);
        app
    }

    #[must_use]
    pub const fn playback(&self) -> &Playback<A> {
        &self.playback
    }

    #[cfg(test)]
    pub const fn playback_mut(&mut self) -> &mut Playback<A> {
        &mut self.playback
    }

    /// The library folder, for display.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Which list keys act on: the focused pane, and what it shows.
    #[must_use]
    pub fn view(&self) -> View {
        match (self.focus, self.main_view()) {
            (Focus::Sidebar, _) => View::Playlists,
            (Focus::Main, MainView::Library) => View::Library,
            (Focus::Main, MainView::Playlist) => View::Playlist,
            (Focus::Main, MainView::Browse(kind)) if kind.is_tracks() => View::Tracks,
            (Focus::Main, MainView::Browse(_)) => View::Groups,
        }
    }

    /// The pane with keyboard focus.
    #[must_use]
    pub const fn focus(&self) -> Focus {
        self.focus
    }

    /// What the main pane shows, whichever pane has focus.
    #[must_use]
    pub fn main_view(&self) -> MainView {
        debug_assert!(
            self.open.is_none() || self.browse.is_none(),
            "a playlist and a browse level are never both shown"
        );
        if self.open.is_some() {
            MainView::Playlist
        } else if let Some(level) = self.browse_level() {
            MainView::Browse(level.kind)
        } else {
            MainView::Library
        }
    }

    /// The artists and albums of the library, for the browse levels.
    #[must_use]
    pub const fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// How many levels deep the shown browse stack is: 1 on the artist or
    /// album list, 0 when nothing is browsed.
    #[must_use]
    pub fn browse_depth(&self) -> usize {
        self.browse_nav().map_or(0, Vec::len)
    }

    /// The tracks (library indices, in album or artist order) of the shown
    /// browse level when it lists tracks; empty otherwise. Its rows
    /// ([`App::main_list`]) are positions in it.
    #[must_use]
    pub fn browse_tracks(&self) -> &[usize] {
        self.browse_level().map_or(&[], |level| &level.tracks)
    }

    /// The browse stack shown in the main pane, if any.
    fn browse_nav(&self) -> Option<&Vec<BrowseLevel>> {
        match self.browse? {
            BrowseRoot::Artists => Some(&self.artists_nav),
            BrowseRoot::Albums => Some(&self.albums_nav),
        }
    }

    /// The browse level shown in the main pane, if any: the top of its stack.
    fn browse_level(&self) -> Option<&BrowseLevel> {
        self.browse_nav()?.last()
    }

    /// The sidebar list: values as in [`SidebarEntry`].
    #[must_use]
    pub fn sidebar_list(&self) -> ListView<'_> {
        self.playlist_list.view()
    }

    /// The main pane's list: library indices, positions in
    /// [`OpenPlaylist::items`] when a playlist is shown, or a browse level's
    /// rows ([`BrowseKind`]).
    #[must_use]
    pub fn main_list(&self) -> ListView<'_> {
        self.main_list_state().view()
    }

    /// The list state of what the main pane shows.
    fn main_list_state(&self) -> &ListState {
        if let Some(open) = &self.open {
            &open.list
        } else if let Some(level) = self.browse_level() {
            &level.list
        } else {
            &self.library
        }
    }

    /// The listed rows of the focused list: library indices in the library,
    /// sidebar values in the sidebar (see [`App::sidebar_list`]), positions
    /// in [`OpenPlaylist::items`] in an open playlist, and a browse level's
    /// rows when browsing (see [`BrowseKind`]).
    #[must_use]
    #[cfg_attr(not(test), expect(dead_code, reason = "used by key-handling tests"))]
    pub fn visible(&self) -> &[usize] {
        &self.list().rows
    }

    /// Row of the selection in [`App::visible`], if anything is listed.
    #[must_use]
    #[cfg_attr(not(test), expect(dead_code, reason = "used by key-handling tests"))]
    pub fn selected(&self) -> Option<usize> {
        self.list().selected_row()
    }

    /// Library index of the selected track, in the library, an open
    /// playlist or a browsed track list.
    #[must_use]
    pub fn selected_track(&self) -> Option<usize> {
        match self.view() {
            View::Library => self.library.selected_value(),
            View::Playlists | View::Groups => None,
            View::Playlist => self
                .open
                .as_ref()
                .and_then(|o| o.items.get(o.list.selected_value()?).copied()),
            View::Tracks => self
                .browse_level()
                .and_then(|level| level.tracks.get(level.list.selected_value()?).copied()),
        }
    }

    /// The search query of the focused list (empty when no filter is active).
    #[must_use]
    pub fn query(&self) -> &str {
        &self.list().query
    }

    /// The playlists as last listed, sorted by name ignoring case.
    #[must_use]
    pub fn playlists(&self) -> &[PlaylistInfo] {
        &self.playlists
    }

    /// The playlist selected in the sidebar; `None` on a fixed row.
    #[must_use]
    pub fn selected_playlist(&self) -> Option<&PlaylistInfo> {
        let value = self.playlist_list.selected_value()?;
        match SidebarEntry::of_value(value) {
            SidebarEntry::Playlist(i) => self.playlists.get(i),
            SidebarEntry::Library | SidebarEntry::Artists | SidebarEntry::Albums => None,
        }
    }

    /// The sidebar value of the entry that is the play order (see
    /// [`SidebarEntry`]), while a track is loaded. `None` before anything
    /// was played, and while a deleted playlist plays on (no entry is its
    /// order).
    #[must_use]
    pub fn sidebar_playing(&self) -> Option<usize> {
        self.playback.current_track()?;
        match self.playback.source() {
            // Artist and album sources mark "Library" until task 5.1 of
            // add-artist-album-browsing marks "Artists" and "Albums".
            Source::Library | Source::Artist(_) | Source::Album(_) => SidebarEntry::Library.value(),
            Source::Playlist(name) => self.sidebar_value_of(name),
            Source::Detached => None,
        }
    }

    /// The sidebar value of playlist `name`, if it is listed.
    fn sidebar_value_of(&self, name: &str) -> Option<usize> {
        let index = self.playlists.iter().position(|info| info.name == name)?;
        SidebarEntry::Playlist(index).value()
    }

    /// The playlist shown in the main pane, if any.
    #[must_use]
    pub const fn open_playlist(&self) -> Option<&OpenPlaylist> {
        self.open.as_ref()
    }

    #[must_use]
    pub const fn mode(&self) -> &Mode {
        &self.mode
    }

    #[must_use]
    pub const fn should_quit(&self) -> bool {
        self.quit
    }

    /// The message for the status area at `now`: the latest transient one
    /// while it lasts, else why playback stopped on its own, if it did.
    #[must_use]
    pub fn status(&self, now: Instant) -> Option<&Notice> {
        self.status
            .as_ref()
            .filter(|(_, until)| now < *until)
            .map(|(notice, _)| notice)
            .or_else(|| self.playback.stop_reason())
    }

    /// Records how many list rows the last frame showed.
    pub fn set_page_size(&mut self, rows: usize) {
        self.page_size = rows.max(1);
    }

    /// Records how many help lines the overlay showed when last drawn, and
    /// pulls an open overlay's scroll back within the new bound, so a taller
    /// overlay never shows past the last line.
    pub fn set_help_rows(&mut self, rows: usize) {
        self.help_rows = rows.max(1);
        let max = self.max_help_scroll();
        if let Mode::Help { scroll } = &mut self.mode {
            *scroll = (*scroll).min(max);
        }
    }

    /// Records whether the last frame was tall enough for the spectrum strip
    /// and how many bars it drew (0 if none), like [`App::set_page_size`].
    pub const fn set_spectrum_layout(&mut self, fits: bool, bars: usize) {
        self.visualizer.fits = fits;
        self.visualizer.bars = bars;
    }

    /// Whether the visualizer is on (`v`), whether or not the strip fits.
    #[must_use]
    pub const fn visualizer_on(&self) -> bool {
        self.visualizer.on
    }

    /// The spectrum bar levels, lowest band first, each in `0.0..=1.0`. Its
    /// length is the bar count of the last update and may differ from the
    /// bars that fit now (right after a resize).
    #[must_use]
    pub fn spectrum_levels(&self) -> &[f32] {
        self.visualizer.spectrum.levels()
    }

    /// Whether the strip is shown and its bars move: a track is playing or a
    /// bar is still falling. The event loop redraws faster while it is.
    #[must_use]
    pub fn visualizer_animating(&self) -> bool {
        let vis = &self.visualizer;
        vis.shown() && (self.playback.state() == PlayState::Playing || !vis.spectrum.is_idle())
    }

    /// The largest help scroll: the last line is then at the bottom.
    fn max_help_scroll(&self) -> usize {
        keys::help_line_count().saturating_sub(self.help_rows)
    }

    /// Raises `notice` for the status area. Within one key press, an error
    /// is never hidden: a later info notice is dropped, and an error raised
    /// after an info notice is shown first with the info after it. Notices
    /// of the same level are joined with `; `. A notice from an earlier key
    /// press that was not shown yet is replaced.
    pub fn notify(&mut self, notice: Notice) {
        let merged = match self.pending.take() {
            Some(old) if self.pending_fresh => match (old.level, notice.level) {
                (Level::Error, Level::Info) => old,
                (Level::Info, Level::Error) => {
                    Notice::error(format!("{}; {}", notice.text, old.text))
                }
                (level, _) => Notice {
                    text: format!("{}; {}", old.text, notice.text),
                    level,
                },
            },
            _ => notice,
        };
        self.pending = Some(merged);
        self.pending_fresh = true;
    }

    /// Periodic housekeeping: saves the session when due (see
    /// [`SAVE_INTERVAL`]), drives playback (auto-advance, skipping broken
    /// tracks) and collects status messages.
    pub fn tick(&mut self, now: Instant) {
        self.settle_pending();
        let save_failed = self.autosave(now);
        let mut fresh = self.pending.take();
        self.playback.tick();
        // Playback messages win: they are usually errors.
        if let Some(notice) = self.playback.take_notice() {
            fresh = Some(notice);
        }
        // A failed save is reported only once per run of failures, so it is
        // joined with the other message rather than replaced by it.
        if let Some(failure) = save_failed {
            fresh = Some(match fresh {
                Some(other) => join_error(&other, &failure),
                None => failure,
            });
        }
        if let Some(notice) = fresh {
            self.status = Some((notice, now + STATUS_TTL));
        }
        if self.status.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.status = None;
        }
        self.update_visualizer(now);
        self.update_lyrics();
    }

    /// Reads the current track's lyrics when the pane is shown and the
    /// current track's path is not the one last read. Keyed on the path, so
    /// seeking (a reload of the same file) or pausing never reads again.
    fn update_lyrics(&mut self) {
        let pane = &mut self.lyrics;
        if !pane.shown() {
            return;
        }
        let Some(track) = self.playback.current_track() else {
            return;
        };
        if pane.loaded_for.as_ref() == Some(&track.path) {
            return;
        }
        pane.lyrics = lyrics::source::load(&track.path);
        pane.loaded_for = Some(track.path.clone());
        pane.generation = pane.generation.wrapping_add(1);
    }

    /// Moves the spectrum on to `now`, after playback has been driven. While
    /// the strip is not shown nothing is measured and the levels are reset,
    /// so it never comes back showing a stale frame. Samples are passed only
    /// while playing with fresh audio; otherwise the bars fall.
    fn update_visualizer(&mut self, now: Instant) {
        let vis = &mut self.visualizer;
        if !vis.shown() {
            vis.spectrum.reset();
            vis.last_written = None;
            return;
        }
        let playing = self.playback.state() == PlayState::Playing;
        let recent = if playing {
            self.playback.recent_samples(&mut vis.samples)
        } else {
            None
        };
        let fresh = match (recent, vis.last_written) {
            (None, _) => {
                // While playing, `None` means nothing is decoded yet (a new
                // load): start over once audio arrives. While paused or
                // stopped the record is kept, so on resume the pre-pause
                // window counts as stale until new audio advances the tap.
                if playing {
                    vis.last_written = None;
                }
                false
            }
            (Some(recent), Some((seen, since))) if seen == (recent.generation, recent.written) => {
                now.saturating_duration_since(since) < STALE_AFTER
            }
            (Some(recent), _) => {
                vis.last_written = Some(((recent.generation, recent.written), now));
                true
            }
        };
        let input = recent
            .filter(|_| fresh)
            .map(|recent| (vis.samples.as_slice(), recent.sample_rate));
        vis.spectrum.update(input, vis.bars, now);
    }

    /// Stops audio, before quitting. The session to save is taken first
    /// (stopping resets the position) and kept for [`App::save_final`];
    /// nothing is written here, so a slow disk cannot hold the screen.
    pub fn shutdown(&mut self) {
        if self.saved.enabled {
            self.settle_pending();
            self.saved.last_words = Some(self.session());
        }
        self.playback.stop();
    }

    /// Applies one key press.
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'))
        {
            self.quit = true;
            return;
        }
        // Notices of this key press are combined only with each other.
        self.pending_fresh = false;
        match &self.mode {
            Mode::Normal => self.normal_key(key),
            Mode::Search => self.search_key(key),
            Mode::Picker(_) => self.picker_key(key),
            Mode::NameInput(_) => self.name_key(key),
            Mode::ConfirmDelete(_) => self.confirm_delete_key(key),
            Mode::Help { .. } => self.help_key(key),
        }
    }

    fn normal_key(&mut self, key: KeyEvent) {
        if self.navigation_key(key.code) {
            return;
        }
        // Letters with Ctrl or Alt are not ours (Shift is: `G`).
        if let KeyCode::Char(_) = key.code
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char('g') => self.list_mut().select_row(0),
            KeyCode::Char('G') => self.list_mut().select_row(usize::MAX),
            KeyCode::Enter => self.activate(),
            KeyCode::Tab | KeyCode::BackTab => match self.focus {
                Focus::Main => self.focus_sidebar(None),
                Focus::Sidebar => self.focus_main(),
            },
            KeyCode::Char('a') if self.view() == View::Library => {
                let tracks = self.selected_track().into_iter().collect();
                self.open_picker(tracks);
            }
            KeyCode::Char('A') if self.view() == View::Library => {
                let tracks = self.library.rows.clone();
                self.open_picker(tracks);
            }
            KeyCode::Char('d') if self.view() == View::Playlist => self.remove_selected(),
            KeyCode::Char('J') if self.view() == View::Playlist => self.move_selected(true),
            KeyCode::Char('K') if self.view() == View::Playlist => self.move_selected(false),
            KeyCode::Char('R') if self.view() == View::Playlists => self.start_rename(),
            KeyCode::Char('D') if self.view() == View::Playlists => self.start_delete(),
            KeyCode::Char(' ') => self.playback.toggle_pause(),
            KeyCode::Char('s') => self.playback.stop(),
            KeyCode::Char('n') => self.playback.next(),
            KeyCode::Char('p') => self.playback.prev(),
            KeyCode::Left => self.playback.seek(false),
            KeyCode::Right => self.playback.seek(true),
            KeyCode::Char('+' | '=') => self.playback.volume_up(),
            KeyCode::Char('-') => self.playback.volume_down(),
            KeyCode::Char('z') => {
                let on = self.playback.toggle_shuffle();
                self.notify(Notice::info(if on { "Shuffle on" } else { "Shuffle off" }));
            }
            KeyCode::Char('r') => {
                let repeat = self.playback.cycle_repeat();
                self.notify(Notice::info(format!("Repeat {}", repeat.label())));
            }
            KeyCode::Char('v') => self.toggle_visualizer(),
            KeyCode::Char('l') => self.toggle_lyrics(),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('?') => self.mode = Mode::Help { scroll: 0 },
            KeyCode::Esc => self.escape(),
            // Search and the name prompt take Backspace before this.
            KeyCode::Backspace if self.focus == Focus::Main => {
                self.browse_back();
            }
            _ => {}
        }
    }

    /// `Esc` in Normal mode: clears the focused list's filter; else, in the
    /// main pane, goes back one browse level; else, from a shown playlist or
    /// the top browse level, focuses the sidebar with that entry selected
    /// (the main pane keeps showing it).
    fn escape(&mut self) {
        if !self.query().is_empty() {
            self.clear_filter();
            return;
        }
        if self.focus != Focus::Main || self.browse_back() {
            return;
        }
        let pick = match (&self.open, self.browse) {
            (Some(open), _) => SidebarPick::Playlist(open.name().to_owned()),
            (None, Some(root)) => root.into(),
            (None, None) => return,
        };
        self.focus_sidebar(Some(pick));
    }

    /// Goes back one browse level, dropping the shown one with its filter
    /// and selection; the parent level was never touched, so it shows the
    /// entry that was opened, selected. Returns whether a level was left:
    /// the bottom (artist or album) list is never left this way.
    fn browse_back(&mut self) -> bool {
        let nav = match self.browse {
            Some(BrowseRoot::Artists) => &mut self.artists_nav,
            Some(BrowseRoot::Albums) => &mut self.albums_nav,
            None => return false,
        };
        if nav.len() <= 1 {
            return false;
        }
        nav.pop();
        true
    }

    /// `Enter` on an artist, album or "All tracks" row: opens the selected
    /// entry as a new level on top of the shown one.
    fn browse_into(&mut self) {
        let catalog = &self.catalog;
        let nav = match self.browse {
            Some(BrowseRoot::Artists) => &mut self.artists_nav,
            Some(BrowseRoot::Albums) => &mut self.albums_nav,
            None => return,
        };
        let Some(kind) = nav.last().and_then(|level| level.child(catalog)) else {
            return;
        };
        nav.push(BrowseLevel::new(kind, catalog));
    }

    /// `v`: turns the visualizer off or on. When the terminal is too short
    /// for the strip the change cannot be seen, so it is announced.
    fn toggle_visualizer(&mut self) {
        let vis = &mut self.visualizer;
        vis.on = !vis.on;
        if !vis.fits {
            self.notify(Notice::info(if self.visualizer.on {
                format!("Visualizer on: needs a terminal at least {SPECTRUM_MIN_HEIGHT} rows tall")
            } else {
                "Visualizer off".to_owned()
            }));
        }
    }

    /// `l`: turns the lyrics pane off or on. When the terminal is too narrow
    /// for the pane the change cannot be seen, so it is announced.
    fn toggle_lyrics(&mut self) {
        let pane = &mut self.lyrics;
        pane.on = !pane.on;
        pane.forget_if_hidden();
        if !pane.fits {
            self.notify(Notice::info(if self.lyrics.on {
                format!("Lyrics on: needs a terminal at least {LYRICS_MIN_WIDTH} columns wide")
            } else {
                "Lyrics off".to_owned()
            }));
        }
    }

    fn search_key(&mut self, key: KeyEvent) {
        if self.navigation_key(key.code) {
            return;
        }
        match key.code {
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Esc => {
                self.clear_filter();
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                if self.list_mut().query.pop().is_some() {
                    self.apply_filter();
                }
            }
            KeyCode::Char(c)
                if accepts_char(&key, c) && self.query().chars().count() < MAX_QUERY_CHARS =>
            {
                self.list_mut().query.push(c);
                self.apply_filter();
            }
            _ => {}
        }
    }

    fn picker_key(&mut self, key: KeyEvent) {
        let Mode::Picker(picker) = &mut self.mode else {
            return;
        };
        let last = picker.choices.len();
        let page = self.page_size;
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => picker.selected = (picker.selected + 1).min(last),
            KeyCode::PageUp => picker.selected = picker.selected.saturating_sub(page),
            KeyCode::PageDown => picker.selected = picker.selected.saturating_add(page).min(last),
            KeyCode::Home | KeyCode::Char('g') => picker.selected = 0,
            KeyCode::End | KeyCode::Char('G') => picker.selected = last,
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => self.pick(),
            _ => {}
        }
    }

    fn name_key(&mut self, key: KeyEvent) {
        let Mode::NameInput(input) = &mut self.mode else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.mode = Mode::Normal,
            KeyCode::Enter => self.submit_name(),
            KeyCode::Backspace => {
                if input.text.pop().is_some() {
                    input.error = None;
                }
            }
            KeyCode::Char(c)
                if accepts_char(&key, c) && input.text.chars().count() < MAX_QUERY_CHARS =>
            {
                input.text.push(c);
                input.error = None;
            }
            _ => {}
        }
    }

    /// A key in the delete confirmation: `y` deletes, anything else cancels.
    fn confirm_delete_key(&mut self, key: KeyEvent) {
        let Mode::ConfirmDelete(name) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let yes = key.code == KeyCode::Char('y')
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        if yes {
            self.delete_playlist(&name);
        } else {
            self.notify(Notice::info(format!("{} was not deleted", quoted(&name))));
        }
    }

    /// A key in the help overlay: scroll keys move it, stopping at the ends;
    /// any other key (`?`, `Esc` and `q` included) closes it and is consumed.
    /// Ctrl+C was already handled by [`App::handle_key`].
    fn help_key(&mut self, key: KeyEvent) {
        let Mode::Help { scroll } = self.mode else {
            return;
        };
        let max = self.max_help_scroll();
        let current = scroll.min(max);
        // Letters with Ctrl or Alt are not scroll keys (Shift is: `G`).
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let moved = match key.code {
            KeyCode::Up => current.saturating_sub(1),
            KeyCode::Down => current.saturating_add(1),
            KeyCode::PageUp => current.saturating_sub(self.help_rows),
            KeyCode::PageDown => current.saturating_add(self.help_rows),
            KeyCode::Home => 0,
            KeyCode::End => max,
            KeyCode::Char('k') if plain => current.saturating_sub(1),
            KeyCode::Char('j') if plain => current.saturating_add(1),
            KeyCode::Char('g') if plain => 0,
            KeyCode::Char('G') if plain => max,
            _ => {
                self.mode = Mode::Normal;
                return;
            }
        };
        self.mode = Mode::Help {
            scroll: moved.min(max),
        };
    }

    /// Selection keys shared by the list modes; `true` when `code` was one.
    fn navigation_key(&mut self, code: KeyCode) -> bool {
        let page = isize::try_from(self.page_size).unwrap_or(isize::MAX);
        match code {
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-page),
            KeyCode::PageDown => self.move_selection(page),
            KeyCode::Home => self.list_mut().select_row(0),
            KeyCode::End => self.list_mut().select_row(usize::MAX),
            _ => return false,
        }
        true
    }

    /// Moves the selection by `delta` rows, stopping at the ends.
    fn move_selection(&mut self, delta: isize) {
        let list = self.list_mut();
        list.select_row(list.selected.saturating_add_signed(delta));
    }

    /// The list state of the focused list.
    fn list(&self) -> &ListState {
        match self.focus {
            Focus::Sidebar => &self.playlist_list,
            Focus::Main => self.main_list_state(),
        }
    }

    fn list_mut(&mut self) -> &mut ListState {
        if self.focus == Focus::Sidebar {
            return &mut self.playlist_list;
        }
        if let Some(open) = &mut self.open {
            return &mut open.list;
        }
        let nav = match self.browse {
            Some(BrowseRoot::Artists) => &mut self.artists_nav,
            Some(BrowseRoot::Albums) => &mut self.albums_nav,
            None => return &mut self.library,
        };
        match nav.last_mut() {
            Some(level) => &mut level.list,
            // A stack is never empty; the library is a harmless fallback.
            None => &mut self.library,
        }
    }

    /// Recomputes the focused list for its query and selects the first
    /// match. With a query, rows that always stay listed are skipped when
    /// anything else matches: the sidebar's fixed rows (the first matching playlist is
    /// selected) and an artist's "All tracks" row (the first matching album).
    fn apply_filter(&mut self) {
        self.refilter_current();
        let always = match self.view() {
            View::Playlists => FIXED_ROWS,
            View::Groups
                if matches!(
                    self.browse_level().map(|level| level.kind),
                    Some(BrowseKind::ArtistAlbums(_))
                ) =>
            {
                ALL_TRACKS_VALUE + 1
            }
            View::Library | View::Playlist | View::Groups | View::Tracks => 0,
        };
        // An emptied query is no filter: the top row, not the first match.
        let skip = if self.query().is_empty() { 0 } else { always };
        let list = self.list_mut();
        let first = list.rows.iter().position(|&v| v >= skip).unwrap_or(0);
        list.select_row(first);
    }

    /// Recomputes the rows of the focused list; the selection is not moved.
    fn refilter_current(&mut self) {
        if self.focus == Focus::Sidebar {
            self.refilter_sidebar();
            return;
        }
        let keys = &self.search_keys;
        if let Some(open) = &mut self.open {
            let items = &open.items;
            open.list.refilter(items.len(), |i, q| {
                keys.get(items[i]).is_some_and(|k| k.contains(q))
            });
            return;
        }
        let catalog = &self.catalog;
        let level = match self.browse {
            Some(BrowseRoot::Artists) => self.artists_nav.last_mut(),
            Some(BrowseRoot::Albums) => self.albums_nav.last_mut(),
            None => {
                self.library
                    .refilter(keys.len(), |i, q| keys[i].contains(q));
                return;
            }
        };
        if let Some(level) = level {
            level.refilter(catalog, keys);
        }
    }

    /// Recomputes the sidebar rows for its query; the fixed rows always
    /// stay. The selection is not moved.
    fn refilter_sidebar(&mut self) {
        let infos = &self.playlists;
        self.playlist_list
            .refilter(
                infos.len() + FIXED_ROWS,
                |value, q| match SidebarEntry::of_value(value) {
                    SidebarEntry::Playlist(i) => infos
                        .get(i)
                        .is_none_or(|info| info.name.to_lowercase().contains(q)),
                    SidebarEntry::Library | SidebarEntry::Artists | SidebarEntry::Albums => true,
                },
            );
    }

    /// Lists the whole focused list again, keeping the selected entry selected.
    fn clear_filter(&mut self) {
        let len = match self.focus {
            Focus::Sidebar => self.playlists.len() + FIXED_ROWS,
            Focus::Main => {
                if let Some(open) = &self.open {
                    open.items.len()
                } else if let Some(level) = self.browse_level() {
                    level.len(&self.catalog)
                } else {
                    self.search_keys.len()
                }
            }
        };
        self.list_mut().clear_filter(len);
    }

    fn resolver(&self) -> &Resolver {
        self.resolver
            .get_or_init(|| Resolver::new(self.playback.tracks()))
    }

    /// `Enter` in normal mode: play a track, or show the sidebar entry.
    fn activate(&mut self) {
        match self.view() {
            View::Library => {
                if let Some(track) = self.library.selected_value() {
                    self.playback.play(track);
                }
            }
            View::Playlists => self.show_selected(),
            View::Groups => self.browse_into(),
            // Playing a browsed track comes with task 5.1 of
            // add-artist-album-browsing.
            View::Tracks => {}
            View::Playlist => {
                let Some(open) = &self.open else {
                    return;
                };
                if let Some(item) = open.list.selected_value() {
                    let source = Source::Playlist(open.name().to_owned());
                    if !self.playback.play_source(source, open.items.clone(), item) {
                        self.notify(Notice::error(format!(
                            "Cannot play {}: its tracks no longer match the library",
                            quoted(open.name())
                        )));
                    }
                }
            }
        }
    }

    /// `Enter` in the sidebar: "Library" shows the library, "Artists" and
    /// "Albums" their browse stack as it was left; a playlist is opened (or,
    /// if already shown, reloaded only if its file changed, so it keeps its
    /// filter and selection). The main pane gets the focus, unless the
    /// playlist could not be read: then focus and the main pane stay as they
    /// were.
    fn show_selected(&mut self) {
        let Some(value) = self.playlist_list.selected_value() else {
            return;
        };
        let index = match SidebarEntry::of_value(value) {
            SidebarEntry::Library => {
                self.show_library();
                self.focus = Focus::Main;
                return;
            }
            SidebarEntry::Artists => {
                self.show_browse(BrowseRoot::Artists);
                return;
            }
            SidebarEntry::Albums => {
                self.show_browse(BrowseRoot::Albums);
                return;
            }
            SidebarEntry::Playlist(index) => index,
        };
        let Some(info) = self.playlists.get(index) else {
            return;
        };
        let name = info.name.clone();
        let shown = |app: &Self| app.open.as_ref().is_some_and(|o| o.name() == name);
        if shown(self) {
            self.check_open();
        } else {
            self.open_named(&name);
        }
        if shown(self) {
            self.focus = Focus::Main;
        }
    }

    /// Shows the library in the main pane, closing a shown playlist or
    /// browse stack (the stacks themselves are kept). Focus is not moved.
    fn show_library(&mut self) {
        self.open = None;
        self.browse = None;
    }

    /// Shows `open` in the main pane in place of what it showed. Focus is
    /// not moved.
    fn show_playlist(&mut self, open: OpenPlaylist) {
        self.browse = None;
        self.open = Some(open);
    }

    /// Shows the browse stack of `root` as it was left and focuses the main
    /// pane, closing a shown playlist.
    fn show_browse(&mut self, root: BrowseRoot) {
        self.open = None;
        self.browse = Some(root);
        self.focus = Focus::Main;
    }

    /// Moves focus to the sidebar and lists the playlists again, keeping
    /// the sidebar selection unless `select` is listed (see
    /// [`Self::refresh_playlists`]).
    fn focus_sidebar(&mut self, select: Option<SidebarPick>) {
        self.focus = Focus::Sidebar;
        self.refresh_playlists(select);
    }

    /// Moves focus to the main pane. A shown playlist is reloaded if
    /// another program changed its file; if the file is gone, the main pane
    /// shows the library instead.
    fn focus_main(&mut self) {
        self.focus = Focus::Main;
        self.check_open();
    }

    /// The sidebar entry selected now.
    fn sidebar_pick(&self) -> Option<SidebarPick> {
        match SidebarEntry::of_value(self.playlist_list.selected_value()?) {
            SidebarEntry::Library => Some(SidebarPick::Library),
            SidebarEntry::Artists => Some(SidebarPick::Artists),
            SidebarEntry::Albums => Some(SidebarPick::Albums),
            SidebarEntry::Playlist(_) => self
                .selected_playlist()
                .map(|info| SidebarPick::Playlist(info.name.clone())),
        }
    }

    /// The sidebar value of `pick`, if it is listed (fixed rows always are).
    fn pick_value(&self, pick: &SidebarPick) -> Option<usize> {
        match pick {
            SidebarPick::Library => SidebarEntry::Library.value(),
            SidebarPick::Artists => SidebarEntry::Artists.value(),
            SidebarPick::Albums => SidebarEntry::Albums.value(),
            SidebarPick::Playlist(name) => self.sidebar_value_of(name),
        }
    }

    /// Selects `pick` in the sidebar if it is listed, else keeps the
    /// selected row (clamped).
    fn restore_sidebar(&mut self, pick: Option<SidebarPick>) {
        let value = pick.and_then(|pick| self.pick_value(&pick));
        self.playlist_list.select_value(value);
    }

    /// Lists the playlists again, keeping the sidebar filter and the
    /// selected entry, or selecting entry `select` if given and listed.
    /// When the sidebar filter hides `select`, the filter is cleared, so
    /// the selection never lands on another entry instead.
    fn refresh_playlists(&mut self, select: Option<SidebarPick>) {
        let keep = self.sidebar_pick();
        match self.store.list(self.resolver()) {
            Ok(infos) => self.playlists = infos,
            Err(err) => {
                self.playlists.clear();
                self.notify(list_error(&err));
            }
        }
        self.refilter_sidebar();
        let listed = select.filter(|pick| self.pick_value(pick).is_some());
        if let Some(value) = listed.as_ref().and_then(|pick| self.pick_value(pick))
            && !self.playlist_list.rows.contains(&value)
        {
            self.playlist_list.query.clear();
            self.refilter_sidebar();
        }
        self.restore_sidebar(listed.or(keep));
    }

    /// Writes the sidebar entry of `playlist`, just saved or read by the
    /// app, in place (inserting it in sorted order if it is new), without
    /// listing the folder again. The sidebar keeps its selected entry.
    fn update_info(&mut self, playlist: &Playlist) {
        let info = PlaylistInfo {
            name: playlist.name().to_owned(),
            tracks: Some(playlist.tracks().len()),
            read_only: playlist.editable().err(),
        };
        let keep = self.sidebar_pick();
        if let Some(slot) = self.playlists.iter_mut().find(|i| i.name == info.name) {
            *slot = info;
        } else {
            // The same order as the store's listing.
            let key = |name: &str| (name.to_lowercase(), name.to_owned());
            let new = key(&info.name);
            let at = self.playlists.partition_point(|i| key(&i.name) < new);
            self.playlists.insert(at, info);
        }
        self.refilter_sidebar();
        self.restore_sidebar(keep);
    }

    /// Reloads the open playlist if another program changed its file, or
    /// closes it (the main pane shows the library) if the file is gone.
    fn check_open(&mut self) {
        let Some(open) = &self.open else {
            return;
        };
        match self.store.changed(&open.playlist) {
            Ok(false) => {}
            Ok(true) => {
                let name = open.name().to_owned();
                if self.reload(&name) {
                    self.notify(Notice::info(format!(
                        "{} was changed by another program and has been reloaded",
                        quoted(&name)
                    )));
                }
            }
            Err(StoreError::NotFound) => {
                let name = open.name().to_owned();
                self.playlist_gone(&name);
            }
            Err(err) => {
                let name = open.name().to_owned();
                self.notify(store_error(&name, &err));
            }
        }
    }

    /// Opens playlist `name` in the main pane. On failure the error is
    /// reported and the open playlist, if any, stays.
    fn open_named(&mut self, name: &str) {
        let playlist = match self.store.load(name, self.resolver()) {
            Ok(playlist) => playlist,
            Err(StoreError::NotFound) => {
                self.playlist_gone(name);
                return;
            }
            Err(err) => {
                self.notify(store_error(name, &err));
                return;
            }
        };
        self.update_info(&playlist);
        let unmatched = playlist.unmatched();
        let read_only = playlist.editable().err();
        let items = playlist.tracks();
        self.follow_reload(name, &items);
        self.show_playlist(OpenPlaylist {
            list: ListState::all(items.len()),
            playlist,
            items,
        });
        let mut parts = Vec::new();
        if unmatched > 0 {
            parts.push(format!(
                "{} not found in the library",
                count(unmatched, "entry", "entries")
            ));
        }
        if let Some(reason) = read_only {
            parts.push(format!("read-only: {reason}"));
        }
        if !parts.is_empty() {
            self.notify(Notice::info(format!(
                "{}: {}",
                quoted(name),
                parts.join("; ")
            )));
        }
    }

    /// Reads playlist `name` again after a change on disk. Returns `false`
    /// (and reports why) when it could not be read.
    fn reload(&mut self, name: &str) -> bool {
        match self.store.load(name, self.resolver()) {
            Ok(playlist) => {
                self.install(playlist);
                true
            }
            Err(StoreError::NotFound) => {
                self.playlist_gone(name);
                false
            }
            Err(err) => {
                self.notify(store_error(name, &err));
                false
            }
        }
    }

    /// The file of playlist `name` disappeared: if it was shown, the main
    /// pane shows the library; if it was playing, playback goes on detached
    /// from it.
    fn playlist_gone(&mut self, name: &str) {
        if self.open.as_ref().is_some_and(|o| o.name() == name) {
            self.show_library();
        }
        self.playback.playlist_deleted(name);
        self.refresh_playlists(None);
        self.notify(Notice::error(format!("{} no longer exists", quoted(name))));
    }

    /// Takes `playlist` as the latest version of its file: it updates its
    /// sidebar entry, replaces the open playlist if that one has the same
    /// name (keeping its filter and selection), and the play order if it is
    /// the playlist playing and the play order does not match it.
    fn install(&mut self, playlist: Playlist) {
        self.update_info(&playlist);
        if self
            .open
            .as_ref()
            .is_some_and(|o| o.name() == playlist.name())
        {
            self.replace_open(playlist);
        } else {
            let items = playlist.tracks();
            self.follow_reload(playlist.name(), &items);
        }
    }

    /// Shows `playlist` in place of the open playlist, whatever its name was
    /// (a rename changes it), keeping the filter and the selected entry, and
    /// makes the play order follow it if it is playing.
    fn replace_open(&mut self, playlist: Playlist) {
        let items = playlist.tracks();
        self.follow_reload(playlist.name(), &items);
        let keys = &self.search_keys;
        let Some(open) = &mut self.open else {
            return;
        };
        let keep = open.list.selected_value();
        open.playlist = playlist;
        open.items = items;
        let items = &open.items;
        open.list.refilter(items.len(), |i, q| {
            keys.get(items[i]).is_some_and(|k| k.contains(q))
        });
        open.list.select_value(keep);
    }

    /// Makes the play order of playlist `name`, if it is playing, hold `items`.
    fn follow_reload(&mut self, name: &str, items: &[usize]) {
        if self.playback.is_playing_playlist(name) && self.playback.items() != items {
            self.playback
                .edit_playlist(name, Edit::Replace(items.to_vec()));
        }
    }

    /// Applies `edit`, which turned the list of playlist `name` from `before`
    /// into `after`, to the play order if that playlist is playing. If the
    /// play order did not hold `before` (the file changed since it started),
    /// it is replaced by `after` instead.
    fn follow_edit(&mut self, name: &str, before: &[usize], after: &[usize], edit: Edit) {
        if !self.playback.is_playing_playlist(name) {
            return;
        }
        if self.playback.items() == before {
            self.playback.edit_playlist(name, edit);
        } else {
            self.follow_reload(name, after);
        }
    }

    /// `a` / `A`: asks which playlist to add `tracks` to.
    fn open_picker(&mut self, tracks: Vec<usize>) {
        if tracks.is_empty() {
            self.notify(Notice::info("No track to add"));
            return;
        }
        let choices = match self.store.list(self.resolver()) {
            Ok(infos) => infos,
            Err(err) => {
                self.notify(list_error(&err));
                Vec::new()
            }
        };
        self.mode = Mode::Picker(Picker {
            tracks,
            choices,
            selected: 0,
        });
    }

    /// `Enter` in the picker.
    fn pick(&mut self) {
        let Mode::Picker(picker) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let Some(choice) = picker.selected.checked_sub(1) else {
            self.mode = Mode::NameInput(NameInput {
                purpose: NamePurpose::Create(picker.tracks),
                text: String::new(),
                error: None,
            });
            return;
        };
        let Some(info) = picker.choices.get(choice) else {
            self.mode = Mode::Picker(picker);
            return;
        };
        if let Some(reason) = info.read_only {
            let notice = Notice::error(format!("Cannot add to {}: {reason}", quoted(&info.name)));
            self.notify(notice);
            self.mode = Mode::Picker(picker);
            return;
        }
        let name = info.name.clone();
        self.add_to(&name, &picker.tracks);
    }

    /// Adds library `tracks` to existing playlist `name` and saves it.
    fn add_to(&mut self, name: &str, tracks: &[usize]) {
        // Unreachable from the keys since the focus model (`a`/`A` need the
        // library shown, so nothing is open), kept as a safeguard.
        // The open copy is what the user last saw: a change made on disk
        // since then is detected on save instead of silently merged.
        let open_copy = self
            .open
            .as_ref()
            .filter(|o| o.name() == name)
            .map(|o| o.playlist.clone());
        let mut playlist = match open_copy {
            Some(playlist) => playlist,
            None => match self.store.load(name, self.resolver()) {
                Ok(playlist) => playlist,
                Err(StoreError::NotFound) => {
                    self.playlist_gone(name);
                    return;
                }
                Err(err) => {
                    self.notify(store_error(name, &err));
                    return;
                }
            },
        };
        let before = playlist.tracks();
        let outcome = match playlist.add(tracks, self.playback.tracks()) {
            Ok(outcome) => outcome,
            Err(err) => {
                self.notify(Notice::error(format!(
                    "Cannot add to {}: {err}",
                    quoted(name)
                )));
                return;
            }
        };
        if outcome.added == 0 {
            self.notify(add_notice(name, outcome));
            return;
        }
        match self.store.save(&mut playlist) {
            Ok(SaveOutcome::Saved) => {
                let after = playlist.tracks();
                let appended = after.get(before.len()..).unwrap_or_default().to_vec();
                self.follow_edit(name, &before, &after, Edit::Append(appended));
                if self.open.as_ref().is_some_and(|o| o.name() == name) {
                    self.install(playlist);
                } else {
                    self.update_info(&playlist);
                }
                self.notify(add_notice(name, outcome));
            }
            Ok(SaveOutcome::ChangedOnDisk) => {
                if self.reload(name) {
                    self.notify(Notice::error(format!(
                        "{} was changed by another program and has been reloaded; nothing was added",
                        quoted(name)
                    )));
                }
            }
            Err(StoreError::NotFound) => self.playlist_gone(name),
            Err(err @ StoreError::ReadOnly(_)) => {
                // Typically a link repointed outside `Playlists/` since the
                // playlist was read: reload so it shows as read-only.
                if self.reload(name) {
                    self.notify(store_error(name, &err));
                }
            }
            Err(err) => self.notify(store_error(name, &err)),
        }
    }

    /// `Enter` in the name prompt.
    fn submit_name(&mut self) {
        let Mode::NameInput(mut input) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let (result, failed) = match &input.purpose {
            NamePurpose::Create(tracks) => (
                self.create_playlist(&input.text, tracks),
                "Playlist not created",
            ),
            NamePurpose::Rename(old) => (
                self.rename_playlist(old, &input.text),
                "Playlist not renamed",
            ),
        };
        if let Err(reason) = result {
            self.notify(Notice::error(format!("{failed}: {reason}")));
            input.error = Some(reason);
            self.mode = Mode::NameInput(input);
        }
    }

    /// Creates playlist `name` holding library `tracks`. A refused name is
    /// returned as `Err` with the reason, so the prompt can stay open; other
    /// failures are reported and close the prompt.
    fn create_playlist(&mut self, name: &str, tracks: &[usize]) -> Result<(), String> {
        let mut playlist = match self.store.blank(name) {
            Ok(playlist) => playlist,
            Err(StoreError::Name(err)) => return Err(err.to_string()),
            Err(err) => {
                self.notify(store_error(name.trim(), &err));
                return Ok(());
            }
        };
        let name = playlist.name().to_owned();
        let outcome = match playlist.add(tracks, self.playback.tracks()) {
            Ok(outcome) => outcome,
            Err(err) => {
                self.notify(Notice::error(format!(
                    "Playlist {} not created: {err}",
                    quoted(&name)
                )));
                return Ok(());
            }
        };
        if outcome.added == 0 {
            // Only possible when no track's path can be stored: an empty
            // playlist is not what was asked for.
            let mut notice = add_notice(&name, outcome);
            notice.text = format!("Playlist {} not created. {}", quoted(&name), notice.text);
            self.notify(notice);
            return Ok(());
        }
        match self.store.create(&mut playlist) {
            Ok(()) => {
                self.update_info(&playlist);
                let mut notice = add_notice(&name, outcome);
                notice.text = format!("Created {}. {}", quoted(&name), notice.text);
                self.notify(notice);
                Ok(())
            }
            Err(StoreError::Name(err)) => Err(err.to_string()),
            Err(err) => {
                self.notify(store_error(&name, &err));
                Ok(())
            }
        }
    }
}

/// The saved session: restoring it at start and saving it while running and
/// at quit (D4–D6 of the `persist-session-state` design).
///
/// The state file is untrusted: a resume point only ever selects a track the
/// scan found (by its path relative to the root) and playlists the sidebar
/// listing found (by exact name), and loads its track paused. Nothing is
/// saved with a store that persists nothing ([`NoState`], so the tests that
/// build an `App` directly never touch a real file) or after a newer state
/// file was seen; then no session is even built.
impl<A: Audio> App<A> {
    /// Installs `store` and continues the session it holds for the library
    /// scanned from `root` (the canonical root): first the settings, then
    /// the playlist that was shown (focused, if it is still listed), then the
    /// resume point, loaded paused (see [`Playback::resume`]). The play
    /// source is the saved playlist if it is listed and still holds the
    /// track, else the library. A resume point that cannot be resumed is
    /// kept and saved again until a track becomes current. A newer state file
    /// gives one notice and turns saving off for the run. Reopening the
    /// shown playlist gives the same notices as opening it by hand (entries
    /// not found, read-only). Called once, after [`App::new`] and before the
    /// first tick, with the exact root the tracks were scanned from: a
    /// differently spelled root matches no track, so nothing is resumed.
    pub fn restore(&mut self, store: Box<dyn StateStore>, root: &Path) {
        let mut saved = SavedState::none();
        saved.store = store;
        saved.root = Some(root.to_path_buf());
        self.saved = saved;
        let loaded = self.saved.store.load();
        self.saved.enabled = self.saved.store.persists() && !loaded.newer;
        if loaded.newer {
            self.notify(Notice::info(
                "Saved state is from a newer kasset; it is not changed this run",
            ));
        }
        if let Some(settings) = loaded.settings {
            self.apply_settings(settings);
        }
        let FolderEntry { resume, open } = loaded.folder;
        if let Some(name) = open {
            self.restore_open(&name);
        }
        if let Some(resume) = resume
            && !self.resume_point(&resume)
        {
            self.saved.pending = Some(resume);
        }
        if self.saved.enabled {
            self.saved.last = Some(self.session());
        }
    }

    /// Writes the session [`App::shutdown`] took. Does nothing (and returns
    /// `None`) when `shutdown` did not run or nothing is saved this run.
    /// Returns the error message, naming the state file, when the save failed.
    pub fn save_final(&mut self) -> Option<String> {
        let session = self.saved.last_words.take()?;
        self.saved
            .store
            .save(&session, self.saved.pending.as_ref())
            .err()
    }

    /// Applies saved settings, without notices.
    fn apply_settings(&mut self, settings: Settings) {
        self.playback.set_volume(settings.volume);
        self.playback.set_shuffle(settings.shuffle);
        self.playback.set_repeat(settings.repeat);
        self.visualizer.on = settings.visualizer;
        self.lyrics.on = settings.lyrics;
        self.lyrics.forget_if_hidden();
    }

    /// Shows saved playlist `name` in the main pane, focused and selected in
    /// the sidebar, if the listing has it.
    fn restore_open(&mut self, name: &str) {
        if self.sidebar_value_of(name).is_none() {
            return;
        }
        self.open_named(name);
        if self.open.as_ref().is_some_and(|o| o.name() == name) {
            self.focus = Focus::Main;
            self.restore_sidebar(Some(SidebarPick::Playlist(name.to_owned())));
        }
    }

    /// Loads the track of `resume` paused, if the scan found it. Returns
    /// whether a track is now current.
    fn resume_point(&mut self, resume: &Resume) -> bool {
        let Some(root) = self.saved.root.as_deref() else {
            return false;
        };
        let Some(track) = self
            .playback
            .tracks()
            .iter()
            .position(|t| relative(root, &t.path).as_ref() == Some(&resume.track))
        else {
            return false;
        };
        let (source, items, item) = self.resume_source(resume, track).unwrap_or_else(|| {
            let all = (0..self.playback.tracks().len()).collect();
            (Source::Library, all, track)
        });
        self.playback.resume(source, items, item, resume.position)
    }

    /// The saved source playlist of `resume` as the play order for library
    /// track `track`: its items and the item to start at (the saved one if
    /// it still is that track, else the first that is). `None` when the
    /// source is the library, or the playlist is not listed, cannot be read
    /// or no longer holds the track.
    fn resume_source(&self, resume: &Resume, track: usize) -> Option<(Source, Vec<usize>, usize)> {
        let SavedSource::Playlist(name) = &resume.source else {
            return None;
        };
        self.sidebar_value_of(name)?;
        let items = match &self.open {
            Some(open) if open.name() == name => open.items.clone(),
            // Read only for its items; the main pane is not changed.
            _ => self.store.load(name, self.resolver()).ok()?.tracks(),
        };
        let item = resume
            .item
            .filter(|&i| items.get(i) == Some(&track))
            .or_else(|| items.iter().position(|&t| t == track))?;
        Some((Source::Playlist(name.clone()), items, item))
    }

    /// The session as it would be saved now. A detached play order is saved
    /// as the library; the position is in whole milliseconds.
    fn session(&self) -> Session {
        #[cfg(test)]
        self.saved.built.set(self.saved.built.get() + 1);
        let playback = &self.playback;
        let settings = Settings {
            volume: playback.volume(),
            shuffle: playback.shuffle(),
            repeat: playback.repeat(),
            visualizer: self.visualizer.on,
            lyrics: self.lyrics.on,
        };
        let resume = self.saved.root.as_deref().and_then(|root| {
            let index = playback.current()?;
            let track = relative(root, &playback.tracks().get(index)?.path)?;
            let (source, item) = match playback.source() {
                Source::Playlist(name) => {
                    (SavedSource::Playlist(name.clone()), playback.current_item())
                }
                // The library's items are its indices. Artist and album
                // sources are saved as the library until task 5.3 of
                // add-artist-album-browsing saves them by name.
                Source::Library | Source::Detached | Source::Artist(_) | Source::Album(_) => {
                    (SavedSource::Library, Some(index))
                }
            };
            Some(Resume {
                track,
                item,
                position: whole_millis(playback.position()),
                source,
            })
        });
        Session {
            settings,
            folder: FolderEntry {
                resume,
                open: self.open.as_ref().map(|o| o.name().to_owned()),
            },
        }
    }

    /// Drops the loaded resume point once a track is current: from then on
    /// the session's own resume point replaces it.
    fn settle_pending(&mut self) {
        if self.playback.current().is_some() {
            self.saved.pending = None;
        }
    }

    /// The periodic save: at most every [`SAVE_INTERVAL`], and only when the
    /// session changed since it was last saved (or restored). Returns the
    /// notice for the first failure; later ones give none, until a save
    /// succeeds.
    fn autosave(&mut self, now: Instant) -> Option<Notice> {
        if !self.saved.enabled {
            return None;
        }
        match self.saved.next_check {
            None => {
                self.saved.next_check = now.checked_add(SAVE_INTERVAL);
                return None;
            }
            Some(due) if now < due => return None,
            Some(_) => self.saved.next_check = now.checked_add(SAVE_INTERVAL),
        }
        let session = self.session();
        if self.saved.last.as_ref() == Some(&session) {
            return None;
        }
        match self.saved.store.save(&session, self.saved.pending.as_ref()) {
            Ok(Saved::Written) => {
                self.saved.last = Some(session);
                self.saved.failing = false;
                None
            }
            // A newer file appeared during the run. Deliberately quiet (the
            // spec announces a newer file only at start): it is kept, and
            // nothing more is saved, not even at quit.
            Ok(Saved::Skipped) => {
                self.saved.enabled = false;
                None
            }
            Err(message) => {
                (!std::mem::replace(&mut self.saved.failing, true)).then(|| save_error(&message))
            }
        }
    }
}

/// `path` relative to `root`, as saved in a resume point.
fn relative(root: &Path, path: &Path) -> Option<RelPath> {
    RelPath::new(path.strip_prefix(root).ok()?)
}

/// `d` cut to whole milliseconds and at most [`MAX_POSITION`], so a position
/// read back from the state file compares equal.
fn whole_millis(d: Duration) -> Duration {
    u64::try_from(d.min(MAX_POSITION).as_millis()).map_or(MAX_POSITION, Duration::from_millis)
}

/// `failure` (an error) shown with `other`: an error comes first, so it is
/// never cut off by an info message.
fn join_error(other: &Notice, failure: &Notice) -> Notice {
    match other.level {
        Level::Error => Notice::error(format!("{}; {}", other.text, failure.text)),
        Level::Info => Notice::error(format!("{}; {}", failure.text, other.text)),
    }
}

/// The status message for a failed save; `message` names the state file.
fn save_error(message: &str) -> Notice {
    let text = sanitize(message).unwrap_or_else(|| "cannot save state".to_owned());
    let mut chars = text.chars();
    let text: String = chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    Notice::error(text)
}

/// Editing the open playlist: `d`, `J` and `K`.
impl<A: Audio> App<A> {
    /// `d`: removes the selected entry (only that occurrence).
    fn remove_selected(&mut self) {
        let Some(item) = self.open.as_ref().and_then(|o| o.list.selected_value()) else {
            return;
        };
        self.edit_open(EntryEdit::Remove(item));
    }

    /// `J` / `K`: moves the selected entry one position down / up. Refused
    /// with a filter active, since neighbours on screen need not be
    /// neighbours in the playlist.
    fn move_selected(&mut self, down: bool) {
        let Some(open) = &self.open else {
            return;
        };
        if !open.list.query.is_empty() {
            self.notify(Notice::info("Clear the filter (Esc) to reorder"));
            return;
        }
        let Some(from) = open.list.selected_value() else {
            return;
        };
        self.edit_open(EntryEdit::Move { from, down });
    }

    /// Applies `edit` to the open playlist and saves it at once. The edit is
    /// made on a copy: if it is refused or the save fails, the playlist on
    /// screen (and on disk) stays as it was. A playlist changed on disk by
    /// another program is reloaded instead and the edit discarded.
    fn edit_open(&mut self, edit: EntryEdit) {
        let Some(open) = &self.open else {
            return;
        };
        let name = open.name().to_owned();
        let before = open.items.clone();
        let mut playlist = open.playlist.clone();
        let applied = match edit {
            EntryEdit::Remove(item) => playlist.remove(item),
            EntryEdit::Move { from, down } => playlist.move_entry(from, down),
        };
        match applied {
            Ok(true) => {}
            // Past either end, or nothing selected: nothing to do.
            Ok(false) => return,
            Err(err) => {
                self.notify(Notice::error(format!(
                    "Cannot edit {}: {err}",
                    quoted(&name)
                )));
                return;
            }
        }
        match self.store.save(&mut playlist) {
            Ok(SaveOutcome::Saved) => {
                let after = playlist.tracks();
                let play_edit = match edit {
                    EntryEdit::Remove(item) => Edit::Remove(item),
                    EntryEdit::Move { from, down } => Edit::Move {
                        from,
                        to: moved_to(from, down),
                    },
                };
                self.follow_edit(&name, &before, &after, play_edit);
                self.install(playlist);
                self.select_after(edit);
                if let EntryEdit::Remove(item) = edit
                    && let Some(&track) = before.get(item)
                {
                    let title = self
                        .playback
                        .tracks()
                        .get(track)
                        .map_or("", |t| t.title.as_str());
                    self.notify(Notice::info(format!(
                        "Removed {} from {}",
                        quoted(title),
                        quoted(&name)
                    )));
                }
            }
            Ok(SaveOutcome::ChangedOnDisk) => {
                if self.reload(&name) {
                    self.notify(Notice::error(format!(
                        "{} was changed by another program and has been reloaded; the edit was not applied",
                        quoted(&name)
                    )));
                }
            }
            Err(StoreError::NotFound) => self.playlist_gone(&name),
            Err(err @ StoreError::ReadOnly(_)) => {
                // Typically a link repointed outside `Playlists/` since the
                // playlist was read: reload so it shows as read-only.
                if self.reload(&name) {
                    self.notify(store_error(&name, &err));
                }
            }
            Err(err) => self.notify(store_error(&name, &err)),
        }
    }

    /// The selection after a saved `edit` of the open playlist: the entry
    /// that followed a removed one (or the new last entry), or the moved
    /// entry at its new place.
    fn select_after(&mut self, edit: EntryEdit) {
        let Some(open) = &mut self.open else {
            return;
        };
        let list = &mut open.list;
        match edit {
            EntryEdit::Remove(item) => {
                // Items after the removed one moved up by one position, so
                // its successor is the first listed position `>= item`.
                let row = list
                    .rows
                    .iter()
                    .position(|&r| r >= item)
                    .unwrap_or(usize::MAX);
                list.select_row(row);
            }
            EntryEdit::Move { from, down } => list.select_value(Some(moved_to(from, down))),
        }
    }
}

/// Renaming and deleting playlists: `R` and `D` in the sidebar.
impl<A: Audio> App<A> {
    /// `R`: asks for a new name for the playlist selected in the sidebar,
    /// prefilled with the current one. Nothing on the "Library" row.
    fn start_rename(&mut self) {
        let Some(name) = self.selected_playlist().map(|info| info.name.clone()) else {
            return;
        };
        // Names made outside kasset may be longer than the input allows;
        // they are still shown whole and can be shortened. Listed names have
        // no control or invisible characters, but may hold other whitespace
        // (no-break or line separator), which is never typed or drawn raw:
        // it is prefilled as a plain space, so `Enter` renames to a valid name.
        let text = name
            .chars()
            .map(|c| if c.is_whitespace() { ' ' } else { c })
            .collect();
        self.mode = Mode::NameInput(NameInput {
            text,
            purpose: NamePurpose::Rename(name),
            error: None,
        });
    }

    /// Renames playlist `old` to `text` (trimmed). A refused name is
    /// returned as `Err` with the reason, so the prompt can stay open; other
    /// failures are reported and close the prompt. Playback of the playlist
    /// goes on under its new name. If the renamed playlist is shown, it is
    /// read again from its new file (so its path and stamp come from disk)
    /// and stays shown under the new name; if that read fails, the main pane
    /// shows the library and the error is reported.
    fn rename_playlist(&mut self, old: &str, text: &str) -> Result<(), String> {
        let new = validate_name(text)
            .map_err(|err| err.to_string())?
            .to_owned();
        if new == old {
            return Ok(());
        }
        match self.store.rename(old, &new) {
            Ok(()) => {
                self.playback.playlist_renamed(old, &new);
                // Raised first: an error reading the file back is joined
                // with it rather than hiding that the rename happened.
                self.notify(Notice::info(format!(
                    "Renamed {} to {}",
                    quoted(old),
                    quoted(&new)
                )));
                if self.open.as_ref().is_some_and(|o| o.name() == old) {
                    // The open copy must never keep the old path: a later
                    // save would recreate the old file.
                    match self.store.load(&new, self.resolver()) {
                        Ok(playlist) => self.replace_open(playlist),
                        Err(err) => {
                            self.show_library();
                            self.notify(store_error(&new, &err));
                        }
                    }
                }
                self.refresh_playlists(Some(SidebarPick::Playlist(new)));
                Ok(())
            }
            Err(StoreError::Name(err)) => Err(err.to_string()),
            Err(StoreError::NotFound) => {
                self.playlist_gone(old);
                Ok(())
            }
            Err(err) => {
                self.notify(store_error(old, &err));
                Ok(())
            }
        }
    }

    /// `D`: asks whether to delete the playlist selected in the sidebar.
    /// Nothing on the "Library" row.
    fn start_delete(&mut self) {
        if let Some(name) = self.selected_playlist().map(|info| info.name.clone()) {
            self.mode = Mode::ConfirmDelete(name);
        }
    }

    /// Deletes the file of playlist `name` (a symlink itself, never its
    /// target; never an audio file). If it was playing, playback goes on;
    /// if it was shown, the main pane shows the library.
    fn delete_playlist(&mut self, name: &str) {
        match self.store.delete(name) {
            Ok(()) => {
                self.playback.playlist_deleted(name);
                if self.open.as_ref().is_some_and(|o| o.name() == name) {
                    self.show_library();
                }
                self.refresh_playlists(None);
                self.notify(Notice::info(format!(
                    "Deleted playlist {}; its songs stay in the library",
                    quoted(name)
                )));
            }
            Err(StoreError::NotFound) => self.playlist_gone(name),
            Err(err) => self.notify(store_error(name, &err)),
        }
    }
}

/// An edit of the open playlist's entries, by item position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryEdit {
    Remove(usize),
    /// Swap with the next (`down`) or previous entry.
    Move {
        from: usize,
        down: bool,
    },
}

/// Where an entry at `from` lands after moving one step down or up.
const fn moved_to(from: usize, down: bool) -> usize {
    if down {
        from.saturating_add(1)
    } else {
        from.saturating_sub(1)
    }
}

/// Whether `c`, typed with `key`'s modifiers, goes into a text input. Only
/// visible characters are accepted: inputs are drawn on screen.
fn accepts_char(key: &KeyEvent, c: char) -> bool {
    !key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        && !c.is_control()
        && !is_invisible_format(c)
}

/// A playlist name for messages: sanitised and in quotes.
fn quoted(name: &str) -> String {
    format!("\"{}\"", sanitize(name).unwrap_or_default())
}

/// `1 entry`, `2 entries`.
fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// The status message for a failed operation on playlist `name`.
fn store_error(name: &str, err: &StoreError) -> Notice {
    // Error texts come from the OS; sanitised all the same.
    let text = format!("Playlist {}: {err}", quoted(name));
    Notice::error(sanitize(&text).unwrap_or(text))
}

/// The status message for a failed listing of the playlists folder.
fn list_error(err: &StoreError) -> Notice {
    let text = format!("Cannot list playlists: {err}");
    Notice::error(sanitize(&text).unwrap_or(text))
}

/// What an addition to playlist `name` did, for the status area.
fn add_notice(name: &str, outcome: AddOutcome) -> Notice {
    let mut parts = vec![if outcome.added > 0 {
        format!(
            "Added {} to {}",
            count(outcome.added, "track", "tracks"),
            quoted(name)
        )
    } else {
        format!("Nothing added to {}", quoted(name))
    }];
    if outcome.skipped > 0 {
        parts.push(format!(
            "{} already there",
            count(outcome.skipped, "track", "tracks")
        ));
    }
    if outcome.unrepresentable > 0 {
        parts.push(format!(
            "{} skipped: the file path cannot be stored in a playlist",
            count(outcome.unrepresentable, "track", "tracks")
        ));
        return Notice::error(parts.join("; "));
    }
    Notice::info(parts.join("; "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::PlayState;
    use crate::queue::Repeat;
    use crate::test_support::{SilentAudio, no_store, track};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn press(app: &mut App<SilentAudio>, keys: &[KeyEvent]) {
        for &k in keys {
            app.handle_key(k);
        }
    }

    fn typed(app: &mut App<SilentAudio>, text: &str) {
        for c in text.chars() {
            app.handle_key(ch(c));
        }
    }

    fn app(n: usize) -> App<SilentAudio> {
        let tracks = (0..n)
            .map(|i| track(&format!("Song {i}"), "Band", "Album"))
            .collect();
        App::new(
            tracks,
            SilentAudio::default(),
            no_store(),
            "/music".to_owned(),
            None,
        )
    }

    fn music() -> App<SilentAudio> {
        let tracks = vec![
            track("Beat It", "Michael Jackson", "Thriller"),
            track("Help!", "The Beatles", "Help!"),
            track("Heartbeats", "The Knife", "Deep Cuts"),
            track("Hey Jude", "The Fab Four", "Past Masters"),
            track("Dreams", "Fleetwood Mac", "Rumours"),
            track("Song", "Band", "UPBEAT DISCO"),
        ];
        App::new(
            tracks,
            SilentAudio::default(),
            no_store(),
            "/music".to_owned(),
            None,
        )
    }

    #[test]
    fn j_and_k_move_the_selection_within_bounds() {
        let mut a = app(3);
        assert_eq!(a.selected(), Some(0));
        press(&mut a, &[ch('j')]);
        assert_eq!(a.selected(), Some(1), "track 2 is selected");
        press(&mut a, &[ch('j'), ch('j'), ch('j')]);
        assert_eq!(a.selected(), Some(2), "stops at the last track");
        press(&mut a, &[ch('k'), key(KeyCode::Up), key(KeyCode::Up)]);
        assert_eq!(a.selected(), Some(0), "stops at the first track");
        press(&mut a, &[key(KeyCode::Down)]);
        assert_eq!(a.selected(), Some(1));
    }

    #[test]
    fn home_end_g_and_shift_g_jump() {
        let mut a = app(10);
        a.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        assert_eq!(a.selected(), Some(9));
        press(&mut a, &[ch('g')]);
        assert_eq!(a.selected(), Some(0));
        press(&mut a, &[key(KeyCode::End)]);
        assert_eq!(a.selected(), Some(9));
        press(&mut a, &[key(KeyCode::Home)]);
        assert_eq!(a.selected(), Some(0));
    }

    #[test]
    fn page_keys_move_by_the_visible_height() {
        let mut a = app(50);
        a.set_page_size(20);
        press(&mut a, &[key(KeyCode::PageDown)]);
        assert_eq!(a.selected(), Some(20));
        press(&mut a, &[key(KeyCode::PageDown), key(KeyCode::PageDown)]);
        assert_eq!(a.selected(), Some(49), "clamped to the end");
        press(&mut a, &[key(KeyCode::PageUp)]);
        assert_eq!(a.selected(), Some(29));
        press(&mut a, &[key(KeyCode::PageUp), key(KeyCode::PageUp)]);
        assert_eq!(a.selected(), Some(0), "clamped to the start");
    }

    #[test]
    fn navigation_on_an_empty_library_is_harmless() {
        let mut a = app(0);
        press(
            &mut a,
            &[
                ch('j'),
                ch('G'),
                key(KeyCode::PageDown),
                key(KeyCode::Enter),
                ch(' '),
                ch('n'),
                ch('p'),
                key(KeyCode::Right),
            ],
        );
        assert_eq!(a.selected(), None);
        assert_eq!(a.selected_track(), None);
        assert_eq!(a.playback().state(), PlayState::Stopped);
    }

    #[test]
    fn selection_is_independent_of_the_playing_track() {
        let mut a = app(6);
        press(&mut a, &[ch('j'), key(KeyCode::Enter)]);
        assert_eq!(a.playback().current(), Some(1));
        press(&mut a, &[ch('j'), ch('j'), ch('j')]);
        assert_eq!(a.selected_track(), Some(4));
        assert_eq!(a.playback().current(), Some(1), "still playing track 2");
    }

    #[test]
    fn transport_keys_drive_playback() {
        let mut a = app(5);
        press(&mut a, &[key(KeyCode::Enter)]);
        assert_eq!(a.playback().state(), PlayState::Playing);
        press(&mut a, &[ch(' ')]);
        assert_eq!(a.playback().state(), PlayState::Paused);
        press(&mut a, &[ch(' ')]);
        assert_eq!(a.playback().state(), PlayState::Playing);
        press(&mut a, &[ch('n')]);
        assert_eq!(a.playback().current(), Some(1));
        press(&mut a, &[ch('p')]);
        assert_eq!(a.playback().current(), Some(0));
        press(&mut a, &[ch('-'), ch('-')]);
        assert_eq!(a.playback().volume().percent(), 90);
        press(&mut a, &[ch('+'), ch('=')]);
        assert_eq!(a.playback().volume().percent(), 100);
        press(&mut a, &[ch('z')]);
        assert!(a.playback().shuffle());
        press(&mut a, &[ch('r')]);
        assert_eq!(a.playback().repeat(), Repeat::All);
        press(&mut a, &[ch('s')]);
        assert_eq!(a.playback().state(), PlayState::Stopped);
    }

    #[test]
    fn seek_keys_move_the_position() {
        let mut a = app(2);
        press(&mut a, &[key(KeyCode::Enter)]);
        a.playback_mut().audio_mut().position = Duration::from_secs(60);
        press(&mut a, &[key(KeyCode::Right)]);
        assert_eq!(a.playback().position(), Duration::from_secs(65));
        a.playback_mut().audio_mut().position = Duration::from_secs(3);
        press(&mut a, &[key(KeyCode::Left)]);
        assert_eq!(a.playback().position(), Duration::ZERO);
    }

    #[test]
    fn q_and_ctrl_c_quit() {
        let mut a = app(1);
        press(&mut a, &[ch('q')]);
        assert!(a.should_quit());

        let mut a = app(1);
        a.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.should_quit());

        let mut a = app(1);
        press(&mut a, &[ch('/')]);
        a.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.should_quit(), "Ctrl+C quits while searching too");
    }

    #[test]
    fn key_releases_and_modified_letters_are_ignored() {
        let mut a = app(3);
        let mut release = ch('j');
        release.kind = KeyEventKind::Release;
        a.handle_key(release);
        assert_eq!(a.selected(), Some(0));
        a.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
        a.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert!(!a.should_quit());
        assert_eq!(a.selected(), Some(0));
    }

    #[test]
    fn filtering_beat_keeps_only_matching_tracks() {
        let mut a = music();
        press(&mut a, &[ch('/')]);
        assert_eq!(*a.mode(), Mode::Search);
        typed(&mut a, "beat");
        // Title "Beat It", artist "The Beatles", title "Heartbeats", album "UPBEAT DISCO".
        assert_eq!(a.visible(), [0, 1, 2, 5]);
        press(&mut a, &[key(KeyCode::Enter)]);
        assert_eq!(*a.mode(), Mode::Normal);
        assert_eq!(a.visible(), [0, 1, 2, 5], "Enter keeps the filter");
        assert_eq!(a.query(), "beat");
    }

    #[test]
    fn filtering_is_live_and_case_insensitive() {
        let mut a = music();
        press(&mut a, &[ch('/')]);
        typed(&mut a, "THE");
        assert_eq!(a.visible(), [1, 2, 3]);
        typed(&mut a, " k");
        assert_eq!(a.visible(), [2]);
        press(&mut a, &[key(KeyCode::Backspace), key(KeyCode::Backspace)]);
        assert_eq!(a.visible(), [1, 2, 3], "backspace widens the match");
        typed(&mut a, "zzz");
        assert_eq!(a.visible(), [] as [usize; 0]);
        assert_eq!(a.selected(), None);
    }

    #[test]
    fn letters_are_typed_not_acted_on_while_searching() {
        let mut a = music();
        press(&mut a, &[ch('/')]);
        typed(&mut a, "q snz");
        assert!(!a.should_quit());
        assert_eq!(a.playback().state(), PlayState::Stopped);
        assert!(!a.playback().shuffle());
        assert_eq!(a.query(), "q snz");
    }

    #[test]
    fn esc_clears_the_filter_and_keeps_the_selected_track() {
        let mut a = music();
        press(&mut a, &[ch('/')]);
        typed(&mut a, "beat");
        press(&mut a, &[key(KeyCode::Enter), ch('j'), ch('j')]);
        assert_eq!(a.selected_track(), Some(2));
        press(&mut a, &[key(KeyCode::Esc)]);
        assert_eq!(a.visible().len(), 6, "full library listed again");
        assert_eq!(a.query(), "");
        assert_eq!(a.selected_track(), Some(2));

        press(&mut a, &[ch('/')]);
        typed(&mut a, "dream");
        press(&mut a, &[key(KeyCode::Esc)]);
        assert_eq!(*a.mode(), Mode::Normal);
        assert_eq!(a.visible().len(), 6, "Esc while typing clears too");
    }

    #[test]
    fn enter_plays_the_selected_filtered_track() {
        let mut a = music();
        press(&mut a, &[ch('/')]);
        typed(&mut a, "dreams");
        press(&mut a, &[key(KeyCode::Enter), key(KeyCode::Enter)]);
        assert_eq!(a.playback().current(), Some(4));
    }

    #[test]
    fn query_rejects_invisible_characters_and_is_bounded() {
        let mut a = music();
        press(&mut a, &[ch('/')]);
        typed(&mut a, "a\u{202E}\u{1b}\u{200B}b");
        assert_eq!(a.query(), "ab");
        typed(&mut a, &"x".repeat(MAX_QUERY_CHARS * 2));
        assert_eq!(a.query().chars().count(), MAX_QUERY_CHARS);
    }

    #[test]
    fn status_messages_expire_but_stop_reasons_stay() {
        let mut a = app(2);
        let t0 = Instant::now();
        press(&mut a, &[ch('z')]);
        a.tick(t0);
        assert_eq!(a.status(t0).map(|n| n.text.as_str()), Some("Shuffle on"));
        let later = t0 + STATUS_TTL;
        a.tick(later);
        assert_eq!(a.status(later), None, "expired");

        a.playback_mut().audio_mut().broken = true;
        press(&mut a, &[key(KeyCode::Enter)]);
        a.tick(later);
        let stopped = a.status(later).map(|n| n.text.clone());
        assert!(
            stopped
                .as_deref()
                .is_some_and(|t| t.contains("Cannot play"))
        );
        let much_later = later + STATUS_TTL * 3;
        a.tick(much_later);
        assert_eq!(a.status(much_later).map(|n| n.text.clone()), stopped);
    }

    #[test]
    fn startup_notice_is_shown_first() {
        let mut a = App::new(
            Vec::new(),
            SilentAudio::default(),
            no_store(),
            "/m".to_owned(),
            Some(Notice::info("3 entries skipped")),
        );
        let now = Instant::now();
        a.tick(now);
        assert_eq!(
            a.status(now).map(|n| n.text.as_str()),
            Some("3 entries skipped")
        );
    }
}

/// Key sequences against playlists in a temporary music folder.
#[cfg(test)]
mod playlist_tests {
    use super::*;
    use crate::playlist::FsStore;
    use crate::test_support::SilentAudio;
    use anyhow::{Result, bail};
    use std::fs;
    use std::path::PathBuf;

    struct Music {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        app: App<SilentAudio>,
    }

    /// A library of `(title, artist)` tracks under `<root>/<artist>/LP/`,
    /// with playlists in `<root>/Playlists`.
    fn music(tracks: &[(&str, &str)]) -> Result<Music> {
        let tmp = tempfile::tempdir()?;
        let root = fs::canonicalize(tmp.path())?;
        let tracks = tracks
            .iter()
            .map(|&(title, artist)| Track {
                path: root.join(format!("{artist}/LP/{title}.mp3")),
                title: title.to_owned(),
                artist: artist.to_owned(),
                album: "LP".to_owned(),
                number: None,
                album_artist: None,
                disc: None,
                duration: Some(Duration::from_secs(200)),
            })
            .collect();
        let app = App::new(
            tracks,
            SilentAudio::default(),
            Box::new(FsStore::new(&root)),
            root.display().to_string(),
            None,
        );
        Ok(Music {
            _tmp: tmp,
            root,
            app,
        })
    }

    /// A, B, C, D by "Band" and "Song" by "Other".
    fn abcd() -> Result<Music> {
        music(&[
            ("A", "Band"),
            ("B", "Band"),
            ("C", "Band"),
            ("D", "Band"),
            ("Song", "Other"),
        ])
    }

    impl Music {
        fn dir(&self) -> PathBuf {
            self.root.join("Playlists")
        }

        fn file(&self, name: &str) -> PathBuf {
            self.dir().join(format!("{name}.m3u8"))
        }

        /// Writes playlist `name` listing the given tracks of "Band".
        fn write(&self, name: &str, titles: &[&str]) -> Result<()> {
            let entries: Vec<String> = titles
                .iter()
                .map(|t| format!("../Band/LP/{t}.mp3\n"))
                .collect();
            let text = format!("#EXTM3U\n{}", entries.concat());
            self.write_raw(name, text.as_bytes())
        }

        fn write_raw(&self, name: &str, bytes: &[u8]) -> Result<()> {
            fs::create_dir_all(self.dir())?;
            fs::write(self.file(name), bytes)?;
            Ok(())
        }

        /// Titles of the entries in the file of playlist `name`.
        fn file_titles(&self, name: &str) -> Result<Vec<String>> {
            Ok(fs::read_to_string(self.file(name))?
                .lines()
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .filter_map(|l| l.rsplit('/').next())
                .map(|l| l.trim_end_matches(".mp3").to_owned())
                .collect())
        }

        /// Titles of the rows shown in the open playlist.
        fn shown_titles(&self) -> Vec<String> {
            let Some(open) = self.app.open_playlist() else {
                return Vec::new();
            };
            self.app
                .visible()
                .iter()
                .map(|&row| self.title(open.items()[row]))
                .collect()
        }

        fn title(&self, track: usize) -> String {
            self.app.playback().tracks()[track].title.clone()
        }

        fn keys(&mut self, keys: &[KeyCode]) {
            for &code in keys {
                self.app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            }
        }

        fn chars(&mut self, text: &str) {
            for c in text.chars() {
                self.app
                    .handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
        }

        /// The status message after the next tick.
        fn status(&mut self) -> String {
            let now = Instant::now();
            self.app.tick(now);
            self.app
                .status(now)
                .map(|n| n.text.clone())
                .unwrap_or_default()
        }

        /// Shows the library in the main pane and focuses it: from the
        /// sidebar, `Enter` on "Library" (which closes an open playlist).
        fn show_library(&mut self) {
            if self.app.view() == View::Library {
                return;
            }
            if self.app.focus() == Focus::Main {
                self.keys(&[KeyCode::Tab]);
            }
            self.keys(&[KeyCode::Home, KeyCode::Enter]);
        }

        /// Shows the library and selects its track titled `title` (no
        /// filter active).
        fn select_in_library(&mut self, title: &str) -> Result<()> {
            self.show_library();
            let Some(index) = self
                .app
                .playback()
                .tracks()
                .iter()
                .position(|t| t.title == title)
            else {
                bail!("no track {title}");
            };
            self.keys(&[KeyCode::Home]);
            for _ in 0..index {
                self.keys(&[KeyCode::Down]);
            }
            Ok(())
        }

        /// Focuses the sidebar (with `Tab` if needed) and selects
        /// playlist `name` in it.
        fn select_playlist(&mut self, name: &str) -> Result<()> {
            if self.app.focus() == Focus::Main {
                self.keys(&[KeyCode::Tab]);
            }
            let Some(value) = self.app.sidebar_value_of(name) else {
                bail!("{name} not listed");
            };
            let Some(row) = self.app.visible().iter().position(|&r| r == value) else {
                bail!("{name} filtered out");
            };
            self.keys(&[KeyCode::Home]);
            for _ in 0..row {
                self.keys(&[KeyCode::Down]);
            }
            Ok(())
        }

        /// Selects playlist `name` in the sidebar and opens it with `Enter`.
        fn open(&mut self, name: &str) -> Result<()> {
            self.select_playlist(name)?;
            self.keys(&[KeyCode::Enter]);
            if self.app.view() != View::Playlist {
                bail!("{name} did not open");
            }
            Ok(())
        }

        fn picker(&self) -> Option<&Picker> {
            match self.app.mode() {
                Mode::Picker(p) => Some(p),
                _ => None,
            }
        }

        fn name_input(&self) -> Option<&NameInput> {
            match self.app.mode() {
                Mode::NameInput(n) => Some(n),
                _ => None,
            }
        }

        /// In the open picker, chooses playlist `name`.
        fn pick(&mut self, name: &str) -> Result<()> {
            let Some(index) = self
                .picker()
                .and_then(|p| p.choices().iter().position(|i| i.name == name))
            else {
                bail!("{name} not offered");
            };
            self.keys(&[KeyCode::Home]);
            for _ in 0..=index {
                self.keys(&[KeyCode::Down]);
            }
            self.keys(&[KeyCode::Enter]);
            Ok(())
        }

        /// In the open picker, creates playlist `name`.
        fn create(&mut self, name: &str) {
            self.keys(&[KeyCode::Home, KeyCode::Enter]);
            self.chars(name);
            self.keys(&[KeyCode::Enter]);
        }
    }

    // --- Focus, opening and leaving playlists ---

    /// Spec *No folder*: only "Library" is listed and nothing is written.
    #[test]
    fn open_playlists_view_without_folder_writes_nothing() -> Result<()> {
        let mut m = abcd()?;
        assert_eq!(m.app.sidebar_list().rows, [0, 1, 2], "only the fixed rows");
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.view(), View::Playlists);
        assert_eq!(m.app.playlists(), []);
        assert!(!m.dir().exists(), "nothing written to disk");
        m.keys(&[KeyCode::Enter, KeyCode::Esc]);
        assert_eq!(m.app.view(), View::Library, "Enter on \"Library\" shows it");
        assert!(!m.dir().exists(), "nothing written to disk");
        Ok(())
    }

    #[test]
    fn playlists_list_shows_only_m3u8_sorted_with_counts() -> Result<()> {
        let mut m = abcd()?;
        m.write("gym", &["A", "B"])?;
        m.write("Running", &["C"])?;
        fs::write(m.dir().join("Old.m3u"), "../Band/LP/A.mp3\n")?;
        fs::write(m.dir().join("notes.txt"), "hi")?;
        m.keys(&[KeyCode::Tab]);
        let listed: Vec<_> = m
            .app
            .playlists()
            .iter()
            .map(|i| (i.name.as_str(), i.tracks))
            .collect();
        assert_eq!(listed, [("gym", Some(2)), ("Running", Some(1))]);
        Ok(())
    }

    #[test]
    fn tab_back_to_library_keeps_its_filter_and_selection() -> Result<()> {
        let mut m = abcd()?;
        m.chars("/band");
        m.keys(&[KeyCode::Enter, KeyCode::Down, KeyCode::Down]);
        assert_eq!(m.app.selected_track(), Some(2));
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.view(), View::Playlists);
        assert_eq!(m.app.query(), "", "the playlists list has its own filter");
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.view(), View::Library);
        assert_eq!(m.app.query(), "band");
        assert_eq!(m.app.visible(), [0, 1, 2, 3]);
        assert_eq!(m.app.selected_track(), Some(2));

        // Spec *Back to library*: from an open playlist, `Enter` on
        // "Library" in the sidebar.
        m.write("Running", &["A"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Tab, KeyCode::Home, KeyCode::Enter]);
        assert_eq!(m.app.view(), View::Library);
        assert_eq!(m.app.focus(), Focus::Main);
        assert!(m.app.open_playlist().is_none());
        assert_eq!(m.app.query(), "band");
        assert_eq!(m.app.selected_track(), Some(2));
        Ok(())
    }

    #[test]
    fn return_to_open_playlist_keeps_its_filter_and_selection() -> Result<()> {
        let mut m = music(&[("A", "Band"), ("B", "Other"), ("C", "Band"), ("D", "Band")])?;
        m.write_raw(
            "Running",
            b"../Band/LP/A.mp3\n../Other/LP/B.mp3\n../Band/LP/C.mp3\n../Band/LP/D.mp3\n",
        )?;
        m.open("Running")?;
        assert_eq!(m.shown_titles(), ["A", "B", "C", "D"], "file order");
        m.chars("/band");
        m.keys(&[KeyCode::Enter, KeyCode::Down]);
        assert_eq!(m.shown_titles(), ["A", "C", "D"]);
        assert_eq!(m.app.selected(), Some(1));
        m.keys(&[KeyCode::Tab, KeyCode::Tab]);
        assert_eq!(m.app.view(), View::Playlist);
        assert_eq!(m.app.query(), "band");
        assert_eq!(m.shown_titles(), ["A", "C", "D"]);
        assert_eq!(m.app.selected(), Some(1));
        assert_eq!(m.app.selected_track(), Some(2), "C");
        Ok(())
    }

    impl Music {
        /// Starts kasset again over the same library and folder, with
        /// startup notice `notice`.
        fn restart(&mut self, notice: Option<Notice>) {
            let tracks = self.app.playback().tracks().to_vec();
            self.app = App::new(
                tracks,
                SilentAudio::default(),
                Box::new(FsStore::new(&self.root)),
                self.root.display().to_string(),
                notice,
            );
        }

        /// Names in the sidebar rows, the fixed rows included.
        fn sidebar_rows(&self) -> Vec<String> {
            self.app
                .sidebar_list()
                .rows
                .iter()
                .map(|&v| match SidebarEntry::of_value(v) {
                    SidebarEntry::Library => "Library".to_owned(),
                    SidebarEntry::Artists => "Artists".to_owned(),
                    SidebarEntry::Albums => "Albums".to_owned(),
                    SidebarEntry::Playlist(i) => self
                        .app
                        .playlists()
                        .get(i)
                        .map_or_else(|| "<missing>".to_owned(), |p| p.name.clone()),
                })
                .collect()
        }

        fn info(&self, name: &str) -> Option<PlaylistInfo> {
            self.app
                .playlists()
                .iter()
                .find(|i| i.name == name)
                .cloned()
        }
    }

    // --- Sidebar list (task 2.1) ---

    #[test]
    fn sidebar_filter_keeps_library_and_selects_the_first_match() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.write("Long run", &["B"])?;
        m.write("Running", &["C"])?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(
            m.sidebar_rows(),
            ["Library", "Artists", "Albums", "Gym", "Long run", "Running"]
        );
        m.chars("/run");
        assert_eq!(
            m.sidebar_rows(),
            ["Library", "Artists", "Albums", "Long run", "Running"]
        );
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Long run"));
        m.chars("ning");
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Running"));
        m.chars("zzz");
        assert_eq!(
            m.sidebar_rows(),
            ["Library", "Artists", "Albums"],
            "never filtered out"
        );
        assert_eq!(m.app.selected(), Some(0));
        Ok(())
    }

    #[test]
    fn sidebar_navigation_reaches_library_and_the_last_playlist() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.write("Running", &["C"])?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.selected(), Some(0), "\"Library\" selected at first");
        m.shift('G');
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Running"));
        m.chars("g");
        assert_eq!(m.app.selected(), Some(0));
        assert_eq!(m.app.selected_playlist(), None);
        m.chars("jk");
        assert_eq!(m.app.selected(), Some(0), "k reaches \"Library\"");
        Ok(())
    }

    // --- Startup listing and notices (task 2.2) ---

    /// Spec *Playlists listed on start*.
    #[test]
    fn playlists_listed_on_start() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.write("Gym", &["B", "C"])?;
        m.restart(None);
        assert_eq!(
            m.sidebar_rows(),
            ["Library", "Artists", "Albums", "Gym", "Running"]
        );
        assert_eq!(m.info("Gym").and_then(|i| i.tracks), Some(2));
        Ok(())
    }

    /// Spec *No folder*, at start.
    #[test]
    fn no_folder_lists_only_library_and_writes_nothing() -> Result<()> {
        let mut m = abcd()?;
        m.restart(None);
        assert_eq!(m.sidebar_rows(), ["Library", "Artists", "Albums"]);
        assert_eq!(m.status(), "");
        assert!(!m.dir().exists());
        Ok(())
    }

    /// Spec *Nothing played yet*, at the state level.
    #[test]
    fn nothing_is_playing_on_start() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.restart(None);
        assert!(m.app.playback().current_track().is_none());
        assert!(!m.app.playback().is_playing_playlist("Running"));
        Ok(())
    }

    #[test]
    fn scan_notice_and_listing_error_are_joined() -> Result<()> {
        let mut m = abcd()?;
        fs::write(m.dir(), "not a folder")?;
        m.restart(Some(Notice::info("3 entries skipped")));
        let now = Instant::now();
        m.app.tick(now);
        let notice = m.app.status(now).cloned();
        let text = notice.as_ref().map_or("", |n| n.text.as_str());
        assert!(text.starts_with("Cannot list playlists"), "{text}");
        assert!(text.ends_with("; 3 entries skipped"), "{text}");
        assert_eq!(notice.map(|n| n.level), Some(Level::Error));
        assert_eq!(m.sidebar_rows(), ["Library", "Artists", "Albums"]);
        Ok(())
    }

    #[test]
    fn info_after_an_error_does_not_hide_it() -> Result<()> {
        let mut m = abcd()?;
        m.app.notify(Notice::error("broken"));
        m.app.notify(Notice::info("fine"));
        m.app.notify(Notice::error("worse"));
        let now = Instant::now();
        m.app.tick(now);
        let notice = m.app.status(now).cloned();
        assert_eq!(
            notice.as_ref().map(|n| n.text.as_str()),
            Some("broken; worse")
        );
        assert_eq!(notice.map(|n| n.level), Some(Level::Error));
        Ok(())
    }

    #[test]
    fn a_later_key_press_replaces_an_unshown_notice() -> Result<()> {
        let mut m = abcd()?;
        m.chars("zz");
        assert_eq!(m.status(), "Shuffle off", "not joined across key presses");
        Ok(())
    }

    /// Measures the cost the startup listing adds to `App::new` (task 2.2):
    /// `cargo test --release startup_listing_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing measurement, run by hand"]
    fn startup_listing_cost() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = fs::canonicalize(tmp.path())?;
        let tracks: Vec<Track> = (0..100_000)
            .map(|i| Track {
                path: root.join(format!("Artist {}/Album {}/{i:06}.mp3", i % 500, i % 37)),
                title: format!("Title {i}"),
                artist: format!("Artist {}", i % 500),
                album: format!("Album {}", i % 37),
                number: None,
                album_artist: None,
                disc: None,
                duration: None,
            })
            .collect();
        let resolver_start = Instant::now();
        let resolver = Resolver::new(&tracks);
        let listed = FsStore::new(&root).list(&resolver)?;
        let listing = resolver_start.elapsed();
        let start = Instant::now();
        let app = App::new(
            tracks,
            SilentAudio::default(),
            Box::new(FsStore::new(&root)),
            String::new(),
            None,
        );
        let total = start.elapsed();
        println!(
            "App::new on 100 000 tracks: {total:?}; listing (resolver + list): {listing:?}; {} playlists",
            listed.len() + app.playlists().len()
        );
        Ok(())
    }

    // --- Focus and keys (task 2.3) ---

    /// Spec *Focus on start*.
    #[test]
    fn focus_on_start_is_the_library() -> Result<()> {
        let m = abcd()?;
        assert_eq!(m.app.focus(), Focus::Main);
        assert_eq!(m.app.main_view(), MainView::Library);
        assert_eq!(m.app.view(), View::Library);
        Ok(())
    }

    /// Spec *Tab moves focus*, with `Tab` and Shift+Tab.
    #[test]
    fn tab_and_back_tab_move_focus_and_keep_the_main_list() -> Result<()> {
        let mut m = abcd()?;
        m.chars("/band");
        m.keys(&[KeyCode::Enter, KeyCode::Down]);
        let before = format!("{:?}", m.app.main_list());
        for back in [KeyCode::Tab, KeyCode::BackTab] {
            m.keys(&[KeyCode::Tab]);
            assert_eq!(m.app.focus(), Focus::Sidebar);
            assert_eq!(format!("{:?}", m.app.main_list()), before);
            m.app.handle_key(KeyEvent::new(back, KeyModifiers::SHIFT));
            assert_eq!(m.app.focus(), Focus::Main);
            assert_eq!(format!("{:?}", m.app.main_list()), before);
        }
        Ok(())
    }

    /// Spec *Keys go to the focused pane* and *Open playlists view*.
    #[test]
    fn keys_go_to_the_focused_pane() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.write("Running", &["B"])?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(
            m.sidebar_rows(),
            ["Library", "Artists", "Albums", "Gym", "Running"]
        );
        assert_eq!(m.app.main_view(), MainView::Library);
        m.chars("j");
        assert_eq!(m.app.sidebar_list().selected, Some(1));
        assert_eq!(m.app.main_list().selected, Some(0), "main pane unchanged");
        Ok(())
    }

    /// Spec *Listing refreshed*.
    #[test]
    fn sidebar_focus_lists_the_playlists_again() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.restart(None);
        m.write("New", &["B"])?;
        assert_eq!(m.listed(), ["Gym"]);
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.listed(), ["Gym", "New"]);
        Ok(())
    }

    /// Spec *Filter the sidebar*: then `Enter` opens the matching playlist.
    #[test]
    fn enter_after_filtering_the_sidebar_opens_the_match() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.write("Running", &["C", "D"])?;
        m.keys(&[KeyCode::Tab]);
        m.chars("/run");
        m.keys(&[KeyCode::Enter, KeyCode::Enter]);
        assert_eq!(m.app.view(), View::Playlist);
        assert_eq!(
            m.app.open_playlist().map(OpenPlaylist::name),
            Some("Running")
        );
        Ok(())
    }

    /// Spec *Open*.
    #[test]
    fn enter_on_a_playlist_lists_it_in_file_order_with_focus() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["C", "A", "B"])?;
        m.select_playlist("Running")?;
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.app.focus(), Focus::Main);
        assert_eq!(m.app.main_view(), MainView::Playlist);
        assert_eq!(m.shown_titles(), ["C", "A", "B"]);
        Ok(())
    }

    /// Spec *Esc from a playlist*: the playlists are listed again.
    #[test]
    fn esc_from_a_playlist_lists_the_playlists_again() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.open("Running")?;
        m.write("Gym", &["B"])?;
        m.keys(&[KeyCode::Esc]);
        assert_eq!(m.app.focus(), Focus::Sidebar);
        assert_eq!(m.listed(), ["Gym", "Running"]);
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Running"));
        assert_eq!(m.app.main_view(), MainView::Playlist);
        m.keys(&[KeyCode::Esc]);
        assert_eq!(m.app.focus(), Focus::Sidebar, "Esc in the sidebar stays");
        Ok(())
    }

    /// Spec *Esc from a playlist*, with a sidebar filter that hides it.
    #[test]
    fn esc_selects_the_shown_playlist_even_when_the_filter_hides_it() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.write("Running", &["B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Tab]);
        m.chars("/gym");
        m.keys(&[KeyCode::Enter, KeyCode::Tab, KeyCode::Esc]);
        assert_eq!(m.app.focus(), Focus::Sidebar);
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Running"));
        assert_eq!(m.app.query(), "", "the filter that hid it is cleared");
        Ok(())
    }

    #[test]
    fn rename_selects_the_new_name_even_when_the_filter_hides_it() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.write("Gym 2", &["A"])?;
        m.write("Running", &["B"])?;
        m.keys(&[KeyCode::Tab]);
        m.chars("/gym");
        m.keys(&[KeyCode::Enter]);
        m.select_playlist("Gym")?;
        m.shift('R');
        m.retype("Weights");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Weights"));
        Ok(())
    }

    /// Spec *Reopen the shown playlist*.
    #[test]
    fn reopening_the_shown_playlist_keeps_its_filter_and_selection() -> Result<()> {
        let mut m = music(&[("A", "Band"), ("B", "Other"), ("C", "Band"), ("D", "Band")])?;
        m.write_raw(
            "Running",
            b"../Band/LP/A.mp3\n../Other/LP/B.mp3\n../Band/LP/C.mp3\n../Band/LP/D.mp3\n",
        )?;
        m.open("Running")?;
        m.chars("/band");
        m.keys(&[KeyCode::Enter, KeyCode::Down]);
        m.open("Running")?;
        assert_eq!(m.app.query(), "band");
        assert_eq!(m.shown_titles(), ["A", "C", "D"]);
        assert_eq!(m.selected_title().as_deref(), Some("C"));
        Ok(())
    }

    #[test]
    fn failed_open_keeps_focus_and_the_shown_playlist() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.write("Running", &["B"])?;
        m.open("Running")?;
        m.select_playlist("Gym")?;
        fs::remove_file(m.file("Gym"))?;
        fs::create_dir(m.file("Gym"))?;
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.app.focus(), Focus::Sidebar);
        assert_eq!(
            m.app.open_playlist().map(OpenPlaylist::name),
            Some("Running")
        );
        assert!(m.status().contains("\"Gym\""));
        Ok(())
    }

    /// Spec *Delete the shown playlist*.
    #[test]
    fn deleting_the_shown_playlist_shows_the_library() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.open("Gym")?;
        m.keys(&[KeyCode::Esc]);
        m.shift('D');
        m.chars("y");
        assert_eq!(m.app.main_view(), MainView::Library);
        assert_eq!(m.app.focus(), Focus::Sidebar);
        assert!(!m.file("Gym").exists());
        Ok(())
    }

    #[test]
    fn rename_and_delete_do_nothing_on_library() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.keys(&[KeyCode::Tab, KeyCode::Home]);
        m.shift('R');
        assert_eq!(*m.app.mode(), Mode::Normal);
        m.shift('D');
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert!(m.file("Gym").exists());
        Ok(())
    }

    /// Spec *Open playlist removed meanwhile*.
    #[test]
    fn open_playlist_removed_meanwhile_falls_back_to_the_library() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.write("Gym", &["B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Tab]);
        fs::remove_file(m.file("Running"))?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.view(), View::Library);
        let names: Vec<_> = m.app.playlists().iter().map(|i| i.name.clone()).collect();
        assert_eq!(names, ["Gym"]);
        let status = m.status();
        assert!(status.contains("\"Running\" no longer exists"), "{status}");
        Ok(())
    }

    #[test]
    fn open_playlist_changed_meanwhile_is_reloaded() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Tab]);
        m.write("Running", &["A", "B", "C"])?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.view(), View::Playlist);
        assert_eq!(m.shown_titles(), ["A", "B", "C"]);
        let status = m.status();
        assert!(status.contains("reloaded"), "{status}");
        Ok(())
    }

    #[test]
    fn enter_opens_and_esc_leaves_after_clearing_the_filter() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["D"])?;
        m.write("Running", &["C", "A", "B"])?;
        m.open("Running")?;
        assert_eq!(m.shown_titles(), ["C", "A", "B"]);
        m.keys(&[KeyCode::Down, KeyCode::Down]);
        m.chars("/c");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.shown_titles(), ["C"], "search inside the playlist");
        m.keys(&[KeyCode::Esc]);
        assert_eq!(m.app.view(), View::Playlist, "first Esc clears the filter");
        assert_eq!(m.shown_titles(), ["C", "A", "B"]);
        assert_eq!(m.app.selected_track(), Some(2), "C stays selected");
        m.keys(&[KeyCode::Esc]);
        assert_eq!(m.app.view(), View::Playlists);
        assert_eq!(
            m.selected_playlist_name().as_deref(),
            Some("Running"),
            "the playlist just left is selected"
        );
        assert_eq!(
            m.app.open_playlist().map(OpenPlaylist::name),
            Some("Running"),
            "and still shown"
        );
        Ok(())
    }

    #[test]
    fn opening_reports_unmatched_entries_and_read_only() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "gone", "B"])?;
        m.open("Running")?;
        assert_eq!(m.shown_titles(), ["A", "B"]);
        let status = m.status();
        assert!(
            status.contains("1 entry not found in the library"),
            "{status}"
        );

        m.keys(&[KeyCode::Esc]);
        m.write_raw("Old", b"../Band/LP/A.mp3\n# caf\xe9\n")?;
        m.keys(&[KeyCode::Tab, KeyCode::Tab]);
        m.keys(&[KeyCode::Esc, KeyCode::Tab]);
        m.open("Old")?;
        assert_eq!(m.shown_titles(), ["A"]);
        let status = m.status();
        assert!(status.contains("not UTF-8"), "{status}");
        Ok(())
    }

    #[test]
    fn enter_in_a_playlist_plays_it_as_the_play_order() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["C", "A"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.app.playback().current(), Some(2));
        assert!(m.app.playback().is_playing_playlist("Running"));
        m.chars("n");
        assert_eq!(m.app.playback().current(), Some(0), "next is A, not D");
        m.chars("n");
        assert_eq!(m.app.playback().current(), Some(0), "stays at the end");
        Ok(())
    }

    // --- Adding and creating (task 3.2) ---

    #[test]
    fn add_one_track() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.select_in_library("C")?;
        m.chars("a");
        assert_eq!(m.picker().map(Picker::track_count), Some(1));
        m.pick("Running")?;
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.file_titles("Running")?, ["A", "C"]);
        let status = m.status();
        assert!(status.contains("Added 1 track to \"Running\""), "{status}");
        Ok(())
    }

    #[test]
    fn add_filtered_tracks_in_library_order() -> Result<()> {
        let mut m = music(&[
            ("One", "Band"),
            ("Two", "Other"),
            ("Three", "Band"),
            ("Four", "Band"),
        ])?;
        m.write_raw("Running", b"#EXTM3U\n")?;
        m.chars("/band");
        m.keys(&[KeyCode::Enter]);
        m.chars("A");
        assert_eq!(m.picker().map(Picker::track_count), Some(3));
        m.pick("Running")?;
        assert_eq!(m.file_titles("Running")?, ["One", "Three", "Four"]);
        let status = m.status();
        assert!(status.contains("Added 3 tracks"), "{status}");
        Ok(())
    }

    #[test]
    fn cancel_changes_nothing() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        let before = fs::read(m.file("Running"))?;
        m.chars("a");
        m.keys(&[KeyCode::Down, KeyCode::Esc]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(fs::read(m.file("Running"))?, before);

        m.chars("a");
        m.keys(&[KeyCode::Enter]);
        m.chars("New");
        m.keys(&[KeyCode::Esc]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert!(!m.file("New").exists());
        Ok(())
    }

    #[test]
    fn entry_limit_refuses_the_whole_addition() -> Result<()> {
        let titles: Vec<String> = (0..25).map(|i| format!("T{i:02}")).collect();
        let tracks: Vec<(&str, &str)> = titles.iter().map(|t| (t.as_str(), "Band")).collect();
        let mut m = music(&tracks)?;
        let content = "../Band/LP/T00.mp3\n".repeat(99_990);
        m.write_raw("Running", content.as_bytes())?;
        m.chars("A");
        m.pick("Running")?;
        let status = m.status();
        assert!(status.contains("would exceed 100 000 entries"), "{status}");
        assert_eq!(fs::read(m.file("Running"))?, content.as_bytes());
        Ok(())
    }

    #[test]
    fn already_present_is_skipped() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["B"])?;
        let before = fs::read(m.file("Running"))?;
        m.select_in_library("B")?;
        m.chars("a");
        m.pick("Running")?;
        assert_eq!(fs::read(m.file("Running"))?, before);
        let status = m.status();
        assert!(status.contains("1 track already there"), "{status}");

        // Mixed: one new, one present.
        m.chars("A");
        m.pick("Running")?;
        assert_eq!(m.file_titles("Running")?, ["B", "A", "C", "D", "Song"]);
        let status = m.status();
        assert!(status.contains("Added 4 tracks"), "{status}");
        assert!(status.contains("1 track already there"), "{status}");
        Ok(())
    }

    #[test]
    fn new_playlist_creates_the_folder_and_file() -> Result<()> {
        let mut m = abcd()?;
        m.select_in_library("B")?;
        m.chars("a");
        assert_eq!(m.picker().map(|p| p.choices().len()), Some(0));
        m.create("  Road trip ");
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.file_titles("Road trip")?, ["B"]);
        let status = m.status();
        assert!(status.contains("Created \"Road trip\""), "{status}");
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.playlists().len(), 1);
        Ok(())
    }

    #[test]
    fn new_playlist_is_not_created_when_no_track_can_be_stored() -> Result<()> {
        let mut m = music(&[("bad\nname", "Band")])?;
        m.chars("a");
        m.create("Running");
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert!(!m.dir().exists(), "nothing written");
        let status = m.status();
        assert!(status.contains("\"Running\" not created"), "{status}");
        assert!(status.contains("cannot be stored"), "{status}");
        Ok(())
    }

    #[test]
    fn invalid_name_is_explained_and_can_be_edited() -> Result<()> {
        let mut m = abcd()?;
        m.chars("a");
        m.create("a/b");
        let error = m.name_input().and_then(NameInput::error).map(str::to_owned);
        assert!(
            error.as_deref().is_some_and(|e| e.contains("'/'")),
            "{error:?}"
        );
        assert!(!m.dir().exists());
        m.keys(&[KeyCode::Backspace, KeyCode::Backspace]);
        assert_eq!(m.name_input().and_then(NameInput::error), None);
        m.chars("x");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.file_titles("ax")?, ["A"]);
        Ok(())
    }

    #[test]
    fn name_taken_ignoring_case_is_refused() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        let before = fs::read(m.file("Running"))?;
        m.select_in_library("B")?;
        m.chars("a");
        m.create("running");
        let error = m.name_input().and_then(NameInput::error).map(str::to_owned);
        assert!(
            error.as_deref().is_some_and(|e| e.contains("already used")),
            "{error:?}"
        );
        let status = m.status();
        assert!(status.contains("already used"), "{status}");
        assert_eq!(fs::read(m.file("Running"))?, before);
        assert_eq!(fs::read_dir(m.dir())?.count(), 1, "nothing else written");
        Ok(())
    }

    #[test]
    fn name_too_long_in_bytes_is_refused() -> Result<()> {
        let mut m = abcd()?;
        m.chars("a");
        m.create(&"あ".repeat(70));
        let error = m.name_input().and_then(NameInput::error).map(str::to_owned);
        assert!(
            error.as_deref().is_some_and(|e| e.contains("too long")),
            "{error:?}"
        );
        assert!(!m.dir().exists());
        Ok(())
    }

    #[test]
    fn read_only_playlist_is_marked_and_refused_in_the_picker() -> Result<()> {
        let mut m = abcd()?;
        let bytes = b"../Band/LP/A.mp3\n# caf\xe9\n";
        m.write_raw("Old", bytes)?;
        m.chars("a");
        let marked = m
            .picker()
            .and_then(|p| p.choices().first())
            .and_then(|i| i.read_only);
        assert!(marked.is_some(), "shown as read-only");
        m.pick("Old")?;
        assert!(m.picker().is_some(), "the picker stays open");
        let status = m.status();
        assert!(status.contains("not UTF-8"), "{status}");
        assert_eq!(fs::read(m.file("Old"))?, bytes);
        Ok(())
    }

    /// D9: the temporary file's path is taken by a directory, so saving fails
    /// whoever runs the test.
    #[test]
    fn write_failure_names_the_playlist_and_changes_nothing() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.open("Running")?;
        let before = fs::read(m.file("Running"))?;
        let blocker = m
            .dir()
            .join(format!(".Running.m3u8.tmp-{}", std::process::id()));
        fs::create_dir(&blocker)?;
        m.select_in_library("B")?;
        m.chars("a");
        m.pick("Running")?;
        let status = m.status();
        assert!(status.contains("\"Running\""), "{status}");
        assert!(status.contains("cannot save"), "{status}");
        assert_eq!(fs::read(m.file("Running"))?, before);
        assert_eq!(
            m.app.playlists().first().and_then(|i| i.tracks),
            Some(1),
            "the sidebar entry is unchanged"
        );
        m.open("Running")?;
        assert_eq!(m.shown_titles(), ["A"], "unchanged on screen too");
        Ok(())
    }

    #[test]
    fn write_failure_on_create_is_reported() -> Result<()> {
        let mut m = abcd()?;
        fs::write(m.dir(), "not a folder")?;
        m.chars("a");
        m.create("Running");
        assert_eq!(*m.app.mode(), Mode::Normal);
        let status = m.status();
        assert!(status.contains("\"Running\""), "{status}");
        assert_eq!(fs::read(m.dir())?, b"not a folder");
        Ok(())
    }

    #[test]
    fn edited_elsewhere_reloads_and_discards_the_addition() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.open("Running")?;
        m.write("Running", &["A", "C"])?;
        // `a` only works with the library shown, which closes the playlist,
        // so the stale open copy is reached through `add_to` directly.
        m.app.add_to("Running", &[1]);
        assert_eq!(m.file_titles("Running")?, ["A", "C"], "not overwritten");
        let status = m.status();
        assert!(status.contains("reloaded"), "{status}");
        assert!(status.contains("nothing was added"), "{status}");
        assert_eq!(m.shown_titles(), ["A", "C"], "the other program's change");
        Ok(())
    }

    #[test]
    fn adding_to_a_playlist_shows_the_new_tracks() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.open("Running")?;
        m.select_in_library("D")?;
        m.chars("a");
        m.pick("Running")?;
        m.open("Running")?;
        assert_eq!(m.shown_titles(), ["A", "D"]);
        m.chars("a");
        assert_eq!(*m.app.mode(), Mode::Normal, "a only adds from the library");
        Ok(())
    }

    #[test]
    fn add_to_playing_playlist_extends_the_play_order() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Down, KeyCode::Enter]);
        assert_eq!(m.app.playback().current(), Some(1), "B plays");
        m.keys(&[KeyCode::Tab]);
        m.select_in_library("D")?;
        m.chars("a");
        m.pick("Running")?;
        assert_eq!(m.app.playback().items(), [0, 1, 3]);
        assert_eq!(m.app.playback().current(), Some(1), "B keeps playing");
        m.chars("n");
        assert_eq!(m.app.playback().current(), Some(3), "D after B");
        Ok(())
    }

    #[test]
    fn add_to_playing_playlist_that_is_not_open() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Down, KeyCode::Enter]);
        m.show_library();
        assert!(m.app.open_playlist().is_none());
        m.select_in_library("D")?;
        m.chars("a");
        m.pick("Running")?;
        m.chars("n");
        assert_eq!(m.app.playback().current(), Some(3), "D after B");
        Ok(())
    }

    #[test]
    fn reload_of_playing_playlist_follows_the_new_order() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Down, KeyCode::Enter]);
        m.keys(&[KeyCode::Tab]);
        m.write("Running", &["A", "B", "D"])?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.shown_titles(), ["A", "B", "D"]);
        assert_eq!(m.app.playback().current(), Some(1), "B keeps playing");
        m.chars("n");
        assert_eq!(m.app.playback().current(), Some(3), "then D");
        Ok(())
    }

    #[test]
    fn playlist_deleted_elsewhere_while_playing_keeps_playing() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Enter, KeyCode::Tab]);
        fs::remove_file(m.file("Running"))?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.view(), View::Library);
        assert!(!m.app.playback().is_playing_playlist("Running"));
        assert_eq!(m.app.playback().current(), Some(0));
        m.chars("n");
        assert_eq!(m.app.playback().current(), Some(1), "its order goes on");
        Ok(())
    }

    #[test]
    fn nothing_to_add_from_an_empty_list() -> Result<()> {
        let mut m = abcd()?;
        m.chars("/zzz");
        m.keys(&[KeyCode::Enter]);
        m.chars("a");
        assert_eq!(*m.app.mode(), Mode::Normal);
        m.chars("A");
        assert_eq!(*m.app.mode(), Mode::Normal);
        let status = m.status();
        assert!(status.contains("No track to add"), "{status}");
        Ok(())
    }

    #[test]
    fn letters_are_typed_into_the_name_not_acted_on() -> Result<()> {
        let mut m = abcd()?;
        m.chars("a");
        m.keys(&[KeyCode::Enter]);
        m.chars("q snz\u{202E}");
        assert!(!m.app.should_quit());
        assert_eq!(m.name_input().map(NameInput::text), Some("q snz"));
        m.chars(&"x".repeat(MAX_QUERY_CHARS * 2));
        assert_eq!(
            m.name_input().map(|n| n.text().chars().count()),
            Some(MAX_QUERY_CHARS)
        );
        Ok(())
    }

    // --- Editing the open playlist (task 4.1) ---

    impl Music {
        /// Selects row `row` of the list on screen.
        fn select_row(&mut self, row: usize) {
            self.keys(&[KeyCode::Home]);
            for _ in 0..row {
                self.keys(&[KeyCode::Down]);
            }
        }

        /// Ends the playing track and lets playback react.
        fn end_track(&mut self) {
            self.app.playback_mut().audio_mut().ended = true;
            self.app.tick(Instant::now());
        }

        fn shift(&mut self, c: char) {
            self.app
                .handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT));
        }

        /// Title of the selected row in the open playlist.
        fn selected_title(&self) -> Option<String> {
            self.app.selected_track().map(|t| self.title(t))
        }

        fn playing_title(&self) -> Option<String> {
            self.app.playback().current().map(|t| self.title(t))
        }
    }

    #[test]
    fn remove_selects_the_following_entry_and_saves() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B", "C"])?;
        m.open("Running")?;
        m.select_row(1);
        m.chars("d");
        assert_eq!(m.shown_titles(), ["A", "C"]);
        assert_eq!(m.selected_title().as_deref(), Some("C"));
        assert_eq!(m.file_titles("Running")?, ["A", "C"]);
        let status = m.status();
        assert!(
            status.contains("Removed \"B\" from \"Running\""),
            "{status}"
        );

        // Removing the last entry selects the new last one.
        m.select_row(1);
        m.chars("d");
        assert_eq!(m.shown_titles(), ["A"]);
        assert_eq!(m.selected_title().as_deref(), Some("A"));
        Ok(())
    }

    #[test]
    fn remove_takes_only_that_occurrence_and_keeps_other_lines() -> Result<()> {
        let mut m = abcd()?;
        m.write_raw(
            "Running",
            b"#EXTM3U\r\n#EXTINF:1,x - A\r\n../Band/LP/A.mp3\r\n# note\r\n../gone.mp3\r\n\
              #EXTINF:2,x - B\r\n../Band/LP/B.mp3\r\n../Band/LP/A.mp3",
        )?;
        m.open("Running")?;
        assert_eq!(m.shown_titles(), ["A", "B", "A"]);
        m.select_row(1);
        m.chars("d");
        assert_eq!(m.shown_titles(), ["A", "A"]);
        assert_eq!(
            fs::read(m.file("Running"))?,
            b"#EXTM3U\r\n#EXTINF:1,x - A\r\n../Band/LP/A.mp3\r\n# note\r\n../gone.mp3\r\n\
              ../Band/LP/A.mp3",
            "the #EXTINF went with B; comments, unmatched entries, CRLF and the \
             missing final newline stayed"
        );
        Ok(())
    }

    #[test]
    fn remove_while_filtered_selects_the_next_listed_entry() -> Result<()> {
        let mut m = music(&[("A", "Band"), ("B", "Other"), ("C", "Band"), ("D", "Band")])?;
        m.write_raw(
            "Running",
            b"../Band/LP/A.mp3\n../Other/LP/B.mp3\n../Band/LP/C.mp3\n../Band/LP/D.mp3\n",
        )?;
        m.open("Running")?;
        m.chars("/band");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.shown_titles(), ["A", "C", "D"]);
        m.chars("d");
        assert_eq!(m.shown_titles(), ["C", "D"], "the filter stays");
        assert_eq!(m.selected_title().as_deref(), Some("C"));
        assert_eq!(m.file_titles("Running")?, ["B", "C", "D"]);
        Ok(())
    }

    #[test]
    fn move_down_and_up_keep_the_entry_selected() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B", "C"])?;
        m.open("Running")?;
        m.shift('J');
        assert_eq!(m.shown_titles(), ["B", "A", "C"]);
        assert_eq!(m.selected_title().as_deref(), Some("A"));
        assert_eq!(m.file_titles("Running")?, ["B", "A", "C"]);

        m.shift('K');
        m.shift('K');
        assert_eq!(m.shown_titles(), ["A", "B", "C"], "past the top: nothing");
        assert_eq!(m.selected_title().as_deref(), Some("A"));
        m.select_row(2);
        let before = fs::read(m.file("Running"))?;
        m.shift('J');
        assert_eq!(
            fs::read(m.file("Running"))?,
            before,
            "past the end: nothing"
        );
        Ok(())
    }

    #[test]
    fn move_skips_over_unmatched_entries_which_stay_in_place() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "gone", "B"])?;
        m.open("Running")?;
        m.shift('J');
        assert_eq!(m.shown_titles(), ["B", "A"]);
        assert_eq!(m.file_titles("Running")?, ["B", "gone", "A"]);
        Ok(())
    }

    #[test]
    fn reorder_is_refused_with_a_filter_active() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B", "C"])?;
        let before = fs::read(m.file("Running"))?;
        m.open("Running")?;
        m.chars("/b");
        m.keys(&[KeyCode::Enter]);
        m.shift('K');
        m.shift('J');
        assert_eq!(fs::read(m.file("Running"))?, before);
        let status = m.status();
        assert!(status.contains("Clear the filter"), "{status}");
        assert!(status.contains("to reorder"), "{status}");
        Ok(())
    }

    #[test]
    fn latin1_playlist_refuses_edits_and_is_unchanged() -> Result<()> {
        let mut m = abcd()?;
        let bytes = b"../Band/LP/A.mp3\n# caf\xe9\n../Band/LP/B.mp3\n";
        m.write_raw("Old", bytes)?;
        m.open("Old")?;
        m.status();
        m.chars("d");
        assert_eq!(m.shown_titles(), ["A", "B"]);
        let status = m.status();
        assert!(status.contains("not UTF-8"), "{status}");
        assert!(status.contains("cannot be edited"), "{status}");
        m.shift('J');
        assert_eq!(m.shown_titles(), ["A", "B"]);
        assert_eq!(fs::read(m.file("Old"))?, bytes);
        Ok(())
    }

    #[test]
    fn consecutive_edits_are_all_saved_without_a_conflict() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B", "C", "D"])?;
        m.open("Running")?;
        m.chars("d");
        m.status();
        m.chars("d");
        let status = m.status();
        assert!(!status.contains("another program"), "{status}");
        m.shift('J');
        assert_eq!(m.file_titles("Running")?, ["D", "C"]);
        assert_eq!(m.shown_titles(), ["D", "C"]);
        Ok(())
    }

    #[test]
    fn edit_of_a_playlist_changed_elsewhere_is_discarded() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B"])?;
        m.open("Running")?;
        m.write("Running", &["A", "B", "C"])?;
        m.chars("d");
        assert_eq!(
            m.file_titles("Running")?,
            ["A", "B", "C"],
            "not overwritten"
        );
        assert_eq!(
            m.shown_titles(),
            ["A", "B", "C"],
            "the other program's change"
        );
        let status = m.status();
        assert!(status.contains("reloaded"), "{status}");
        Ok(())
    }

    /// D9: the temporary file's path is taken by a directory.
    #[test]
    fn failed_save_leaves_the_list_unchanged() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B", "C"])?;
        m.open("Running")?;
        let before = fs::read(m.file("Running"))?;
        let blocker = m
            .dir()
            .join(format!(".Running.m3u8.tmp-{}", std::process::id()));
        fs::create_dir(&blocker)?;
        m.select_row(1);
        m.chars("d");
        m.shift('J');
        assert_eq!(m.shown_titles(), ["A", "B", "C"]);
        assert_eq!(m.selected_title().as_deref(), Some("B"));
        assert_eq!(fs::read(m.file("Running"))?, before);
        let status = m.status();
        assert!(status.contains("\"Running\""), "{status}");
        assert!(status.contains("cannot save"), "{status}");

        fs::remove_dir(&blocker)?;
        m.chars("d");
        assert_eq!(
            m.shown_titles(),
            ["A", "C"],
            "works once the folder is writable"
        );
        Ok(())
    }

    #[test]
    fn edit_of_a_playlist_deleted_elsewhere_shows_the_library() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B"])?;
        m.open("Running")?;
        fs::remove_file(m.file("Running"))?;
        m.chars("d");
        assert_eq!(m.app.view(), View::Library);
        assert!(!m.file("Running").exists(), "not recreated");
        let status = m.status();
        assert!(status.contains("no longer exists"), "{status}");
        Ok(())
    }

    /// Plays B from "Running" (A, B, C) and removes it.
    fn remove_playing_b(m: &mut Music) -> Result<()> {
        m.write("Running", &["A", "B", "C"])?;
        m.open("Running")?;
        m.select_row(1);
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.playing_title().as_deref(), Some("B"));
        m.chars("d");
        assert_eq!(m.shown_titles(), ["A", "C"]);
        assert_eq!(m.playing_title().as_deref(), Some("B"), "B keeps playing");
        Ok(())
    }

    #[test]
    fn removed_playing_track_finishes_then_the_next_plays() -> Result<()> {
        let mut m = abcd()?;
        remove_playing_b(&mut m)?;
        m.end_track();
        assert_eq!(m.playing_title().as_deref(), Some("C"));
        Ok(())
    }

    #[test]
    fn removed_playing_track_is_not_repeated_with_repeat_one() -> Result<()> {
        let mut m = abcd()?;
        m.chars("rr");
        assert_eq!(m.app.playback().repeat(), crate::queue::Repeat::One);
        remove_playing_b(&mut m)?;
        m.end_track();
        assert_eq!(m.playing_title().as_deref(), Some("C"));
        m.end_track();
        assert_eq!(m.playing_title().as_deref(), Some("C"), "then C repeats");
        Ok(())
    }

    #[test]
    fn removed_last_entry_stops_or_wraps() -> Result<()> {
        for repeat_all in [false, true] {
            let mut m = abcd()?;
            if repeat_all {
                m.chars("r");
            }
            m.write("Running", &["A", "B", "C"])?;
            m.open("Running")?;
            m.select_row(2);
            m.keys(&[KeyCode::Enter]);
            m.chars("d");
            assert_eq!(m.playing_title().as_deref(), Some("C"));
            m.end_track();
            if repeat_all {
                assert_eq!(m.playing_title().as_deref(), Some("A"));
            } else {
                assert_eq!(
                    m.app.playback().state(),
                    crate::playback::PlayState::Stopped
                );
            }
        }
        Ok(())
    }

    #[test]
    fn previous_after_removing_the_playing_track() -> Result<()> {
        let mut m = abcd()?;
        remove_playing_b(&mut m)?;
        m.app.playback_mut().audio_mut().position = Duration::from_secs(2);
        m.chars("p");
        assert_eq!(m.playing_title().as_deref(), Some("A"));
        Ok(())
    }

    #[test]
    fn reorder_while_playing_changes_what_plays_next() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B", "C"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Enter]);
        m.select_row(2);
        m.shift('K');
        assert_eq!(m.shown_titles(), ["A", "C", "B"]);
        m.end_track();
        assert_eq!(m.playing_title().as_deref(), Some("C"));
        m.end_track();
        assert_eq!(m.playing_title().as_deref(), Some("B"));
        Ok(())
    }

    #[test]
    fn edits_of_a_playlist_that_is_not_playing_leave_the_play_order() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B", "C"])?;
        m.keys(&[KeyCode::Enter]);
        m.open("Running")?;
        m.chars("d");
        m.chars("n");
        assert_eq!(m.playing_title().as_deref(), Some("B"), "library order");
        Ok(())
    }

    // --- Renaming and deleting (task 4.2) ---

    impl Music {
        fn listed(&self) -> Vec<String> {
            self.app
                .playlists()
                .iter()
                .map(|i| i.name.clone())
                .collect()
        }

        fn selected_playlist_name(&self) -> Option<String> {
            self.app.selected_playlist().map(|i| i.name.clone())
        }

        /// Clears the name prompt and types `name`.
        fn retype(&mut self, name: &str) {
            let len = self.name_input().map_or(0, |n| n.text().chars().count());
            for _ in 0..len {
                self.keys(&[KeyCode::Backspace]);
            }
            self.chars(name);
        }
    }

    #[test]
    fn rename_prefills_and_renames_the_file() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.write("Running", &["B"])?;
        let before = fs::read(m.file("Running"))?;
        m.keys(&[KeyCode::Tab]);
        m.select_playlist("Running")?;
        m.shift('R');
        assert_eq!(m.name_input().map(NameInput::text), Some("Running"));
        m.retype("Morning run");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.listed(), ["Gym", "Morning run"]);
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Morning run"));
        assert!(!m.file("Running").exists());
        assert_eq!(
            fs::read(m.file("Morning run"))?,
            before,
            "content untouched"
        );
        let status = m.status();
        assert!(
            status.contains("Renamed \"Running\" to \"Morning run\""),
            "{status}"
        );
        Ok(())
    }

    #[test]
    fn rename_prefill_turns_special_spaces_into_plain_spaces() -> Result<()> {
        let mut m = abcd()?;
        m.write("a\u{a0}b\u{2028}c", &["A"])?;
        m.select_playlist("a\u{a0}b\u{2028}c")?;
        m.shift('R');
        assert_eq!(m.name_input().map(NameInput::text), Some("a b c"));
        m.keys(&[KeyCode::Enter]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.listed(), ["a b c"]);
        assert!(!m.file("a\u{a0}b\u{2028}c").exists());
        assert!(m.file("a b c").exists());
        Ok(())
    }

    #[test]
    fn cancel_rename_keeps_the_name() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["B"])?;
        m.select_playlist("Running")?;
        m.shift('R');
        m.retype("Other");
        m.keys(&[KeyCode::Esc]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.listed(), ["Running"]);
        assert!(m.file("Running").exists());
        assert!(!m.file("Other").exists());
        Ok(())
    }

    #[test]
    fn rename_unchanged_does_nothing() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["B"])?;
        m.select_playlist("Running")?;
        m.shift('R');
        m.keys(&[KeyCode::Enter]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.listed(), ["Running"]);
        Ok(())
    }

    #[test]
    fn rename_may_change_only_the_case() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["B"])?;
        m.select_playlist("Running")?;
        m.shift('R');
        m.retype("RUNNING");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.listed(), ["RUNNING"]);
        Ok(())
    }

    #[test]
    fn rename_refuses_taken_and_invalid_names_and_keeps_the_prompt() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.write("Running", &["B"])?;
        m.keys(&[KeyCode::Tab]);
        m.select_playlist("Running")?;
        m.shift('R');
        m.retype("gym");
        m.keys(&[KeyCode::Enter]);
        let error = m.name_input().and_then(NameInput::error).map(str::to_owned);
        assert!(
            error.as_deref().is_some_and(|e| e.contains("already used")),
            "{error:?}"
        );
        let status = m.status();
        assert!(status.contains("Playlist not renamed"), "{status}");

        m.retype("a:b");
        m.keys(&[KeyCode::Enter]);
        let error = m.name_input().and_then(NameInput::error).map(str::to_owned);
        assert!(
            error.as_deref().is_some_and(|e| e.contains("':'")),
            "{error:?}"
        );
        assert_eq!(m.listed(), ["Gym", "Running"]);
        Ok(())
    }

    #[test]
    fn renaming_the_playing_playlist_keeps_playing_it() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Enter, KeyCode::Esc]);
        m.shift('R');
        m.retype("Morning");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.playing_title().as_deref(), Some("A"));
        assert!(m.app.playback().is_playing_playlist("Morning"));
        // Still the same play order, and edits under the new name follow it.
        m.keys(&[KeyCode::Enter]);
        assert_eq!(
            m.app.open_playlist().map(OpenPlaylist::name),
            Some("Morning")
        );
        m.select_row(1);
        m.chars("d");
        m.end_track();
        assert_eq!(
            m.app.playback().state(),
            crate::playback::PlayState::Stopped,
            "B was removed, so the order ends after A"
        );
        Ok(())
    }

    #[test]
    fn rename_of_a_playlist_deleted_meanwhile_reports_it() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["B"])?;
        m.select_playlist("Running")?;
        m.shift('R');
        fs::remove_file(m.file("Running"))?;
        m.retype("Other");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert!(!m.file("Other").exists());
        let status = m.status();
        assert!(status.contains("no longer exists"), "{status}");
        Ok(())
    }

    #[test]
    fn v_in_prompts_keeps_their_meaning() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.select_playlist("Gym")?;
        m.shift('D');
        m.chars("v");
        assert_eq!(*m.app.mode(), Mode::Normal, "v cancels the delete");
        assert!(m.file("Gym").exists());
        assert!(m.app.visualizer_on());

        m.shift('R');
        m.chars("v");
        let Mode::NameInput(input) = m.app.mode() else {
            bail!("not renaming");
        };
        assert_eq!(input.text(), "Gymv", "v is typed into the name");
        assert!(m.app.visualizer_on());
        Ok(())
    }

    #[test]
    fn confirm_delete_removes_only_the_playlist_file() -> Result<()> {
        let mut m = abcd()?;
        let songs: Vec<PathBuf> = m
            .app
            .playback()
            .tracks()
            .iter()
            .map(|t| t.path.clone())
            .collect();
        for song in &songs {
            if let Some(dir) = song.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::write(song, b"audio")?;
        }
        m.write("Gym", &["A", "B"])?;
        m.write("Running", &["C"])?;
        m.keys(&[KeyCode::Tab]);
        m.select_playlist("Gym")?;
        m.shift('D');
        assert_eq!(*m.app.mode(), Mode::ConfirmDelete("Gym".to_owned()));
        m.chars("y");
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.listed(), ["Running"]);
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Running"));
        assert!(!m.file("Gym").exists());
        for song in &songs {
            assert_eq!(fs::read(song)?, b"audio", "{}", song.display());
        }
        assert_eq!(
            m.app.playback().tracks().len(),
            5,
            "songs stay in the library"
        );
        let status = m.status();
        assert!(status.contains("Deleted playlist \"Gym\""), "{status}");
        Ok(())
    }

    #[test]
    fn any_other_key_cancels_delete() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.select_playlist("Gym")?;
        for cancel in [
            KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT),
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ] {
            m.shift('D');
            m.app.handle_key(cancel);
            assert_eq!(*m.app.mode(), Mode::Normal);
            assert_eq!(m.listed(), ["Gym"]);
            assert!(m.file("Gym").exists());
        }
        let status = m.status();
        assert!(status.contains("\"Gym\" was not deleted"), "{status}");
        Ok(())
    }

    #[test]
    fn deleting_the_playing_playlist_keeps_playing() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Enter, KeyCode::Esc]);
        m.shift('D');
        m.chars("y");
        assert!(!m.file("Running").exists());
        assert_eq!(m.playing_title().as_deref(), Some("A"));
        assert!(!m.app.playback().is_playing_playlist("Running"));
        m.end_track();
        assert_eq!(m.playing_title().as_deref(), Some("B"), "its order goes on");
        Ok(())
    }

    #[test]
    fn failed_delete_is_reported_and_keeps_the_playlist() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.keys(&[KeyCode::Tab]);
        // A directory with the playlist's name cannot be deleted as a file;
        // it is not listed either, so the store is asked directly.
        fs::remove_file(m.file("Gym"))?;
        fs::create_dir(m.file("Gym"))?;
        m.app.mode = Mode::ConfirmDelete("Gym".to_owned());
        m.chars("y");
        assert!(m.file("Gym").is_dir());
        let status = m.status();
        assert!(status.contains("\"Gym\""), "{status}");
        assert!(status.contains("cannot delete"), "{status}");
        Ok(())
    }

    #[test]
    fn rename_and_delete_keys_only_act_in_the_playlists_list() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.shift('R');
        m.shift('D');
        assert_eq!(*m.app.mode(), Mode::Normal, "not in the library");
        m.open("Running")?;
        m.shift('R');
        m.shift('D');
        assert_eq!(*m.app.mode(), Mode::Normal, "not in an open playlist");
        m.chars("d");
        assert_eq!(m.app.view(), View::Playlist);
        m.show_library();
        m.chars("dJK");
        assert_eq!(
            m.app.view(),
            View::Library,
            "d/J/K do nothing in the library"
        );
        Ok(())
    }

    // --- Rename follows the shown playlist (task 2.4) ---

    /// Spec *Rename the shown playlist*: later edits save to the new file.
    #[test]
    fn renaming_the_shown_playlist_keeps_it_shown_under_the_new_name() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B", "C"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Esc]);
        m.shift('R');
        m.retype("Morning run");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(
            m.app.open_playlist().map(OpenPlaylist::name),
            Some("Morning run")
        );
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.shown_titles(), ["A", "B", "C"]);
        m.chars("d");
        assert_eq!(m.file_titles("Morning run")?, ["B", "C"]);
        assert!(!m.file("Running").exists(), "not recreated");
        Ok(())
    }

    #[test]
    fn renaming_the_shown_playing_playlist_keeps_the_play_order() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["C", "A", "B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Down, KeyCode::Enter, KeyCode::Esc]);
        let items = m.app.playback().items().to_vec();
        m.shift('R');
        m.retype("Morning");
        m.keys(&[KeyCode::Enter]);
        assert!(m.app.playback().is_playing_playlist("Morning"));
        assert_eq!(m.app.playback().items(), items);
        assert_eq!(m.playing_title().as_deref(), Some("A"));
        m.end_track();
        assert_eq!(m.playing_title().as_deref(), Some("B"));
        Ok(())
    }

    /// A store whose renamed file vanishes at once, so reading it back fails.
    #[derive(Debug)]
    struct VanishingRename(FsStore, PathBuf);

    impl PlaylistStore for VanishingRename {
        fn list(&self, r: &Resolver) -> Result<Vec<PlaylistInfo>, StoreError> {
            self.0.list(r)
        }
        fn load(&self, name: &str, r: &Resolver) -> Result<Playlist, StoreError> {
            self.0.load(name, r)
        }
        fn changed(&self, p: &Playlist) -> Result<bool, StoreError> {
            self.0.changed(p)
        }
        fn blank(&self, name: &str) -> Result<Playlist, StoreError> {
            self.0.blank(name)
        }
        fn create(&self, p: &mut Playlist) -> Result<(), StoreError> {
            self.0.create(p)
        }
        fn save(&self, p: &mut Playlist) -> Result<SaveOutcome, StoreError> {
            self.0.save(p)
        }
        fn rename(&self, from: &str, to: &str) -> Result<(), StoreError> {
            self.0.rename(from, to)?;
            fs::remove_file(self.1.join(format!("{to}.m3u8"))).map_err(|_| StoreError::NotFound)
        }
        fn delete(&self, name: &str) -> Result<(), StoreError> {
            self.0.delete(name)
        }
    }

    #[test]
    fn failed_read_after_renaming_the_shown_playlist_shows_the_library() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        let tracks = m.app.playback().tracks().to_vec();
        m.app = App::new(
            tracks,
            SilentAudio::default(),
            Box::new(VanishingRename(FsStore::new(&m.root), m.dir())),
            String::new(),
            None,
        );
        m.open("Running")?;
        m.keys(&[KeyCode::Esc]);
        m.shift('R');
        m.retype("Morning");
        m.keys(&[KeyCode::Enter]);
        assert!(m.app.open_playlist().is_none());
        assert_eq!(m.app.main_view(), MainView::Library);
        let now = Instant::now();
        m.app.tick(now);
        let notice = m.app.status(now).cloned();
        let text = notice.as_ref().map_or("", |n| n.text.as_str());
        assert!(text.starts_with("Playlist \"Morning\":"), "{text}");
        assert!(
            text.ends_with("; Renamed \"Running\" to \"Morning\""),
            "the rename is still reported: {text}"
        );
        assert_eq!(notice.map(|n| n.level), Some(Level::Error));
        Ok(())
    }

    // --- The sidebar stays current (task 2.5) ---

    /// Spec *Sidebar kept current*.
    #[test]
    fn sidebar_follows_the_apps_own_edits() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B", "C"])?;
        m.restart(None);
        m.select_in_library("B")?;
        m.chars("a");
        m.create("Road trip");
        assert_eq!(m.app.focus(), Focus::Main);
        assert_eq!(m.listed(), ["Road trip", "Running"]);
        assert_eq!(m.info("Road trip").and_then(|i| i.tracks), Some(1));

        m.select_in_library("D")?;
        m.chars("a");
        m.pick("Road trip")?;
        assert_eq!(m.info("Road trip").and_then(|i| i.tracks), Some(2));

        m.open("Running")?;
        m.chars("d");
        assert_eq!(m.app.focus(), Focus::Main);
        assert_eq!(m.info("Running").and_then(|i| i.tracks), Some(2));
        Ok(())
    }

    #[test]
    fn sidebar_selection_stays_on_its_playlist_after_an_insert() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.select_playlist("Running")?;
        m.keys(&[KeyCode::Tab]);
        m.chars("a");
        m.create("Gym");
        assert_eq!(m.listed(), ["Gym", "Running"]);
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Running"));
        Ok(())
    }

    #[test]
    fn failed_edit_leaves_the_sidebar_entry() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A", "B", "C"])?;
        m.open("Running")?;
        let before = m.info("Running");
        fs::create_dir(
            m.dir()
                .join(format!(".Running.m3u8.tmp-{}", std::process::id())),
        )?;
        m.chars("d");
        assert_eq!(m.info("Running"), before);
        assert_eq!(before.and_then(|i| i.tracks), Some(3));
        Ok(())
    }

    /// Rename and delete do not rewrite a playlist's contents, so they are
    /// allowed on read-only playlists; only entry edits are refused.
    #[test]
    fn read_only_playlist_can_be_renamed_and_deleted() -> Result<()> {
        let mut m = abcd()?;
        let bytes = b"../Band/LP/A.mp3\n# caf\xe9\n";
        m.write_raw("Old", bytes)?;
        m.select_playlist("Old")?;
        m.shift('R');
        m.retype("Older");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.listed(), ["Older"]);
        assert_eq!(fs::read(m.file("Older"))?, bytes, "byte for byte");
        m.shift('D');
        m.chars("y");
        assert_eq!(m.listed(), [] as [String; 0]);
        assert!(!m.file("Older").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_playlist_rename_and_delete_keep_the_target() -> Result<()> {
        let mut m = abcd()?;
        let target = m.root.join("elsewhere.m3u8");
        fs::write(&target, "../Band/LP/A.mp3\n")?;
        fs::create_dir_all(m.dir())?;
        std::os::unix::fs::symlink(&target, m.file("Linked"))?;
        m.select_playlist("Linked")?;
        m.shift('R');
        m.retype("Moved");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.listed(), ["Moved"]);
        assert!(
            fs::symlink_metadata(m.file("Moved"))?
                .file_type()
                .is_symlink()
        );
        assert!(fs::symlink_metadata(m.file("Linked")).is_err());
        m.shift('D');
        m.chars("y");
        assert!(
            fs::symlink_metadata(m.file("Moved")).is_err(),
            "link removed"
        );
        assert_eq!(
            fs::read(&target)?,
            b"../Band/LP/A.mp3\n",
            "target untouched"
        );
        Ok(())
    }

    /// A playlist that links outside `Playlists/` can be played but not
    /// edited, from the picker or inside it, so its target is never written.
    #[cfg(unix)]
    #[test]
    fn playlist_linked_outside_is_refused_for_edits() -> Result<()> {
        let mut m = abcd()?;
        let target = m.root.join("outside.m3u8");
        let original = b"../Band/LP/A.mp3\n../Band/LP/B.mp3\n";
        fs::write(&target, original)?;
        fs::create_dir_all(m.dir())?;
        std::os::unix::fs::symlink(&target, m.file("Linked"))?;

        m.select_in_library("C")?;
        m.chars("a");
        let marked = m
            .picker()
            .and_then(|p| p.choices().iter().find(|i| i.name == "Linked"))
            .and_then(|i| i.read_only);
        assert_eq!(marked, Some(crate::playlist::ReadOnlyReason::LinksOutside));
        m.pick("Linked")?;
        let status = m.status();
        assert!(status.contains("outside the Playlists folder"), "{status}");
        m.keys(&[KeyCode::Esc]);

        m.open("Linked")?;
        m.chars("d");
        m.shift('J');
        let status = m.status();
        assert!(status.contains("outside the Playlists folder"), "{status}");
        assert_eq!(fs::read(&target)?, original);
        Ok(())
    }

    /// The link is repointed outside after the playlist was opened: the edit
    /// is refused, nothing is written, and the playlist now shows read-only.
    #[cfg(unix)]
    #[test]
    fn link_repointed_outside_while_open_is_reloaded_read_only() -> Result<()> {
        let mut m = abcd()?;
        fs::create_dir_all(m.dir())?;
        let inside = m.dir().join("real.m3u8.bak");
        let content = b"../Band/LP/A.mp3\n../Band/LP/B.mp3\n";
        fs::write(&inside, content)?;
        std::os::unix::fs::symlink(&inside, m.file("Linked"))?;
        m.open("Linked")?;
        assert!(
            m.app
                .open_playlist()
                .is_some_and(|o| o.playlist().editable().is_ok())
        );

        let outside = m.root.join("outside.m3u8");
        fs::write(&outside, content)?;
        fs::remove_file(m.file("Linked"))?;
        std::os::unix::fs::symlink(&outside, m.file("Linked"))?;
        m.chars("d");
        let status = m.status();
        assert!(status.contains("outside the Playlists folder"), "{status}");
        assert_eq!(
            m.app.open_playlist().map(|o| o.playlist().editable()),
            Some(Err(crate::playlist::ReadOnlyReason::LinksOutside))
        );
        assert_eq!(fs::read(&outside)?, content);
        assert_eq!(fs::read(&inside)?, content);
        Ok(())
    }

    // --- Help overlay ---

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    fn press(app: &mut App<SilentAudio>, keys: &[KeyEvent]) {
        for &k in keys {
            app.handle_key(k);
        }
    }

    /// Lines in the help, checked to be longer than every test view below.
    fn help_total() -> usize {
        let total = keys::help_line_count();
        assert!(total > 12, "the help is longer than the test views");
        total
    }

    fn help_scroll(app: &App<SilentAudio>) -> Option<usize> {
        match app.mode() {
            Mode::Help { scroll } => Some(*scroll),
            _ => None,
        }
    }

    /// Spec *Open and close help*: `?` opens it, `q` closes it without
    /// quitting.
    #[test]
    fn question_mark_opens_help_and_q_closes_it_without_quitting() -> Result<()> {
        let mut m = abcd()?;
        let a = &mut m.app;
        press(a, &[ch('?')]);
        assert_eq!(*a.mode(), Mode::Help { scroll: 0 });
        press(a, &[ch('q')]);
        assert_eq!(*a.mode(), Mode::Normal);
        assert!(!a.should_quit(), "q only closed the overlay");
        press(a, &[ch('q')]);
        assert!(a.should_quit(), "q quits again once closed");
        Ok(())
    }

    #[test]
    fn shifted_question_mark_opens_help() -> Result<()> {
        let mut m = abcd()?;
        let a = &mut m.app;
        a.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT));
        assert_eq!(help_scroll(a), Some(0));
        Ok(())
    }

    #[test]
    fn help_opens_from_the_sidebar_too() -> Result<()> {
        let mut m = abcd()?;
        m.keys(&[KeyCode::Tab]);
        m.chars("?");
        assert_eq!(help_scroll(&m.app), Some(0));
        m.keys(&[KeyCode::Esc]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.app.view(), View::Playlists, "focus unchanged");
        Ok(())
    }

    #[test]
    fn help_keys_close_and_are_consumed() -> Result<()> {
        for close in [
            ch('?'),
            key(KeyCode::Esc),
            ch('q'),
            ch(' '),
            ch('n'),
            ch('r'),
            ch('s'),
            ch('z'),
            ch('/'),
            key(KeyCode::Tab),
            key(KeyCode::Left),
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::ALT),
        ] {
            let mut m = abcd()?;
            let a = &mut m.app;
            press(a, &[ch('?'), close]);
            assert_eq!(*a.mode(), Mode::Normal, "{close:?} closes help");
            assert_eq!(a.focus(), Focus::Main, "{close:?} did nothing else");
            assert_eq!(a.selected(), Some(0), "{close:?} did nothing else");
            assert_eq!(a.query(), "", "{close:?} did nothing else");
            assert_eq!(a.playback().current(), None, "{close:?} did nothing else");
            assert!(!a.should_quit(), "{close:?} does not quit");
            let now = Instant::now();
            a.tick(now);
            assert!(a.status(now).is_none(), "{close:?} raised no notice");
        }
        Ok(())
    }

    #[test]
    fn question_mark_with_ctrl_or_alt_does_not_open_help() -> Result<()> {
        let mut m = abcd()?;
        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            m.app
                .handle_key(KeyEvent::new(KeyCode::Char('?'), modifiers));
            assert_eq!(*m.app.mode(), Mode::Normal, "{modifiers:?}+?");
        }
        Ok(())
    }

    #[test]
    fn repeated_j_scrolls_help() -> Result<()> {
        let mut m = abcd()?;
        let a = &mut m.app;
        a.set_help_rows(5);
        press(a, &[ch('?')]);
        a.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('j'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));
        assert_eq!(help_scroll(a), Some(1));
        Ok(())
    }

    #[test]
    fn ctrl_c_quits_from_help() -> Result<()> {
        let mut m = abcd()?;
        let a = &mut m.app;
        press(a, &[ch('?')]);
        a.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(a.should_quit());
        Ok(())
    }

    #[test]
    fn key_releases_leave_help_open() -> Result<()> {
        let mut m = abcd()?;
        let a = &mut m.app;
        press(a, &[ch('?')]);
        a.handle_key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert_eq!(help_scroll(a), Some(0));
        Ok(())
    }

    /// `Enter` with the overlay open neither plays (library) nor opens
    /// (sidebar) anything: it only closes the overlay.
    #[test]
    fn enter_in_help_neither_plays_nor_opens() -> Result<()> {
        let mut m = abcd()?;
        m.write("Gym", &["A"])?;
        m.chars("?");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.app.playback().current(), None, "nothing played");

        m.select_playlist("Gym")?;
        m.chars("?");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(*m.app.mode(), Mode::Normal);
        assert_eq!(m.app.view(), View::Playlists, "focus stays on the sidebar");
        assert!(m.app.open_playlist().is_none(), "Gym was not opened");
        Ok(())
    }

    /// Spec *Not while typing*: `?` is a search character in Search mode.
    #[test]
    fn question_mark_while_searching_is_typed() -> Result<()> {
        let mut m = abcd()?;
        let a = &mut m.app;
        press(a, &[ch('/'), ch('?')]);
        assert_eq!(*a.mode(), Mode::Search);
        assert_eq!(a.query(), "?");
        Ok(())
    }

    #[test]
    fn question_mark_in_a_name_prompt_is_typed() -> Result<()> {
        let mut m = abcd()?;
        m.chars("a");
        m.keys(&[KeyCode::Enter]); // "New playlist…"
        m.chars("Why?");
        assert_eq!(m.name_input().map(NameInput::text), Some("Why?"));
        Ok(())
    }

    #[test]
    fn help_scroll_stops_at_the_last_page_and_k_moves_back_at_once() -> Result<()> {
        let total = help_total();
        let rows = 5;
        let last = total - rows;
        let mut m = abcd()?;
        let a = &mut m.app;
        a.set_help_rows(rows);
        press(a, &[ch('?'), ch('j'), key(KeyCode::Down)]);
        assert_eq!(help_scroll(a), Some(2));
        press(a, &[ch('k'), key(KeyCode::Up), key(KeyCode::Up)]);
        assert_eq!(help_scroll(a), Some(0), "stops at the top");

        for _ in 0..total * 2 {
            press(a, &[ch('j')]);
        }
        assert_eq!(help_scroll(a), Some(last), "stops at the last page");
        press(a, &[ch('k')]);
        assert_eq!(help_scroll(a), Some(last - 1), "k moves back at once");
        Ok(())
    }

    #[test]
    fn help_page_and_jump_keys() -> Result<()> {
        let total = help_total();
        let rows = 4;
        let last = total - rows;
        let mut m = abcd()?;
        let a = &mut m.app;
        a.set_help_rows(rows);
        press(a, &[ch('?'), key(KeyCode::PageDown)]);
        assert_eq!(help_scroll(a), Some(rows), "PgDn moves by help_rows");
        press(a, &[key(KeyCode::PageUp)]);
        assert_eq!(help_scroll(a), Some(0));
        press(a, &[key(KeyCode::PageUp)]);
        assert_eq!(help_scroll(a), Some(0), "PgUp stops at the top");
        a.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT));
        assert_eq!(help_scroll(a), Some(last));
        press(a, &[key(KeyCode::PageDown)]);
        assert_eq!(help_scroll(a), Some(last), "PgDn stops at the last page");
        press(a, &[ch('g')]);
        assert_eq!(help_scroll(a), Some(0));
        press(a, &[key(KeyCode::End)]);
        assert_eq!(help_scroll(a), Some(last));
        press(a, &[key(KeyCode::Home)]);
        assert_eq!(help_scroll(a), Some(0));
        Ok(())
    }

    #[test]
    fn help_that_fits_does_not_scroll() -> Result<()> {
        let mut m = abcd()?;
        let a = &mut m.app;
        a.set_help_rows(keys::help_line_count() + 10);
        press(
            a,
            &[ch('?'), ch('j'), key(KeyCode::PageDown), key(KeyCode::End)],
        );
        assert_eq!(help_scroll(a), Some(0));
        Ok(())
    }

    #[test]
    fn taller_help_rows_reclamp_the_scroll() -> Result<()> {
        let total = help_total();
        let mut m = abcd()?;
        let a = &mut m.app;
        a.set_help_rows(3);
        press(a, &[ch('?'), key(KeyCode::End)]);
        assert_eq!(help_scroll(a), Some(total - 3));
        a.set_help_rows(10);
        assert_eq!(help_scroll(a), Some(total - 10), "re-clamped on resize");
        a.set_help_rows(total + 5);
        assert_eq!(help_scroll(a), Some(0), "everything fits");
        a.set_help_rows(3);
        assert_eq!(help_scroll(a), Some(0), "a shorter view keeps the scroll");
        Ok(())
    }

    #[test]
    fn help_rows_is_at_least_one_and_kept_between_openings() -> Result<()> {
        let total = help_total();
        let mut m = abcd()?;
        let a = &mut m.app;
        a.set_help_rows(0);
        press(a, &[ch('?'), key(KeyCode::PageDown)]);
        assert_eq!(help_scroll(a), Some(1), "a zero height counts as 1");
        a.set_help_rows(6);
        press(a, &[key(KeyCode::Esc), ch('?')]);
        assert_eq!(help_scroll(a), Some(0), "reopens at the top");
        press(a, &[key(KeyCode::End)]);
        assert_eq!(help_scroll(a), Some(total - 6), "6 rows kept");
        Ok(())
    }

    #[test]
    fn help_rows_outside_help_change_nothing_else() -> Result<()> {
        let mut m = abcd()?;
        let a = &mut m.app;
        a.set_help_rows(2);
        assert_eq!(*a.mode(), Mode::Normal);
        press(a, &[ch('/')]);
        a.set_help_rows(1);
        assert_eq!(*a.mode(), Mode::Search);
        Ok(())
    }
}

/// The visualizer state, `v`, and how the bars follow playback.
#[cfg(test)]
mod visualizer_tests {
    use super::*;
    use crate::test_support::{ToneAudio, no_store, track};
    use crate::visualizer::FALL_TIME;

    /// About one animation frame.
    const STEP: Duration = Duration::from_millis(33);
    const BARS: usize = 40;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    /// Two tracks over a 1 kHz tone at half scale (about −6 dB), before the
    /// first frame: the strip's size is not known yet.
    fn tone_app() -> App<ToneAudio> {
        App::new(
            vec![track("A", "Band", "LP"), track("B", "Band", "LP")],
            ToneAudio::new(1_000.0, 0.5),
            no_store(),
            "/music".to_owned(),
            None,
        )
    }

    /// As [`tone_app`], with the strip drawn with [`BARS`] bars.
    fn shown() -> App<ToneAudio> {
        let mut a = tone_app();
        a.set_spectrum_layout(true, BARS);
        a
    }

    /// Ticks `frames` times, [`STEP`] apart, starting at `from`; returns the
    /// time of the last tick.
    fn run(a: &mut App<ToneAudio>, from: Instant, frames: u32) -> Instant {
        let mut now = from;
        for i in 0..frames {
            now = from + STEP * i;
            a.tick(now);
        }
        now
    }

    fn peak(a: &App<ToneAudio>) -> f32 {
        a.spectrum_levels().iter().copied().fold(0.0, f32::max)
    }

    fn idle(a: &App<ToneAudio>) -> bool {
        a.spectrum_levels().iter().all(|&level| level == 0.0)
    }

    /// The status text after a tick at `now`.
    fn status(a: &mut App<ToneAudio>, now: Instant) -> Option<String> {
        a.tick(now);
        a.status(now).map(|n| n.text.clone())
    }

    /// Plays the first track and ticks until the bars are up; returns the
    /// time of the last tick.
    fn playing(a: &mut App<ToneAudio>, t0: Instant) -> Instant {
        a.handle_key(key(KeyCode::Enter));
        assert_eq!(a.playback().state(), PlayState::Playing);
        // The tone fills the tap over a few ticks.
        run(a, t0, 4)
    }

    #[test]
    fn on_at_start_and_not_animating_before_the_first_frame() {
        let a = tone_app();
        assert!(a.visualizer_on());
        assert!(!a.visualizer_animating());
        assert_eq!(a.spectrum_levels(), [] as [f32; 0]);
    }

    #[test]
    fn bars_rise_while_playing() {
        let mut a = shown();
        playing(&mut a, Instant::now());
        assert_eq!(a.spectrum_levels().len(), BARS);
        assert!(peak(&a) > 0.5, "{:?}", a.spectrum_levels());
        assert!(a.visualizer_animating());
    }

    #[test]
    fn pause_makes_the_bars_fall_to_idle_in_about_a_second() {
        let mut a = shown();
        let t = playing(&mut a, Instant::now());
        let top = peak(&a);
        assert!(top > 0.5);

        a.handle_key(ch(' '));
        assert_eq!(a.playback().state(), PlayState::Paused);
        a.tick(t + FALL_TIME / 2);
        assert!((peak(&a) - (top - 0.5)).abs() < 0.01, "{}", peak(&a));
        assert!(a.visualizer_animating(), "still falling");
        a.tick(t + FALL_TIME);
        assert!(idle(&a), "{:?}", a.spectrum_levels());
        assert!(!a.visualizer_animating(), "paused and idle");
        a.tick(t + FALL_TIME * 3);
        assert!(idle(&a), "stays at zero while paused");

        a.handle_key(ch(' '));
        run(&mut a, t + FALL_TIME * 4, 3);
        assert!(peak(&a) > 0.5, "back when resumed");
    }

    #[test]
    fn stop_makes_the_bars_fall_too() {
        let mut a = shown();
        let t = playing(&mut a, Instant::now());
        a.handle_key(ch('s'));
        a.tick(t + FALL_TIME);
        assert!(idle(&a));
        assert!(!a.visualizer_animating());
    }

    #[test]
    fn nothing_played_yet_shows_no_bars() {
        let mut a = shown();
        run(&mut a, Instant::now(), 10);
        assert_eq!(a.spectrum_levels().len(), BARS);
        assert!(idle(&a));
        assert!(!a.visualizer_animating());
    }

    #[test]
    fn a_new_track_drops_the_old_tracks_audio() {
        let mut a = shown();
        let t = playing(&mut a, Instant::now());
        let top = peak(&a);
        a.handle_key(ch('n'));
        assert_eq!(a.playback().current(), Some(1));
        // The new track's tap holds too little audio for a frame yet: the
        // old bars fall instead of being measured again.
        // Re-measuring the old window would put the bars straight back at
        // `top`; instead they fall by one step. (`ToneAudio`, like `Player`,
        // starts each load with an empty tap.)
        a.tick(t + STEP);
        assert!(peak(&a) < top - 0.02, "{} vs {top}", peak(&a));
    }

    #[test]
    fn resuming_does_not_show_the_window_from_before_the_pause() {
        let mut a = shown();
        let t = playing(&mut a, Instant::now());
        a.handle_key(ch(' '));
        a.tick(t + FALL_TIME);
        assert!(idle(&a));

        // Resumed, but no new audio has reached the tap yet.
        a.playback_mut().audio_mut().advancing = false;
        a.handle_key(ch(' '));
        assert_eq!(a.playback().state(), PlayState::Playing);
        run(&mut a, t + FALL_TIME * 2, 3);
        assert!(
            idle(&a),
            "old window shown again: {:?}",
            a.spectrum_levels()
        );

        a.playback_mut().audio_mut().advancing = true;
        run(&mut a, t + FALL_TIME * 3, 2);
        assert!(peak(&a) > 0.5, "new audio shows");
    }

    #[test]
    fn turned_off_before_playing_measures_nothing() {
        let mut a = shown();
        a.handle_key(ch('v'));
        playing(&mut a, Instant::now());
        assert_eq!(a.spectrum_levels(), [] as [f32; 0], "no update ever ran");
        assert!(!a.visualizer_animating());
    }

    #[test]
    fn no_bars_means_not_animating() {
        let mut a = tone_app();
        a.set_spectrum_layout(true, 0);
        playing(&mut a, Instant::now());
        assert_eq!(a.spectrum_levels(), [] as [f32; 0]);
        assert!(!a.visualizer_animating());
    }

    #[test]
    fn a_new_load_is_fresh_even_when_its_count_matches_the_old_tap() {
        let mut a = shown();
        // One call fills a whole window, so the old and the new tap both
        // report `written == FFT_SIZE` on their first tick.
        a.playback_mut().audio_mut().step = FFT_SIZE as u64;
        let t0 = Instant::now();
        a.handle_key(key(KeyCode::Enter));
        a.tick(t0);
        let top = peak(&a);
        assert!(top > 0.5, "{:?}", a.spectrum_levels());

        // A new load (next track) long after the old count was first seen:
        // its audio is new, so it is measured, not treated as stale.
        a.handle_key(ch('n'));
        assert_eq!(a.playback().state(), PlayState::Playing);
        a.tick(t0 + STALE_AFTER * 2);
        assert!((peak(&a) - top).abs() < 0.01, "measured: {}", peak(&a));
    }

    #[test]
    fn a_stalled_tap_lets_the_bars_fall_while_playing() {
        let mut a = shown();
        let t = playing(&mut a, Instant::now());
        let top = peak(&a);
        a.playback_mut().audio_mut().advancing = false;

        // `written` last changed at `t`: until STALE_AFTER after that, the
        // same window is still measured and the bars hold.
        a.tick(t + STEP);
        a.tick(t + STALE_AFTER.saturating_sub(Duration::from_millis(1)));
        assert!((peak(&a) - top).abs() < 0.01, "held: {}", peak(&a));

        // From then on it is stale, and the bars fall although still Playing.
        let stale = t + STALE_AFTER;
        a.tick(stale);
        a.tick(stale + FALL_TIME / 2);
        assert!(peak(&a) < top - 0.4, "falling: {}", peak(&a));
        a.tick(stale + FALL_TIME);
        assert!(idle(&a), "{:?}", a.spectrum_levels());
        assert_eq!(a.playback().state(), PlayState::Playing);
        assert!(a.visualizer_animating(), "playing keeps the frame rate up");

        a.playback_mut().audio_mut().advancing = true;
        run(&mut a, stale + FALL_TIME * 2, 2);
        assert!(peak(&a) > 0.5, "back when audio flows again");
    }

    #[test]
    fn nothing_is_measured_while_the_strip_does_not_fit() {
        let mut a = tone_app();
        a.set_spectrum_layout(false, 0);
        let t = playing(&mut a, Instant::now());
        assert_eq!(a.spectrum_levels(), [] as [f32; 0], "no update ever ran");
        assert!(!a.visualizer_animating());

        // Bars fed back without room (inconsistent, but harmless).
        a.set_spectrum_layout(false, BARS);
        run(&mut a, t, 3);
        assert_eq!(a.spectrum_levels(), [] as [f32; 0]);
        assert!(!a.visualizer_animating());
    }

    #[test]
    fn hiding_the_strip_resets_the_bars() {
        let mut a = shown();
        let t = playing(&mut a, Instant::now());
        assert!(peak(&a) > 0.5);

        a.set_spectrum_layout(false, 0);
        a.tick(t + STEP);
        assert!(idle(&a), "too short: reset at once, no fall");
        assert!(!a.visualizer_animating());

        a.set_spectrum_layout(true, BARS);
        a.handle_key(ch('v'));
        assert!(!a.visualizer_animating(), "off while playing");
        a.tick(t + STEP * 2);
        assert!(idle(&a));
    }

    #[test]
    fn v_toggles_without_a_notice_while_the_strip_is_visible() {
        let mut a = shown();
        let now = Instant::now();
        a.handle_key(ch('v'));
        assert!(!a.visualizer_on());
        assert_eq!(status(&mut a, now), None);
        a.handle_key(ch('v'));
        assert!(a.visualizer_on());
        assert_eq!(status(&mut a, now), None);
    }

    #[test]
    fn v_works_from_the_sidebar_too() {
        let mut a = shown();
        a.handle_key(key(KeyCode::Tab));
        assert_eq!(a.focus(), Focus::Sidebar);
        a.handle_key(ch('v'));
        assert!(!a.visualizer_on());
    }

    #[test]
    fn v_announces_the_change_while_the_terminal_is_too_short() {
        let mut a = tone_app();
        a.set_spectrum_layout(false, 0);
        let now = Instant::now();
        a.handle_key(ch('v'));
        assert!(!a.visualizer_on());
        assert_eq!(status(&mut a, now).as_deref(), Some("Visualizer off"));

        a.handle_key(ch('v'));
        assert!(a.visualizer_on(), "on, waiting for room");
        assert_eq!(
            status(&mut a, now).as_deref(),
            Some("Visualizer on: needs a terminal at least 24 rows tall")
        );

        // The strip shows once the terminal is tall enough.
        a.set_spectrum_layout(true, BARS);
        assert!(a.visualizer_on());
    }

    #[test]
    fn v_with_ctrl_or_alt_does_nothing() {
        let mut a = shown();
        a.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT));
        a.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL));
        assert!(a.visualizer_on());
    }

    #[test]
    fn v_is_typed_into_a_search() {
        let mut a = shown();
        a.handle_key(ch('/'));
        a.handle_key(ch('v'));
        assert_eq!(a.query(), "v");
        assert!(a.visualizer_on());
    }

    #[test]
    fn v_closes_the_help_overlay_and_does_nothing_else() {
        let mut a = shown();
        a.handle_key(ch('?'));
        a.handle_key(ch('v'));
        assert_eq!(*a.mode(), Mode::Normal);
        assert!(a.visualizer_on());
    }

    #[test]
    fn v_in_the_picker_and_name_prompt_keeps_their_meaning() -> anyhow::Result<()> {
        let mut a = shown();
        a.handle_key(ch('a'));
        assert!(matches!(a.mode(), Mode::Picker(_)));
        a.handle_key(ch('v'));
        assert!(matches!(a.mode(), Mode::Picker(_)), "not a picker key");
        a.handle_key(key(KeyCode::Enter));
        a.handle_key(ch('v'));
        let Mode::NameInput(input) = a.mode() else {
            anyhow::bail!("not naming a playlist: {:?}", a.mode());
        };
        assert_eq!(input.text(), "v");
        assert!(a.visualizer_on());
        Ok(())
    }
}

/// The lyrics pane's state, `l`, and the lyric deadline.
#[cfg(test)]
mod lyrics_tests {
    use super::*;
    use crate::lyrics::Line;
    use crate::test_support::{SilentAudio, no_store, wav_bytes};
    use anyhow::{Result, bail};
    use std::fs;
    use std::path::Path;

    const SYNCED: &str = "[ar:Band]\n[00:01.00]First\n[00:03.00]Second\n";
    const PLAIN: &str = "Just words\nNo timing\n";

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    /// A folder with three tracks in library order: `one.wav` with a synced
    /// sidecar, `two.wav` with no lyrics at all, and `three.wav` with a
    /// plain sidecar. The pane is on, but no frame has been drawn yet.
    struct Folder {
        tmp: tempfile::TempDir,
        app: App<SilentAudio>,
    }

    impl Folder {
        fn new() -> Result<Self> {
            let tmp = tempfile::tempdir()?;
            let mut tracks = Vec::new();
            for (name, lyrics) in [("one", Some(SYNCED)), ("two", None), ("three", Some(PLAIN))] {
                let path = tmp.path().join(format!("{name}.wav"));
                fs::write(&path, wav_bytes())?;
                if let Some(text) = lyrics {
                    fs::write(path.with_extension("lrc"), text)?;
                }
                tracks.push(Track {
                    path,
                    title: name.to_owned(),
                    artist: "Band".to_owned(),
                    album: "LP".to_owned(),
                    number: None,
                    album_artist: None,
                    disc: None,
                    duration: Some(Duration::from_secs(200)),
                });
            }
            let app = App::new(
                tracks,
                SilentAudio::default(),
                no_store(),
                "/music".to_owned(),
                None,
            );
            Ok(Self { tmp, app })
        }

        /// As [`Folder::new`], with the pane drawn (wide enough).
        fn shown() -> Result<Self> {
            let mut f = Self::new()?;
            f.app.set_lyrics_layout(true);
            Ok(f)
        }

        fn sidecar(&self, name: &str) -> std::path::PathBuf {
            self.tmp.path().join(format!("{name}.lrc"))
        }

        fn press(&mut self, keys: &[KeyEvent]) {
            for &k in keys {
                self.app.handle_key(k);
            }
        }

        /// Plays the selected track and ticks once.
        fn play(&mut self, now: Instant) {
            self.press(&[key(KeyCode::Enter)]);
            self.app.tick(now);
        }

        fn set_position(&mut self, position: Duration) {
            self.app.playback_mut().audio_mut().position = position;
        }

        /// The status text after a tick at `now`.
        fn status(&mut self, now: Instant) -> Option<String> {
            self.app.tick(now);
            self.app.status(now).map(|n| n.text.clone())
        }
    }

    /// The text of every line, synced or plain.
    fn texts(lyrics: Option<&Lyrics>) -> Option<Vec<String>> {
        lyrics.map(|lyrics| match lyrics {
            Lyrics::Synced(lines) => lines.iter().map(|l| l.text.clone()).collect(),
            Lyrics::Plain(lines) => lines.clone(),
        })
    }

    fn first_text(app: &App<SilentAudio>) -> Option<String> {
        texts(app.lyrics()).and_then(|t| t.into_iter().next())
    }

    fn file_name(app: &App<SilentAudio>) -> Option<&str> {
        app.playback()
            .current_track()
            .and_then(|t| t.path.file_name())
            .and_then(|n| n.to_str())
    }

    #[test]
    fn on_at_start_with_nothing_loaded() -> Result<()> {
        let mut f = Folder::new()?;
        assert!(f.app.lyrics_on());
        assert!(!f.app.lyrics_shown(), "no frame yet");
        f.app.tick(Instant::now());
        assert!(!f.app.lyrics_ready(), "nothing playing");
        assert_eq!(f.app.lyrics(), None);
        Ok(())
    }

    #[test]
    fn lyrics_load_when_a_track_starts() -> Result<()> {
        let mut f = Folder::shown()?;
        assert!(f.app.lyrics_shown());
        f.press(&[key(KeyCode::Enter)]);
        assert_eq!(file_name(&f.app), Some("one.wav"));
        assert!(!f.app.lyrics_ready(), "not looked up before the tick");
        assert_eq!(f.app.lyrics(), None);

        f.app.tick(Instant::now());
        assert!(f.app.lyrics_ready());
        assert_eq!(
            f.app.lyrics(),
            Some(&Lyrics::Synced(vec![
                Line {
                    at: Duration::from_secs(1),
                    text: "First".to_owned()
                },
                Line {
                    at: Duration::from_secs(3),
                    text: "Second".to_owned()
                },
            ]))
        );
        Ok(())
    }

    #[test]
    fn seeking_does_not_read_again() -> Result<()> {
        let mut f = Folder::shown()?;
        let now = Instant::now();
        f.play(now);
        assert_eq!(first_text(&f.app).as_deref(), Some("First"));

        // Gone from disk: a re-read would find no lyrics.
        fs::remove_file(f.sidecar("one"))?;
        f.set_position(Duration::from_secs(10));
        f.press(&[key(KeyCode::Right)]);
        assert_eq!(
            f.app.playback().position(),
            Duration::from_secs(15),
            "reloaded at the new position"
        );
        f.app.tick(now);
        f.press(&[key(KeyCode::Left)]);
        assert_eq!(f.app.playback().position(), Duration::from_secs(10));
        f.app.tick(now);
        f.press(&[ch(' ')]);
        f.app.tick(now);
        f.press(&[ch(' ')]);
        f.app.tick(now);
        assert_eq!(f.app.playback().state(), PlayState::Playing);
        assert_eq!(file_name(&f.app), Some("one.wav"));
        assert_eq!(first_text(&f.app).as_deref(), Some("First"), "kept");
        Ok(())
    }

    #[test]
    fn replaying_the_same_track_after_stop_does_not_read_again() -> Result<()> {
        let mut f = Folder::shown()?;
        let now = Instant::now();
        f.play(now);
        f.press(&[ch('s')]);
        f.app.tick(now);
        assert_eq!(f.app.playback().state(), PlayState::Stopped);
        assert_eq!(
            first_text(&f.app).as_deref(),
            Some("First"),
            "still the last played track"
        );

        fs::remove_file(f.sidecar("one"))?;
        f.play(now);
        assert_eq!(file_name(&f.app), Some("one.wav"));
        assert_eq!(first_text(&f.app).as_deref(), Some("First"), "kept");
        Ok(())
    }

    #[test]
    fn auto_advance_reads_the_next_tracks_lyrics_in_the_same_tick() -> Result<()> {
        let mut f = Folder::shown()?;
        let now = Instant::now();
        f.press(&[ch('j'), key(KeyCode::Enter)]);
        f.app.tick(now);
        assert_eq!(file_name(&f.app), Some("two.wav"));
        assert!(f.app.lyrics_ready());
        assert_eq!(f.app.lyrics(), None);

        // The track plays out: one tick advances and reads.
        f.app.playback_mut().audio_mut().ended = true;
        f.app.tick(now);
        assert_eq!(file_name(&f.app), Some("three.wav"));
        assert!(f.app.lyrics_ready());
        assert_eq!(first_text(&f.app).as_deref(), Some("Just words"));
        Ok(())
    }

    #[test]
    fn next_track_without_lyrics_has_none() -> Result<()> {
        let mut f = Folder::shown()?;
        let now = Instant::now();
        f.play(now);
        assert!(f.app.lyrics().is_some());

        f.press(&[ch('n')]);
        assert_eq!(file_name(&f.app), Some("two.wav"));
        assert!(
            !f.app.lyrics_ready(),
            "old lyrics not shown as the new ones"
        );
        assert_eq!(f.app.lyrics(), None);
        f.app.tick(now);
        assert!(f.app.lyrics_ready());
        assert_eq!(f.app.lyrics(), None);

        f.press(&[ch('n')]);
        f.app.tick(now);
        assert_eq!(file_name(&f.app), Some("three.wav"));
        assert!(matches!(f.app.lyrics(), Some(Lyrics::Plain(_))));
        Ok(())
    }

    #[test]
    fn nothing_is_read_while_the_pane_does_not_fit() -> Result<()> {
        let mut f = Folder::new()?;
        f.app.set_lyrics_layout(false);
        let now = Instant::now();
        f.play(now);
        assert!(!f.app.lyrics_ready());
        assert!(f.app.lyrics.loaded_for.is_none(), "nothing read");

        // Widened: ready only after the next tick.
        f.app.set_lyrics_layout(true);
        assert!(!f.app.lyrics_ready());
        assert_eq!(f.app.lyrics(), None);
        f.app.tick(now);
        assert!(f.app.lyrics_ready());
        assert_eq!(first_text(&f.app).as_deref(), Some("First"));

        // Narrowed again: the cache is dropped at once.
        f.app.set_lyrics_layout(false);
        assert!(!f.app.lyrics_ready());
        assert!(f.app.lyrics.loaded_for.is_none());
        assert!(f.app.lyrics.lyrics.is_none());
        Ok(())
    }

    #[test]
    fn showing_again_reads_a_sidecar_edited_while_hidden() -> Result<()> {
        let mut f = Folder::shown()?;
        let now = Instant::now();
        f.play(now);
        assert_eq!(first_text(&f.app).as_deref(), Some("First"));

        f.press(&[ch('l')]);
        assert!(!f.app.lyrics_on());
        assert!(!f.app.lyrics_ready());
        fs::write(f.sidecar("one"), "[00:01.00]Edited\n")?;
        f.app.tick(now);
        assert!(f.app.lyrics.loaded_for.is_none(), "nothing read while off");

        f.press(&[ch('l')]);
        f.app.tick(now);
        assert_eq!(first_text(&f.app).as_deref(), Some("Edited"));
        Ok(())
    }

    #[test]
    fn deadline_is_the_time_until_the_next_line() -> Result<()> {
        let mut f = Folder::shown()?;
        let now = Instant::now();
        f.press(&[key(KeyCode::Enter)]);
        assert_eq!(f.app.lyrics_deadline(now), Some(now), "load due");
        f.app.tick(now);

        f.set_position(Duration::ZERO);
        assert_eq!(
            f.app.lyrics_deadline(now),
            Some(now + Duration::from_secs(1))
        );
        f.set_position(Duration::from_millis(1_500));
        assert_eq!(
            f.app.lyrics_deadline(now),
            Some(now + Duration::from_millis(1_500))
        );
        // Exactly on a line: the next one.
        f.set_position(Duration::from_secs(1));
        assert_eq!(
            f.app.lyrics_deadline(now),
            Some(now + Duration::from_secs(2))
        );
        // Just short of a line: never less than the minimum wait.
        f.set_position(Duration::from_millis(2_995));
        assert_eq!(f.app.lyrics_deadline(now), Some(now + LYRICS_MIN_WAIT));
        f.set_position(Duration::from_secs(3) - Duration::from_nanos(1));
        assert_eq!(f.app.lyrics_deadline(now), Some(now + LYRICS_MIN_WAIT));
        // After the last line nothing is due.
        f.set_position(Duration::from_secs(3));
        assert_eq!(f.app.lyrics_deadline(now), None);
        Ok(())
    }

    #[test]
    fn no_deadline_unless_playing_shown_and_synced() -> Result<()> {
        let mut f = Folder::shown()?;
        let now = Instant::now();
        f.play(now);
        assert!(f.app.lyrics_deadline(now).is_some());

        f.press(&[ch(' ')]);
        assert_eq!(f.app.playback().state(), PlayState::Paused);
        assert_eq!(f.app.lyrics_deadline(now), None, "paused");
        f.press(&[ch(' ')]);
        assert!(f.app.lyrics_deadline(now).is_some());

        f.press(&[ch('l')]);
        assert_eq!(f.app.lyrics_deadline(now), None, "hidden");
        f.press(&[ch('l')]);
        f.app.tick(now);
        assert!(f.app.lyrics_deadline(now).is_some());

        f.app.set_lyrics_layout(false);
        assert_eq!(f.app.lyrics_deadline(now), None, "too narrow");
        f.app.set_lyrics_layout(true);
        f.app.tick(now);
        assert!(f.app.lyrics_deadline(now).is_some());

        f.press(&[ch('s')]);
        assert_eq!(f.app.playback().state(), PlayState::Stopped);
        assert_eq!(f.app.lyrics_deadline(now), None, "stopped");

        // No lyrics, then plain lyrics.
        f.press(&[ch('j'), key(KeyCode::Enter)]);
        f.app.tick(now);
        assert_eq!(file_name(&f.app), Some("two.wav"));
        assert_eq!(f.app.lyrics_deadline(now), None, "no lyrics");
        f.press(&[ch('n')]);
        f.app.tick(now);
        assert_eq!(file_name(&f.app), Some("three.wav"));
        assert_eq!(f.app.playback().state(), PlayState::Playing);
        assert_eq!(f.app.lyrics_deadline(now), None, "plain");
        Ok(())
    }

    /// Widening (or showing the pane) asks for an immediate frame so the
    /// lyrics load at once, even while paused; once read, it asks no more,
    /// including for a track without lyrics.
    #[test]
    fn a_due_load_asks_for_an_immediate_frame() -> Result<()> {
        let mut f = Folder::shown()?;
        let now = Instant::now();
        f.play(now);
        f.press(&[ch(' ')]);
        assert_eq!(f.app.playback().state(), PlayState::Paused);
        f.app.set_lyrics_layout(false);
        assert_eq!(f.app.lyrics_deadline(now), None, "too narrow");
        f.app.set_lyrics_layout(true);
        assert_eq!(f.app.lyrics_deadline(now), Some(now), "widened");
        f.app.tick(now);
        assert!(f.app.lyrics_ready());
        assert_eq!(f.app.lyrics_deadline(now), None, "paused, loaded");

        f.press(&[ch('l'), ch('l')]);
        assert_eq!(f.app.lyrics_deadline(now), Some(now), "shown again");

        // A track without lyrics: one immediate frame, then none.
        f.press(&[ch('n')]);
        assert_eq!(file_name(&f.app), Some("two.wav"));
        assert_eq!(f.app.lyrics_deadline(now), Some(now), "track changed");
        f.app.tick(now);
        assert!(f.app.lyrics().is_none());
        assert_eq!(f.app.lyrics_deadline(now), None, "no lyrics");
        Ok(())
    }

    #[test]
    fn l_toggles_without_a_notice_while_the_pane_fits() -> Result<()> {
        let mut f = Folder::shown()?;
        let now = Instant::now();
        f.press(&[ch('l')]);
        assert!(!f.app.lyrics_on());
        assert!(!f.app.lyrics_shown());
        assert_eq!(f.status(now), None);
        f.press(&[ch('l')]);
        assert!(f.app.lyrics_on());
        assert!(f.app.lyrics_shown());
        assert_eq!(f.status(now), None);
        Ok(())
    }

    #[test]
    fn l_announces_the_change_while_the_terminal_is_too_narrow() -> Result<()> {
        let mut f = Folder::new()?;
        f.app.set_lyrics_layout(false);
        let now = Instant::now();
        f.press(&[ch('l')]);
        assert!(!f.app.lyrics_on());
        assert_eq!(f.status(now).as_deref(), Some("Lyrics off"));

        f.press(&[ch('l')]);
        assert!(f.app.lyrics_on(), "on, waiting for room");
        assert_eq!(
            f.status(now).as_deref(),
            Some("Lyrics on: needs a terminal at least 100 columns wide")
        );

        f.app.set_lyrics_layout(true);
        assert!(f.app.lyrics_shown());
        Ok(())
    }

    #[test]
    fn set_lyrics_on_raises_no_notice_and_drops_the_cache() -> Result<()> {
        let mut f = Folder::shown()?;
        let now = Instant::now();
        f.play(now);
        assert!(f.app.lyrics_ready());
        f.app.set_lyrics_layout(true);
        f.app.set_lyrics_on(false);
        assert!(!f.app.lyrics_on());
        assert!(!f.app.lyrics_ready());
        assert_eq!(f.status(now), None);

        f.app.set_lyrics_layout(false);
        f.app.set_lyrics_on(true);
        assert!(f.app.lyrics_on());
        assert_eq!(f.status(now), None, "no notice even while too narrow");
        Ok(())
    }

    #[test]
    fn l_works_from_the_sidebar_too() -> Result<()> {
        let mut f = Folder::shown()?;
        f.press(&[key(KeyCode::Tab)]);
        assert_eq!(f.app.focus(), Focus::Sidebar);
        f.press(&[ch('l')]);
        assert!(!f.app.lyrics_on());
        Ok(())
    }

    #[test]
    fn l_with_ctrl_or_alt_does_nothing() -> Result<()> {
        let mut f = Folder::shown()?;
        f.press(&[
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL),
        ]);
        assert!(f.app.lyrics_on());
        Ok(())
    }

    #[test]
    fn l_is_typed_into_a_search() -> Result<()> {
        let mut f = Folder::shown()?;
        f.press(&[ch('/'), ch('l')]);
        assert_eq!(f.app.query(), "l");
        assert!(f.app.lyrics_on());
        Ok(())
    }

    #[test]
    fn l_closes_the_help_overlay_and_does_nothing_else() -> Result<()> {
        let mut f = Folder::shown()?;
        f.press(&[ch('?'), ch('l')]);
        assert_eq!(*f.app.mode(), Mode::Normal);
        assert!(f.app.lyrics_on());
        Ok(())
    }

    #[test]
    fn l_in_the_picker_and_name_prompt_keeps_their_meaning() -> Result<()> {
        let mut f = Folder::shown()?;
        f.press(&[ch('a')]);
        assert!(matches!(f.app.mode(), Mode::Picker(_)));
        f.press(&[ch('l')]);
        assert!(matches!(f.app.mode(), Mode::Picker(_)), "not a picker key");
        f.press(&[key(KeyCode::Enter), ch('l')]);
        let Mode::NameInput(input) = f.app.mode() else {
            bail!("not naming a playlist: {:?}", f.app.mode());
        };
        assert_eq!(input.text(), "l");
        assert!(f.app.lyrics_on());
        Ok(())
    }

    #[test]
    fn l_cancels_a_delete_confirmation_and_does_nothing_else() -> Result<()> {
        let mut f = Folder::shown()?;
        f.app.mode = Mode::ConfirmDelete("Gym".to_owned());
        f.press(&[ch('l')]);
        assert_eq!(*f.app.mode(), Mode::Normal);
        assert!(f.app.lyrics_on());
        Ok(())
    }

    #[test]
    fn a_track_path_that_vanished_reads_as_no_lyrics() -> Result<()> {
        let mut f = Folder::shown()?;
        let dir: &Path = f.tmp.path();
        for name in ["one.wav", "one.lrc"] {
            fs::remove_file(dir.join(name))?;
        }
        f.play(Instant::now());
        assert!(f.app.lyrics_ready());
        assert_eq!(f.app.lyrics(), None);
        Ok(())
    }
}

/// Restoring and saving the session (`persist-session-state`, group 4).
#[cfg(test)]
mod state_tests {
    use super::*;
    use crate::player::Volume;
    use crate::playlist::FsStore;
    use crate::queue::Repeat;
    use crate::state::store::{FsStateStore, Loaded};
    use crate::state::{Parsed, RootKey, StateFile, merge, parse, render};
    use crate::test_support::SilentAudio;
    use anyhow::{Context as _, Result};
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::rc::Rc;

    /// One state file in memory, shared by every store made from it, like
    /// one file on disk shared by several kasset runs.
    #[derive(Debug, Default, Clone)]
    struct Disk(Rc<DiskInner>);

    #[derive(Debug, Default)]
    struct DiskInner {
        bytes: RefCell<Vec<u8>>,
        /// Calls to `save`, failed ones included.
        saves: Cell<usize>,
        /// Every `save` fails while set.
        fail: Cell<bool>,
    }

    impl Disk {
        fn with(bytes: &[u8]) -> Self {
            let disk = Self::default();
            disk.0.bytes.replace(bytes.to_vec());
            disk
        }

        fn saves(&self) -> usize {
            self.0.saves.get()
        }

        fn set_failing(&self, fail: bool) {
            self.0.fail.set(fail);
        }

        fn bytes(&self) -> Vec<u8> {
            self.0.bytes.borrow().clone()
        }

        fn file(&self) -> Result<StateFile> {
            match parse(&self.0.bytes.borrow()) {
                Parsed::Valid(file) => Ok(file),
                other => anyhow::bail!("not a valid state file: {other:?}"),
            }
        }

        /// The saved entry for library `root`.
        fn entry(&self, root: &Path) -> Result<FolderEntry> {
            let key = RootKey::new(root).context("root key")?;
            self.file()?
                .folders
                .into_iter()
                .find(|(k, _)| *k == key)
                .map(|(_, entry)| entry)
                .context("no entry for the root")
        }

        fn store(&self, root: &Path) -> Result<Box<dyn StateStore>> {
            Ok(Box::new(MemStore {
                disk: self.clone(),
                root: RootKey::new(root).context("root key")?,
            }))
        }
    }

    /// A [`StateStore`] over a [`Disk`], merging like `FsStateStore`.
    #[derive(Debug)]
    struct MemStore {
        disk: Disk,
        root: RootKey,
    }

    impl StateStore for MemStore {
        fn load(&mut self) -> Loaded {
            match parse(&self.disk.0.bytes.borrow()) {
                Parsed::Valid(file) => Loaded {
                    settings: file.settings,
                    folder: file
                        .folders
                        .into_iter()
                        .find(|(key, _)| *key == self.root)
                        .map(|(_, entry)| entry)
                        .unwrap_or_default(),
                    newer: false,
                },
                Parsed::Newer => Loaded {
                    newer: true,
                    ..Loaded::default()
                },
                Parsed::Invalid => Loaded::default(),
            }
        }

        fn save(&mut self, session: &Session, keep: Option<&Resume>) -> Result<Saved, String> {
            let inner = &self.disk.0;
            inner.saves.set(inner.saves.get() + 1);
            if inner.fail.get() {
                return Err("cannot save state to /state/kasset/state: disk full".to_owned());
            }
            let current = match parse(&inner.bytes.borrow()) {
                Parsed::Valid(file) => file,
                Parsed::Newer => return Ok(Saved::Skipped),
                Parsed::Invalid => StateFile::default(),
            };
            let merged = merge(current, &self.root, session, keep);
            inner.bytes.replace(render(&merged));
            Ok(Saved::Written)
        }
    }

    /// A store that persists nothing but counts saves, like [`NoState`].
    #[derive(Debug, Default)]
    struct Inert(Rc<Cell<usize>>);

    impl StateStore for Inert {
        fn load(&mut self) -> Loaded {
            Loaded::default()
        }

        fn save(&mut self, _: &Session, _: Option<&Resume>) -> Result<Saved, String> {
            self.0.set(self.0.get() + 1);
            Ok(Saved::Skipped)
        }

        fn persists(&self) -> bool {
            false
        }
    }

    /// A library folder of A, B, C, D by "Band" (`Band/LP/<title>.mp3`, not
    /// created) with playlists in `Playlists/`, and the app running on it.
    struct Lib {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        tracks: Vec<Track>,
        app: App<SilentAudio>,
    }

    impl Lib {
        fn new() -> Result<Self> {
            let tmp = tempfile::tempdir()?;
            let root = fs::canonicalize(tmp.path())?;
            let tracks: Vec<Track> = ["A", "B", "C", "D"]
                .iter()
                .map(|title| Track {
                    path: root.join(format!("Band/LP/{title}.mp3")),
                    title: (*title).to_owned(),
                    artist: "Band".to_owned(),
                    album: "LP".to_owned(),
                    number: None,
                    album_artist: None,
                    disc: None,
                    duration: Some(Duration::from_secs(200)),
                })
                .collect();
            let app = Self::fresh(&root, tracks.clone());
            Ok(Self {
                _tmp: tmp,
                root,
                tracks,
                app,
            })
        }

        fn fresh(root: &Path, tracks: Vec<Track>) -> App<SilentAudio> {
            App::new(
                tracks,
                SilentAudio::default(),
                Box::new(FsStore::new(root)),
                root.display().to_string(),
                None,
            )
        }

        /// Starts kasset again on this folder, restoring from `store`.
        fn start(&mut self, store: Box<dyn StateStore>) {
            self.app = Self::fresh(&self.root, self.tracks.clone());
            self.app.restore(store, &self.root);
        }

        /// Starts kasset again on this folder, restoring from `disk`.
        fn start_on(&mut self, disk: &Disk) -> Result<()> {
            self.start(disk.store(&self.root)?);
            Ok(())
        }

        /// Writes playlist `name` listing the given titles.
        fn write(&self, name: &str, titles: &[&str]) -> Result<()> {
            let dir = self.root.join("Playlists");
            fs::create_dir_all(&dir)?;
            let entries: Vec<String> = titles
                .iter()
                .map(|t| {
                    format!(
                        "../Band/LP/{t}.mp3
"
                    )
                })
                .collect();
            let entries = entries.concat();
            fs::write(
                dir.join(format!("{name}.m3u8")),
                format!("#EXTM3U\n{entries}"),
            )?;
            Ok(())
        }

        fn index(&self, title: &str) -> Result<usize> {
            self.tracks
                .iter()
                .position(|t| t.title == title)
                .context("no such track")
        }

        fn current_title(&self) -> Option<String> {
            self.app.playback().current_track().map(|t| t.title.clone())
        }

        fn shown(&self) -> Option<&str> {
            self.app.open_playlist().map(OpenPlaylist::name)
        }

        /// From the main pane: `Tab`, then `Enter` on the first playlist
        /// (the row after the sidebar's fixed rows).
        fn open_first_playlist(&mut self) {
            self.key(KeyCode::Tab);
            self.key(KeyCode::Home);
            for _ in 0..FIXED_ROWS {
                self.key(KeyCode::Down);
            }
            self.key(KeyCode::Enter);
        }

        fn key(&mut self, code: KeyCode) {
            self.app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }

        /// Plays library track `title` with `Enter` (the library is shown).
        fn play(&mut self, title: &str) -> Result<()> {
            let index = self.index(title)?;
            self.key(KeyCode::Home);
            for _ in 0..index {
                self.key(KeyCode::Down);
            }
            self.key(KeyCode::Enter);
            assert_eq!(self.current_title().as_deref(), Some(title));
            Ok(())
        }

        fn set_position(&mut self, position: Duration) {
            self.app.playback_mut().audio_mut().position = position;
        }

        /// The status text after a tick at `now`.
        fn status(&mut self, now: Instant) -> String {
            self.app.tick(now);
            self.app
                .status(now)
                .map(|n| n.text.clone())
                .unwrap_or_default()
        }

        /// Quits: `shutdown`, then the final save.
        fn quit(&mut self) -> Option<String> {
            self.app.shutdown();
            self.app.save_final()
        }

        /// A state file holding `settings` and, for this folder, `entry`.
        fn disk_with(&self, settings: Option<Settings>, entry: FolderEntry) -> Result<Disk> {
            let file = StateFile {
                settings,
                folders: vec![(RootKey::new(&self.root).context("root key")?, entry)],
            };
            Ok(Disk::with(&render(&file)))
        }
    }

    /// A resume point at `title`, in whole milliseconds.
    fn resume_at(title: &str, item: Option<usize>, ms: u64, source: SavedSource) -> Resume {
        Resume {
            track: RelPath::new(Path::new(&format!("Band/LP/{title}.mp3")))
                .unwrap_or_else(|| unreachable!("a valid relative path")),
            item,
            position: Duration::from_millis(ms),
            source,
        }
    }

    fn playlist(name: &str) -> SavedSource {
        SavedSource::Playlist(name.to_owned())
    }

    const PAUSED_AT: Duration = Duration::from_secs(135);

    // --- Settings ---

    #[test]
    fn settings_restored() -> Result<()> {
        let mut lib = Lib::new()?;
        let disk = Disk::default();
        lib.start_on(&disk)?;
        for _ in 0..8 {
            lib.key(KeyCode::Char('-'));
        }
        for c in ['z', 'r', 'v', 'l'] {
            lib.key(KeyCode::Char(c));
        }
        assert_eq!(lib.quit(), None);
        lib.start_on(&disk)?;
        let playback = lib.app.playback();
        assert_eq!(playback.volume().percent(), 60);
        assert!(playback.shuffle());
        assert_eq!(playback.repeat(), Repeat::All);
        assert!(!lib.app.visualizer_on());
        assert!(!lib.app.lyrics_on());
        Ok(())
    }

    #[test]
    fn restoring_hidden_panes_shows_no_notice() -> Result<()> {
        let mut lib = Lib::new()?;
        let settings = Settings {
            visualizer: false,
            lyrics: false,
            ..Settings::default()
        };
        let disk = lib.disk_with(Some(settings), FolderEntry::default())?;
        lib.start_on(&disk)?;
        assert!(!lib.app.visualizer_on());
        assert!(!lib.app.lyrics_on());
        assert_eq!(lib.status(Instant::now()), "");
        Ok(())
    }

    #[test]
    fn first_run_has_the_defaults() -> Result<()> {
        let mut lib = Lib::new()?;
        lib.start_on(&Disk::default())?;
        let playback = lib.app.playback();
        assert_eq!(playback.volume(), Volume::MAX);
        assert!(!playback.shuffle());
        assert_eq!(playback.repeat(), Repeat::Off);
        assert!(lib.app.visualizer_on());
        assert!(lib.app.lyrics_on());
        assert_eq!(playback.current(), None);
        assert_eq!(lib.app.focus(), Focus::Main);
        assert_eq!(lib.app.main_view(), MainView::Library);
        assert_eq!(lib.status(Instant::now()), "", "no error on a first run");
        Ok(())
    }

    #[test]
    fn settings_shared_across_folders() -> Result<()> {
        let disk = Disk::default();
        let mut music = Lib::new()?;
        music.start_on(&disk)?;
        for _ in 0..12 {
            music.key(KeyCode::Char('-'));
        }
        assert_eq!(music.quit(), None);
        let mut other = Lib::new()?;
        other.start_on(&disk)?;
        assert_eq!(other.app.playback().volume().percent(), 40);
        Ok(())
    }

    // --- Resume points ---

    #[test]
    fn separate_folders_keep_separate_resume_points() -> Result<()> {
        let disk = Disk::default();
        let mut music = Lib::new()?;
        music.start_on(&disk)?;
        music.play("A")?;
        assert_eq!(music.quit(), None);
        let mut other = Lib::new()?;
        other.start_on(&disk)?;
        other.play("B")?;
        assert_eq!(other.quit(), None);
        music.start_on(&disk)?;
        assert_eq!(music.current_title().as_deref(), Some("A"));
        other.start_on(&disk)?;
        assert_eq!(other.current_title().as_deref(), Some("B"));
        Ok(())
    }

    #[test]
    fn resumed_paused_at_the_saved_position() -> Result<()> {
        let mut lib = Lib::new()?;
        let disk = Disk::default();
        lib.start_on(&disk)?;
        lib.play("C")?;
        lib.set_position(PAUSED_AT);
        assert_eq!(lib.quit(), None);
        lib.start_on(&disk)?;
        assert_eq!(lib.current_title().as_deref(), Some("C"));
        assert_eq!(lib.app.playback().state(), PlayState::Paused);
        assert_eq!(lib.app.playback().position(), PAUSED_AT);
        let audio = lib.app.playback().audio();
        assert!(audio.loaded && audio.paused, "loaded paused");
        lib.key(KeyCode::Char(' '));
        assert_eq!(lib.app.playback().state(), PlayState::Playing);
        assert_eq!(
            lib.app.playback().position(),
            PAUSED_AT,
            "continues from there"
        );
        Ok(())
    }

    #[test]
    fn album_and_artist_sources_are_saved_as_the_library_for_now() -> Result<()> {
        // Interim until tasks 5.1 and 5.3 of add-artist-album-browsing.
        let mut lib = Lib::new()?;
        let disk = Disk::default();
        lib.start_on(&disk)?;
        for source in [Source::Album(0), Source::Artist(0)] {
            assert!(
                lib.app
                    .playback_mut()
                    .play_source(source.clone(), vec![2, 0], 0)
            );
            assert_eq!(lib.app.sidebar_playing(), Some(0), "{source:?}");
            let resume = lib.app.session().folder.resume;
            assert_eq!(
                resume.as_ref().map(|r| (r.source.clone(), r.item)),
                Some((SavedSource::Library, Some(2))),
                "{source:?}: saved as the library, item = library index"
            );
        }
        Ok(())
    }

    #[test]
    fn resumed_in_its_playlist() -> Result<()> {
        let mut lib = Lib::new()?;
        lib.write("Running", &["D", "B", "C"])?;
        let disk = Disk::default();
        lib.start_on(&disk)?;
        lib.open_first_playlist();
        assert_eq!(lib.shown(), Some("Running"));
        lib.key(KeyCode::Down);
        lib.key(KeyCode::Enter);
        assert_eq!(lib.current_title().as_deref(), Some("B"));
        assert_eq!(lib.quit(), None);

        lib.start_on(&disk)?;
        assert_eq!(lib.shown(), Some("Running"));
        assert_eq!(lib.app.focus(), Focus::Main);
        assert_eq!(lib.app.view(), View::Playlist);
        assert_eq!(
            lib.app.selected_playlist().map(|p| p.name.as_str()),
            Some("Running")
        );
        assert_eq!(lib.current_title().as_deref(), Some("B"));
        assert_eq!(
            *lib.app.playback().source(),
            Source::Playlist("Running".to_owned())
        );
        assert_eq!(lib.app.sidebar_playing(), SidebarEntry::Playlist(0).value());
        lib.key(KeyCode::Char('n'));
        assert_eq!(
            lib.current_title().as_deref(),
            Some("C"),
            "next follows Running"
        );
        lib.key(KeyCode::Char('p'));
        lib.key(KeyCode::Char('p'));
        assert_eq!(
            lib.current_title().as_deref(),
            Some("D"),
            "previous follows Running"
        );
        Ok(())
    }

    #[test]
    fn track_gone_still_shows_the_playlist() -> Result<()> {
        let mut lib = Lib::new()?;
        lib.write("Running", &["A", "B"])?;
        let entry = FolderEntry {
            resume: Some(resume_at("Gone", None, 5_000, playlist("Running"))),
            open: Some("Running".to_owned()),
        };
        let disk = lib.disk_with(None, entry)?;
        lib.start_on(&disk)?;
        assert_eq!(lib.app.playback().current(), None);
        assert!(!lib.app.playback().audio().loaded, "nothing loaded");
        assert_eq!(lib.shown(), Some("Running"));
        assert_eq!(lib.app.focus(), Focus::Main);
        assert_eq!(lib.status(Instant::now()), "", "no error");
        Ok(())
    }

    #[test]
    fn playlist_gone_resumes_from_the_library() -> Result<()> {
        let mut lib = Lib::new()?;
        let entry = FolderEntry {
            resume: Some(resume_at("C", Some(0), 5_000, playlist("Gym"))),
            open: Some("Gym".to_owned()),
        };
        let disk = lib.disk_with(None, entry)?;
        lib.start_on(&disk)?;
        assert_eq!(lib.current_title().as_deref(), Some("C"));
        assert_eq!(*lib.app.playback().source(), Source::Library);
        assert_eq!(lib.app.playback().current_item(), Some(lib.index("C")?));
        assert_eq!(lib.app.main_view(), MainView::Library);
        assert_eq!(lib.app.focus(), Focus::Main);
        Ok(())
    }

    #[test]
    fn track_no_longer_in_the_playlist() -> Result<()> {
        let mut lib = Lib::new()?;
        lib.write("Gym", &["A", "B"])?;
        let entry = FolderEntry {
            resume: Some(resume_at("C", Some(1), 5_000, playlist("Gym"))),
            open: Some("Gym".to_owned()),
        };
        let disk = lib.disk_with(None, entry)?;
        lib.start_on(&disk)?;
        assert_eq!(lib.current_title().as_deref(), Some("C"));
        assert_eq!(*lib.app.playback().source(), Source::Library);
        assert_eq!(lib.shown(), Some("Gym"), "still shown");
        Ok(())
    }

    #[test]
    fn source_and_shown_playlist_differ() -> Result<()> {
        let mut lib = Lib::new()?;
        lib.write("Gym", &["D", "C", "A"])?;
        let disk = Disk::default();
        lib.start_on(&disk)?;
        lib.open_first_playlist();
        lib.key(KeyCode::Down);
        lib.key(KeyCode::Enter);
        assert_eq!(lib.current_title().as_deref(), Some("C"));
        // Back to the library, which stays shown.
        lib.key(KeyCode::Tab);
        lib.key(KeyCode::Home);
        lib.key(KeyCode::Enter);
        assert_eq!(lib.app.main_view(), MainView::Library);
        assert_eq!(lib.quit(), None);

        lib.start_on(&disk)?;
        assert_eq!(lib.app.main_view(), MainView::Library);
        assert_eq!(lib.app.focus(), Focus::Main);
        assert_eq!(
            *lib.app.playback().source(),
            Source::Playlist("Gym".to_owned())
        );
        assert_eq!(lib.current_title().as_deref(), Some("C"));
        lib.key(KeyCode::Char('n'));
        assert_eq!(
            lib.current_title().as_deref(),
            Some("A"),
            "next follows Gym"
        );
        Ok(())
    }

    #[test]
    fn saved_item_is_used_when_it_still_is_the_track() -> Result<()> {
        let mut lib = Lib::new()?;
        lib.write("Twice", &["A", "B", "A", "C"])?;
        let entry = FolderEntry {
            resume: Some(resume_at("A", Some(2), 0, playlist("Twice"))),
            open: None,
        };
        lib.start_on(&lib.disk_with(None, entry)?)?;
        assert_eq!(lib.app.playback().current_item(), Some(2));
        lib.key(KeyCode::Char('n'));
        assert_eq!(lib.current_title().as_deref(), Some("C"));

        // A saved item that now names another track: the first match wins.
        let entry = FolderEntry {
            resume: Some(resume_at("A", Some(1), 0, playlist("Twice"))),
            open: None,
        };
        lib.start_on(&lib.disk_with(None, entry)?)?;
        assert_eq!(lib.app.playback().current_item(), Some(0));
        Ok(())
    }

    #[test]
    fn a_path_the_scan_did_not_find_is_never_loaded() -> Result<()> {
        let mut lib = Lib::new()?;
        for track in [
            "../../etc/passwd",
            "/etc/passwd",
            "Band/LP/Missing.mp3",
            "Band/LP",
        ] {
            let text = format!(
                "kasset-state 1\nfolder {}\ntrack {track}\nposition-ms 1000\nsource library\n",
                lib.root.display()
            );
            lib.start_on(&Disk::with(text.as_bytes()))?;
            assert!(!lib.app.playback().audio().loaded, "{track}: no load");
            assert_eq!(lib.app.playback().current(), None, "{track}");
        }
        Ok(())
    }

    #[test]
    fn track_cannot_be_opened() -> Result<()> {
        let mut lib = Lib::new()?;
        let entry = FolderEntry {
            resume: Some(resume_at("B", None, 5_000, SavedSource::Library)),
            open: None,
        };
        let disk = lib.disk_with(None, entry)?;
        lib.app = Lib::fresh(&lib.root, lib.tracks.clone());
        lib.app.playback_mut().audio_mut().broken = true;
        lib.app.restore(disk.store(&lib.root)?, &lib.root);
        assert!(lib.status(Instant::now()).starts_with("Cannot resume 'B'"));
        assert_eq!(lib.app.playback().current(), None);
        assert_eq!(lib.app.playback().state(), PlayState::Stopped);
        Ok(())
    }

    #[test]
    fn nothing_to_resume_keeps_the_shown_playlist() -> Result<()> {
        let mut lib = Lib::new()?;
        lib.write("Running", &["A"])?;
        let state = tempfile::tempdir()?;
        let file = state.path().join("kasset/state");
        lib.start(Box::new(FsStateStore::new(file.clone(), &lib.root)));
        lib.open_first_playlist();
        assert_eq!(lib.quit(), None);
        let Parsed::Valid(saved) = parse(&fs::read(&file)?) else {
            anyhow::bail!("the saved file does not parse");
        };
        let entry = &saved.folders.first().context("one folder")?.1;
        assert_eq!(entry.resume, None, "no track");
        assert_eq!(entry.open.as_deref(), Some("Running"));

        lib.start(Box::new(FsStateStore::new(file, &lib.root)));
        assert_eq!(lib.app.playback().current(), None);
        assert_eq!(lib.shown(), Some("Running"));
        Ok(())
    }

    #[test]
    fn an_unresumable_point_is_kept_until_something_plays() -> Result<()> {
        let mut lib = Lib::new()?;
        let gone = resume_at("Gone", Some(7), 61_000, playlist("Gym"));
        let entry = FolderEntry {
            resume: Some(gone.clone()),
            open: None,
        };
        let disk = lib.disk_with(None, entry)?;
        lib.start_on(&disk)?;
        assert_eq!(lib.app.playback().current(), None);
        assert_eq!(lib.quit(), None);
        assert_eq!(disk.entry(&lib.root)?.resume, Some(gone), "kept whole");

        lib.start_on(&disk)?;
        lib.play("D")?;
        lib.set_position(Duration::from_secs(3));
        assert_eq!(lib.quit(), None);
        let saved = disk.entry(&lib.root)?.resume.context("a resume point")?;
        assert_eq!(saved, resume_at("D", Some(3), 3_000, SavedSource::Library));
        Ok(())
    }

    #[test]
    fn resumed_track_shows_its_lyrics() -> Result<()> {
        let mut lib = Lib::new()?;
        let dir = lib.root.join("Band/LP");
        fs::create_dir_all(&dir)?;
        fs::write(dir.join("B.lrc"), "[00:01.00]First\n[00:03.00]Second\n")?;
        let entry = FolderEntry {
            resume: Some(resume_at("B", None, 2_000, SavedSource::Library)),
            open: None,
        };
        lib.start_on(&lib.disk_with(None, entry)?)?;
        lib.app.set_lyrics_layout(true);
        lib.app.tick(Instant::now());
        assert!(lib.app.lyrics_ready(), "not \"Nothing playing\"");
        let Some(Lyrics::Synced(lines)) = lib.app.lyrics() else {
            anyhow::bail!("no synced lyrics: {:?}", lib.app.lyrics());
        };
        assert_eq!(lines.first().map(|l| l.text.as_str()), Some("First"));
        Ok(())
    }

    // --- When the session is saved ---

    #[test]
    fn no_session_is_built_without_a_store() -> Result<()> {
        let mut plain = Lib::new()?;
        let saves = Rc::new(Cell::new(0));
        let mut inert = Lib::new()?;
        inert.start(Box::new(Inert(Rc::clone(&saves))));
        for lib in [&mut plain, &mut inert] {
            lib.play("A")?;
            let start = Instant::now();
            for s in 0..300 {
                lib.set_position(Duration::from_secs(s));
                lib.app.tick(start + Duration::from_secs(s));
            }
            assert_eq!(lib.quit(), None);
            assert_eq!(lib.app.saved.built.get(), 0, "no session built");
        }
        assert_eq!(saves.get(), 0, "never asked to save");
        Ok(())
    }

    #[test]
    fn nothing_changed_saves_nothing() -> Result<()> {
        let mut lib = Lib::new()?;
        let entry = FolderEntry {
            resume: Some(resume_at("A", Some(0), 10_000, SavedSource::Library)),
            open: None,
        };
        let disk = lib.disk_with(Some(Settings::default()), entry)?;
        lib.start_on(&disk)?;
        assert_eq!(lib.app.playback().state(), PlayState::Paused);
        let start = Instant::now();
        for s in 0..=300 {
            lib.app.tick(start + Duration::from_secs(s));
        }
        assert_eq!(disk.saves(), 0);
        Ok(())
    }

    #[test]
    fn saved_every_30_seconds_while_playing() -> Result<()> {
        let mut lib = Lib::new()?;
        let disk = Disk::default();
        lib.start_on(&disk)?;
        let start = Instant::now();
        lib.app.tick(start);
        lib.play("B")?;
        for s in 1..30 {
            lib.set_position(Duration::from_secs(s));
            lib.app.tick(start + Duration::from_secs(s));
        }
        assert_eq!(disk.saves(), 0, "not before 30 s");
        lib.set_position(Duration::from_millis(30_250));
        lib.app.tick(start + SAVE_INTERVAL);
        assert_eq!(disk.saves(), 1);
        let saved = disk.entry(&lib.root)?.resume.context("a resume point")?;
        assert_eq!(saved.position, Duration::from_millis(30_250));
        lib.set_position(Duration::from_secs(45));
        lib.app.tick(start + Duration::from_secs(59));
        assert_eq!(disk.saves(), 1, "at most every 30 s");
        lib.app.tick(start + Duration::from_secs(60));
        assert_eq!(disk.saves(), 2);
        Ok(())
    }

    #[test]
    fn repeated_failures_are_reported_once() -> Result<()> {
        let mut lib = Lib::new()?;
        let disk = Disk::default();
        lib.start_on(&disk)?;
        disk.set_failing(true);
        let start = Instant::now();
        lib.app.tick(start);
        lib.play("A")?;
        let mut errors = 0;
        for i in 1..=4 {
            let now = start + SAVE_INTERVAL * i;
            lib.set_position(Duration::from_secs(u64::from(i)));
            // A notice lasts `STATUS_TTL`, far less than an interval.
            errors += usize::from(lib.status(now).starts_with("Cannot save state to"));
        }
        assert_eq!(disk.saves(), 4, "tried each interval");
        assert_eq!(errors, 1, "reported once");

        disk.set_failing(false);
        lib.set_position(Duration::from_secs(10));
        lib.app.tick(start + SAVE_INTERVAL * 5);
        assert_eq!(lib.status(start + SAVE_INTERVAL * 5), "");
        disk.set_failing(true);
        lib.set_position(Duration::from_secs(11));
        let now = start + SAVE_INTERVAL * 6;
        assert!(
            lib.status(now)
                .starts_with("Cannot save state to /state/kasset/state")
        );
        Ok(())
    }

    #[test]
    fn a_failed_save_is_not_hidden_by_a_playback_error() -> Result<()> {
        let mut lib = Lib::new()?;
        let disk = Disk::default();
        lib.start_on(&disk)?;
        disk.set_failing(true);
        let start = Instant::now();
        lib.app.tick(start);
        lib.play("A")?;
        // In the tick that saves, the track ends and the next cannot play.
        let audio = lib.app.playback_mut().audio_mut();
        audio.ended = true;
        audio.broken = true;
        let status = lib.status(start + SAVE_INTERVAL);
        assert_eq!(disk.saves(), 1);
        assert!(status.starts_with("Cannot play '"), "{status}");
        assert!(status.contains("; Cannot save state to"), "{status}");
        assert_eq!(
            lib.app.status(start + SAVE_INTERVAL).map(|n| n.level),
            Some(Level::Error)
        );
        Ok(())
    }

    #[test]
    fn resumes_over_a_scanned_library() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().join("Band").join("LP");
        fs::create_dir_all(&dir)?;
        for name in ["one.wav", "two.wav"] {
            fs::write(dir.join(name), crate::test_support::wav_bytes())?;
        }
        // The root as `main` gets it: resolved, then scanned.
        let root = crate::cli::resolve_root(Some(tmp.path()))?;
        let disk = Disk::default();
        let start = |disk: &Disk| -> Result<App<SilentAudio>> {
            let tracks = crate::library::scan(&root)?.tracks;
            let mut app = Lib::fresh(&root, tracks);
            app.restore(disk.store(&root)?, &root);
            Ok(app)
        };
        let mut app = start(&disk)?;
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let played = app.playback().current_track().map(|t| t.path.clone());
        assert!(played.is_some());
        app.shutdown();
        assert_eq!(app.save_final(), None);
        let saved = disk.entry(&root)?.resume.context("a resume point")?;
        let expected = RelPath::new(Path::new("Band/LP/two.wav")).context("relative path")?;
        assert_eq!(saved.track, expected);

        let app = start(&disk)?;
        assert_eq!(
            app.playback().current_track().map(|t| t.path.clone()),
            played
        );
        assert_eq!(app.playback().state(), PlayState::Paused);
        Ok(())
    }

    #[test]
    fn a_newer_file_is_never_written() -> Result<()> {
        let mut lib = Lib::new()?;
        let original = b"kasset-state 2\nvolume 10\n".to_vec();
        let disk = Disk::with(&original);
        lib.start_on(&disk)?;
        assert_eq!(lib.app.playback().volume(), Volume::MAX, "defaults");
        let start = Instant::now();
        let status = lib.status(start);
        assert!(
            status.starts_with("Saved state is from a newer kasset"),
            "{status}"
        );
        assert_eq!(lib.app.status(start).map(|n| n.level), Some(Level::Info));
        lib.play("A")?;
        lib.key(KeyCode::Char('-'));
        for s in 1..=120 {
            lib.set_position(Duration::from_secs(s));
            lib.app.tick(start + Duration::from_secs(s));
        }
        assert_eq!(lib.quit(), None);
        assert_eq!(disk.saves(), 0);
        assert_eq!(disk.bytes(), original);
        Ok(())
    }

    #[test]
    fn saved_at_quit() -> Result<()> {
        let mut lib = Lib::new()?;
        let disk = Disk::default();
        lib.start_on(&disk)?;
        assert_eq!(lib.app.save_final(), None, "nothing without shutdown");
        assert_eq!(disk.saves(), 0);
        lib.play("C")?;
        lib.key(KeyCode::Char('-'));
        lib.set_position(Duration::from_millis(42_123));
        lib.app.shutdown();
        assert_eq!(lib.app.playback().position(), Duration::ZERO, "stopped");
        assert_eq!(disk.saves(), 0, "shutdown writes nothing");
        assert_eq!(lib.app.save_final(), None);
        assert_eq!(disk.saves(), 1);
        let file = disk.file()?;
        assert_eq!(file.settings.map(|s| s.volume.percent()), Some(95));
        let saved = disk.entry(&lib.root)?.resume.context("a resume point")?;
        assert_eq!(
            saved.position,
            Duration::from_millis(42_123),
            "from before the stop"
        );
        assert_eq!(lib.app.save_final(), None, "written once");
        assert_eq!(disk.saves(), 1);
        Ok(())
    }

    #[test]
    fn a_failed_final_save_returns_the_error() -> Result<()> {
        let mut lib = Lib::new()?;
        let disk = Disk::default();
        lib.start_on(&disk)?;
        disk.set_failing(true);
        let error = lib.quit().context("an error")?;
        assert!(error.starts_with("cannot save state to /state/kasset/state"));
        Ok(())
    }
}

/// Key sequences for browsing by artist and album (tasks 4.1–4.3 of
/// add-artist-album-browsing).
#[cfg(test)]
mod browse_tests {
    use super::*;
    use crate::library::sort_tracks;
    use crate::playlist::FsStore;
    use crate::test_support::SilentAudio;
    use anyhow::{Result, bail};
    use std::fs;

    struct Lib {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        app: App<SilentAudio>,
    }

    /// `(title, artist, album, number, album artist)` of every track.
    type Spec<'a> = (&'a str, &'a str, &'a str, u32, Option<&'a str>);

    /// Air with "Moon Safari" (3 tracks) and "Talkie Walkie" (2, listed
    /// in the wrong order), Björk with "Debut" (2), and the compilation
    /// "Hits" by "Various Artists" with guests "A" and "B".
    const MUSIC: &[Spec<'static>] = &[
        ("Venus", "Air", "Talkie Walkie", 2, None),
        ("Cherry Blossom Girl", "Air", "Talkie Walkie", 1, None),
        ("La Femme d'Argent", "Air", "Moon Safari", 1, None),
        ("Sexy Boy", "Air", "Moon Safari", 2, None),
        ("Kelly Watch the Stars", "Air", "Moon Safari", 3, None),
        ("Human Behaviour", "Björk", "Debut", 1, None),
        ("Crying", "Björk", "Debut", 2, None),
        ("First Hit", "A", "Hits", 1, Some("Various Artists")),
        ("Second Hit", "B", "Hits", 2, Some("Various Artists")),
    ];

    fn lib(specs: &[Spec<'_>]) -> Result<Lib> {
        let tmp = tempfile::tempdir()?;
        let root = fs::canonicalize(tmp.path())?;
        let mut tracks: Vec<Track> = specs
            .iter()
            .map(|&(title, artist, album, number, album_artist)| Track {
                path: root.join(format!("{artist}/{album}/{title}.mp3")),
                title: title.to_owned(),
                artist: artist.to_owned(),
                album: album.to_owned(),
                number: Some(number),
                album_artist: album_artist.map(str::to_owned),
                disc: None,
                duration: Some(Duration::from_secs(200)),
            })
            .collect();
        sort_tracks(&mut tracks);
        let app = App::new(
            tracks,
            SilentAudio::default(),
            Box::new(FsStore::new(&root)),
            root.display().to_string(),
            None,
        );
        Ok(Lib {
            _tmp: tmp,
            root,
            app,
        })
    }

    fn music() -> Result<Lib> {
        lib(MUSIC)
    }

    impl Lib {
        fn keys(&mut self, keys: &[KeyCode]) {
            for &code in keys {
                self.app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            }
        }

        fn chars(&mut self, text: &str) {
            for c in text.chars() {
                self.app
                    .handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            }
        }

        /// Writes playlist `name` listing tracks `(artist, album, title)`.
        fn write(&self, name: &str, entries: &[(&str, &str, &str)]) -> Result<()> {
            let dir = self.root.join("Playlists");
            fs::create_dir_all(&dir)?;
            let lines: Vec<String> = entries
                .iter()
                .map(|(artist, album, title)| format!("../{artist}/{album}/{title}.mp3\n"))
                .collect();
            let lines = lines.concat();
            fs::write(dir.join(format!("{name}.m3u8")), lines)?;
            Ok(())
        }

        /// Focuses the sidebar if needed and selects `entry` in it.
        fn select_in_sidebar(&mut self, entry: SidebarEntry) -> Result<()> {
            if self.app.focus() == Focus::Main {
                self.keys(&[KeyCode::Tab]);
            }
            let Some(value) = entry.value() else {
                bail!("{entry:?} has no value");
            };
            let Some(row) = self.app.visible().iter().position(|&v| v == value) else {
                bail!("{entry:?} not listed");
            };
            self.keys(&[KeyCode::Home]);
            for _ in 0..row {
                self.keys(&[KeyCode::Down]);
            }
            Ok(())
        }

        /// `Enter` on "Artists" (or "Albums") in the sidebar.
        fn show(&mut self, entry: SidebarEntry) -> Result<()> {
            self.select_in_sidebar(entry)?;
            self.keys(&[KeyCode::Enter]);
            Ok(())
        }

        /// The names of the main pane's rows as listed: artist names, album
        /// titles (an artist's albums with "All tracks" first), or track
        /// titles.
        fn names(&self) -> Vec<String> {
            let catalog = self.app.catalog();
            let list = self.app.main_list();
            let tracks = self.app.playback().tracks();
            let browsed = self.app.browse_tracks();
            list.rows
                .iter()
                .map(|&value| match self.app.main_view() {
                    MainView::Browse(BrowseKind::Artists) => {
                        catalog.artist(value).map(|a| a.name().to_owned())
                    }
                    MainView::Browse(BrowseKind::ArtistAlbums(a)) => {
                        match artist_album_of_value(value) {
                            None => Some("All tracks".to_owned()),
                            Some(i) => catalog
                                .artist(a)
                                .and_then(|artist| artist.albums().get(i))
                                .and_then(|&b| catalog.album(b))
                                .map(|album| album.title().to_owned()),
                        }
                    }
                    MainView::Browse(BrowseKind::Albums) => {
                        catalog.album(value).map(|album| album.title().to_owned())
                    }
                    MainView::Browse(BrowseKind::Album(_) | BrowseKind::ArtistTracks(_)) => browsed
                        .get(value)
                        .and_then(|&t| tracks.get(t))
                        .map(|t| t.title.clone()),
                    MainView::Library => tracks.get(value).map(|t| t.title.clone()),
                    MainView::Playlist => None,
                })
                .map(|name| name.unwrap_or_else(|| "<missing>".to_owned()))
                .collect()
        }

        /// The name of the selected row of the main pane.
        fn selected_name(&self) -> Option<String> {
            let row = self.app.main_list().selected?;
            self.names().get(row).cloned()
        }

        /// Selects the main pane's row named `name` (focused, no filter
        /// needed: rows as listed).
        fn select(&mut self, name: &str) -> Result<()> {
            let Some(row) = self.names().iter().position(|n| n == name) else {
                bail!("{name} not listed in {:?}", self.names());
            };
            self.keys(&[KeyCode::Home]);
            for _ in 0..row {
                self.keys(&[KeyCode::Down]);
            }
            Ok(())
        }

        /// Selects `name` in the main pane and opens it with `Enter`.
        fn open(&mut self, name: &str) -> Result<()> {
            self.select(name)?;
            self.keys(&[KeyCode::Enter]);
            Ok(())
        }

        fn album(&self, title: &str) -> Option<usize> {
            self.app
                .catalog()
                .albums()
                .iter()
                .position(|album| album.title() == title)
        }

        fn artist(&self, name: &str) -> Option<usize> {
            self.app
                .catalog()
                .artists()
                .iter()
                .position(|artist| artist.name() == name)
        }
    }

    // --- Sidebar fixed rows (task 4.1) ---

    #[test]
    fn fixed_rows_come_first_and_playlists_after_them() -> Result<()> {
        let mut m = music()?;
        m.write("Gym", &[("Air", "Moon Safari", "Sexy Boy")])?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.visible(), [0, 1, 2, 3]);
        let entries: Vec<SidebarEntry> = m
            .app
            .visible()
            .iter()
            .map(|&v| SidebarEntry::of_value(v))
            .collect();
        assert_eq!(
            entries,
            [
                SidebarEntry::Library,
                SidebarEntry::Artists,
                SidebarEntry::Albums,
                SidebarEntry::Playlist(0)
            ]
        );
        for row in 0..FIXED_ROWS {
            m.keys(&[KeyCode::Home]);
            for _ in 0..row {
                m.keys(&[KeyCode::Down]);
            }
            assert_eq!(m.app.selected_playlist(), None, "row {row}");
        }
        m.keys(&[KeyCode::End]);
        assert_eq!(
            m.app.selected_playlist().map(|p| p.name.as_str()),
            Some("Gym")
        );
        // `R` and `D` do nothing on the fixed rows.
        m.select_in_sidebar(SidebarEntry::Artists)?;
        m.app
            .handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
        m.app
            .handle_key(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT));
        assert_eq!(*m.app.mode(), Mode::Normal);
        Ok(())
    }

    #[test]
    fn sidebar_entry_values_round_trip() {
        for value in 0..10 {
            assert_eq!(SidebarEntry::of_value(value).value(), Some(value));
        }
        assert_eq!(SidebarEntry::Playlist(usize::MAX).value(), None);
        assert_eq!(
            SidebarEntry::of_value(usize::MAX),
            SidebarEntry::Playlist(usize::MAX - FIXED_ROWS)
        );
    }

    #[test]
    fn sidebar_filter_with_no_playlist_match_keeps_the_fixed_rows() -> Result<()> {
        let mut m = music()?;
        m.write("Gym", &[])?;
        m.keys(&[KeyCode::Tab]);
        m.chars("/zzz");
        assert_eq!(m.app.visible(), [0, 1, 2]);
        assert_eq!(m.app.selected(), Some(0));
        m.keys(&[KeyCode::Esc, KeyCode::Esc]);
        assert_eq!(m.app.visible(), [0, 1, 2, 3]);
        // A match selects the first matching playlist, not a fixed row.
        m.chars("/gy");
        assert_eq!(m.app.visible(), [0, 1, 2, 3]);
        assert_eq!(
            m.app.selected_playlist().map(|p| p.name.as_str()),
            Some("Gym")
        );
        Ok(())
    }

    // --- Browse state (task 4.2) ---

    /// Spec *Open the artist list*.
    #[test]
    fn open_the_artist_list() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Artists)?;
        assert_eq!(m.app.focus(), Focus::Main);
        assert_eq!(m.app.view(), View::Groups);
        assert_eq!(m.app.main_view(), MainView::Browse(BrowseKind::Artists));
        assert_eq!(m.app.browse_depth(), 1);
        assert_eq!(m.names(), ["Air", "Björk", "Various Artists"]);
        assert_eq!(m.selected_name().as_deref(), Some("Air"));
        assert_eq!(m.app.selected_track(), None);

        m.show(SidebarEntry::Albums)?;
        assert_eq!(m.app.main_view(), MainView::Browse(BrowseKind::Albums));
        assert_eq!(m.names(), ["Moon Safari", "Talkie Walkie", "Debut", "Hits"]);
        Ok(())
    }

    /// Spec *Open an artist* and *Album tracks* (album order).
    #[test]
    fn open_an_artist_then_an_album() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Artists)?;
        m.open("Air")?;
        let air = m.artist("Air");
        assert_eq!(
            Some(m.app.main_view()),
            air.map(|a| MainView::Browse(BrowseKind::ArtistAlbums(a)))
        );
        assert_eq!(m.app.view(), View::Groups);
        assert_eq!(m.app.browse_depth(), 2);
        assert_eq!(m.names(), ["All tracks", "Moon Safari", "Talkie Walkie"]);
        assert_eq!(m.selected_name().as_deref(), Some("All tracks"));

        m.open("Talkie Walkie")?;
        assert_eq!(
            Some(m.app.main_view()),
            m.album("Talkie Walkie")
                .map(|b| MainView::Browse(BrowseKind::Album(b)))
        );
        assert_eq!(m.app.view(), View::Tracks);
        assert_eq!(m.app.browse_depth(), 3);
        assert_eq!(m.names(), ["Cherry Blossom Girl", "Venus"], "by number");
        let venus = m
            .app
            .playback()
            .tracks()
            .iter()
            .position(|t| t.title == "Venus");
        m.keys(&[KeyCode::Down]);
        assert_eq!(m.app.selected_track(), venus);
        // A track level opens nothing.
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.app.browse_depth(), 3);
        Ok(())
    }

    /// Spec *All tracks*.
    #[test]
    fn all_tracks_lists_the_artist_album_by_album() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Artists)?;
        m.open("Air")?;
        m.open("All tracks")?;
        assert_eq!(
            Some(m.app.main_view()),
            m.artist("Air")
                .map(|a| MainView::Browse(BrowseKind::ArtistTracks(a)))
        );
        assert_eq!(m.app.view(), View::Tracks);
        assert_eq!(
            m.names(),
            [
                "La Femme d'Argent",
                "Sexy Boy",
                "Kelly Watch the Stars",
                "Cherry Blossom Girl",
                "Venus"
            ]
        );
        Ok(())
    }

    /// Spec *Back to the same place*: the stack and each level's selection
    /// survive showing something else.
    #[test]
    fn back_to_the_same_place() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Artists)?;
        m.open("Air")?;
        m.open("Moon Safari")?;
        m.select("Sexy Boy")?;
        m.show(SidebarEntry::Library)?;
        assert_eq!(m.app.main_view(), MainView::Library);
        m.show(SidebarEntry::Albums)?;
        m.select("Debut")?;
        m.show(SidebarEntry::Artists)?;
        assert_eq!(m.app.focus(), Focus::Main);
        assert_eq!(
            Some(m.app.main_view()),
            m.album("Moon Safari")
                .map(|b| MainView::Browse(BrowseKind::Album(b)))
        );
        assert_eq!(m.selected_name().as_deref(), Some("Sexy Boy"));
        m.show(SidebarEntry::Albums)?;
        assert_eq!(m.selected_name().as_deref(), Some("Debut"));

        // `Enter` on "Artists" while it is shown only focuses the main pane.
        m.show(SidebarEntry::Artists)?;
        m.show(SidebarEntry::Artists)?;
        assert_eq!(m.app.browse_depth(), 3);
        assert_eq!(m.selected_name().as_deref(), Some("Sexy Boy"));
        Ok(())
    }

    /// Spec *Library after browsing*.
    #[test]
    fn library_after_browsing() -> Result<()> {
        let mut m = music()?;
        m.chars("/air");
        m.keys(&[KeyCode::Enter, KeyCode::Down]);
        let before = format!("{:?}", m.app.main_list());
        m.show(SidebarEntry::Albums)?;
        m.open("Moon Safari")?;
        m.show(SidebarEntry::Library)?;
        assert_eq!(m.app.main_view(), MainView::Library);
        assert_eq!(m.app.view(), View::Library);
        assert_eq!(m.app.focus(), Focus::Main);
        assert_eq!(m.app.browse_depth(), 0);
        assert_eq!(format!("{:?}", m.app.main_list()), before, "kept as left");
        Ok(())
    }

    /// Spec *Playlist after browsing*, and opening a playlist then "Albums"
    /// closes the playlist.
    #[test]
    fn playlists_and_browsing_replace_each_other() -> Result<()> {
        let mut m = music()?;
        m.write("Gym", &[("Air", "Moon Safari", "Sexy Boy")])?;
        m.show(SidebarEntry::Artists)?;
        m.show(SidebarEntry::Playlist(0))?;
        assert_eq!(m.app.main_view(), MainView::Playlist);
        assert_eq!(m.app.focus(), Focus::Main);
        assert_eq!(m.app.open_playlist().map(OpenPlaylist::name), Some("Gym"));
        assert_eq!(m.app.browse_depth(), 0);

        m.show(SidebarEntry::Albums)?;
        assert!(m.app.open_playlist().is_none());
        assert_eq!(m.app.main_view(), MainView::Browse(BrowseKind::Albums));
        assert_eq!(m.app.focus(), Focus::Main);
        // Tab and back keeps the album list (no playlist to check).
        m.keys(&[KeyCode::Tab, KeyCode::Tab]);
        assert_eq!(m.app.main_view(), MainView::Browse(BrowseKind::Albums));
        Ok(())
    }

    /// Spec *Empty library*.
    #[test]
    fn empty_library_browses_nothing_harmlessly() -> Result<()> {
        let mut m = lib(&[])?;
        for entry in [SidebarEntry::Artists, SidebarEntry::Albums] {
            m.show(entry)?;
            assert_eq!(m.app.view(), View::Groups);
            assert_eq!(m.app.visible(), [] as [usize; 0]);
            assert_eq!(m.app.selected(), None);
            m.keys(&[
                KeyCode::Enter,
                KeyCode::Down,
                KeyCode::End,
                KeyCode::PageDown,
                KeyCode::Char('j'),
                KeyCode::Char('G'),
                KeyCode::Char('a'),
                KeyCode::Char('A'),
                KeyCode::Backspace,
                KeyCode::Enter,
            ]);
            assert_eq!(*m.app.mode(), Mode::Normal);
            assert_eq!(m.app.browse_depth(), 1);
            assert_eq!(m.app.selected(), None);
            m.chars("/x");
            assert_eq!(m.app.visible(), [] as [usize; 0]);
            m.keys(&[KeyCode::Esc, KeyCode::Esc]);
            assert_eq!(m.app.focus(), Focus::Sidebar);
        }
        Ok(())
    }

    // --- Going back (task 4.3) ---

    /// Spec *Back from an album*.
    #[test]
    fn back_from_an_album() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Artists)?;
        m.open("Air")?;
        m.open("Talkie Walkie")?;
        m.keys(&[KeyCode::Backspace]);
        assert_eq!(m.app.browse_depth(), 2);
        assert_eq!(m.names(), ["All tracks", "Moon Safari", "Talkie Walkie"]);
        assert_eq!(m.selected_name().as_deref(), Some("Talkie Walkie"));
        m.keys(&[KeyCode::Backspace]);
        assert_eq!(m.app.main_view(), MainView::Browse(BrowseKind::Artists));
        assert_eq!(m.selected_name().as_deref(), Some("Air"));

        // `Esc` goes back the same way.
        m.open("Air")?;
        m.open("Moon Safari")?;
        m.keys(&[KeyCode::Esc]);
        assert_eq!(m.selected_name().as_deref(), Some("Moon Safari"));
        m.keys(&[KeyCode::Esc]);
        assert_eq!(m.app.main_view(), MainView::Browse(BrowseKind::Artists));
        assert_eq!(m.app.focus(), Focus::Main);

        // Through "Albums", one level.
        m.show(SidebarEntry::Albums)?;
        m.open("Debut")?;
        m.keys(&[KeyCode::Backspace]);
        assert_eq!(m.app.main_view(), MainView::Browse(BrowseKind::Albums));
        assert_eq!(m.selected_name().as_deref(), Some("Debut"));
        Ok(())
    }

    /// Spec *Back to the sidebar*.
    #[test]
    fn back_to_the_sidebar() -> Result<()> {
        let mut m = music()?;
        m.write("Gym", &[])?;
        for (entry, kind) in [
            (SidebarEntry::Artists, BrowseKind::Artists),
            (SidebarEntry::Albums, BrowseKind::Albums),
        ] {
            m.show(entry)?;
            m.keys(&[KeyCode::Down]);
            m.keys(&[KeyCode::Esc]);
            assert_eq!(m.app.focus(), Focus::Sidebar);
            assert_eq!(m.app.main_view(), MainView::Browse(kind), "still shown");
            assert_eq!(
                m.app.sidebar_list().selected.and_then(|row| m
                    .app
                    .sidebar_list()
                    .rows
                    .get(row)
                    .copied()),
                entry.value()
            );
            // Tab comes back to the same row.
            m.keys(&[KeyCode::Tab]);
            assert_eq!(m.app.main_list().selected, Some(1));
        }
        Ok(())
    }

    /// `Esc` from the artist list selects "Artists" even when the sidebar
    /// filter hides every playlist.
    #[test]
    fn back_to_a_filtered_sidebar() -> Result<()> {
        let mut m = music()?;
        m.write("Gym", &[])?;
        m.keys(&[KeyCode::Tab]);
        m.chars("/gym");
        m.keys(&[KeyCode::Enter]);
        m.show(SidebarEntry::Artists)?;
        m.keys(&[KeyCode::Esc]);
        assert_eq!(m.app.query(), "gym", "the sidebar keeps its filter");
        assert_eq!(
            m.app.sidebar_list().selected.and_then(|row| m
                .app
                .sidebar_list()
                .rows
                .get(row)
                .copied()),
            SidebarEntry::Artists.value()
        );
        Ok(())
    }

    /// Spec *Esc clears the filter first*.
    #[test]
    fn esc_clears_the_filter_first() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Albums)?;
        m.open("Moon Safari")?;
        m.chars("/sexy");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.names(), ["Sexy Boy"]);
        m.keys(&[KeyCode::Esc]);
        assert_eq!(m.app.query(), "");
        assert_eq!(m.app.browse_depth(), 2, "the album is still shown");
        assert_eq!(m.names().len(), 3);
        assert_eq!(m.selected_name().as_deref(), Some("Sexy Boy"), "kept");
        Ok(())
    }

    /// Spec *Backspace goes back through a filter*.
    #[test]
    fn backspace_goes_back_through_a_filter() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Albums)?;
        m.open("Moon Safari")?;
        m.chars("/sexy");
        m.keys(&[KeyCode::Enter, KeyCode::Backspace]);
        assert_eq!(m.app.main_view(), MainView::Browse(BrowseKind::Albums));
        assert_eq!(m.selected_name().as_deref(), Some("Moon Safari"));
        assert_eq!(m.app.query(), "");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.app.query(), "", "opened afresh");
        assert_eq!(m.names().len(), 3, "unfiltered");
        assert_eq!(m.app.selected(), Some(0));
        Ok(())
    }

    /// Spec *Filter artists*.
    #[test]
    fn filter_artists() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Artists)?;
        m.chars("/BJ");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.names(), ["Björk"]);
        // A guest track artist is not an artist entry; titles do not match.
        m.chars("/");
        m.keys(&[KeyCode::Backspace, KeyCode::Backspace]);
        m.chars("venus");
        assert_eq!(m.names(), [] as [String; 0]);
        Ok(())
    }

    /// Albums filter by title or album artist.
    #[test]
    fn filter_albums_by_title_or_artist() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Albums)?;
        m.chars("/various");
        assert_eq!(m.names(), ["Hits"]);
        m.keys(&[KeyCode::Esc]);
        m.chars("/safari");
        assert_eq!(m.names(), ["Moon Safari"]);
        // Never across the two fields.
        m.keys(&[KeyCode::Esc]);
        m.chars("/safari air");
        assert_eq!(m.names(), [] as [String; 0]);
        assert_eq!(m.app.selected(), None);
        Ok(())
    }

    /// Spec *Filter an artist's albums*.
    #[test]
    fn filter_an_artists_albums() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Artists)?;
        m.open("Air")?;
        m.chars("/talk");
        assert_eq!(m.names(), ["All tracks", "Talkie Walkie"]);
        assert_eq!(m.selected_name().as_deref(), Some("Talkie Walkie"));
        m.chars("zzz");
        assert_eq!(m.names(), ["All tracks"], "never filtered out");
        assert_eq!(m.selected_name().as_deref(), Some("All tracks"));
        m.keys(&[KeyCode::Enter, KeyCode::Enter]);
        assert_eq!(m.names().len(), 5, "All tracks opens unfiltered");
        Ok(())
    }

    /// Emptying the query with Backspace selects the top row again, not
    /// the first album (or, in the sidebar, the first playlist).
    #[test]
    fn emptied_query_selects_the_top_row() -> Result<()> {
        let mut m = music()?;
        m.write("Gym", &[])?;
        m.show(SidebarEntry::Artists)?;
        m.open("Air")?;
        m.chars("/x");
        m.keys(&[KeyCode::Backspace]);
        assert_eq!(m.app.query(), "");
        assert_eq!(m.selected_name().as_deref(), Some("All tracks"));
        m.keys(&[KeyCode::Esc, KeyCode::Tab]);
        m.chars("/x");
        m.keys(&[KeyCode::Backspace]);
        assert_eq!(m.app.selected(), Some(0), "\"Library\"");
        Ok(())
    }

    /// Tracks filter as in the library: title, artist or album.
    #[test]
    fn filter_browsed_tracks() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Albums)?;
        m.open("Hits")?;
        m.chars("/second");
        assert_eq!(m.names(), ["Second Hit"]);
        m.keys(&[KeyCode::Esc]);
        m.chars("/hits");
        assert_eq!(m.names(), ["First Hit", "Second Hit"], "by album");
        Ok(())
    }

    /// Spec *Filter kept while going deeper*.
    #[test]
    fn filter_kept_while_going_deeper() -> Result<()> {
        let mut m = music()?;
        m.show(SidebarEntry::Artists)?;
        m.chars("/a");
        m.keys(&[KeyCode::Enter]);
        assert_eq!(m.names(), ["Air", "Various Artists"]);
        m.open("Various Artists")?;
        assert_eq!(m.app.query(), "", "a new level has no filter");
        m.keys(&[KeyCode::Backspace]);
        assert_eq!(m.app.query(), "a");
        assert_eq!(m.names(), ["Air", "Various Artists"]);
        assert_eq!(m.selected_name().as_deref(), Some("Various Artists"));
        Ok(())
    }

    /// `Backspace` at the top level, in the library and in the sidebar does
    /// nothing.
    #[test]
    fn backspace_at_the_top_does_nothing() -> Result<()> {
        let mut m = music()?;
        m.keys(&[KeyCode::Down, KeyCode::Backspace]);
        assert_eq!(m.app.view(), View::Library);
        assert_eq!(m.app.selected(), Some(1));
        m.show(SidebarEntry::Artists)?;
        m.keys(&[KeyCode::Down, KeyCode::Backspace]);
        assert_eq!(m.app.browse_depth(), 1);
        assert_eq!(m.app.focus(), Focus::Main);
        assert_eq!(m.selected_name().as_deref(), Some("Björk"));
        m.open("Björk")?;
        m.keys(&[KeyCode::Tab, KeyCode::Backspace]);
        assert_eq!(m.app.focus(), Focus::Sidebar);
        assert_eq!(m.app.browse_depth(), 2, "not from the sidebar");
        Ok(())
    }

    /// Backspace typed in Search and in the name prompt edits the text.
    #[test]
    fn backspace_still_edits_typed_text() -> Result<()> {
        let mut m = music()?;
        m.write("Gym", &[])?;
        m.show(SidebarEntry::Albums)?;
        m.open("Moon Safari")?;
        m.chars("/sexyx");
        m.keys(&[KeyCode::Backspace]);
        assert_eq!(*m.app.mode(), Mode::Search);
        assert_eq!(m.app.query(), "sexy");
        assert_eq!(m.app.browse_depth(), 2);
        m.keys(&[KeyCode::Backspace; 5]);
        assert_eq!(m.app.query(), "");
        assert_eq!(m.app.browse_depth(), 2, "an empty query stays put");
        m.keys(&[KeyCode::Enter]);

        m.select_in_sidebar(SidebarEntry::Playlist(0))?;
        m.app
            .handle_key(KeyEvent::new(KeyCode::Char('R'), KeyModifiers::SHIFT));
        m.keys(&[KeyCode::Backspace]);
        let Mode::NameInput(input) = m.app.mode() else {
            bail!("no name prompt");
        };
        assert_eq!(input.text(), "Gy");
        m.keys(&[KeyCode::Esc]);
        assert_eq!(m.app.main_view(), MainView::Browse(BrowseKind::Album(0)));
        assert_eq!(m.app.browse_depth(), 2);
        Ok(())
    }
}
