//! Playback control: ties the play order ([`Queue`]) to an audio output
//! ([`Audio`]) and implements the transport commands, automatic advance and
//! skipping tracks that cannot be played. It also passes the newest decoded
//! samples through for the visualizer ([`Playback::recent_samples`]), so
//! nothing above it reaches the audio output directly.
//!
//! The play order's identity is a [`Source`]: the library, a playlist, an
//! artist or an album of the browse catalog (both by catalog index, valid
//! for one run), or a deleted playlist playing on ([`Source::Detached`]).
//! Only playlist sources follow playlist edits, renames and deletions.
//!
//! On top of the play order sits the *up-next queue* (`up_next`, D1 of
//! add-play-queue): library indices (duplicates allowed, at most
//! [`MAX_UP_NEXT`]) that play before the play order continues. A track taken
//! from it becomes `queued_current`, while the play order's cursor stays on
//! the track played before the queue took over. [`Playback::current`] covers
//! both; [`Playback::current_item`] is `None` while a queued track is
//! current, and [`Playback::source_current`] / [`Playback::source_item`] give
//! the play order's position. Every move to another track (`n`, a track's
//! end, skipping a failure) goes through one rule (`advance`, D3): repeat
//! one replays the current track at its end (a removed, detached play order
//! track cannot be replayed and moves on), else the first queued track
//! plays, else the play order moves on. `advance` clears `queued_current`
//! only when the play order actually moves; starting a new play order,
//! going back with `p` and a failed resume clear it too. Shuffle never applies to the queue,
//! no track goes back into it by itself, starting a new play order keeps it,
//! and queued failures never count toward stopping the play order.
//! `up_next_generation` changes exactly when `up_next` does, and
//! `up_next_taken` counts tracks taken from its front, so the interface can
//! keep its selection on the same entry.
//!
//! A saved session is continued with [`Playback::resume`] (after the
//! `set_volume` / `set_shuffle` / `set_repeat` setters restore the
//! settings): it loads one track paused at a position (the play order's, or
//! a queued one with the play order's cursor set without loading it, or with
//! [`Playback::resume_queued`] a queued one with nothing current in the
//! library) and, unlike every other way of starting a track, never moves on
//! to another track when that one fails.

use std::time::Duration;

use crate::library::Track;
use crate::player::{Audio, Recent, SEEK_STEP, SeekFailed, Status, Volume, seek_target};
use crate::queue::{Advance, Queue, Repeat};
use crate::text::{display_path, sanitize};

/// Pressing previous later than this into a track restarts it instead.
pub const PREV_RESTART_AFTER: Duration = Duration::from_secs(3);

/// A track that ends more than this before its decoder-reported length is
/// reported as having ended early (most likely a decode error mid-file).
const EARLY_END_TOLERANCE: Duration = Duration::from_secs(2);

/// Most tracks tried in a single call before deferring to the next
/// [`Playback::tick`], so a run of unplayable files cannot freeze the UI.
const MAX_ATTEMPTS_PER_CALL: usize = 16;

/// Most tracks the up-next queue holds (and the state file keeps per folder).
pub const MAX_UP_NEXT: usize = 1000;

/// Transport state shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Stopped,
    Playing,
    Paused,
}

/// Where the play order comes from; compared by the interface to decide
/// whether an open playlist, album or artist is the one playing.
/// `Playback` only stores and compares it: the items always come from the
/// caller, and only [`Source::Playlist`] follows playlist edits, renames and
/// deletions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Source {
    /// The whole library, in library order.
    #[default]
    Library,
    /// The saved playlist with this name.
    Playlist(String),
    /// All tracks of the artist with this index in the run's
    /// `browse::Catalog`, album by album. The index is only valid within one
    /// run and is never saved.
    Artist(usize),
    /// The tracks of the album with this index in the run's
    /// `browse::Catalog`, in album order. Only valid within one run.
    Album(usize),
    /// A playlist that was deleted while playing: its play order keeps
    /// going, but it no longer belongs to any playlist.
    Detached,
}

/// A change to the list of the playlist that is playing, forwarded with
/// [`Playback::edit_playlist`]. Positions are item positions (the
/// playlist's matched entries, in file order); tracks are library indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// The entry at this position was removed.
    Remove(usize),
    /// The entry at `from` was moved to `to`.
    Move { from: usize, to: usize },
    /// These tracks were added at the end.
    Append(Vec<usize>),
    /// The playlist was reloaded and now holds these tracks.
    Replace(Vec<usize>),
}

/// How serious a [`Notice`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Error,
}

/// A message for the status area. Text is already sanitised for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub text: String,
    pub level: Level,
}

impl Notice {
    /// An informational message; `text` must already be safe to display.
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            level: Level::Info,
        }
    }

    /// An error message; `text` must already be safe to display.
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            level: Level::Error,
        }
    }
}

/// The library, its play order and the audio output, driven by user commands
/// and [`Playback::tick`].
#[derive(Debug)]
pub struct Playback<A: Audio> {
    tracks: Vec<Track>,
    audio: A,
    queue: Queue,
    /// What `queue` plays: the library or a playlist.
    source: Source,
    state: PlayState,
    volume: Volume,
    /// Tracks that failed in a row; reset as soon as one plays.
    failures: usize,
    /// The current track still has to be (re)tried on the next tick.
    retry: bool,
    /// Latest transient message, drained by the UI.
    notice: Option<Notice>,
    /// Why playback stopped on its own, if it did; stays until playback
    /// starts again, so transient messages cannot hide it for good.
    stop_reason: Option<Notice>,
    /// Library indices waiting to play before the play order continues, in
    /// the order they play. At most [`MAX_UP_NEXT`], all valid indices.
    up_next: Vec<usize>,
    /// The queued track that is current (taken out of `up_next`), if one is.
    /// While it is set, `queue`'s cursor stays on the play order's track
    /// played before it.
    queued_current: Option<usize>,
    /// Tracks taken from the front of `up_next` to play them (wrapping).
    up_next_taken: u64,
    /// Changes (wrapping) every time `up_next` does, and only then.
    up_next_generation: u64,
}

impl<A: Audio> Playback<A> {
    /// Playback over `tracks` (in display order) through `audio`, stopped,
    /// at full volume, with shuffle and repeat off.
    pub fn new(tracks: Vec<Track>, mut audio: A) -> Self {
        let volume = Volume::default();
        audio.set_volume(volume);
        Self {
            queue: Queue::new(tracks.len()),
            source: Source::Library,
            tracks,
            audio,
            state: PlayState::Stopped,
            volume,
            failures: 0,
            retry: false,
            notice: None,
            stop_reason: None,
            up_next: Vec::new(),
            queued_current: None,
            up_next_taken: 0,
            up_next_generation: 0,
        }
    }

    /// The audio output, for tests that script it.
    #[cfg(test)]
    pub const fn audio_mut(&mut self) -> &mut A {
        &mut self.audio
    }

    /// The audio output, for tests that inspect it.
    #[cfg(test)]
    pub const fn audio(&self) -> &A {
        &self.audio
    }

    /// All tracks, in library order.
    #[must_use]
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Library index of the current track (playing, paused, or last played):
    /// the queued track that is current, else the play order's.
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        self.queued_current.or_else(|| self.queue.current())
    }

    /// Whether the current track came from the up-next queue.
    #[must_use]
    pub const fn playing_queued(&self) -> bool {
        self.queued_current.is_some()
    }

    /// Library index of the play order's current track, even while a queued
    /// track is current (then it is the track played before the queue took
    /// over).
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used from group 4 of add-play-queue")
    )]
    pub fn source_current(&self) -> Option<usize> {
        self.queue.current()
    }

    /// Position of the play order's current track in the source's list, even
    /// while a queued track is current; see [`Playback::current_item`].
    #[must_use]
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used from group 4 of add-play-queue")
    )]
    pub fn source_item(&self) -> Option<usize> {
        self.queue.current_item()
    }

    /// The current track, if any.
    #[must_use]
    pub fn current_track(&self) -> Option<&Track> {
        self.current().and_then(|i| self.tracks.get(i))
    }

    #[must_use]
    pub const fn state(&self) -> PlayState {
        self.state
    }

    #[must_use]
    pub const fn volume(&self) -> Volume {
        self.volume
    }

    #[must_use]
    pub const fn shuffle(&self) -> bool {
        self.queue.shuffle()
    }

    #[must_use]
    pub const fn repeat(&self) -> Repeat {
        self.queue.repeat()
    }

    /// Elapsed time in the current track (zero when stopped), never past its
    /// known length.
    #[must_use]
    pub fn position(&self) -> Duration {
        if self.state == PlayState::Stopped {
            return Duration::ZERO;
        }
        let pos = self.audio.position();
        self.duration().map_or(pos, |d| pos.min(d))
    }

    /// The newest decoded mono samples of the loaded track, for the
    /// visualizer; see [`Audio::recent_samples`].
    pub fn recent_samples(&self, out: &mut Vec<f32>) -> Option<Recent> {
        self.audio.recent_samples(out)
    }

    /// Length of the current track: from the decoder while it is loaded,
    /// else from its tags. The decoder is preferred because a tag-based
    /// estimate (e.g. MP3 without a Xing header) can be too short, which would
    /// cut seeking and the progress display short.
    #[must_use]
    pub fn duration(&self) -> Option<Duration> {
        self.audio
            .duration()
            .or_else(|| self.current_track().and_then(|t| t.duration))
    }

    /// The latest transient status message, removing it.
    pub fn take_notice(&mut self) -> Option<Notice> {
        self.notice.take()
    }

    /// Why playback stopped by itself (e.g. no playable tracks), kept until
    /// playback starts again. Meant to be shown whenever no transient
    /// message is.
    #[must_use]
    pub const fn stop_reason(&self) -> Option<&Notice> {
        self.stop_reason.as_ref()
    }

    /// Plays library track `track` from the start, replacing the current one,
    /// with the whole library as the play order. The up-next queue is kept.
    /// Out-of-range indices are ignored.
    pub fn play(&mut self, track: usize) {
        if track >= self.tracks.len() {
            return;
        }
        self.queued_current = None;
        if self.source != Source::Library {
            self.queue.reset((0..self.tracks.len()).collect());
            self.source = Source::Library;
        }
        if self.queue.select(track, &mut rand::rng()) {
            self.failures = 0;
            self.start_current(false);
        }
    }

    /// Pauses or resumes. When stopped, starts the current track again from
    /// the beginning, or with nothing current the first queued track; does
    /// nothing if there is neither.
    pub fn toggle_pause(&mut self) {
        match self.state {
            // While unplayable tracks are still being skipped (`retry`), only
            // the state changes: the next attempt then loads paused.
            PlayState::Playing => {
                self.audio.set_paused(true);
                self.state = PlayState::Paused;
            }
            PlayState::Paused => {
                self.audio.set_paused(false);
                self.state = PlayState::Playing;
            }
            PlayState::Stopped => {
                if self.current().is_some() {
                    self.failures = 0;
                    self.start_current(false);
                } else if !self.up_next.is_empty() && self.advance(Advance::Manual) {
                    // Nothing current, so `advance` takes the first queued
                    // track (manual causes never replay).
                    self.failures = 0;
                    self.start_current(false);
                }
            }
        }
    }

    /// Stops playback; the current track (a queued one too) stays selected and
    /// restarts from the beginning when resumed.
    pub fn stop(&mut self) {
        self.audio.stop();
        self.state = PlayState::Stopped;
        self.retry = false;
    }

    /// Starts the first queued track, else the next track in play order (the
    /// first if nothing is current).
    pub fn next(&mut self) {
        if self.advance(Advance::Manual) {
            self.failures = 0;
            self.start_current(false);
        } else if !self.queue.is_empty() || self.queued_current.is_some() {
            self.notice = Some(Notice::info("Already at the last track"));
        }
    }

    /// Restarts the current track if it is more than
    /// [`PREV_RESTART_AFTER`] in, else starts the previous track in play order
    /// (restarting the current one when there is no previous). For a queued
    /// track "previous" is the play order's track played before it (from the
    /// start), or the queued track again when the play order has nothing
    /// current; the queued track never goes back into the queue.
    pub fn prev(&mut self) {
        if self.current().is_none() {
            return;
        }
        if self.state == PlayState::Stopped || self.position() <= PREV_RESTART_AFTER {
            if self.queued_current.is_some() {
                if self.queue.current().is_some() {
                    self.queued_current = None;
                }
            } else {
                // At the start of the order the current track is kept and
                // restarted.
                self.queue.prev();
            }
        }
        self.failures = 0;
        self.start_current(false);
    }

    /// Seeks the current track by [`SEEK_STEP`], clamped to its bounds.
    /// Seeking to the end finishes the track as if it had played out.
    pub fn seek(&mut self, forward: bool) {
        if self.state == PlayState::Stopped {
            return;
        }
        let Some(index) = self.current() else {
            return;
        };
        let duration = self.duration();
        let target = seek_target(self.position(), SEEK_STEP, forward, duration);
        if duration.is_some_and(|d| target >= d) {
            self.track_ended();
            return;
        }
        let paused = self.state == PlayState::Paused;
        let path = self.tracks[index].path.clone();
        if let Err(err) = self.audio.load(&path, target, paused) {
            let past_end = err
                .downcast_ref::<SeekFailed>()
                .is_some_and(|failure| failure.past_end);
            if forward && past_end {
                // Only reachable with an unknown length: a known one is
                // clamped above.
                self.track_ended();
            } else {
                // The output keeps playing the track where it was.
                self.notice = Some(Notice::error(format!("Cannot seek: {}", reason(&err))));
            }
        }
    }

    /// Raises the volume one step.
    pub fn volume_up(&mut self) {
        self.set_volume(self.volume.up());
    }

    /// Lowers the volume one step.
    pub fn volume_down(&mut self) {
        self.set_volume(self.volume.down());
    }

    /// Flips shuffle, keeping the current track. Returns the new state.
    pub fn toggle_shuffle(&mut self) -> bool {
        self.queue.toggle_shuffle(&mut rand::rng())
    }

    /// Moves to the next repeat mode and returns it.
    pub const fn cycle_repeat(&mut self) -> Repeat {
        self.queue.cycle_repeat()
    }

    /// Sets the volume and passes it to the audio output.
    pub fn set_volume(&mut self, volume: Volume) {
        self.volume = volume;
        self.audio.set_volume(volume);
    }

    /// Turns shuffle on or off, keeping the current track; see
    /// [`Queue::set_shuffle`].
    pub fn set_shuffle(&mut self, on: bool) {
        self.queue.set_shuffle(on, &mut rand::rng());
    }

    /// Sets the repeat mode.
    pub const fn set_repeat(&mut self, repeat: Repeat) {
        self.queue.set_repeat(repeat);
    }

    /// Housekeeping, to be called regularly (every UI frame): reports output
    /// device errors, advances when the current track ends and skips tracks
    /// that fail to decode.
    pub fn tick(&mut self) {
        if let Some(err) = self.audio.take_device_error() {
            let err = sanitize(&err).unwrap_or_else(|| "unknown error".to_owned());
            self.notice = Some(Notice::error(format!("Audio device error: {err}")));
        }
        if self.retry {
            self.start_current(self.state == PlayState::Paused);
            return;
        }
        if self.state == PlayState::Stopped {
            return;
        }
        match self.audio.status() {
            Status::Active => {
                if !self.audio.position().is_zero() {
                    self.failures = 0;
                }
            }
            Status::Ended => {
                self.report_early_end();
                self.track_ended();
            }
            Status::Failed(why) => {
                if self.record_failure(&why) {
                    self.start_current(false);
                }
            }
            Status::Idle => self.state = PlayState::Stopped,
        }
    }

    /// Moves to the track to play after the current one (D3 of
    /// add-play-queue) and returns whether one is now current to start:
    ///
    /// 1. at a track's end with repeat one, the current track is kept (a
    ///    queued one, or the play order's unless it is detached, which
    ///    cannot be replayed and moves on as before), so the queue waits;
    /// 2. else the first queued track is taken out of the queue;
    /// 3. else the play order moves on. Only when it does is the queued
    ///    track (if one was current) left behind; at the end of the order
    ///    with repeat off nothing changes and `false` is returned.
    fn advance(&mut self, cause: Advance) -> bool {
        if cause == Advance::Ended && self.queue.repeat() == Repeat::One {
            if self.queued_current.is_some() {
                return true;
            }
            if self.queue.current().is_some() && !self.queue.is_detached() {
                // `Queue::next` keeps the current item for this cause.
                return self.queue.next(cause).is_some();
            }
        }
        if !self.up_next.is_empty() {
            let track = self.up_next.remove(0);
            self.queued_current = Some(track);
            self.up_next_taken = self.up_next_taken.wrapping_add(1);
            self.up_next_changed();
            return true;
        }
        if self.queue.next(cause).is_some() {
            self.queued_current = None;
            true
        } else {
            false
        }
    }

    /// Whether every entry of `tracks` is a library index.
    fn valid_tracks(&self, tracks: &[usize]) -> bool {
        tracks.iter().all(|&t| t < self.tracks.len())
    }

    /// The current track finished normally: continue per repeat mode, the
    /// queue first.
    fn track_ended(&mut self) {
        self.failures = 0;
        if self.advance(Advance::Ended) {
            // Seeking to the end while paused moves on to the next track paused.
            self.start_current(self.state == PlayState::Paused);
        } else {
            self.stop();
        }
    }

    /// Decoders end a track quietly when they hit corrupt data mid-file; tell
    /// the user when the track stopped well short of its decoded length.
    fn report_early_end(&mut self) {
        let Some(length) = self.audio.duration() else {
            return;
        };
        let reached = self.audio.position();
        if reached.saturating_add(EARLY_END_TOLERANCE) < length {
            let name = self.current_track().map_or_else(String::new, file_name);
            self.notice = Some(Notice::error(format!(
                "'{name}' ended early at {} of {}; the file may be damaged",
                clock(reached),
                clock(length)
            )));
        }
    }

    /// Loads the current track from the start and plays it (or leaves it
    /// paused). Tracks that fail are reported and skipped; after
    /// [`MAX_ATTEMPTS_PER_CALL`] failures the rest is deferred to the next tick.
    fn start_current(&mut self, paused: bool) {
        // Any pending retry is superseded by this attempt.
        self.retry = false;
        for _ in 0..MAX_ATTEMPTS_PER_CALL {
            let Some(index) = self.current() else {
                self.stop();
                return;
            };
            let path = self.tracks[index].path.clone();
            match self.audio.load(&path, Duration::ZERO, paused) {
                Ok(()) => {
                    self.state = if paused {
                        PlayState::Paused
                    } else {
                        PlayState::Playing
                    };
                    self.stop_reason = None;
                    return;
                }
                Err(err) => {
                    if !self.record_failure(&reason(&err)) {
                        return;
                    }
                }
            }
        }
        // Keep the transport running so the UI shows progress is being made.
        self.state = if paused {
            PlayState::Paused
        } else {
            PlayState::Playing
        };
        self.retry = true;
    }

    /// Reports the current track as unplayable and moves past it: to the
    /// next queued track, else along the play order. Returns `false` when
    /// playback stopped: end of the order with repeat off, or every track in
    /// the order failed in a row.
    fn record_failure(&mut self, why: &str) -> bool {
        self.audio.stop();
        let queued = self.queued_current.is_some();
        // `failures` counts tracks of the play order; a queued track, or a
        // removed track that is still current, is not one of them, so its
        // failure must not use up an attempt meant for the tracks still in
        // the order.
        if !queued && !self.queue.is_detached() {
            self.failures = self.failures.saturating_add(1);
        }
        let name = self.current_track().map_or_else(String::new, file_name);
        let mut notice = Notice::error(format!("Cannot play '{name}': {why}"));

        // A queued track has already left `up_next` and each one is tried
        // once, so moving on through the queue cannot loop.
        let more = if self.up_next.is_empty() {
            self.failures < self.queue.len() && self.advance(Advance::Failed)
        } else {
            self.advance(Advance::Failed)
        };
        if !more {
            if !queued && self.up_next.is_empty() && self.failures >= self.queue.len() {
                notice.text.push_str(" (no playable tracks, stopped)");
            }
            self.state = PlayState::Stopped;
            self.retry = false;
            self.stop_reason = Some(notice.clone());
        }
        self.notice = Some(notice);
        more
    }
}

