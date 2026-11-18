//! UI state and the mapping from keys to actions. No terminal I/O happens
//! here, and files are only reached through a [`PlaylistStore`], so
//! everything is testable with a fake audio output and a temporary folder.
//!
//! The screen has two panes: a sidebar and a main pane. Exactly one has
//! keyboard focus ([`Focus`]); `Tab` / Shift+Tab move it. Keys act on the
//! focused pane only ([`App::view`]).
//!
//! - The sidebar lists a fixed "Library" row (value 0), then the saved
//!   playlists (value `i + 1` for `playlists[i]`). The "Library" row is
//!   never filtered out, so the sidebar is never empty. `Enter` shows the
//!   selected entry in the main pane and focuses it.
//! - The main pane shows the open playlist if there is one, else the
//!   library ([`MainView`]). Only `Enter` on a sidebar playlist opens one; it
//!   is closed by `Enter` on "Library", by deleting it, and when its file
//!   disappears.
//!
//! Each list (library, sidebar, open playlist) keeps its own filter and
//! selection. The playlists are listed in [`App::new`], so the sidebar is
//! filled on the first frame, again whenever the sidebar gains focus, and
//! after rename and delete. The app's own edits (add, create, `d`/`J`/`K`,
//! reloads) update the edited playlist's sidebar entry in place.
//!
//! `?` opens the help overlay ([`Mode::Help`]). Its scroll is bounded by the
//! number of help lines the last frame showed ([`App::set_help_rows`], fed
//! by the event loop like the list page size), so scrolling stops at the
//! last page. Any key other than a scroll key closes it and does nothing
//! else.
//!
//! Status messages raised during one key press go through [`App::notify`]:
//! an error is never hidden by an info message raised after it, and two
//! messages of the same level are joined.
//!
//! Every playlist change (add, remove, move, rename, delete) goes to disk
//! before the next key is handled. Edits are made on a copy and only shown
//! once saved, so a failed or conflicting save leaves the screen as it was.
//! Changes to the playlist that is playing are forwarded to the play order.

use std::cell::OnceCell;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::keys;
use crate::library::Track;
use crate::playback::{Edit, Level, Notice, Playback, Source};
use crate::player::Audio;
use crate::playlist::{
    AddOutcome, Playlist, PlaylistInfo, PlaylistStore, Resolver, SaveOutcome, StoreError,
    validate_name,
};
use crate::text::{is_invisible_format, sanitize};

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
    /// The sidebar ("Library" and the playlists).
    Playlists,
    /// The main pane, showing the open playlist.
    Playlist,
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
}

/// One list as drawn: its rows, the selected row and its filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListView<'a> {
    /// Values of the rows listed, in order: library indices, sidebar values
    /// (0 is "Library", `i + 1` is [`App::playlists`]`[i]`), or positions in
    /// [`OpenPlaylist::items`].
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
    Playlist(String),
}

