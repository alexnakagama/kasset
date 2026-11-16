//! UI state and the mapping from keys to actions. No terminal I/O happens
//! here, and files are only reached through a [`PlaylistStore`], so
//! everything is testable with a fake audio output and a temporary folder.
//!
//! The screen has two sides, switched with `Tab`: the library and the
//! playlists. The playlists side shows either the list of playlists or one
//! open playlist. Each list (library, playlists, open playlist) keeps its own
//! filter and selection.

use std::cell::OnceCell;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::library::Track;
use crate::playback::{Edit, Notice, Playback, Source};
use crate::player::Audio;
use crate::playlist::{AddOutcome, Playlist, PlaylistInfo, PlaylistStore, Resolver, StoreError};
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

/// Which list is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Library,
    /// The list of playlists.
    Playlists,
    /// One open playlist.
    Playlist,
}

/// Which side `Tab` toggles between.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Library,
    Playlists,
}

/// Rows, selection and filter of one list. Rows are positions in the list's
/// full contents (library indices, playlist infos or playlist items).
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
}

/// The playlist shown on the playlists side.
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
    /// Built on first use: users who never touch playlists do not pay for it.
    resolver: OnceCell<Resolver>,
    /// Library root, ready for display.
    root: String,
    /// Per track, lower-cased "title artist album" for case-insensitive search.
    search_keys: Vec<String>,
    side: Side,
    /// Rows are library indices.
    library: ListState,
    /// The playlists as last listed.
    playlists: Vec<PlaylistInfo>,
    /// Rows are indices into `playlists`.
    playlist_list: ListState,
    /// The open playlist, if any; shown instead of the list on the playlists side.
    open: Option<OpenPlaylist>,
    mode: Mode,
    /// Transient message and when it expires.
    status: Option<(Notice, Instant)>,
    /// Message raised by a key press, stamped and shown on the next tick.
    pending: Option<Notice>,
    /// Rows the list showed in the last frame (`PageUp`/`PageDown` step).
    page_size: usize,
    quit: bool,
}

impl<A: Audio> App<A> {
    /// The app over `tracks` (already sorted), playing through `audio`, with
    /// playlists kept in `store`. `root` is the library folder as shown to
    /// the user; `notice`, if any, is shown in the status area on the first
    /// frame.
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
        Self {
            playback: Playback::new(tracks, audio),
            store,
            resolver: OnceCell::new(),
            root,
            search_keys,
            side: Side::Library,
            library,
            playlists: Vec::new(),
            playlist_list: ListState::default(),
            open: None,
            mode: Mode::Normal,
            status: None,
            pending: notice,
            page_size: 1,
            quit: false,
        }
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

    /// Which list is on screen.
    #[must_use]
    pub const fn view(&self) -> View {
        match (self.side, &self.open) {
            (Side::Library, _) => View::Library,
            (Side::Playlists, None) => View::Playlists,
            (Side::Playlists, Some(_)) => View::Playlist,
        }
    }