/// Playlists as the play order.
impl<A: Audio> Playback<A> {
    /// Where the play order comes from.
    #[must_use]
    pub const fn source(&self) -> &Source {
        &self.source
    }

    /// Position of the current track in the source's list (for a playlist,
    /// the index among its matched entries), or `None` when nothing is
    /// current, the current entry was removed from the list, or the current
    /// track is a queued one.
    #[must_use]
    pub fn current_item(&self) -> Option<usize> {
        if self.queued_current.is_some() {
            return None;
        }
        self.queue.current_item()
    }

    /// The source's list as the play order knows it: library indices, in
    /// list order (duplicates kept).
    #[must_use]
    pub fn items(&self) -> &[usize] {
        self.queue.items()
    }

    /// Makes `items` (library indices, duplicates allowed) the play order,
    /// coming from `source`, and plays the item at position `start` from the
    /// beginning. Shuffle and repeat stay as they are, and so does the
    /// up-next queue. Returns `false` and changes nothing when `start` is out
    /// of range or an item is not a library index.
    pub fn play_source(&mut self, source: Source, items: Vec<usize>, start: usize) -> bool {
        if start >= items.len() || !self.valid_tracks(&items) {
            return false;
        }
        self.queued_current = None;
        self.queue.reset(items);
        self.source = source;
        if self.queue.select(start, &mut rand::rng()) {
            self.failures = 0;
            self.start_current(false);
        }
        true
    }

    /// Continues a saved session: makes `items` (library indices) from
    /// `source` the play order with the item at position `item` current, and
    /// loads one track **paused** at `position`: that item's track, or with
    /// `queued` (a library index) that queued track, played after the item
    /// (the item's track is then not loaded; the queue and then the play
    /// order continue after it as usual). Shuffle and repeat stay as they
    /// are (with shuffle on, the new order starts at `item`), and so does the
    /// up-next queue.
    ///
    /// The track starts at zero instead when its tag length is known and
    /// `position` is at or past it, and a load that fails to seek
    /// ([`SeekFailed`], for any reason) is retried once at zero, since the
    /// file itself may be playable. Other load errors are not retried.
    /// Unlike every other way of starting a track, a failure never moves on
    /// to another one: nothing is current (the play order keeps its list and
    /// source, with no cursor), playback is stopped, and the notice (kept as
    /// the stop reason) says "Cannot resume …".
    ///
    /// Returns whether the track was loaded. Returns `false` and changes
    /// nothing when `item` is out of range, an item is not a library index,
    /// or `queued` is not one.
    pub fn resume(
        &mut self,
        source: Source,
        items: Vec<usize>,
        item: usize,
        position: Duration,
        queued: Option<usize>,
    ) -> bool {
        if item >= items.len()
            || !self.valid_tracks(&items)
            || queued.is_some_and(|q| q >= self.tracks.len())
        {
            return false;
        }
        // `items` were checked to be library indices, so `items[item]` is
        // the track `select` makes current, with or without shuffle.
        let Some(&track) = items.get(item) else {
            return false;
        };
        let Some(load) = ResumeLoad::new(&self.tracks, queued.unwrap_or(track), position) else {
            return false;
        };
        self.audio.stop();
        self.retry = false;
        self.failures = 0;
        self.queue.reset(items);
        self.source = source;
        let selected = self.queue.select(item, &mut rand::rng());
        debug_assert!(selected, "`item` was checked to be in range");
        self.queued_current = queued;
        self.finish_resume(&load)
    }

    /// Continues a saved session whose queued track was played with nothing
    /// current in the play order: the library becomes the play order with
    /// nothing current, and queued track `track` (a library index) is loaded
    /// **paused** at `position`, by the rules of [`Playback::resume`]. Once
    /// it and the queue are done, the library plays from its first track.
    /// Returns whether the track was loaded; `false` with nothing changed for
    /// an invalid index.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used from group 4 of add-play-queue")
    )]
    pub fn resume_queued(&mut self, track: usize, position: Duration) -> bool {
        let Some(load) = ResumeLoad::new(&self.tracks, track, position) else {
            return false;
        };
        self.audio.stop();
        self.retry = false;
        self.failures = 0;
        self.queue.reset((0..self.tracks.len()).collect());
        self.source = Source::Library;
        self.queued_current = Some(track);
        self.finish_resume(&load)
    }

    /// Loads the track of a resume paused (with the seek fallback) once the
    /// play order is set; on failure nothing is current, see
    /// [`Playback::resume`].
    fn finish_resume(&mut self, load: &ResumeLoad) -> bool {
        let mut loaded = self.audio.load(&load.path, load.start, true);
        if !load.start.is_zero() && loaded.as_ref().is_err_and(anyhow::Error::is::<SeekFailed>) {
            loaded = self.audio.load(&load.path, Duration::ZERO, true);
        }
        match loaded {
            Ok(()) => {
                self.state = PlayState::Paused;
                self.stop_reason = None;
                true
            }
            Err(err) => {
                self.audio.stop();
                let notice =
                    Notice::error(format!("Cannot resume '{}': {}", load.name, reason(&err)));
                // Same list and source, but nothing current.
                let items = self.queue.items().to_vec();
                self.queue.reset(items);
                self.queued_current = None;
                self.state = PlayState::Stopped;
                self.stop_reason = Some(notice.clone());
                self.notice = Some(notice);
                false
            }
        }
    }

    /// Applies a change made to playlist `name` to the play order, if that
    /// playlist is the one playing; the current track keeps playing. Returns
    /// whether the play order changed. Edits with out-of-range positions or
    /// invalid library indices are ignored (and return `false`).
    pub fn edit_playlist(&mut self, name: &str, edit: Edit) -> bool {
        if !self.is_playing_playlist(name) {
            return false;
        }
        match edit {
            Edit::Remove(item) => self.queue.remove_item(item),
            Edit::Move { from, to } => self.queue.move_item(from, to),
            Edit::Append(tracks) => {
                if !self.valid_tracks(&tracks) {
                    return false;
                }
                self.queue.append_items(&tracks, &mut rand::rng());
                true
            }
            Edit::Replace(tracks) => {
                if !self.valid_tracks(&tracks) {
                    return false;
                }
                self.queue.replace_items(tracks, &mut rand::rng());
                true
            }
        }
    }

    /// Follows a rename of playlist `old` to `new` if it is the one playing.
    pub fn playlist_renamed(&mut self, old: &str, new: &str) {
        if self.is_playing_playlist(old) {
            self.source = Source::Playlist(new.to_owned());
        }
    }

    /// Follows the deletion of playlist `name`: if it is the one playing,
    /// playback goes on through its play order, now detached from any
    /// playlist.
    pub fn playlist_deleted(&mut self, name: &str) {
        if self.is_playing_playlist(name) {
            self.source = Source::Detached;
        }
    }

    /// Whether playlist `name` is the play order.
    #[must_use]
    pub fn is_playing_playlist(&self, name: &str) -> bool {
        matches!(&self.source, Source::Playlist(playing) if playing == name)
    }
}

/// The up-next queue: tracks that play before the play order continues.
impl<A: Audio> Playback<A> {
    /// The queued tracks (library indices), in the order they play. The
    /// queued track that is current is not among them.
    #[must_use]
    pub fn up_next(&self) -> &[usize] {
        &self.up_next
    }

    /// Changes every time [`Playback::up_next`] does, and only then, so
    /// positions in it are valid for one generation.
    #[must_use]
    pub const fn up_next_generation(&self) -> u64 {
        self.up_next_generation
    }

    /// How many tracks were taken from the front of the queue to play them
    /// (wrapping): when it grows by `k`, every remaining entry moved up `k`
    /// positions.
    #[must_use]
    pub const fn up_next_taken(&self) -> u64 {
        self.up_next_taken
    }

