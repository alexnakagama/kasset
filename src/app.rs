//! UI state and the mapping from keys to actions. No terminal I/O happens
//! here, so everything is testable with a fake audio output.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::library::Track;
use crate::playback::{Notice, Playback};
use crate::player::Audio;
use crate::text::is_invisible_format;

/// How long a transient status message stays on screen.
pub const STATUS_TTL: Duration = Duration::from_secs(5);

/// Longest search query accepted, in characters.
pub const MAX_QUERY_CHARS: usize = 128;

/// What keys currently do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Navigation and transport keys.
    Normal,
    /// Typing a search query.
    Search,
}

/// Everything the UI shows and the user can change.
#[derive(Debug)]
pub struct App<A: Audio> {
    playback: Playback<A>,
    /// Library root, ready for display.
    root: String,
    /// Per track, lower-cased "title artist album" for case-insensitive search.
    search_keys: Vec<String>,
    /// Library indices of the tracks listed, in library order.
    visible: Vec<usize>,
    /// Index into `visible` of the selected row (0 when the list is empty).
    selected: usize,
    /// The search query as typed (only printable, visible characters).
    query: String,
    /// `query` lower-cased, the form matched against `search_keys`.
    query_lower: String,
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
    /// The app over `tracks` (already sorted), playing through `audio`.
    /// `root` is the library folder as shown to the user; `notice`, if any,
    /// is shown in the status area on the first frame.
    pub fn new(tracks: Vec<Track>, audio: A, root: String, notice: Option<Notice>) -> Self {
        let search_keys = tracks
            .iter()
            .map(|t| format!("{}\n{}\n{}", t.title, t.artist, t.album).to_lowercase())
            .collect();
        let visible = (0..tracks.len()).collect();
        Self {
            playback: Playback::new(tracks, audio),
            root,
            search_keys,
            visible,
            selected: 0,
            query: String::new(),
            query_lower: String::new(),
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

    /// Library indices of the listed tracks.
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// Row of the selected track in [`App::visible`], if anything is listed.
    #[must_use]
    pub fn selected(&self) -> Option<usize> {
        (!self.visible.is_empty()).then_some(self.selected)
    }

    /// Library index of the selected track.
    #[must_use]
    pub fn selected_track(&self) -> Option<usize> {
        self.visible.get(self.selected).copied()
    }

    /// The search query (empty when no filter is active).
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
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
        match self.mode {
            Mode::Normal => self.normal_key(key),
            Mode::Search => self.search_key(key),
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
            KeyCode::Char('g') => self.select_row(0),
            KeyCode::Char('G') => self.select_row(usize::MAX),
            KeyCode::Enter => {
                if let Some(track) = self.selected_track() {
                    self.playback.play(track);
                }
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
            KeyCode::Esc if !self.query.is_empty() => self.clear_filter(),
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
                if self.query.pop().is_some() {
                    self.apply_filter();
                }
            }
            // Only visible characters: the query is drawn on screen.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !c.is_control()
                    && !is_invisible_format(c)
                    && self.query.chars().count() < MAX_QUERY_CHARS =>
            {
                self.query.push(c);
                self.apply_filter();
            }
            _ => {}
        }
    }

    /// Selection keys shared by both modes; `true` when `code` was one.
    fn navigation_key(&mut self, code: KeyCode) -> bool {
        let page = isize::try_from(self.page_size).unwrap_or(isize::MAX);
        match code {
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => self.move_selection(-page),
            KeyCode::PageDown => self.move_selection(page),
            KeyCode::Home => self.select_row(0),
            KeyCode::End => self.select_row(usize::MAX),
            _ => return false,
        }
        true
    }

    /// Moves the selection by `delta` rows, stopping at the ends.
    fn move_selection(&mut self, delta: isize) {
        self.select_row(self.selected.saturating_add_signed(delta));
    }

    /// Selects `row`, clamped to the list.
    fn select_row(&mut self, row: usize) {
        self.selected = row.min(self.visible.len().saturating_sub(1));
    }

    /// Recomputes the list for the current query and selects the first match.
    fn apply_filter(&mut self) {
        self.query_lower = self.query.to_lowercase();
        let query = self.query_lower.as_str();
        self.visible = self
            .search_keys
            .iter()
            .enumerate()
            .filter(|(_, key)| key.contains(query))
            .map(|(i, _)| i)
            .collect();
        self.selected = 0;
    }

    /// Lists the whole library again, keeping the selected track selected.
    fn clear_filter(&mut self) {
        let keep = self.selected_track();
        self.query.clear();
        self.query_lower.clear();
        self.visible = (0..self.search_keys.len()).collect();
        // Without a filter, row and library index coincide.
        self.selected = keep.unwrap_or(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::PlayState;
    use crate::queue::Repeat;
    use crate::test_support::{SilentAudio, track};

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
        App::new(tracks, SilentAudio::default(), "/music".to_owned(), None)
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
        App::new(tracks, SilentAudio::default(), "/music".to_owned(), None)
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
        assert_eq!(a.mode(), Mode::Search);
        typed(&mut a, "beat");
        // Title "Beat It", artist "The Beatles", title "Heartbeats", album "UPBEAT DISCO".
        assert_eq!(a.visible(), [0, 1, 2, 5]);
        press(&mut a, &[key(KeyCode::Enter)]);
        assert_eq!(a.mode(), Mode::Normal);
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
        assert_eq!(a.mode(), Mode::Normal);
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