    /// The listed rows of the list on screen: library indices in the library
    /// view, indices into [`App::playlists`] in the playlists view, and
    /// positions in [`OpenPlaylist::items`] in an open playlist.
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.list().rows
    }

    /// Row of the selection in [`App::visible`], if anything is listed.
    #[must_use]
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

    /// The search query of the list on screen (empty when no filter is active).
    #[must_use]
    pub fn query(&self) -> &str {
        &self.list().query
    }

    /// The playlists as last listed.
    #[must_use]
    pub fn playlists(&self) -> &[PlaylistInfo] {
        &self.playlists
    }

    /// The playlist open on the playlists side, if any.
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
        match &self.mode {
            Mode::Normal => self.normal_key(key),
            Mode::Search => self.search_key(key),
            Mode::Picker(_) => self.picker_key(key),
            Mode::NameInput(_) => self.name_key(key),
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
            KeyCode::Tab => self.switch_side(),
            KeyCode::Char('a') if self.view() == View::Library => {
                let tracks = self.selected_track().into_iter().collect();
                self.open_picker(tracks);
            }
            KeyCode::Char('A') if self.view() == View::Library => {
                let tracks = self.library.rows.clone();
                self.open_picker(tracks);
            }
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
                self.pending = Some(Notice::info(if on { "Shuffle on" } else { "Shuffle off" }));
            }
            KeyCode::Char('r') => {
                let repeat = self.playback.cycle_repeat();
                self.pending = Some(Notice::info(format!("Repeat {}", repeat.label())));
            }
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Esc => {
                if !self.query().is_empty() {
                    self.clear_filter();
                } else if self.view() == View::Playlist {
                    self.close_playlist();
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

    /// The list state of the list on screen.
    const fn list(&self) -> &ListState {
        match (self.side, &self.open) {
            (Side::Library, _) => &self.library,
            (Side::Playlists, None) => &self.playlist_list,
            (Side::Playlists, Some(open)) => &open.list,
        }
    }

    const fn list_mut(&mut self) -> &mut ListState {
        match (self.side, &mut self.open) {
            (Side::Library, _) => &mut self.library,
            (Side::Playlists, None) => &mut self.playlist_list,
            (Side::Playlists, Some(open)) => &mut open.list,
        }
    }

    /// Recomputes the list on screen for its query and selects the first match.
    fn apply_filter(&mut self) {
        self.refilter_current();
        self.list_mut().selected = 0;
    }

    /// Recomputes the rows of the list on screen; the selection is not moved.
    fn refilter_current(&mut self) {
        let keys = &self.search_keys;
        match (self.side, &mut self.open) {
            (Side::Library, _) => self
                .library
                .refilter(keys.len(), |i, q| keys[i].contains(q)),
            (Side::Playlists, Some(open)) => {
                let items = &open.items;
                open.list.refilter(items.len(), |i, q| {
                    keys.get(items[i]).is_some_and(|k| k.contains(q))
                });
            }
            (Side::Playlists, None) => {
                let infos = &self.playlists;
                self.playlist_list
                    .refilter(infos.len(), |i, q| infos[i].name.to_lowercase().contains(q));
            }
        }
    }

    /// Lists the whole list on screen again, keeping the selected entry selected.
    fn clear_filter(&mut self) {
        let len = match (self.side, &self.open) {
            (Side::Library, _) => self.search_keys.len(),
            (Side::Playlists, None) => self.playlists.len(),
            (Side::Playlists, Some(open)) => open.items.len(),
        };
        self.list_mut().clear_filter(len);
    }

    fn resolver(&self) -> &Resolver {
        self.resolver
            .get_or_init(|| Resolver::new(self.playback.tracks()))
    }

    /// `Enter` in normal mode: play a track or open a playlist.
    fn activate(&mut self) {
        match self.view() {
            View::Library => {
                if let Some(track) = self.library.selected_value() {
                    self.playback.play(track);
                }
            }
            View::Playlists => {
                let name = self
                    .playlist_list
                    .selected_value()
                    .and_then(|i| self.playlists.get(i))
                    .map(|info| info.name.clone());
                if let Some(name) = name {
                    self.open_named(&name);
                }
            }
            View::Playlist => {
                let Some(open) = &self.open else {
                    return;
                };
                if let Some(item) = open.list.selected_value() {
                    let source = Source::Playlist(open.name().to_owned());
                    if !self.playback.play_source(source, open.items.clone(), item) {
                        self.pending = Some(Notice::error(format!(
                            "Cannot play {}: its tracks no longer match the library",
                            quoted(open.name())
                        )));
                    }
                }
            }
        }
    }

    /// `Tab`: shows the other side. Entering the playlists side refreshes
    /// the listing and reloads the open playlist if its file changed.
    fn switch_side(&mut self) {
        match self.side {
            Side::Playlists => self.side = Side::Library,
            Side::Library => {
                self.side = Side::Playlists;
                self.refresh_playlists(None);
                self.check_open();
            }
        }
    }

    /// Lists the playlists again, keeping the filter and the selected
    /// playlist (or selecting `select`, if given and listed).
    fn refresh_playlists(&mut self, select: Option<&str>) {
        let keep = select.map(str::to_owned).or_else(|| {
            self.playlist_list
                .selected_value()
                .and_then(|i| self.playlists.get(i))
                .map(|info| info.name.clone())
        });
        match self.store.list(self.resolver()) {
            Ok(infos) => self.playlists = infos,
            Err(err) => {
                self.playlists.clear();
                self.pending = Some(list_error(&err));
            }
        }
        let infos = &self.playlists;
        self.playlist_list
            .refilter(infos.len(), |i, q| infos[i].name.to_lowercase().contains(q));
        let value = keep.and_then(|name| infos.iter().position(|info| info.name == name));
        self.playlist_list.select_value(value);
    }

    /// Reloads the open playlist if another program changed its file, or
    /// goes back to the list if the file is gone.
    fn check_open(&mut self) {
        let Some(open) = &self.open else {
            return;
        };
        match self.store.changed(&open.playlist) {
            Ok(false) => {}
            Ok(true) => {
                let name = open.name().to_owned();
                if self.reload(&name) {
                    self.pending = Some(Notice::info(format!(
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
                self.pending = Some(store_error(&name, &err));
            }
        }
    }

    /// Opens playlist `name` from the playlists list.
    fn open_named(&mut self, name: &str) {
        let playlist = match self.store.load(name, self.resolver()) {
            Ok(playlist) => playlist,
            Err(StoreError::NotFound) => {
                self.playlist_gone(name);
                return;
            }
            Err(err) => {
                self.pending = Some(store_error(name, &err));
                return;
            }
        };
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
            self.pending = Some(Notice::info(format!(
                "{}: {}",
                quoted(name),
                parts.join("; ")
            )));
        }
    }

    /// `Esc` in an open playlist: back to the list, with that playlist selected.
    fn close_playlist(&mut self) {
        if let Some(open) = self.open.take() {
            self.refresh_playlists(Some(open.name()));
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
                self.pending = Some(store_error(name, &err));
                false
            }
        }
    }

    /// The file of playlist `name` disappeared: if it was open, go back to
    /// the list; if it was playing, playback goes on detached from it.
    fn playlist_gone(&mut self, name: &str) {
        if self.open.as_ref().is_some_and(|o| o.name() == name) {
            self.open = None;
        }
        self.playback.playlist_deleted(name);
        self.refresh_playlists(None);
        self.pending = Some(Notice::error(format!("{} no longer exists", quoted(name))));
    }

    /// Takes `playlist` as the latest version of its file: it replaces the
    /// open playlist if that one has the same name (keeping its filter and
    /// selection), and the play order if it is the playlist playing and the
    /// play order does not match it.
    fn install(&mut self, playlist: Playlist) {
        let items = playlist.tracks();
        self.follow_reload(playlist.name(), &items);
        let keys = &self.search_keys;
        match &mut self.open {
            Some(open) if open.name() == playlist.name() => {
                let keep = open.list.selected_value();
                open.playlist = playlist;
                open.items = items;
                let items = &open.items;
                open.list.refilter(items.len(), |i, q| {
                    keys.get(items[i]).is_some_and(|k| k.contains(q))
                });
                open.list.select_value(keep);
            }
            Some(_) | None => {}
        }
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
            self.pending = Some(Notice::info("No track to add"));
            return;
        }
        let choices = match self.store.list(self.resolver()) {
            Ok(infos) => infos,
            Err(err) => {
                self.pending = Some(list_error(&err));
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
            self.pending = Some(Notice::error(format!(
                "Cannot add to {}: {reason}",
                quoted(&info.name)
            )));
            self.mode = Mode::Picker(picker);
            return;
        }
        let name = info.name.clone();
        self.add_to(&name, &picker.tracks);
    }

    /// Adds library `tracks` to existing playlist `name` and saves it.
    fn add_to(&mut self, name: &str, tracks: &[usize]) {
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
                    self.pending = Some(store_error(name, &err));
                    return;
                }
            },
        };
        let before = playlist.tracks();
        let outcome = match playlist.add(tracks, self.playback.tracks()) {
            Ok(outcome) => outcome,
            Err(err) => {
                self.pending = Some(Notice::error(format!(
                    "Cannot add to {}: {err}",
                    quoted(name)
                )));
                return;
            }
        };
        if outcome.added == 0 {
            self.pending = Some(add_notice(name, outcome));
            return;
        }
        match self.store.save(&mut playlist) {
            Ok(crate::playlist::SaveOutcome::Saved) => {
                let after = playlist.tracks();
                let appended = after.get(before.len()..).unwrap_or_default().to_vec();
                self.follow_edit(name, &before, &after, Edit::Append(appended));
                if self.open.as_ref().is_some_and(|o| o.name() == name) {
                    self.install(playlist);
                }
                self.pending = Some(add_notice(name, outcome));
            }
            Ok(crate::playlist::SaveOutcome::ChangedOnDisk) => {
                if self.reload(name) {
                    self.pending = Some(Notice::error(format!(
                        "{} was changed by another program and has been reloaded; nothing was added",
                        quoted(name)
                    )));
                }
            }
            Err(StoreError::NotFound) => self.playlist_gone(name),
            Err(err) => self.pending = Some(store_error(name, &err)),
        }
    }

    /// `Enter` in the name prompt.
    fn submit_name(&mut self) {
        let Mode::NameInput(mut input) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        let result = match &input.purpose {
            NamePurpose::Create(tracks) => self.create_playlist(&input.text, tracks),
        };
        if let Err(reason) = result {
            self.pending = Some(Notice::error(format!("Playlist not created: {reason}")));
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
                self.pending = Some(store_error(name.trim(), &err));
                return Ok(());
            }
        };
        let name = playlist.name().to_owned();
        let outcome = match playlist.add(tracks, self.playback.tracks()) {
            Ok(outcome) => outcome,
            Err(err) => {
                self.pending = Some(Notice::error(format!(
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
            self.pending = Some(notice);
            return Ok(());
        }
        match self.store.create(&mut playlist) {
            Ok(()) => {
                let mut notice = add_notice(&name, outcome);
                notice.text = format!("Created {}. {}", quoted(&name), notice.text);
                self.pending = Some(notice);
                Ok(())
            }
            Err(StoreError::Name(err)) => Err(err.to_string()),
            Err(err) => {
                self.pending = Some(store_error(&name, &err));
                Ok(())
            }
        }
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

        /// Selects the library track titled `title` (no filter active).
        fn select_in_library(&mut self, title: &str) -> Result<()> {
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

        /// From the library: `Tab`, then opens playlist `name`.
        fn open(&mut self, name: &str) -> Result<()> {
            self.keys(&[KeyCode::Tab]);
            let Some(index) = self.app.playlists().iter().position(|i| i.name == name) else {
                bail!("{name} not listed");
            };
            let Some(row) = self.app.visible().iter().position(|&r| r == index) else {
                bail!("{name} filtered out");
            };
            self.keys(&[KeyCode::Home]);
            for _ in 0..row {
                self.keys(&[KeyCode::Down]);
            }
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

    // --- Switching sides, opening and leaving playlists (task 3.1) ---

    #[test]
    fn open_playlists_view_without_folder_writes_nothing() -> Result<()> {
        let mut m = abcd()?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.view(), View::Playlists);
        assert_eq!(m.app.playlists(), []);
        assert!(!m.dir().exists(), "nothing written to disk");
        m.keys(&[KeyCode::Enter, KeyCode::Esc]);
        assert_eq!(m.app.view(), View::Playlists);
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

        // From an open playlist too.
        m.write("Running", &["A"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.view(), View::Library);
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

    #[test]
    fn open_playlist_removed_meanwhile_falls_back_to_the_list() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.write("Gym", &["B"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Tab]);
        fs::remove_file(m.file("Running"))?;
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.app.view(), View::Playlists);
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
        let selected = m.app.selected().map(|row| m.app.visible()[row]);
        assert_eq!(
            selected
                .and_then(|i| m.app.playlists().get(i))
                .map(|i| i.name.as_str()),
            Some("Running"),
            "the playlist just left is selected"
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
        m.keys(&[KeyCode::Tab]);
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
        m.keys(&[KeyCode::Tab]);
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
        m.keys(&[KeyCode::Tab]);
        m.write("Running", &["A", "C"])?;
        m.select_in_library("B")?;
        m.chars("a");
        m.pick("Running")?;
        assert_eq!(m.file_titles("Running")?, ["A", "C"], "not overwritten");
        let status = m.status();
        assert!(status.contains("reloaded"), "{status}");
        assert!(status.contains("nothing was added"), "{status}");
        m.keys(&[KeyCode::Tab]);
        assert_eq!(m.shown_titles(), ["A", "C"], "the other program's change");
        Ok(())
    }

    #[test]
    fn adding_to_the_open_playlist_shows_the_new_tracks() -> Result<()> {
        let mut m = abcd()?;
        m.write("Running", &["A"])?;
        m.open("Running")?;
        m.keys(&[KeyCode::Tab]);
        m.select_in_library("D")?;
        m.chars("a");
        m.pick("Running")?;
        m.keys(&[KeyCode::Tab]);
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
        m.keys(&[KeyCode::Down, KeyCode::Enter, KeyCode::Esc, KeyCode::Tab]);
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
        assert_eq!(m.app.view(), View::Playlists);
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
}