    /// Adds library tracks `tracks`, in their order, to the end of the queue,
    /// or to its front with `front`. Only the first ones that fit under
    /// [`MAX_UP_NEXT`] are added. Returns how many were added: 0 (nothing
    /// changed) when none fit, `tracks` is empty or one of them is not a
    /// library index.
    pub fn enqueue(&mut self, tracks: &[usize], front: bool) -> usize {
        if !self.valid_tracks(tracks) {
            return 0;
        }
        let room = MAX_UP_NEXT.saturating_sub(self.up_next.len());
        let Some(added) = tracks
            .get(..tracks.len().min(room))
            .filter(|t| !t.is_empty())
        else {
            return 0;
        };
        if front {
            self.up_next.splice(0..0, added.iter().copied());
        } else {
            self.up_next.extend_from_slice(added);
        }
        self.up_next_changed();
        added.len()
    }

    /// Removes the queued entry at position `pos`. Returns `false` (nothing
    /// changed) when `pos` is out of range.
    pub fn remove_queued(&mut self, pos: usize) -> bool {
        if pos >= self.up_next.len() {
            return false;
        }
        self.up_next.remove(pos);
        self.up_next_changed();
        true
    }

    /// Moves the queued entry at position `from` to position `to` (the
    /// entries in between shift by one). Returns `false` (nothing changed)
    /// when either position is out of range; `from == to` changes nothing
    /// and returns `true`.
    pub fn move_queued(&mut self, from: usize, to: usize) -> bool {
        let len = self.up_next.len();
        if from >= len || to >= len {
            return false;
        }
        if from != to {
            let track = self.up_next.remove(from);
            self.up_next.insert(to, track);
            self.up_next_changed();
        }
        true
    }

    /// Takes the queued entry at position `pos` out of the queue and plays it
    /// now as the current queued track. The play order's cursor does not
    /// move, so the rest of the queue and then the play order continue after
    /// it. Returns `false` (nothing changed) when `pos` is out of range.
    pub fn play_queued(&mut self, pos: usize) -> bool {
        if pos >= self.up_next.len() {
            return false;
        }
        let track = self.up_next.remove(pos);
        self.up_next_changed();
        self.queued_current = Some(track);
        self.failures = 0;
        self.start_current(false);
        true
    }

    /// Empties the queue; a queued track that is current keeps playing.
    /// Returns whether anything was removed.
    pub fn clear_queued(&mut self) -> bool {
        if self.up_next.is_empty() {
            return false;
        }
        self.up_next.clear();
        self.up_next_changed();
        true
    }

    /// Replaces the queue with library tracks `tracks` (a restored session),
    /// keeping the first [`MAX_UP_NEXT`]. Returns `false` and changes nothing
    /// when one of them is not a library index.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used from group 4 of add-play-queue")
    )]
    pub fn restore_up_next(&mut self, mut tracks: Vec<usize>) -> bool {
        if !self.valid_tracks(&tracks) {
            return false;
        }
        tracks.truncate(MAX_UP_NEXT);
        if tracks != self.up_next {
            self.up_next = tracks;
            self.up_next_changed();
        }
        true
    }

    /// Records a change to `up_next`.
    const fn up_next_changed(&mut self) {
        self.up_next_generation = self.up_next_generation.wrapping_add(1);
    }
}

/// What [`Playback::resume`] loads: the track's file, where to start and
/// its sanitised name for the failure notice.
struct ResumeLoad {
    path: std::path::PathBuf,
    start: Duration,
    name: String,
}

impl ResumeLoad {
    /// The load of library track `track` at `position` (zero when its tag
    /// length is known and `position` is at or past it); `None` for an
    /// invalid index.
    fn new(tracks: &[Track], track: usize, position: Duration) -> Option<Self> {
        let track = tracks.get(track)?;
        let start = if track.duration.is_some_and(|length| position >= length) {
            Duration::ZERO
        } else {
            position
        };
        Some(Self {
            path: track.path.clone(),
            start,
            name: sanitize(&track.title).unwrap_or_else(|| file_name(track)),
        })
    }
}

