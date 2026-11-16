//! Playback control: ties the play order ([`Queue`]) to an audio output
//! ([`Audio`]) and implements the transport commands, automatic advance and
//! skipping tracks that cannot be played.

use std::time::Duration;

use crate::library::Track;
use crate::player::{Audio, SEEK_STEP, SeekFailed, Status, Volume, seek_target};
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

/// Transport state shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Stopped,
    Playing,
    Paused,
}

impl PlayState {
    /// Label for the now-playing panel.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Playing => "Playing",
            Self::Paused => "Paused",
        }
    }
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
    fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            level: Level::Info,
        }
    }

    fn error(text: impl Into<String>) -> Self {
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
}

impl<A: Audio> Playback<A> {
    /// Playback over `tracks` (in display order) through `audio`, stopped,
    /// at full volume, with shuffle and repeat off.
    pub fn new(tracks: Vec<Track>, mut audio: A) -> Self {
        let volume = Volume::default();
        audio.set_volume(volume);
        Self {
            queue: Queue::new(tracks.len()),
            tracks,
            audio,
            state: PlayState::Stopped,
            volume,
            failures: 0,
            retry: false,
            notice: None,
            stop_reason: None,
        }
    }

    /// All tracks, in library order.
    #[must_use]
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Library index of the current track (playing, paused, or last played).
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        self.queue.current()
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

    /// Plays library track `track` from the start, replacing the current one.
    pub fn play(&mut self, track: usize) {
        if self.queue.select(track, &mut rand::rng()) {
            self.failures = 0;
            self.start_current(false);
        }
    }

    /// Pauses or resumes. When stopped, starts the current track again from
    /// the beginning; does nothing if there is no current track.
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
                }
            }
        }
    }

    /// Stops playback; the current track stays selected and restarts from the
    /// beginning when resumed.
    pub fn stop(&mut self) {
        self.audio.stop();
        self.state = PlayState::Stopped;
        self.retry = false;
    }

    /// Starts the next track in play order (the first if nothing is current).
    pub fn next(&mut self) {
        if self.queue.next(Advance::Manual).is_some() {
            self.failures = 0;
            self.start_current(false);
        } else if !self.queue.is_empty() {
            self.notice = Some(Notice::info("Already at the last track"));
        }
    }

    /// Restarts the current track if it is more than
    /// [`PREV_RESTART_AFTER`] in, else starts the previous track in play order
    /// (restarting the current one when there is no previous).
    pub fn prev(&mut self) {
        if self.current().is_none() {
            return;
        }
        if self.state == PlayState::Stopped || self.position() <= PREV_RESTART_AFTER {
            // At the start of the order the current track is kept and restarted.
            self.queue.prev();
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

    /// Housekeeping, to be called regularly (every UI frame): reports output
    /// device errors, advances when the current track ends and skips tracks
    /// that fail to decode.
    pub fn tick(&mut self) {
        if let Some(err) = self.audio.take_device_error() {
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

    fn set_volume(&mut self, volume: Volume) {
        self.volume = volume;
        self.audio.set_volume(volume);
    }

    /// The current track finished normally: continue per repeat mode.
    fn track_ended(&mut self) {
        self.failures = 0;
        if self.queue.next(Advance::Ended).is_some() {
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

    /// Reports the current track as unplayable and moves the queue past it.
    /// Returns `false` when playback stopped: end of the order with repeat off,
    /// or every track in the order failed in a row.
    fn record_failure(&mut self, why: &str) -> bool {
        self.audio.stop();
        self.failures += 1;
        let name = self.current_track().map_or_else(String::new, file_name);
        let mut notice = Notice::error(format!("Cannot play '{name}': {why}"));

        let more = self.failures < self.queue.len() && self.queue.next(Advance::Failed).is_some();
        if !more {
            if self.failures >= self.queue.len() {
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
}