/// Rows, selection and filter of one list. Rows are values naming entries of
/// the list's full contents: library indices, sidebar values (0 is
/// "Library", `i + 1` is `playlists[i]`: never index `playlists` with a raw
/// row value) or positions in the open playlist's items.
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
    /// The sidebar: value 0 is "Library", `i + 1` is `playlists[i]`.
    playlist_list: ListState,
    /// The playlist shown in the main pane, if any (else the library is).
    open: Option<OpenPlaylist>,
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
    quit: bool,
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
            mode: Mode::Normal,
            status: None,
            pending: notice,
            pending_fresh: true,
            page_size: 1,
            help_rows: 1,
            quit: false,
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
    pub const fn view(&self) -> View {
        match (self.focus, &self.open) {
            (Focus::Sidebar, _) => View::Playlists,
            (Focus::Main, None) => View::Library,
            (Focus::Main, Some(_)) => View::Playlist,
        }
    }

    /// The pane with keyboard focus.
    #[must_use]
    pub const fn focus(&self) -> Focus {
        self.focus
    }

    /// What the main pane shows, whichever pane has focus.
    #[must_use]
    pub const fn main_view(&self) -> MainView {
        if self.open.is_some() {
            MainView::Playlist
        } else {
            MainView::Library
        }
    }

    /// The sidebar list: value 0 is "Library", `i + 1` is
    /// [`App::playlists`]`[i]`.
    #[must_use]
    pub fn sidebar_list(&self) -> ListView<'_> {
        self.playlist_list.view()
    }

    /// The main pane's list: library indices, or positions in
    /// [`OpenPlaylist::items`] when a playlist is shown.
    #[must_use]
    pub fn main_list(&self) -> ListView<'_> {
        self.open.as_ref().map_or(&self.library, |o| &o.list).view()
    }

    /// The listed rows of the focused list: library indices in the library,
    /// sidebar values in the sidebar (see [`App::sidebar_list`]), and
    /// positions in [`OpenPlaylist::items`] in an open playlist.
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

    /// Library index of the selected track, in the library or an open playlist.
    #[must_use]
    pub fn selected_track(&self) -> Option<usize> {
        match self.view() {
            View::Library => self.library.selected_value(),
            View::Playlists => None,
            View::Playlist => self
                .open
                .as_ref()
                .and_then(|o| o.items.get(o.list.selected_value()?).copied()),
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

    /// The playlist selected in the sidebar; `None` on the "Library" row.
    #[must_use]
    pub fn selected_playlist(&self) -> Option<&PlaylistInfo> {
        let value = self.playlist_list.selected_value()?;
        self.playlists.get(value.checked_sub(1)?)
    }

    /// The sidebar value of the entry that is the play order (0 for
    /// "Library"), while a track is loaded. `None` before anything was
    /// played, and while a deleted playlist plays on (no entry is its order).
    #[must_use]
    pub fn sidebar_playing(&self) -> Option<usize> {
        self.playback.current_track()?;
        match self.playback.source() {
            Source::Library => Some(0),
            Source::Playlist(name) => self.sidebar_value_of(name),
            Source::Detached => None,
        }
    }

    /// The sidebar value of playlist `name`, if it is listed.
    fn sidebar_value_of(&self, name: &str) -> Option<usize> {
        self.playlists
            .iter()
            .position(|info| info.name == name)
            .map(|i| i + 1)
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

    /// Periodic housekeeping: drives playback (auto-advance, skipping broken
    /// tracks) and collects status messages.
    pub fn tick(&mut self, now: Instant) {
        if let Some(notice) = self.pending.take() {
            self.status = Some((notice, now + STATUS_TTL));
        }
        self.playback.tick();
        // Playback messages win: they are usually errors.
        if let Some(notice) = self.playback.take_notice() {
            self.status = Some((notice, now + STATUS_TTL));
        }
        if self.status.as_ref().is_some_and(|(_, until)| now >= *until) {
            self.status = None;
        }
    }

    /// Stops audio, before quitting.
    pub fn shutdown(&mut self) {
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
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('?') => self.mode = Mode::Help { scroll: 0 },
            KeyCode::Esc => {
                if !self.query().is_empty() {
                    self.clear_filter();
                } else if let (Focus::Main, Some(open)) = (self.focus, &self.open) {
                    let name = open.name().to_owned();
                    self.focus_sidebar(Some(&name));
                }
            }
            _ => {}
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
    const fn list(&self) -> &ListState {
        match (self.focus, &self.open) {
            (Focus::Sidebar, _) => &self.playlist_list,
            (Focus::Main, None) => &self.library,
            (Focus::Main, Some(open)) => &open.list,
        }
    }

    const fn list_mut(&mut self) -> &mut ListState {
        match (self.focus, &mut self.open) {
            (Focus::Sidebar, _) => &mut self.playlist_list,
            (Focus::Main, None) => &mut self.library,
            (Focus::Main, Some(open)) => &mut open.list,
        }
    }

    /// Recomputes the focused list for its query and selects the first
    /// match: in the sidebar, the first matching playlist rather than the
    /// "Library" row, which always matches.
    fn apply_filter(&mut self) {
        self.refilter_current();
        let first = usize::from(self.focus == Focus::Sidebar && self.playlist_list.rows.len() > 1);
        self.list_mut().select_row(first);
    }

    /// Recomputes the rows of the focused list; the selection is not moved.
    fn refilter_current(&mut self) {
        let keys = &self.search_keys;
        match (self.focus, &mut self.open) {
            (Focus::Sidebar, _) => self.refilter_sidebar(),
            (Focus::Main, None) => self
                .library
                .refilter(keys.len(), |i, q| keys[i].contains(q)),
            (Focus::Main, Some(open)) => {
                let items = &open.items;
                open.list.refilter(items.len(), |i, q| {
                    keys.get(items[i]).is_some_and(|k| k.contains(q))
                });
            }
        }
    }

    /// Recomputes the sidebar rows for its query; "Library" (value 0)
    /// always stays. The selection is not moved.
    fn refilter_sidebar(&mut self) {
        let infos = &self.playlists;
        self.playlist_list.refilter(infos.len() + 1, |value, q| {
            value
                .checked_sub(1)
                .and_then(|i| infos.get(i))
                .is_none_or(|info| info.name.to_lowercase().contains(q))
        });
    }

    /// Lists the whole focused list again, keeping the selected entry selected.
    fn clear_filter(&mut self) {
        let len = match (self.focus, &self.open) {
            (Focus::Sidebar, _) => self.playlists.len() + 1,
            (Focus::Main, None) => self.search_keys.len(),
            (Focus::Main, Some(open)) => open.items.len(),
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

    /// `Enter` in the sidebar: "Library" shows the library; a playlist is
    /// opened (or, if already shown, reloaded only if its file changed, so
    /// it keeps its filter and selection). The main pane gets the focus,
    /// unless the playlist could not be read: then focus and the main pane
    /// stay as they were.
    fn show_selected(&mut self) {
        let Some(value) = self.playlist_list.selected_value() else {
            return;
        };
        let Some(index) = value.checked_sub(1) else {
            // "Library".
            self.open = None;
            self.focus = Focus::Main;
            return;
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

    /// Moves focus to the sidebar and lists the playlists again, keeping
    /// the sidebar selection unless `select` names a listed playlist (see
    /// [`Self::refresh_playlists`]).
    fn focus_sidebar(&mut self, select: Option<&str>) {
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
        match self.playlist_list.selected_value()? {
            0 => Some(SidebarPick::Library),
            _ => self
                .selected_playlist()
                .map(|info| SidebarPick::Playlist(info.name.clone())),
        }
    }

    /// Selects `pick` in the sidebar if it is listed, else keeps the
    /// selected row (clamped).
    fn restore_sidebar(&mut self, pick: Option<SidebarPick>) {
        let value = match pick {
            Some(SidebarPick::Library) => Some(0),
            Some(SidebarPick::Playlist(name)) => self.sidebar_value_of(&name),
            None => None,
        };
        self.playlist_list.select_value(value);
    }

    /// Lists the playlists again, keeping the sidebar filter and the
    /// selected entry, or selecting playlist `select` if given and listed.
    /// When the sidebar filter hides `select`, the filter is cleared, so
    /// the selection never lands on another playlist instead.
    fn refresh_playlists(&mut self, select: Option<&str>) {
        let keep = self.sidebar_pick();
        match self.store.list(self.resolver()) {
            Ok(infos) => self.playlists = infos,
            Err(err) => {
                self.playlists.clear();
                self.notify(list_error(&err));
            }
        }
        self.refilter_sidebar();
        let listed = select.filter(|name| self.sidebar_value_of(name).is_some());
        if let Some(value) = listed.and_then(|name| self.sidebar_value_of(name))
            && !self.playlist_list.rows.contains(&value)
        {
            self.playlist_list.query.clear();
            self.refilter_sidebar();
        }
        self.restore_sidebar(
            listed.map_or(keep, |name| Some(SidebarPick::Playlist(name.to_owned()))),
        );
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
        self.open = Some(OpenPlaylist {
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
            self.open = None;
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
        // they are still shown whole and can be shortened.
        self.mode = Mode::NameInput(NameInput {
            text: name.clone(),
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
                            self.open = None;
                            self.notify(store_error(&new, &err));
                        }
                    }
                }
                self.refresh_playlists(Some(&new));
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
                    self.open = None;
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
        assert_eq!(m.app.sidebar_list().rows, [0], "only \"Library\"");
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

        /// Names in the sidebar rows, "Library" included.
        fn sidebar_rows(&self) -> Vec<String> {
            self.app
                .sidebar_list()
                .rows
                .iter()
                .map(|&v| match v.checked_sub(1) {
                    None => "Library".to_owned(),
                    Some(i) => self
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
        assert_eq!(m.sidebar_rows(), ["Library", "Gym", "Long run", "Running"]);
        m.chars("/run");
        assert_eq!(m.sidebar_rows(), ["Library", "Long run", "Running"]);
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Long run"));
        m.chars("ning");
        assert_eq!(m.selected_playlist_name().as_deref(), Some("Running"));
        m.chars("zzz");
        assert_eq!(m.sidebar_rows(), ["Library"], "never filtered out");
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
        assert_eq!(m.sidebar_rows(), ["Library", "Gym", "Running"]);
        assert_eq!(m.info("Gym").and_then(|i| i.tracks), Some(2));
        Ok(())
    }

    /// Spec *No folder*, at start.
    #[test]
    fn no_folder_lists_only_library_and_writes_nothing() -> Result<()> {
        let mut m = abcd()?;
        m.restart(None);
        assert_eq!(m.sidebar_rows(), ["Library"]);
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
        assert_eq!(m.sidebar_rows(), ["Library"]);
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
        assert_eq!(m.sidebar_rows(), ["Library", "Gym", "Running"]);
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