/// Formats `d` as `m:ss` (minutes are not capped, e.g. `75:03`).
#[must_use]
pub fn clock(d: Duration) -> String {
    let secs = d.as_secs();
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The file name of `track` for messages, sanitised.
fn file_name(track: &Track) -> String {
    track
        .path
        .file_name()
        .map_or_else(|| display_path(&track.path), |n| display_path(n.as_ref()))
}

/// One-line, sanitised description of a playback error and its causes.
fn reason(err: &anyhow::Error) -> String {
    sanitize(&format!("{err:#}")).unwrap_or_else(|| "unknown error".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Result, bail};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};

    /// Audio output that records what it is asked to do.
    #[derive(Debug, Default)]
    struct FakeAudio {
        broken: HashSet<PathBuf>,
        loads: Vec<(PathBuf, Duration, bool)>,
        loaded: Option<PathBuf>,
        status: Option<Status>,
        position: Duration,
        paused: bool,
        volume: Option<Volume>,
        device_error: Option<String>,
        /// Length the "decoder" reports for any loaded file.
        decoded_length: Option<Duration>,
        /// Make seeks (loads at a non-zero start) fail; `true` = past the end.
        seek_failure: Option<bool>,
        /// Files that fail to open when loaded from the start (but may still
        /// reach `seek_failure` when loaded at a position).
        broken_from_start: HashSet<PathBuf>,
    }

    impl Audio for FakeAudio {
        fn load(&mut self, path: &Path, start: Duration, paused: bool) -> Result<()> {
            self.loads.push((path.to_owned(), start, paused));
            if self.broken.contains(path) {
                bail!("not a playable audio file");
            }
            if let Some(past_end) = self.seek_failure.filter(|_| !start.is_zero()) {
                return Err(SeekFailed::for_test(past_end).into());
            }
            if start.is_zero() && self.broken_from_start.contains(path) {
                bail!("cannot decode the file");
            }
            self.loaded = Some(path.to_owned());
            self.status = Some(Status::Active);
            self.position = start;
            self.paused = paused;
            Ok(())
        }
        fn set_paused(&mut self, paused: bool) {
            self.paused = paused;
        }
        fn stop(&mut self) {
            self.loaded = None;
            self.status = None;
            self.position = Duration::ZERO;
        }
        fn set_volume(&mut self, volume: Volume) {
            self.volume = Some(volume);
        }
        fn position(&self) -> Duration {
            self.position
        }
        fn duration(&self) -> Option<Duration> {
            self.loaded.as_ref().and(self.decoded_length)
        }
        fn status(&self) -> Status {
            self.status.clone().unwrap_or(Status::Idle)
        }
        fn take_device_error(&mut self) -> Option<String> {
            self.device_error.take()
        }
    }

    fn path(i: usize) -> PathBuf {
        PathBuf::from(format!("/music/{i:02}.mp3"))
    }

    fn library(n: usize) -> Vec<Track> {
        (0..n)
            .map(|i| Track {
                path: path(i),
                title: format!("Song {i}"),
                artist: "Band".to_owned(),
                album: "Album".to_owned(),
                number: u32::try_from(i + 1).ok(),
                album_artist: None,
                disc: None,
                duration: Some(Duration::from_secs(200)),
            })
            .collect()
    }

    fn playback(n: usize, broken: &[usize]) -> Playback<FakeAudio> {
        let audio = FakeAudio {
            broken: broken.iter().map(|&i| path(i)).collect(),
            ..FakeAudio::default()
        };
        Playback::new(library(n), audio)
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    fn set_repeat(p: &mut Playback<FakeAudio>, repeat: Repeat) {
        while p.repeat() != repeat {
            p.cycle_repeat();
        }
    }

    #[test]
    fn starts_stopped_at_full_volume() {
        let p = playback(3, &[]);
        assert_eq!(p.state(), PlayState::Stopped);
        assert_eq!(p.current(), None);
        assert_eq!(p.volume(), Volume::MAX);
        assert_eq!(p.audio.volume, Some(Volume::MAX));
        assert_eq!(p.position(), Duration::ZERO);
    }

    #[test]
    fn play_starts_track_from_beginning() {
        let mut p = playback(3, &[]);
        p.play(1);
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.current(), Some(1));
        assert_eq!(p.audio.loads, [(path(1), Duration::ZERO, false)]);
        p.play(7);
        assert_eq!(p.current(), Some(1), "out-of-range index is ignored");
    }

    #[test]
    fn pause_resume_keeps_position() {
        let mut p = playback(3, &[]);
        p.play(0);
        p.audio.position = secs(42);
        p.toggle_pause();
        assert_eq!(p.state(), PlayState::Paused);
        assert!(p.audio.paused);
        assert_eq!(p.position(), secs(42));
        p.toggle_pause();
        assert_eq!(p.state(), PlayState::Playing);
        assert!(!p.audio.paused);
        assert_eq!(p.audio.loads.len(), 1, "no reload on resume");
    }

    #[test]
    fn stop_resets_and_space_restarts() {
        let mut p = playback(3, &[]);
        p.play(2);
        p.audio.position = secs(30);
        p.stop();
        assert_eq!(p.state(), PlayState::Stopped);
        assert_eq!(p.position(), Duration::ZERO);
        assert_eq!(p.audio.loaded, None);
        p.toggle_pause();
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(2), Duration::ZERO, false))
        );
    }

    #[test]
    fn space_with_nothing_current_does_nothing() {
        let mut p = playback(3, &[]);
        p.toggle_pause();
        assert_eq!(p.state(), PlayState::Stopped);
        assert_eq!(p.audio.loads.len(), 0);
    }

    #[test]
    fn next_and_prev() {
        let mut p = playback(5, &[]);
        p.play(2);
        p.next();
        assert_eq!(p.current(), Some(3));
        p.audio.position = secs(2);
        p.prev();
        assert_eq!(p.current(), Some(2), "early previous goes back");
        p.audio.position = secs(70);
        p.prev();
        assert_eq!(p.current(), Some(2), "late previous restarts");
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(2), Duration::ZERO, false))
        );
    }

    #[test]
    fn prev_on_first_track_restarts_it() {
        let mut p = playback(3, &[]);
        p.play(0);
        p.audio.position = secs(1);
        p.prev();
        assert_eq!(p.current(), Some(0));
        assert_eq!(p.audio.loads.len(), 2);
    }

    #[test]
    fn next_at_last_track_keeps_playing() {
        let mut p = playback(2, &[]);
        p.play(1);
        p.next();
        assert_eq!(p.current(), Some(1));
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.audio.loads.len(), 1);
        assert!(p.take_notice().is_some());
    }

    #[test]
    fn auto_advance_and_stop_at_end() {
        let mut p = playback(2, &[]);
        p.play(0);
        p.audio.status = Some(Status::Ended);
        p.tick();
        assert_eq!(p.current(), Some(1));
        assert_eq!(p.state(), PlayState::Playing);
        p.audio.status = Some(Status::Ended);
        p.tick();
        assert_eq!(p.state(), PlayState::Stopped, "repeat off stops at the end");
    }

    #[test]
    fn repeat_all_and_one_on_end() {
        let mut p = playback(2, &[]);
        set_repeat(&mut p, Repeat::All);
        p.play(1);
        p.audio.status = Some(Status::Ended);
        p.tick();
        assert_eq!(p.current(), Some(0), "repeat all wraps");

        set_repeat(&mut p, Repeat::One);
        p.audio.status = Some(Status::Ended);
        p.tick();
        assert_eq!(p.current(), Some(0), "repeat one replays");
        assert_eq!(p.audio.loads.len(), 3);
    }

    #[test]
    fn seek_is_clamped_and_keeps_pause() {
        let mut p = playback(2, &[]);
        p.play(0);
        p.audio.position = secs(60);
        p.seek(true);
        assert_eq!(p.audio.loads.last(), Some(&(path(0), secs(65), false)));
        p.audio.position = secs(3);
        p.toggle_pause();
        p.seek(false);
        assert_eq!(p.audio.loads.last(), Some(&(path(0), secs(0), true)));
        assert_eq!(p.state(), PlayState::Paused);
    }

    #[test]
    fn seek_past_end_advances() {
        let mut p = playback(2, &[]);
        p.play(0);
        p.audio.position = secs(198);
        p.seek(true);
        assert_eq!(p.current(), Some(1));
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(1), Duration::ZERO, false))
        );
    }

    #[test]
    fn seek_failure_is_reported_and_track_continues() {
        let mut p = playback(2, &[]);
        p.play(0);
        p.audio.broken.insert(path(0));
        p.audio.position = secs(10);
        p.seek(true);
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.audio.loaded, Some(path(0)));
        let notice = p.take_notice();
        assert!(
            notice.is_some_and(|n| n.level == Level::Error && n.text.starts_with("Cannot seek"))
        );
    }

    #[test]
    fn seek_when_stopped_does_nothing() {
        let mut p = playback(2, &[]);
        p.seek(true);
        assert_eq!(p.audio.loads.len(), 0);
    }

    #[test]
    fn volume_changes_reach_the_output() {
        let mut p = playback(1, &[]);
        p.volume_down();
        assert_eq!(p.volume().percent(), 95);
        assert_eq!(p.audio.volume, Some(Volume::new(95)));
        p.volume_up();
        p.volume_up();
        assert_eq!(p.volume(), Volume::MAX);
    }

    #[test]
    fn corrupt_file_is_reported_and_skipped_even_with_repeat_one() {
        let mut p = playback(4, &[1]);
        set_repeat(&mut p, Repeat::One);
        p.play(1);
        assert_eq!(p.current(), Some(2));
        assert_eq!(p.state(), PlayState::Playing);
        let notice = p.take_notice();
        assert!(
            notice
                .as_ref()
                .is_some_and(|n| n.level == Level::Error && n.text.contains("01.mp3")),
            "{notice:?}"
        );
    }

    #[test]
    fn decode_failure_during_playback_skips() {
        let mut p = playback(3, &[]);
        set_repeat(&mut p, Repeat::One);
        p.play(0);
        p.audio.status = Some(Status::Failed("no audio could be decoded".to_owned()));
        p.tick();
        assert_eq!(p.current(), Some(1));
        assert_eq!(p.state(), PlayState::Playing);
        assert!(p.take_notice().is_some_and(|n| n.text.contains("00.mp3")));
    }

    #[test]
    fn every_track_failing_stops_instead_of_looping() {
        for repeat in [Repeat::Off, Repeat::All, Repeat::One] {
            let mut p = playback(3, &[0, 1, 2]);
            set_repeat(&mut p, repeat);
            p.play(1);
            for _ in 0..5 {
                p.tick();
            }
            assert_eq!(p.state(), PlayState::Stopped, "{repeat:?}");
            assert!(p.audio.loads.len() <= 3, "{repeat:?}: {:?}", p.audio.loads);
            let notice = p.take_notice();
            assert!(
                notice.as_ref().is_some_and(|n| n.level == Level::Error),
                "{repeat:?}: {notice:?}"
            );
            assert_eq!(p.stop_reason(), notice.as_ref(), "{repeat:?}");
        }
    }

    #[test]
    fn all_failing_with_repeat_all_reports_no_playable_tracks() {
        let mut p = playback(3, &[0, 1, 2]);
        set_repeat(&mut p, Repeat::All);
        p.play(1);
        assert_eq!(p.audio.loads.len(), 3);
        assert!(
            p.take_notice()
                .is_some_and(|n| n.text.contains("no playable tracks"))
        );
    }

    #[test]
    fn long_runs_of_failures_are_spread_over_ticks() {
        let n = MAX_ATTEMPTS_PER_CALL * 2 + 3;
        let broken: Vec<usize> = (0..n - 1).collect();
        let mut p = playback(n, &broken);
        p.play(0);
        assert_eq!(p.audio.loads.len(), MAX_ATTEMPTS_PER_CALL);
        assert_eq!(p.state(), PlayState::Playing);
        for _ in 0..3 {
            p.tick();
        }
        assert_eq!(p.current(), Some(n - 1), "reaches the playable track");
        assert_eq!(p.audio.loaded, Some(path(n - 1)));
    }

    #[test]
    fn stop_cancels_pending_retries() {
        let n = MAX_ATTEMPTS_PER_CALL + 5;
        let broken: Vec<usize> = (0..n).collect();
        let mut p = playback(n, &broken);
        p.play(0);
        p.stop();
        p.tick();
        assert_eq!(p.audio.loads.len(), MAX_ATTEMPTS_PER_CALL);
        assert_eq!(p.state(), PlayState::Stopped);
    }

    #[test]
    fn failure_count_resets_after_a_track_plays() {
        // 0 fails, 1 plays, then 2 fails: with 3 tracks and repeat all, 2's
        // failure must not count 0's and stop playback.
        let mut p = playback(3, &[0, 2]);
        set_repeat(&mut p, Repeat::All);
        p.play(0);
        assert_eq!(p.current(), Some(1));
        p.audio.position = secs(1);
        p.tick();
        p.audio.status = Some(Status::Ended);
        p.tick();
        assert_eq!(
            p.current(),
            Some(1),
            "2 is skipped, 0 is skipped, back to 1"
        );
        assert_eq!(p.state(), PlayState::Playing);
    }

    #[test]
    fn device_errors_become_notices() {
        let mut p = playback(1, &[]);
        p.audio.device_error = Some("stream lost".to_owned());
        p.tick();
        assert!(
            p.take_notice()
                .is_some_and(|n| n.text.contains("stream lost"))
        );
    }

    #[test]
    fn device_errors_are_sanitized() {
        let mut p = playback(1, &[]);
        p.audio.device_error = Some("bad\x1b]0;title\x07 device".to_owned());
        p.tick();
        let text = p.take_notice().map(|n| n.text).unwrap_or_default();
        assert!(text.contains("device"), "{text:?}");
        assert!(!text.contains(['\x1b', '\x07']), "{text:?}");
    }

    #[test]
    fn shuffle_toggle_keeps_current() {
        let mut p = playback(20, &[]);
        p.play(5);
        assert!(p.toggle_shuffle());
        assert_eq!(p.current(), Some(5));
        assert!(!p.toggle_shuffle());
        assert_eq!(p.current(), Some(5));
    }

    #[test]
    fn position_is_clamped_to_duration() {
        let mut p = playback(1, &[]);
        p.play(0);
        p.audio.position = secs(500);
        assert_eq!(p.position(), secs(200));
    }

    #[test]
    fn empty_library_is_inert() {
        let mut p = playback(0, &[]);
        p.play(0);
        p.next();
        p.prev();
        p.toggle_pause();
        p.seek(true);
        p.tick();
        assert_eq!(p.state(), PlayState::Stopped);
        assert_eq!(p.audio.loads.len(), 0);
        assert!(p.take_notice().is_none());
    }

    #[test]
    fn playing_a_track_cancels_pending_retries() {
        let n = MAX_ATTEMPTS_PER_CALL * 2 + 3;
        let broken: Vec<usize> = (0..n - 1).collect();
        let mut p = playback(n, &broken);
        p.play(0);
        assert_eq!(p.audio.loads.len(), MAX_ATTEMPTS_PER_CALL);
        p.play(n - 1);
        let loads = p.audio.loads.len();
        p.tick();
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(n - 1), Duration::ZERO, false))
        );
        assert_eq!(p.audio.loads.len(), loads, "the good track is not reloaded");
        assert_eq!(p.state(), PlayState::Playing);
    }

    #[test]
    fn pause_while_skipping_is_kept() {
        let n = MAX_ATTEMPTS_PER_CALL + 3;
        let broken: Vec<usize> = (0..n - 1).collect();
        let mut p = playback(n, &broken);
        p.play(0);
        p.toggle_pause();
        assert_eq!(p.state(), PlayState::Paused);
        p.tick();
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(n - 1), Duration::ZERO, true))
        );
        assert_eq!(p.state(), PlayState::Paused);
        assert!(p.audio.paused);
    }

    #[test]
    fn decoder_length_wins_over_tag_estimate() {
        let mut p = playback(2, &[]);
        p.audio.decoded_length = Some(secs(300)); // tags say 200 s
        p.play(0);
        assert_eq!(p.duration(), Some(secs(300)));
        p.audio.position = secs(250);
        assert_eq!(p.position(), secs(250));
        p.seek(true);
        assert_eq!(p.current(), Some(0), "not past the real end");
        assert_eq!(p.audio.loads.last(), Some(&(path(0), secs(255), false)));
    }

    #[test]
    fn tag_length_is_used_until_decoded() {
        let mut p = playback(1, &[]);
        p.audio.decoded_length = Some(secs(300));
        assert_eq!(p.duration(), None, "nothing current yet");
        p.play(0);
        p.stop();
        assert_eq!(p.duration(), Some(secs(200)));
    }

    #[test]
    fn failed_forward_seek_with_unknown_length_ends_track() {
        let mut tracks = library(2);
        tracks[0].duration = None;
        let mut p = Playback::new(tracks, FakeAudio::default());
        p.play(0);
        p.audio.seek_failure = Some(true);
        p.audio.position = secs(10);
        p.seek(false);
        assert_eq!(p.current(), Some(0), "backward failure keeps the track");
        assert!(
            p.take_notice()
                .is_some_and(|n| n.text.starts_with("Cannot seek"))
        );
        p.seek(true);
        assert_eq!(p.current(), Some(1), "seek past the end finishes the track");
        assert_eq!(p.state(), PlayState::Playing);
    }

    #[test]
    fn unsupported_seek_keeps_the_track() {
        let mut tracks = library(2);
        tracks[0].duration = None;
        let mut p = Playback::new(tracks, FakeAudio::default());
        p.play(0);
        p.audio.seek_failure = Some(false);
        p.audio.position = secs(10);
        p.seek(true);
        assert_eq!(p.current(), Some(0));
        assert_eq!(p.audio.loaded, Some(path(0)));
        assert!(
            p.take_notice()
                .is_some_and(|n| n.text.starts_with("Cannot seek"))
        );
    }

    #[test]
    fn seek_to_end_while_paused_stays_paused() {
        let mut p = playback(2, &[]);
        p.play(0);
        p.toggle_pause();
        p.audio.position = secs(198);
        p.seek(true);
        assert_eq!(p.current(), Some(1));
        assert_eq!(p.state(), PlayState::Paused);
        assert_eq!(p.audio.loads.last(), Some(&(path(1), Duration::ZERO, true)));
    }

    #[test]
    fn stop_reason_survives_transient_notices() {
        let mut p = playback(3, &[0, 1, 2]);
        p.play(0);
        assert_eq!(p.state(), PlayState::Stopped);
        let reason = p.stop_reason().cloned();
        assert!(reason.as_ref().is_some_and(|n| n.text.contains("02.mp3")));

        p.audio.device_error = Some("xrun".to_owned());
        p.tick();
        assert!(p.take_notice().is_some_and(|n| n.text.contains("xrun")));
        assert_eq!(p.stop_reason(), reason.as_ref(), "still available");

        p.audio.broken.clear();
        p.play(1);
        assert_eq!(p.stop_reason(), None, "cleared once playback resumes");
    }

    #[test]
    fn early_end_is_reported_and_playback_continues() {
        let mut p = playback(2, &[]);
        p.audio.decoded_length = Some(secs(200));
        p.play(0);
        p.audio.position = secs(61);
        p.audio.status = Some(Status::Ended);
        p.tick();
        assert_eq!(p.current(), Some(1));
        let notice = p.take_notice();
        assert!(
            notice
                .as_ref()
                .is_some_and(|n| n.text.contains("00.mp3") && n.text.contains("1:01 of 3:20")),
            "{notice:?}"
        );

        p.audio.position = secs(199);
        p.audio.status = Some(Status::Ended);
        p.tick();
        assert_eq!(p.take_notice(), None, "a normal end is silent");
    }

    #[test]
    fn clock_formats_minutes_and_seconds() {
        assert_eq!(clock(Duration::ZERO), "0:00");
        assert_eq!(clock(Duration::from_millis(65_900)), "1:05");
        assert_eq!(clock(secs(4_503)), "75:03");
    }

    // ---- Playlists as the play order ----

    fn running() -> Source {
        Source::Playlist("Running".to_owned())
    }

    /// Ends the current track and lets playback react.
    fn end_track(p: &mut Playback<FakeAudio>) {
        p.audio.status = Some(Status::Ended);
        p.tick();
    }

    #[test]
    fn playlist_advance_stops_at_its_end() {
        let mut p = playback(6, &[]);
        assert!(p.play_source(running(), vec![4, 1, 3], 1));
        assert_eq!(p.source(), &running());
        assert_eq!(p.current(), Some(1));
        assert_eq!(p.current_item(), Some(1));
        end_track(&mut p);
        assert_eq!(
            p.current(),
            Some(3),
            "next in the playlist, not the library"
        );
        end_track(&mut p);
        assert_eq!(p.state(), PlayState::Stopped, "repeat off stops at its end");
        assert_eq!(p.current(), Some(3));
    }

    #[test]
    fn playlist_wraps_with_repeat_all() {
        let mut p = playback(6, &[]);
        set_repeat(&mut p, Repeat::All);
        p.play_source(running(), vec![4, 1, 3], 2);
        end_track(&mut p);
        assert_eq!(p.current(), Some(4));
        assert_eq!(p.current_item(), Some(0));
        p.audio.position = secs(1);
        p.prev();
        assert_eq!(p.current(), Some(3), "previous wraps within the playlist");
    }

    #[test]
    fn failure_skip_stays_within_playlist() {
        let mut p = playback(6, &[1]);
        p.play_source(running(), vec![4, 1, 3], 1);
        assert_eq!(p.current(), Some(3), "skips 1 to the next playlist entry");
        assert_eq!(p.state(), PlayState::Playing);

        let mut p = playback(6, &[0, 5]);
        set_repeat(&mut p, Repeat::All);
        p.play_source(running(), vec![0, 5], 0);
        assert_eq!(
            p.state(),
            PlayState::Stopped,
            "playable library tracks are not used"
        );
        assert!(
            p.stop_reason()
                .is_some_and(|n| n.text.contains("no playable tracks"))
        );
        assert!(
            p.audio
                .loads
                .iter()
                .all(|(path_, ..)| [path(0), path(5)].contains(path_))
        );
    }

    #[test]
    fn playing_from_library_restores_library_order() {
        let mut p = playback(6, &[]);
        p.play_source(running(), vec![4, 1], 0);
        p.play(2);
        assert_eq!(p.source(), &Source::Library);
        assert_eq!(p.current_item(), Some(2));
        p.next();
        assert_eq!(p.current(), Some(3));
        p.audio.position = secs(1);
        p.prev();
        p.prev();
        assert_eq!(p.current(), Some(1));
    }

    #[test]
    fn shuffle_and_repeat_survive_a_source_change() {
        let mut p = playback(10, &[]);
        set_repeat(&mut p, Repeat::One);
        p.toggle_shuffle();
        p.play_source(running(), vec![7, 8, 9], 1);
        assert!(p.shuffle());
        assert_eq!(p.repeat(), Repeat::One);
        assert_eq!(p.current(), Some(8), "the chosen entry plays first");
        p.play(3);
        assert!(p.shuffle());
        assert_eq!(p.current(), Some(3));
    }

    #[test]
    fn invalid_sources_are_refused() {
        let mut p = playback(3, &[]);
        p.play(0);
        assert!(
            !p.play_source(running(), vec![1, 2], 2),
            "start out of range"
        );
        assert!(!p.play_source(running(), vec![], 0), "empty");
        assert!(
            !p.play_source(running(), vec![1, 3], 0),
            "not a library index"
        );
        assert_eq!(p.source(), &Source::Library);
        assert_eq!(p.current(), Some(0));
        assert_eq!(p.audio.loads.len(), 1);
    }

    #[test]
    fn edits_apply_only_to_the_playing_playlist() {
        let mut p = playback(6, &[]);
        p.play_source(running(), vec![0, 1, 2], 0);
        assert!(!p.edit_playlist("Gym", Edit::Remove(1)));
        assert!(
            !p.edit_playlist("running", Edit::Remove(1)),
            "names are exact"
        );
        end_track(&mut p);
        assert_eq!(p.current(), Some(1));

        let mut p = playback(6, &[]);
        p.play(0);
        assert!(
            !p.edit_playlist("Running", Edit::Remove(0)),
            "library is playing"
        );
        assert_eq!(p.current_item(), Some(0));
    }

    #[test]
    fn removing_playing_entry_lets_it_finish() {
        // A, B, C = 0, 1, 2; B plays and is removed.
        let mut p = playback(6, &[]);
        p.play_source(running(), vec![0, 1, 2], 1);
        let loads = p.audio.loads.len();
        assert!(p.edit_playlist("Running", Edit::Remove(1)));
        assert_eq!(p.current(), Some(1), "B keeps playing");
        assert_eq!(p.current_item(), None, "no entry has the playing marker");
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.audio.loads.len(), loads, "not restarted");
        p.tick();
        assert_eq!(p.current(), Some(1));
        end_track(&mut p);
        assert_eq!(p.current(), Some(2), "C plays when B ends");
        assert_eq!(p.current_item(), Some(1));
    }

    #[test]
    fn removed_playing_entry_with_repeat_one() {
        let mut p = playback(6, &[]);
        set_repeat(&mut p, Repeat::One);
        p.play_source(running(), vec![0, 1, 2], 1);
        p.edit_playlist("Running", Edit::Remove(1));
        end_track(&mut p);
        assert_eq!(p.current(), Some(2));
        end_track(&mut p);
        assert_eq!(p.current(), Some(2), "then C repeats");
    }

    #[test]
    fn removed_last_entry_stops_or_wraps() {
        let mut p = playback(6, &[]);
        p.play_source(running(), vec![0, 1, 2], 2);
        p.edit_playlist("Running", Edit::Remove(2));
        end_track(&mut p);
        assert_eq!(p.state(), PlayState::Stopped);

        let mut p = playback(6, &[]);
        set_repeat(&mut p, Repeat::All);
        p.play_source(running(), vec![0, 1, 2], 2);
        p.edit_playlist("Running", Edit::Remove(2));
        end_track(&mut p);
        assert_eq!(p.current(), Some(0));
        assert_eq!(p.state(), PlayState::Playing);
    }

    #[test]
    fn previous_after_removal_goes_to_preceding_entry() {
        let mut p = playback(6, &[]);
        p.play_source(running(), vec![0, 1, 2], 1);
        p.audio.position = secs(2);
        p.edit_playlist("Running", Edit::Remove(1));
        p.prev();
        assert_eq!(p.current(), Some(0));
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(0), Duration::ZERO, false))
        );

        // Later than the restart threshold, the removed track restarts.
        let mut p = playback(6, &[]);
        p.play_source(running(), vec![0, 1, 2], 1);
        p.audio.position = secs(30);
        p.edit_playlist("Running", Edit::Remove(1));
        p.prev();
        assert_eq!(p.current(), Some(1));
        end_track(&mut p);
        assert_eq!(p.current(), Some(2), "and still moves on to C");
    }

    #[test]
    fn failing_removed_track_does_not_use_up_an_attempt() {
        // D (5) is removed while it is being retried; X (1) fails too, Y (2)
        // plays. Each of the three must be tried before giving up.
        let n = MAX_ATTEMPTS_PER_CALL + 5;
        let mut p = playback(n, &[]);
        set_repeat(&mut p, Repeat::All);
        p.play_source(running(), vec![5, 1, 2], 0);
        p.edit_playlist("Running", Edit::Remove(0));
        assert!(p.queue.is_detached());
        p.audio.broken.extend([path(5), path(1)]);
        p.audio.status = Some(Status::Failed("bad data".to_owned()));
        p.tick();
        assert_eq!(p.current(), Some(2), "Y is reached");
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.audio.loaded, Some(path(2)));

        // All three failing still stops, and says so.
        let mut p = playback(n, &[]);
        set_repeat(&mut p, Repeat::All);
        p.play_source(running(), vec![5, 1, 2], 0);
        p.edit_playlist("Running", Edit::Remove(0));
        p.audio.broken.extend([path(5), path(1), path(2)]);
        p.audio.status = Some(Status::Failed("bad data".to_owned()));
        p.tick();
        assert_eq!(p.state(), PlayState::Stopped);
        assert!(
            p.stop_reason()
                .is_some_and(|n| n.text.contains("no playable tracks"))
        );
        let tried: Vec<_> = p
            .audio
            .loads
            .iter()
            .skip(1)
            .map(|(l, ..)| l.clone())
            .collect();
        assert_eq!(tried, [path(1), path(2)], "each tried once, no extra round");
    }

    #[test]
    fn reorder_while_playing() {
        let mut p = playback(6, &[]);
        p.play_source(running(), vec![0, 1, 2], 0);
        assert!(p.edit_playlist("Running", Edit::Move { from: 2, to: 1 }));
        end_track(&mut p);
        assert_eq!(p.current(), Some(2), "C plays after A");
        assert!(!p.edit_playlist("Running", Edit::Move { from: 0, to: 3 }));
    }

    #[test]
    fn appended_tracks_play_next() {
        // Running lists A, B; B plays; D is added from the library.
        let mut p = playback(6, &[]);
        p.play_source(running(), vec![0, 1], 1);
        assert!(p.edit_playlist("Running", Edit::Append(vec![3])));
        end_track(&mut p);
        assert_eq!(p.current(), Some(3), "D plays when B ends");
        assert!(
            !p.edit_playlist("Running", Edit::Append(vec![6])),
            "not a library index"
        );
        end_track(&mut p);
        assert_eq!(
            p.state(),
            PlayState::Stopped,
            "the bad append changed nothing"
        );
    }

    #[test]
    fn reload_keeps_playing_track() {
        // Running plays B; the file is changed to A, B, X.
        let mut p = playback(6, &[]);
        p.play_source(running(), vec![0, 1], 1);
        let loads = p.audio.loads.len();
        assert!(p.edit_playlist("Running", Edit::Replace(vec![0, 1, 5])));
        assert_eq!(p.current(), Some(1));
        assert_eq!(p.audio.loads.len(), loads, "B is not restarted");
        end_track(&mut p);
        assert_eq!(p.current(), Some(5), "X plays when B ends");
        assert!(!p.edit_playlist("Running", Edit::Replace(vec![9])));
        assert_eq!(p.current(), Some(5));
    }

    #[test]
    fn rename_follows_and_delete_detaches() {
        let mut p = playback(6, &[]);
        p.play_source(running(), vec![0, 1, 2], 0);
        p.playlist_renamed("Gym", "Other");
        assert_eq!(p.source(), &running());
        p.playlist_renamed("Running", "Morning run");
        assert_eq!(p.source(), &Source::Playlist("Morning run".to_owned()));
        assert!(p.is_playing_playlist("Morning run"));
        assert!(!p.edit_playlist("Running", Edit::Remove(2)));
        assert!(p.edit_playlist("Morning run", Edit::Remove(2)));
        assert_eq!(p.state(), PlayState::Playing);

        p.playlist_deleted("Gym");
        assert!(p.is_playing_playlist("Morning run"));
        p.playlist_deleted("Morning run");
        assert_eq!(p.source(), &Source::Detached);
        assert!(!p.edit_playlist("Morning run", Edit::Remove(0)));
        assert_eq!(p.state(), PlayState::Playing, "playback is not interrupted");
        end_track(&mut p);
        assert_eq!(p.current(), Some(1), "its play order keeps going");
        end_track(&mut p);
        assert_eq!(p.state(), PlayState::Stopped);
    }

    #[test]
    fn library_play_after_detached_source_restores_library() {
        let mut p = playback(4, &[]);
        p.play_source(running(), vec![3], 0);
        p.playlist_deleted("Running");
        p.play(0);
        assert_eq!(p.source(), &Source::Library);
        end_track(&mut p);
        assert_eq!(p.current(), Some(1));
    }

    // ---- Albums and artists as the play order ----

    #[test]
    fn album_plays_to_its_end() {
        // The album lists A, B, C (library 4, 1, 3); B plays, repeat off.
        let mut p = playback(6, &[]);
        assert!(p.play_source(Source::Album(0), vec![4, 1, 3], 1));
        assert_eq!(p.source(), &Source::Album(0));
        assert_eq!(p.current(), Some(1));
        assert_eq!(p.current_item(), Some(1));
        end_track(&mut p);
        assert_eq!(p.current(), Some(3), "C, not the next library track");
        assert_eq!(p.current_item(), Some(2));
        end_track(&mut p);
        assert_eq!(p.state(), PlayState::Stopped, "repeat off stops at its end");
        assert_eq!(p.current(), Some(3));
        assert_eq!(p.source(), &Source::Album(0), "still the album's order");
    }

    #[test]
    fn album_wraps_with_repeat_all() {
        let mut p = playback(6, &[]);
        set_repeat(&mut p, Repeat::All);
        assert!(p.play_source(Source::Album(0), vec![4, 1, 3], 2));
        end_track(&mut p);
        assert_eq!(p.current(), Some(4), "A follows C");
        assert_eq!(p.current_item(), Some(0));
        p.audio.position = secs(1);
        p.prev();
        assert_eq!(p.current(), Some(3), "previous wraps within the album");
    }

    #[test]
    fn artist_order_spans_its_albums() {
        // Two albums concatenated: [5, 2] then [0, 4].
        let mut p = playback(6, &[]);
        assert!(p.play_source(Source::Artist(1), vec![5, 2, 0, 4], 1));
        end_track(&mut p);
        assert_eq!(p.current(), Some(0), "the second album's first track");
        assert_eq!(p.source(), &Source::Artist(1));
    }

    #[test]
    fn playlist_changes_leave_album_and_artist_sources_alone() {
        for source in [Source::Album(0), Source::Artist(0)] {
            let mut p = playback(6, &[]);
            assert!(p.play_source(source.clone(), vec![2, 3, 4], 0));
            p.playlist_renamed("Running", "Morning run");
            p.playlist_deleted("Running");
            assert!(!p.is_playing_playlist("Running"));
            assert!(!p.edit_playlist("Running", Edit::Remove(0)));
            assert!(!p.edit_playlist("Running", Edit::Replace(vec![5])));
            assert!(!p.edit_playlist("Running", Edit::Append(vec![5])));
            assert!(!p.edit_playlist("Running", Edit::Move { from: 0, to: 2 }));
            assert_eq!(p.source(), &source, "{source:?} unchanged");
            assert_eq!(p.items(), &[2, 3, 4], "{source:?} items unchanged");
            assert_eq!(p.current(), Some(2));
            assert_eq!(p.state(), PlayState::Playing);
        }
    }

    #[test]
    fn switching_from_a_playlist_to_an_album_stops_following_it() {
        let mut p = playback(6, &[]);
        assert!(p.play_source(running(), vec![2, 3, 4], 0));
        assert!(
            p.is_playing_playlist("Running"),
            "control: the playlist is followed"
        );
        assert!(p.edit_playlist("Running", Edit::Remove(2)));
        assert!(p.play_source(Source::Album(0), vec![2, 3, 4], 0));
        assert!(!p.is_playing_playlist("Running"));
        assert!(!p.edit_playlist("Running", Edit::Remove(0)));
        p.playlist_renamed("Running", "Other");
        p.playlist_deleted("Running");
        assert_eq!(p.source(), &Source::Album(0));
        assert_eq!(p.items(), &[2, 3, 4]);
    }

    #[test]
    fn album_and_artist_sources_are_told_apart() {
        let mut p = playback(6, &[]);
        assert!(p.play_source(Source::Album(1), vec![0, 1], 0));
        assert_ne!(p.source(), &Source::Album(0));
        assert_ne!(p.source(), &Source::Artist(1));
        assert!(
            !p.play_source(Source::Artist(0), vec![0, 9], 0),
            "not a library index"
        );
        assert_eq!(
            p.source(),
            &Source::Album(1),
            "a refused play changes nothing"
        );
        assert!(
            !p.play_source(Source::Artist(0), vec![0, 1], 2),
            "out of range"
        );
        assert_eq!(p.source(), &Source::Album(1));
    }

    #[test]
    fn shuffle_stays_in_the_album() {
        // Shuffle on, repeat off, a 4-track album in a 10-track library.
        for _ in 0..20 {
            let mut p = playback(10, &[]);
            assert!(p.toggle_shuffle());
            let album = vec![7, 2, 9, 4];
            assert!(p.play_source(Source::Album(3), album.clone(), 2));
            assert_eq!(p.current(), Some(9), "the chosen track plays first");
            let mut played = vec![9];
            for _ in 0..3 {
                end_track(&mut p);
                assert_eq!(p.state(), PlayState::Playing);
                played.extend(p.current());
            }
            end_track(&mut p);
            assert_eq!(p.state(), PlayState::Stopped, "then playback stops");
            played.sort_unstable();
            let mut expected = album;
            expected.sort_unstable();
            assert_eq!(played, expected, "every album track exactly once");
            assert_eq!(p.source(), &Source::Album(3));
        }
    }

    #[test]
    fn library_play_after_album_restores_library() {
        let mut p = playback(6, &[]);
        assert!(p.play_source(Source::Album(0), vec![4, 1], 0));
        p.play(2);
        assert_eq!(p.source(), &Source::Library);
        end_track(&mut p);
        assert_eq!(p.current(), Some(3), "library order again");
    }

    #[test]
    fn resume_keeps_an_album_source() {
        let mut p = playback(6, &[]);
        assert!(p.resume(Source::Album(2), vec![4, 1, 3], 1, secs(5), None));
        assert_eq!(p.source(), &Source::Album(2));
        assert_eq!(p.state(), PlayState::Paused);
        p.toggle_pause();
        end_track(&mut p);
        assert_eq!(p.current(), Some(3), "next follows the album");
    }

    #[test]
    fn recent_samples_pass_the_tone_through_while_playing() {
        use crate::test_support::{ToneAudio, track};
        use crate::visualizer::FFT_SIZE;
        let tracks = vec![track("a", "x", "y"), track("b", "x", "y")];
        let mut p = Playback::new(tracks, ToneAudio::new(1_000.0, 0.5));
        let mut out = vec![1.0; 8];
        assert_eq!(p.recent_samples(&mut out), None, "nothing played yet");
        assert_eq!(out, Vec::<f32>::new());

        p.play(0);
        assert_eq!(p.state(), PlayState::Playing);
        let _first = p.recent_samples(&mut out);
        let recent = p.recent_samples(&mut out);
        assert_eq!(
            recent,
            Some(Recent {
                sample_rate: 44_100,
                written: 2 * ToneAudio::STEP,
                generation: 1,
            })
        );
        assert_eq!(out.len(), FFT_SIZE);
        let peak = out.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
        assert!((0.45..=0.5).contains(&peak), "peak {peak}");

        p.toggle_pause();
        let paused = p.recent_samples(&mut out).map(|r| r.written);
        assert_eq!(
            paused,
            Some(2 * ToneAudio::STEP),
            "no new audio while paused"
        );

        p.stop();
        assert_eq!(p.recent_samples(&mut out), None);
        assert_eq!(out, Vec::<f32>::new());
    }

    /// Resumes item `item` of `items` from the library at `position`.
    fn resume_at(
        p: &mut Playback<FakeAudio>,
        items: Vec<usize>,
        item: usize,
        at: Duration,
    ) -> bool {
        p.resume(Source::Library, items, item, at, None)
    }

    /// Asserts that a failed resume left nothing current, stopped, with one
    /// "Cannot resume" error notice that is also the stop reason.
    fn assert_resume_failed(p: &mut Playback<FakeAudio>) {
        assert_eq!(p.current(), None);
        assert_eq!(p.current_item(), None);
        assert_eq!(p.state(), PlayState::Stopped);
        assert_eq!(p.audio.loaded, None);
        let notice = p.take_notice().unwrap_or_else(|| Notice::info("none"));
        assert_eq!(notice.level, Level::Error);
        assert!(
            notice.text.starts_with("Cannot resume 'Song 1': "),
            "{}",
            notice.text
        );
        assert_eq!(p.take_notice(), None, "one notice");
        assert_eq!(p.stop_reason(), Some(&notice));
        assert!(
            p.audio
                .loads
                .iter()
                .all(|(loaded, _, _)| *loaded == path(1)),
            "no other track is loaded"
        );
        let loads = p.audio.loads.len();
        p.tick();
        p.tick();
        assert_eq!(p.audio.loads.len(), loads, "nothing is retried later");
        assert_eq!(p.current(), None);
    }

    #[test]
    fn resume_loads_paused_at_the_position() {
        let mut p = playback(3, &[]);
        assert!(resume_at(&mut p, vec![0, 1, 2], 1, secs(135)));
        assert_eq!(p.state(), PlayState::Paused);
        assert_eq!(p.current(), Some(1));
        assert_eq!(p.current_item(), Some(1));
        assert_eq!(p.source(), &Source::Library);
        assert_eq!(p.audio.loads, [(path(1), secs(135), true)]);
        assert!(p.audio.paused, "never plays on its own");
        assert_eq!(p.position(), secs(135));
        assert_eq!(p.take_notice(), None);
        assert_eq!(p.stop_reason(), None);

        p.toggle_pause();
        assert_eq!(p.state(), PlayState::Playing);
        assert!(!p.audio.paused);
        assert_eq!(p.position(), secs(135), "continues from there");
        assert_eq!(p.audio.loads.len(), 1, "no reload");
    }

    #[test]
    fn resume_past_the_tag_length_starts_at_zero() {
        // `library` gives every track a 200 s tag length.
        for at in [secs(200), secs(201), secs(7 * 24 * 3600)] {
            let mut p = playback(3, &[]);
            assert!(resume_at(&mut p, vec![0, 1, 2], 1, at));
            assert_eq!(p.audio.loads, [(path(1), Duration::ZERO, true)], "{at:?}");
            assert_eq!(p.state(), PlayState::Paused);
        }
        let mut p = playback(3, &[]);
        assert!(resume_at(&mut p, vec![0, 1, 2], 1, secs(199)));
        assert_eq!(p.audio.loads, [(path(1), secs(199), true)], "just inside");
    }

    #[test]
    fn resume_without_a_tag_length_tries_the_position() {
        let mut tracks = library(2);
        tracks[1].duration = None;
        let mut p = Playback::new(tracks, FakeAudio::default());
        assert!(resume_at(&mut p, vec![0, 1], 1, secs(900)));
        assert_eq!(p.audio.loads, [(path(1), secs(900), true)]);
    }

    #[test]
    fn resume_retries_at_zero_after_any_seek_failure() {
        for past_end in [true, false] {
            let mut p = playback(3, &[]);
            p.audio.seek_failure = Some(past_end);
            assert!(resume_at(&mut p, vec![0, 1, 2], 1, secs(60)), "{past_end}");
            assert_eq!(
                p.audio.loads,
                [(path(1), secs(60), true), (path(1), Duration::ZERO, true)]
            );
            assert_eq!(p.state(), PlayState::Paused);
            assert_eq!(p.current(), Some(1));
            assert_eq!(p.position(), Duration::ZERO);
            assert_eq!(p.take_notice(), None);
        }
    }

    #[test]
    fn resume_failing_after_the_retry_loads_nothing_else() {
        let mut p = playback(3, &[]);
        p.audio.seek_failure = Some(false);
        p.audio.broken_from_start.insert(path(1));
        assert!(!resume_at(&mut p, vec![0, 1, 2], 1, secs(60)));
        assert_eq!(
            p.audio.loads,
            [(path(1), secs(60), true), (path(1), Duration::ZERO, true)],
            "exactly two loads"
        );
        assert_resume_failed(&mut p);
    }

    #[test]
    fn resume_of_a_broken_file_is_not_retried() {
        for at in [secs(60), Duration::ZERO] {
            let mut p = playback(3, &[1]);
            set_repeat(&mut p, Repeat::All);
            assert!(!resume_at(&mut p, vec![0, 1, 2], 1, at));
            assert_eq!(p.audio.loads, [(path(1), at, true)], "exactly one load");
            assert_resume_failed(&mut p);
        }
    }

    #[test]
    fn failed_resume_keeps_the_source_and_space_does_nothing() {
        let mut p = playback(6, &[1]);
        assert!(!p.resume(running(), vec![4, 1, 3], 1, secs(5), None));
        assert_eq!(p.source(), &running());
        assert_eq!(p.items(), [4, 1, 3]);
        p.toggle_pause();
        assert_eq!(p.audio.loads.len(), 1, "nothing current to start");
        assert_eq!(p.state(), PlayState::Stopped);
        p.next();
        assert_eq!(p.current(), Some(4), "next starts the list from its top");
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.stop_reason(), None);
    }

    #[test]
    fn resume_name_is_sanitised() {
        let mut tracks = library(2);
        tracks[1].title = "Bad\u{1b}[2J\nName".to_owned();
        let audio = FakeAudio {
            broken: [path(1)].into_iter().collect(),
            ..FakeAudio::default()
        };
        let mut p = Playback::new(tracks, audio);
        assert!(!resume_at(&mut p, vec![0, 1], 1, secs(5)));
        let text = p.take_notice().map(|n| n.text).unwrap_or_default();
        assert!(text.starts_with("Cannot resume '"), "{text}");
        assert!(!text.chars().any(char::is_control), "{text:?}");
    }

    #[test]
    fn resume_with_shuffle_starts_a_shuffled_order_at_the_track() {
        let mut p = playback(6, &[]);
        p.set_shuffle(true);
        assert!(p.resume(running(), vec![4, 2, 0, 3, 1], 2, secs(10), None));
        assert_eq!(p.current(), Some(0));
        assert_eq!(p.current_item(), Some(2));
        assert_eq!(p.source(), &running());
        assert_eq!(p.state(), PlayState::Paused);
        assert!(p.shuffle());

        let mut seen = vec![0];
        for _ in 0..4 {
            p.next();
            seen.push(p.current().unwrap_or(usize::MAX));
        }
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1, 2, 3, 4], "each item of the source once");
        assert!(!seen.contains(&5), "never the library");
        p.next();
        assert_eq!(p.current(), seen.last().copied(), "end of the order");
        p.audio.position = secs(1);
        p.prev();
        assert_eq!(
            p.current(),
            Some(seen[3]),
            "previous follows the same order"
        );
    }

    #[test]
    fn resume_replaces_a_playing_track() {
        let mut p = playback(3, &[]);
        p.play(2);
        assert!(resume_at(&mut p, vec![0, 1, 2], 0, secs(5)));
        assert_eq!(p.audio.loaded, Some(path(0)));
        assert_eq!(
            p.audio.loads,
            [(path(2), Duration::ZERO, false), (path(0), secs(5), true)]
        );
        assert_eq!(p.state(), PlayState::Paused);

        let mut p = playback(3, &[1]);
        p.play(2);
        assert!(!resume_at(&mut p, vec![0, 1, 2], 1, secs(5)));
        assert_eq!(
            p.audio.loads,
            [(path(2), Duration::ZERO, false), (path(1), secs(5), true)]
        );
        assert_eq!(p.audio.loaded, None, "the old track is not left audible");
        assert_eq!(p.current(), None);
        assert_eq!(p.state(), PlayState::Stopped);
    }

    #[test]
    fn resume_cancels_a_pending_retry() {
        let n = MAX_ATTEMPTS_PER_CALL * 2 + 3;
        for (item, ok) in [(n - 1, true), (0, false)] {
            let broken: Vec<usize> = (0..n - 1).collect();
            let mut p = playback(n, &broken);
            p.play(0);
            assert!(p.retry, "skipping is deferred to the next tick");
            assert_eq!(resume_at(&mut p, (0..n).collect(), item, secs(5)), ok);
            let loads = p.audio.loads.len();
            for _ in 0..3 {
                p.tick();
            }
            assert_eq!(
                p.audio.loads.len(),
                loads,
                "no deferred skipping after resume"
            );
            if ok {
                assert_eq!(p.current(), Some(n - 1));
                assert_eq!(p.state(), PlayState::Paused);
            } else {
                assert_eq!(p.current(), None);
                assert_eq!(p.state(), PlayState::Stopped);
            }
        }
    }

    #[test]
    fn resume_duplicates_with_repeat_one_from_a_detached_queue() {
        let mut p = playback(3, &[]);
        p.set_repeat(Repeat::One);
        assert!(p.play_source(running(), vec![1, 1, 1], 0));
        assert!(p.edit_playlist("Running", Edit::Remove(0)));
        assert!(p.queue.is_detached());
        let before = p.audio.loads.len();

        assert!(p.resume(running(), vec![1, 1, 1], 2, secs(5), None));
        assert_eq!(p.current_item(), Some(2));
        assert_eq!(p.current(), Some(1));
        assert!(!p.queue.is_detached());
        assert_eq!(p.audio.loads.len(), before + 1, "exactly one load");
        assert_eq!(p.repeat(), Repeat::One, "modes are kept");

        p.toggle_pause();
        end_track(&mut p);
        assert_eq!(p.current_item(), Some(2), "repeat one replays the item");
    }

    #[test]
    fn resume_name_falls_back_to_the_file_name_and_is_capped() {
        let mut tracks = library(2);
        tracks[1].title = "\u{1b}\u{7}".to_owned();
        let audio = FakeAudio {
            broken: [path(1)].into_iter().collect(),
            ..FakeAudio::default()
        };
        let mut p = Playback::new(tracks, audio);
        assert!(!resume_at(&mut p, vec![0, 1], 1, secs(5)));
        let text = p.take_notice().map(|n| n.text).unwrap_or_default();
        assert!(text.starts_with("Cannot resume '01.mp3': "), "{text}");

        let mut tracks = library(2);
        tracks[1].title = "x".repeat(10_000);
        let audio = FakeAudio {
            broken: [path(1)].into_iter().collect(),
            ..FakeAudio::default()
        };
        let mut p = Playback::new(tracks, audio);
        assert!(!resume_at(&mut p, vec![0, 1], 1, secs(5)));
        let text = p.take_notice().map(|n| n.text).unwrap_or_default();
        assert!(text.chars().count() < 600, "{} chars", text.chars().count());
    }

    #[test]
    fn invalid_resume_changes_nothing() {
        let mut p = playback(3, &[]);
        p.play(2);
        assert!(
            !resume_at(&mut p, vec![0, 1], 2, secs(5)),
            "item out of range"
        );
        assert!(
            !resume_at(&mut p, vec![0, 7], 1, secs(5)),
            "not a library index"
        );
        assert!(!resume_at(&mut p, Vec::new(), 0, secs(5)), "empty list");
        assert_eq!(p.current(), Some(2));
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.audio.loads.len(), 1);
    }

    #[test]
    fn setters_change_what_is_reported() {
        let mut p = playback(3, &[]);
        p.set_volume(Volume::new(60));
        assert_eq!(p.volume(), Volume::new(60));
        assert_eq!(p.audio.volume, Some(Volume::new(60)), "reaches the audio");
        p.volume_up();
        assert_eq!(p.volume(), Volume::new(65));

        p.play(1);
        p.set_shuffle(true);
        assert!(p.shuffle());
        assert_eq!(p.current(), Some(1), "shuffle keeps the current track");
        p.set_shuffle(false);
        assert!(!p.shuffle());
        assert_eq!(p.current(), Some(1));

        for repeat in [Repeat::One, Repeat::All, Repeat::Off] {
            p.set_repeat(repeat);
            assert_eq!(p.repeat(), repeat);
        }
        assert_eq!(p.audio.loads.len(), 1, "no setter reloads the track");
    }

    // ---- Up-next queue ----

    /// An album listing A, B, C (library tracks 3, 4, 5).
    fn album() -> Source {
        Source::Album(0)
    }

    const A: usize = 3;
    const B: usize = 4;
    const C: usize = 5;
    const X: usize = 7;
    const Y: usize = 8;
    const Z: usize = 9;

    /// Ten library tracks, playing A from the album with repeat `repeat`.
    fn playing_album(repeat: Repeat, broken: &[usize], start: usize) -> Playback<FakeAudio> {
        let mut p = playback(10, broken);
        p.set_repeat(repeat);
        assert!(p.play_source(album(), vec![A, B, C], start));
        p
    }

    /// The files loaded so far, in order.
    fn loaded(p: &Playback<FakeAudio>) -> Vec<PathBuf> {
        p.audio.loads.iter().map(|(l, ..)| l.clone()).collect()
    }

    #[test]
    fn enqueue_adds_to_the_end_or_the_front() {
        let mut p = playback(10, &[]);
        assert_eq!(p.enqueue(&[A, B], false), 2);
        assert_eq!(p.enqueue(&[C], false), 1);
        assert_eq!(p.up_next(), [A, B, C], "add to the end");

        let mut p = playback(10, &[]);
        assert_eq!(p.enqueue(&[A, B], false), 2);
        assert_eq!(p.enqueue(&[C, X], true), 2);
        assert_eq!(p.up_next(), [C, X, A, B], "play next keeps their order");

        let mut p = playback(10, &[]);
        p.enqueue(&[A], false);
        p.enqueue(&[A], false);
        assert_eq!(p.up_next(), [A, A], "duplicates allowed");
        assert_eq!(p.audio.loads.len(), 0, "queueing never plays");
        assert_eq!(p.state(), PlayState::Stopped);
    }

    #[test]
    fn a_full_queue_takes_the_first_tracks_that_fit() {
        let mut p = playback(20, &[]);
        let filler = vec![0; MAX_UP_NEXT - 5];
        assert_eq!(p.enqueue(&filler, false), MAX_UP_NEXT - 5);
        let album: Vec<usize> = (10..20).collect();
        assert_eq!(p.enqueue(&album, false), 5);
        assert_eq!(p.up_next().len(), MAX_UP_NEXT);
        assert_eq!(p.up_next()[MAX_UP_NEXT - 5..], [10, 11, 12, 13, 14]);

        let generation = p.up_next_generation();
        assert_eq!(p.enqueue(&[1], false), 0, "none fit");
        assert_eq!(p.enqueue(&[1], true), 0);
        assert_eq!(p.up_next_generation(), generation);

        let mut p = playback(20, &[]);
        p.enqueue(&filler, false);
        assert_eq!(p.enqueue(&album, true), 5, "the front takes the first ones");
        assert_eq!(p.up_next()[..6], [10, 11, 12, 13, 14, 0]);

        let mut p = playback(3, &[]);
        assert!(p.restore_up_next(vec![1; MAX_UP_NEXT + 7]));
        assert_eq!(p.up_next().len(), MAX_UP_NEXT, "a restore is capped too");
    }

    #[test]
    fn invalid_queue_operations_change_nothing() {
        let mut p = playback(10, &[]);
        p.enqueue(&[A, B], false);
        let generation = p.up_next_generation();
        assert_eq!(p.enqueue(&[C, 10], false), 0, "one invalid index");
        assert_eq!(p.enqueue(&[usize::MAX], true), 0);
        assert_eq!(p.enqueue(&[], false), 0);
        assert!(!p.remove_queued(2));
        assert!(!p.move_queued(0, 2));
        assert!(!p.move_queued(2, 0));
        assert!(!p.play_queued(2));
        assert!(!p.restore_up_next(vec![C, 10]));
        assert_eq!(p.up_next(), [A, B]);
        assert_eq!(p.up_next_generation(), generation);
        assert_eq!(p.current(), None);
        assert_eq!(p.audio.loads.len(), 0);

        let mut p = playback(10, &[]);
        assert!(!p.clear_queued(), "nothing to clear");
        assert_eq!(p.up_next_generation(), 0);
    }

    #[test]
    fn the_generation_changes_exactly_when_the_queue_does() {
        let mut p = playing_album(Repeat::Off, &[], 0);
        let mut last = p.up_next_generation();
        let mut changed = |p: &Playback<FakeAudio>| {
            let now = p.up_next_generation();
            let changed = now != last;
            last = now;
            changed
        };
        p.enqueue(&[X, Y, Z], false);
        assert!(changed(&p), "enqueue");
        assert!(p.move_queued(1, 1));
        assert!(!changed(&p), "a move in place changes nothing");
        assert!(p.move_queued(2, 0));
        assert_eq!(p.up_next(), [Z, X, Y]);
        assert!(changed(&p), "move");
        assert!(p.remove_queued(1));
        assert_eq!(p.up_next(), [Z, Y]);
        assert!(changed(&p), "remove");
        assert!(p.restore_up_next(vec![Z, Y]));
        assert!(!changed(&p), "restoring the same list changes nothing");
        p.audio.position = secs(5);
        p.seek(true);
        p.toggle_pause();
        p.toggle_pause();
        p.cycle_repeat();
        p.cycle_repeat();
        p.cycle_repeat();
        p.tick();
        assert!(!changed(&p), "transport commands leave the queue alone");

        let taken = p.up_next_taken();
        p.next();
        assert_eq!(p.up_next(), [Y]);
        assert!(changed(&p), "taking the front to play it");
        assert_eq!(p.up_next_taken(), taken + 1);
        assert!(p.play_queued(0));
        assert!(changed(&p), "play now");
        assert_eq!(
            p.up_next_taken(),
            taken + 1,
            "play now is not taken by advancing"
        );
        p.enqueue(&[X], true);
        assert!(changed(&p));
        assert!(p.clear_queued());
        assert!(changed(&p), "clear");
        p.next();
        assert!(!changed(&p), "the play order moving on");
    }

    #[test]
    fn a_queued_track_is_current_but_not_an_item_of_the_play_order() {
        let mut p = playing_album(Repeat::Off, &[], 1);
        p.enqueue(&[X], false);
        assert!(!p.playing_queued());
        assert_eq!(p.current_item(), Some(1));
        p.next();
        assert_eq!(p.current(), Some(X));
        assert!(p.playing_queued());
        assert_eq!(p.current_item(), None);
        assert_eq!(p.current_track().map(|t| t.path.clone()), Some(path(X)));
        assert_eq!(p.source_current(), Some(B), "the play order stays on B");
        assert_eq!(p.source_item(), Some(1));
        assert_eq!(p.source(), &album());
        assert_eq!(p.items(), [A, B, C]);
    }

    #[test]
    fn a_queued_track_plays_when_the_current_one_ends() {
        let mut p = playing_album(Repeat::Off, &[], 0);
        p.enqueue(&[X], false);
        end_track(&mut p);
        assert_eq!(p.current(), Some(X));
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.up_next(), [] as [usize; 0]);
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(X), Duration::ZERO, false))
        );
    }

    #[test]
    fn next_takes_the_queue_first() {
        let mut p = playback(10, &[]);
        p.play(3);
        p.enqueue(&[X, Y], false);
        p.next();
        assert_eq!(p.current(), Some(X));
        assert_eq!(p.up_next(), [Y]);

        let mut p = playback(10, &[]);
        p.enqueue(&[X], false);
        p.next();
        assert_eq!(p.current(), Some(X), "nothing played yet");
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.source_current(), None);
    }

    #[test]
    fn the_play_order_continues_after_the_queue() {
        let mut p = playing_album(Repeat::Off, &[], 0);
        p.enqueue(&[X, Y], false);
        end_track(&mut p);
        end_track(&mut p);
        assert_eq!(p.current(), Some(Y));
        end_track(&mut p);
        assert_eq!(p.current(), Some(B), "back to the album");
        assert_eq!(p.current_item(), Some(1));
        assert!(!p.playing_queued());
        assert_eq!(loaded(&p), [path(A), path(X), path(Y), path(B)]);
    }

    #[test]
    fn the_queue_at_the_end_of_the_play_order() {
        let mut p = playing_album(Repeat::Off, &[], 2);
        p.enqueue(&[X], false);
        end_track(&mut p);
        assert_eq!(p.current(), Some(X));
        end_track(&mut p);
        assert_eq!(p.state(), PlayState::Stopped, "stops after X");
        assert_eq!(p.current(), Some(X), "X stays current");
        assert!(p.playing_queued());
        p.toggle_pause();
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(X), Duration::ZERO, false))
        );
        assert_eq!(p.current(), Some(X), "Space plays X again");
    }

    #[test]
    fn next_on_the_last_queued_track_at_the_end_keeps_it() {
        let mut p = playing_album(Repeat::Off, &[], 2);
        p.enqueue(&[X], false);
        end_track(&mut p);
        let loads = p.audio.loads.len();
        p.take_notice();
        p.next();
        assert_eq!(p.current(), Some(X));
        assert!(p.playing_queued());
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.audio.loads.len(), loads, "not reloaded");
        assert_eq!(
            p.take_notice(),
            Some(Notice::info("Already at the last track"))
        );
    }

    #[test]
    fn repeat_all_continues_the_play_order_after_the_queue() {
        let mut p = playing_album(Repeat::All, &[], 2);
        p.enqueue(&[X], false);
        end_track(&mut p);
        assert_eq!(p.current(), Some(X));
        end_track(&mut p);
        assert_eq!(p.current(), Some(A));
        assert_eq!(p.current_item(), Some(0));
        assert_eq!(p.up_next(), [] as [usize; 0], "X is not queued again");
        end_track(&mut p);
        assert_eq!(p.current(), Some(B));
    }

    #[test]
    fn shuffle_does_not_reorder_the_queue() {
        let mut p = playback(20, &[]);
        p.set_shuffle(true);
        assert!(p.play_source(album(), (10..20).collect(), 0));
        p.enqueue(&[X, Y, Z], false);
        let mut played = Vec::new();
        for _ in 0..3 {
            end_track(&mut p);
            played.extend(p.current());
        }
        assert_eq!(played, [X, Y, Z]);
        end_track(&mut p);
        assert!(!p.playing_queued());
        assert!(p.current().is_some_and(|t| (10..20).contains(&t)));
    }

    #[test]
    fn repeat_one_holds_the_queue_until_next() {
        // On a play order track.
        let mut p = playing_album(Repeat::One, &[], 0);
        p.enqueue(&[X], false);
        end_track(&mut p);
        assert_eq!(p.current(), Some(A), "A plays again");
        assert_eq!(p.up_next(), [X]);
        assert_eq!(loaded(&p), [path(A), path(A)]);
        p.next();
        assert_eq!(p.current(), Some(X));

        // On a queued track.
        let mut p = playing_album(Repeat::One, &[], 0);
        p.enqueue(&[X, Y], false);
        p.next();
        end_track(&mut p);
        assert_eq!(p.current(), Some(X), "X plays again");
        assert!(p.playing_queued());
        assert_eq!(p.up_next(), [Y]);
        assert_eq!(loaded(&p), [path(A), path(X), path(X)]);
        p.next();
        assert_eq!(p.current(), Some(Y));
        p.next();
        assert_eq!(p.current(), Some(B), "then the album moves on from A");
    }

    #[test]
    fn repeat_one_on_a_removed_playlist_entry_takes_the_queue() {
        // A detached track cannot be replayed, so it moves on as before:
        // here to the queue first.
        let mut p = playback(10, &[]);
        p.set_repeat(Repeat::One);
        assert!(p.play_source(running(), vec![A, B, C], 0));
        assert!(p.edit_playlist("Running", Edit::Remove(0)));
        p.enqueue(&[X], false);
        end_track(&mut p);
        assert_eq!(p.current(), Some(X));
        p.next();
        assert_eq!(p.current(), Some(B), "the entry after the removed one");
    }

    #[test]
    fn repeat_all_never_puts_a_track_back_in_the_queue() {
        let mut p = playing_album(Repeat::All, &[], 0);
        p.enqueue(&[X], false);
        end_track(&mut p);
        assert_eq!(p.current(), Some(X));
        end_track(&mut p);
        assert_eq!(p.up_next(), [] as [usize; 0]);
        assert_eq!(p.current(), Some(B));
        assert!(!p.playing_queued());
    }

    #[test]
    fn a_corrupt_queued_track_is_reported_and_skipped() {
        let mut p = playing_album(Repeat::Off, &[X], 0);
        p.enqueue(&[X, Y], false);
        end_track(&mut p);
        assert_eq!(p.current(), Some(Y));
        assert_eq!(p.state(), PlayState::Playing);
        let notice = p.take_notice();
        assert!(
            notice
                .as_ref()
                .is_some_and(|n| n.level == Level::Error && n.text.contains("07.mp3")),
            "{notice:?}"
        );
        assert_eq!(loaded(&p), [path(A), path(X), path(Y)]);
    }

    #[test]
    fn a_failing_queued_track_with_an_empty_play_order_moves_on_through_the_queue() {
        let mut p = playback(10, &[X]);
        assert!(p.play_source(running(), vec![A], 0));
        assert!(p.edit_playlist("Running", Edit::Remove(0)));
        assert_eq!(p.items(), [] as [usize; 0]);
        p.enqueue(&[X, Y], false);
        p.next();
        assert_eq!(p.current(), Some(Y));
        assert_eq!(p.state(), PlayState::Playing);
        let notice = p.take_notice();
        assert!(
            notice
                .as_ref()
                .is_some_and(|n| n.text.contains("07.mp3") && !n.text.contains("no playable")),
            "{notice:?}"
        );

        // The last queued track failing stops, without the suffix.
        p.audio.broken.insert(path(Z));
        p.enqueue(&[Z], false);
        p.next();
        assert_eq!(p.state(), PlayState::Stopped);
        let reason = p.stop_reason().map(|n| n.text.clone()).unwrap_or_default();
        assert!(reason.contains("09.mp3"), "{reason}");
        assert!(!reason.contains("no playable"), "{reason}");
    }

    #[test]
    fn a_failing_queued_track_is_followed_by_the_play_order() {
        let mut p = playing_album(Repeat::Off, &[X], 0);
        p.enqueue(&[X], false);
        end_track(&mut p);
        assert_eq!(p.current(), Some(B));
        assert_eq!(p.current_item(), Some(1));
        assert!(!p.playing_queued());
        assert_eq!(p.state(), PlayState::Playing);
        assert!(p.take_notice().is_some_and(|n| n.text.contains("07.mp3")));
    }

    #[test]
    fn queued_failures_do_not_count_toward_stopping_the_play_order() {
        let mut p = playback(10, &[A, B, C, X, Y]);
        p.set_repeat(Repeat::All);
        p.enqueue(&[X, Y], false);
        assert!(p.play_source(album(), vec![A, B, C], 0));
        assert_eq!(p.state(), PlayState::Stopped);
        assert_eq!(
            loaded(&p),
            [path(A), path(X), path(Y), path(B), path(C)],
            "every track tried once, each play order track before giving up"
        );
        assert!(
            p.stop_reason()
                .is_some_and(|n| n.text.contains("no playable tracks"))
        );
    }

    #[test]
    fn previous_on_a_queued_track() {
        // Early: back to the play order's track, from the start.
        let mut p = playing_album(Repeat::Off, &[], 0);
        p.enqueue(&[X], false);
        p.next();
        p.audio.position = secs(2);
        p.prev();
        assert_eq!(p.current(), Some(A));
        assert_eq!(p.current_item(), Some(0));
        assert!(!p.playing_queued());
        assert_eq!(p.up_next(), [] as [usize; 0], "X is not queued again");
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(A), Duration::ZERO, false))
        );

        // Late: X restarts.
        let mut p = playing_album(Repeat::Off, &[], 0);
        p.enqueue(&[X], false);
        p.next();
        p.audio.position = secs(70);
        p.prev();
        assert_eq!(p.current(), Some(X));
        assert!(p.playing_queued());
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(X), Duration::ZERO, false))
        );

        // No play order position: X restarts.
        let mut p = playback(10, &[]);
        p.enqueue(&[X], false);
        p.next();
        p.audio.position = secs(2);
        p.prev();
        assert_eq!(p.current(), Some(X));
        assert!(p.playing_queued());
        assert_eq!(loaded(&p), [path(X), path(X)]);
    }

    #[test]
    fn stop_keeps_the_queued_track_current() {
        let mut p = playing_album(Repeat::Off, &[], 0);
        p.enqueue(&[X, Y], false);
        p.next();
        p.audio.position = secs(30);
        p.stop();
        assert_eq!(p.current(), Some(X));
        p.toggle_pause();
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(X), Duration::ZERO, false))
        );
        assert_eq!(p.up_next(), [Y], "the queue does not move");
    }

    #[test]
    fn space_with_nothing_loaded_plays_the_first_queued_track() {
        let mut p = playback(10, &[]);
        assert!(p.restore_up_next(vec![X, Y]));
        p.toggle_pause();
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.current(), Some(X));
        assert_eq!(p.up_next(), [Y]);

        let mut p = playback(10, &[]);
        p.toggle_pause();
        assert_eq!(
            p.state(),
            PlayState::Stopped,
            "nothing queued, nothing to play"
        );
        assert_eq!(p.up_next_generation(), 0);
    }

    #[test]
    fn a_new_play_order_keeps_the_queue() {
        let mut p = playback(10, &[]);
        p.play(1);
        p.enqueue(&[X], false);
        assert!(p.play_source(album(), vec![A, B, C], 1));
        assert_eq!(p.up_next(), [X]);
        end_track(&mut p);
        assert_eq!(p.current(), Some(X));

        // Playing a track while a queued one is current leaves it behind.
        p.enqueue(&[Y], false);
        p.play(2);
        assert!(!p.playing_queued());
        assert_eq!(p.current(), Some(2));
        assert_eq!(p.current_item(), Some(2));
        assert_eq!(p.up_next(), [Y]);
        p.next();
        assert_eq!(p.current(), Some(Y));
        assert!(p.play_source(album(), vec![A, B, C], 0));
        assert!(!p.playing_queued());
        assert_eq!(p.current_item(), Some(0));
    }

    #[test]
    fn play_a_queued_track_now() {
        let mut p = playing_album(Repeat::Off, &[], 0);
        p.enqueue(&[X, Y, Z], false);
        assert!(p.play_queued(1));
        assert_eq!(p.current(), Some(Y));
        assert!(p.playing_queued());
        assert_eq!(p.up_next(), [X, Z]);
        assert_eq!(p.source_item(), Some(0), "the play order's cursor stays");
        end_track(&mut p);
        assert_eq!(p.current(), Some(X));
        end_track(&mut p);
        assert_eq!(p.current(), Some(Z));
        end_track(&mut p);
        assert_eq!(p.current(), Some(B));
        assert_eq!(p.current_item(), Some(1));
    }

    #[test]
    fn playing_a_broken_queued_track_now_moves_on() {
        let mut p = playing_album(Repeat::Off, &[X], 0);
        p.enqueue(&[X, Y], false);
        assert!(p.play_queued(0));
        assert_eq!(p.current(), Some(Y));
        assert!(p.take_notice().is_some_and(|n| n.text.contains("07.mp3")));
    }

    #[test]
    fn playlist_edits_while_a_queued_track_plays() {
        // Removing the play order's current entry.
        let mut p = playback(10, &[]);
        assert!(p.play_source(running(), vec![A, B, C], 0));
        p.enqueue(&[X], false);
        p.next();
        assert!(p.edit_playlist("Running", Edit::Remove(0)));
        assert_eq!(p.current(), Some(X));
        assert!(p.playing_queued());
        end_track(&mut p);
        assert_eq!(p.current(), Some(B), "the entry after the removed one");
        assert_eq!(p.current_item(), Some(0));

        // Moving entries.
        let mut p = playback(10, &[]);
        assert!(p.play_source(running(), vec![A, B, C], 0));
        p.enqueue(&[X], false);
        p.next();
        assert!(p.edit_playlist("Running", Edit::Move { from: 2, to: 1 }));
        assert_eq!(p.current(), Some(X));
        end_track(&mut p);
        assert_eq!(p.current(), Some(C), "C now follows A");
        assert_eq!(p.current_item(), Some(1));

        // Reloading the playlist.
        let mut p = playback(10, &[]);
        assert!(p.play_source(running(), vec![A, B, C], 1));
        p.enqueue(&[X], false);
        p.next();
        assert!(p.edit_playlist("Running", Edit::Replace(vec![C, B, 6])));
        assert_eq!(p.current(), Some(X));
        assert_eq!(p.source_item(), Some(1), "B followed to its new position");
        end_track(&mut p);
        assert_eq!(p.current(), Some(6));
        assert_eq!(p.current_item(), Some(2));

        // Appending.
        let mut p = playback(10, &[]);
        assert!(p.play_source(running(), vec![A], 0));
        p.enqueue(&[X], false);
        p.next();
        assert!(p.edit_playlist("Running", Edit::Append(vec![B])));
        assert_eq!(p.current(), Some(X));
        end_track(&mut p);
        assert_eq!(p.current(), Some(B));
    }

    #[test]
    fn resume_a_queued_track_after_the_play_order_item() {
        let mut p = playback(10, &[]);
        assert!(p.restore_up_next(vec![Y]));
        assert!(p.resume(album(), vec![A, B, C], 0, secs(30), Some(X)));
        assert_eq!(p.current(), Some(X));
        assert!(p.playing_queued());
        assert_eq!(p.current_item(), None);
        assert_eq!(p.source_item(), Some(0));
        assert_eq!(p.source_current(), Some(A));
        assert_eq!(p.source(), &album());
        assert_eq!(p.state(), PlayState::Paused);
        assert_eq!(
            p.audio.loads,
            [(path(X), secs(30), true)],
            "A is not loaded"
        );
        assert_eq!(p.up_next(), [Y], "the queue is kept");

        p.toggle_pause();
        end_track(&mut p);
        assert_eq!(p.current(), Some(Y));
        end_track(&mut p);
        assert_eq!(p.current(), Some(B));
        assert_eq!(p.current_item(), Some(1));
    }

    #[test]
    fn resume_a_queued_track_uses_the_seek_fallback() {
        let mut p = playback(10, &[]);
        p.audio.seek_failure = Some(false);
        assert!(p.resume(album(), vec![A, B, C], 1, secs(30), Some(X)));
        assert_eq!(
            p.audio.loads,
            [(path(X), secs(30), true), (path(X), Duration::ZERO, true)]
        );
        assert_eq!(p.current(), Some(X));

        let mut p = playback(10, &[]);
        assert!(p.resume(album(), vec![A, B, C], 1, secs(500), Some(X)));
        assert_eq!(
            p.audio.loads,
            [(path(X), Duration::ZERO, true)],
            "past its tag length"
        );
    }

    #[test]
    fn a_queued_track_that_fails_to_resume_leaves_nothing_current() {
        let mut p = playback(10, &[X]);
        assert!(!p.resume(album(), vec![A, B, C], 1, secs(30), Some(X)));
        assert_eq!(p.current(), None);
        assert!(!p.playing_queued());
        assert_eq!(p.source_item(), None);
        assert_eq!(p.source(), &album());
        assert_eq!(p.items(), [A, B, C]);
        assert_eq!(p.state(), PlayState::Stopped);
        assert_eq!(loaded(&p), [path(X)], "nothing else is loaded");
        let notice = p.take_notice().unwrap_or_else(|| Notice::info("none"));
        assert_eq!(notice.level, Level::Error);
        assert!(
            notice.text.starts_with("Cannot resume 'Song 7': "),
            "{}",
            notice.text
        );
        assert_eq!(p.stop_reason(), Some(&notice));
        p.tick();
        assert_eq!(p.audio.loads.len(), 1, "nothing is retried later");
    }

    #[test]
    fn resume_a_queued_track_with_nothing_current_in_the_play_order() {
        let mut p = playback(10, &[]);
        assert!(p.play_source(album(), vec![A, B, C], 0));
        assert!(p.resume_queued(X, secs(12)));
        assert_eq!(p.current(), Some(X));
        assert!(p.playing_queued());
        assert_eq!(p.source(), &Source::Library);
        assert_eq!(p.items(), (0..10).collect::<Vec<_>>());
        assert_eq!(p.source_current(), None);
        assert_eq!(p.state(), PlayState::Paused);
        assert_eq!(p.audio.loads.last(), Some(&(path(X), secs(12), true)));

        p.toggle_pause();
        end_track(&mut p);
        assert_eq!(p.current(), Some(0), "the library from its start");
        assert!(!p.playing_queued());

        let mut p = playback(10, &[X]);
        assert!(!p.resume_queued(X, secs(12)));
        assert_eq!(p.current(), None);
        assert!(!p.playing_queued());
        assert!(
            p.take_notice()
                .is_some_and(|n| n.text.starts_with("Cannot resume 'Song 7': "))
        );
    }

    #[test]
    fn invalid_queued_resumes_change_nothing() {
        let mut p = playing_album(Repeat::Off, &[], 0);
        assert!(!p.resume(album(), vec![A, B, C], 0, secs(5), Some(10)));
        assert!(!p.resume_queued(10, secs(5)));
        assert!(!p.resume_queued(usize::MAX, secs(5)));
        assert_eq!(p.current(), Some(A));
        assert_eq!(p.state(), PlayState::Playing);
        assert_eq!(p.audio.loads.len(), 1);
    }

    #[test]
    fn a_plain_resume_leaves_a_queued_track_behind() {
        let mut p = playing_album(Repeat::Off, &[], 0);
        p.enqueue(&[X, Y], false);
        p.next();
        assert!(p.resume(album(), vec![A, B, C], 2, secs(5), None));
        assert!(!p.playing_queued());
        assert_eq!(p.current(), Some(C));
        assert_eq!(p.up_next(), [Y]);
    }

    #[test]
    fn seeking_to_the_end_of_a_queued_track_follows_the_advance_rule() {
        // At the end of the play order with repeat off: stops on X.
        let mut p = playing_album(Repeat::Off, &[], 2);
        p.enqueue(&[X], false);
        p.next();
        p.audio.position = secs(198);
        p.seek(true);
        assert_eq!(p.state(), PlayState::Stopped);
        assert_eq!(p.current(), Some(X));
        assert!(p.playing_queued());

        // Repeat one: X plays again and the queue waits.
        let mut p = playing_album(Repeat::One, &[], 0);
        p.enqueue(&[X, Y], false);
        p.next();
        p.audio.position = secs(198);
        p.seek(true);
        assert_eq!(p.current(), Some(X));
        assert_eq!(p.up_next(), [Y]);
        assert_eq!(
            p.audio.loads.last(),
            Some(&(path(X), Duration::ZERO, false))
        );
    }

    #[test]
    fn long_runs_of_failing_queued_tracks_are_spread_over_ticks() {
        let n = MAX_ATTEMPTS_PER_CALL + 4;
        let broken: Vec<usize> = (0..n).collect();
        let mut p = playback(n + 1, &broken);
        p.enqueue(&broken, false);
        p.next();
        assert_eq!(p.audio.loads.len(), MAX_ATTEMPTS_PER_CALL);
        assert!(p.retry, "the rest is deferred");
        assert_eq!(p.state(), PlayState::Playing);
        for _ in 0..3 {
            p.tick();
        }
        // The play order (the whole library) starts at track 0 once the
        // queue is used up; it is broken too, and so on up to track n.
        assert_eq!(p.current(), Some(n), "reaches the playable track");
        assert_eq!(p.audio.loaded, Some(path(n)));
        assert_eq!(p.up_next(), [] as [usize; 0]);
        let queued_loads = p.audio.loads.iter().take(n).map(|(l, ..)| l.clone());
        assert!(
            queued_loads.eq(broken.iter().map(|&i| path(i))),
            "each queued track tried once"
        );
        assert!(p.stop_reason().is_none());
    }

    #[test]
    fn a_queued_track_failing_while_it_plays_moves_on() {
        let mut p = playing_album(Repeat::Off, &[], 0);
        p.enqueue(&[X, Y], false);
        p.next();
        p.audio.status = Some(Status::Failed("bad data".to_owned()));
        p.tick();
        assert_eq!(p.current(), Some(Y));
        assert_eq!(p.state(), PlayState::Playing);
        assert!(p.take_notice().is_some_and(|n| n.text.contains("07.mp3")));
        assert_eq!(p.failures, 0, "a queued failure is not counted");
    }

    #[test]
    fn a_failing_queued_track_with_repeat_one_is_not_retried() {
        let mut p = playing_album(Repeat::One, &[X], 0);
        p.enqueue(&[X, Y], false);
        p.next();
        assert_eq!(p.current(), Some(Y));
        assert_eq!(loaded(&p), [path(A), path(X), path(Y)]);

        // Failing during playback too.
        let mut p = playing_album(Repeat::One, &[], 0);
        p.enqueue(&[X, Y], false);
        p.next();
        p.audio.status = Some(Status::Failed("bad data".to_owned()));
        p.tick();
        assert_eq!(p.current(), Some(Y));
        p.tick();
        assert_eq!(loaded(&p), [path(A), path(X), path(Y)], "X not retried");
    }

    #[test]
    fn space_after_a_failed_resume_plays_the_queue() {
        let mut p = playback(10, &[A]);
        assert!(p.restore_up_next(vec![X]));
        assert!(!p.resume(album(), vec![A, B, C], 0, secs(5), None));
        assert_eq!(p.current(), None);
        p.toggle_pause();
        assert_eq!(
            p.current(),
            Some(X),
            "with nothing current, Space takes the queue"
        );
        assert_eq!(p.state(), PlayState::Playing);
    }
}
