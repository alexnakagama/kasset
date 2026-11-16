//! Play order: which library track comes next, honouring shuffle and repeat.
//!
//! Pure logic with no audio or terminal dependencies. Tracks are referred to by
//! their index in the library; the queue never looks at the tracks themselves.

use rand::Rng;
use rand::seq::SliceRandom;

/// What happens when the play order runs out or a track ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    /// Stop after the last track.
    #[default]
    Off,
    /// Wrap from the last track to the first (and back with previous).
    All,
    /// Replay the current track when it ends.
    One,
}

impl Repeat {
    /// The next mode in the cycle off → all → one → off.
    #[must_use]
    pub const fn cycle(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    /// Short label for the status line.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }
}

/// Why the queue is being advanced; repeat modes treat each cause differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// The user asked for the next track. Repeat one does not hold the user on
    /// the current track; any repeat mode wraps at the end.
    Manual,
    /// The current track played to its end: repeat one replays it, repeat all
    /// wraps, repeat off stops at the end.
    Ended,
    /// The current track could not be played. Never replays it (that could loop
    /// forever); wraps at the end unless repeat is off.
    Failed,
}

/// The order tracks play in and the position of the current one.
#[derive(Debug, Clone)]
pub struct Queue {
    /// A permutation of `0..len`: identity when linear, random when shuffled.
    order: Vec<usize>,
    /// Index into `order` of the current track.
    pos: Option<usize>,
    shuffle: bool,
    repeat: Repeat,
}

impl Queue {
    /// A linear queue over `len` tracks with nothing current.
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self {
            order: (0..len).collect(),
            pos: None,
            shuffle: false,
            repeat: Repeat::Off,
        }
    }

    /// Number of tracks in the play order.
    #[must_use]
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether the play order holds no tracks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Library index of the current track, if any.
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        self.pos.map(|p| self.order[p])
    }

    /// Whether shuffle is on.
    #[must_use]
    pub const fn shuffle(&self) -> bool {
        self.shuffle
    }

    /// The current repeat mode.
    #[must_use]
    pub const fn repeat(&self) -> Repeat {
        self.repeat
    }

    /// The play order as library indices.
    #[must_use]
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// Makes library track `track` current. With shuffle on, the order is
    /// reshuffled with `track` first so every other track still plays once
    /// before any repeats. Returns `false` (and changes nothing) when `track`
    /// is out of range.
    pub fn select<R: Rng + ?Sized>(&mut self, track: usize, rng: &mut R) -> bool {
        if track >= self.order.len() {
            return false;
        }
        if self.shuffle {
            self.reshuffle(Some(track), rng);
        } else {
            self.pos = Some(track);
        }
        true
    }

    /// Advances the play order and returns the new current track, or `None`
    /// when there is no next track (the current track is left unchanged).
    ///
    /// With nothing current, starts from the first track in the order.
    pub fn next(&mut self, cause: Advance) -> Option<usize> {
        let len = self.order.len();
        let Some(pos) = self.pos else {
            self.pos = (len > 0).then_some(0);
            return self.current();
        };
        if cause == Advance::Ended && self.repeat == Repeat::One {
            return self.current();
        }
        let wraps = match cause {
            Advance::Ended => self.repeat == Repeat::All,
            Advance::Manual | Advance::Failed => self.repeat != Repeat::Off,
        };
        if pos + 1 < len {
            self.pos = Some(pos + 1);
        } else if wraps {
            self.pos = Some(0);
        } else {
            return None;
        }
        self.current()
    }

    /// Steps back in the play order and returns the new current track, or
    /// `None` when already at the start and repeat is off (or nothing is
    /// current). Any repeat mode other than off wraps to the last track.
    pub fn prev(&mut self) -> Option<usize> {
        let pos = self.pos?;
        if pos > 0 {
            self.pos = Some(pos - 1);
        } else if self.repeat != Repeat::Off {
            self.pos = Some(self.order.len() - 1);
        } else {
            return None;
        }
        self.current()
    }

    /// Turns shuffle on or off, keeping the current track current.
    ///
    /// Turning it on builds a fresh random order (Fisher–Yates) with the
    /// current track first; turning it off restores library order.
    pub fn set_shuffle<R: Rng + ?Sized>(&mut self, on: bool, rng: &mut R) {
        let current = self.current();
        self.shuffle = on;
        if on {
            self.reshuffle(current, rng);
        } else {
            self.order = (0..self.order.len()).collect();
            self.pos = current;
        }
    }

    /// Flips shuffle; see [`Queue::set_shuffle`]. Returns the new state.
    pub fn toggle_shuffle<R: Rng + ?Sized>(&mut self, rng: &mut R) -> bool {
        self.set_shuffle(!self.shuffle, rng);
        self.shuffle
    }

    /// Moves to the next repeat mode and returns it.
    pub const fn cycle_repeat(&mut self) -> Repeat {
        self.repeat = self.repeat.cycle();
        self.repeat
    }

    /// Random permutation of all tracks with `first` (if any) moved to the front.
    fn reshuffle<R: Rng + ?Sized>(&mut self, first: Option<usize>, rng: &mut R) {
        // `SliceRandom::shuffle` is a Fisher–Yates shuffle. Swapping one element
        // to the front afterwards leaves the rest uniformly permuted.
        self.order.shuffle(rng);
        self.pos = first.and_then(|track| {
            let at = self.order.iter().position(|&t| t == track)?;
            self.order.swap(0, at);
            Some(0)
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use std::collections::HashSet;

    fn rng() -> StdRng {
        StdRng::seed_from_u64(0x6b61_7373_6574)
    }

    fn queue(len: usize, repeat: Repeat) -> Queue {
        let mut q = Queue::new(len);
        q.repeat = repeat;
        q
    }

    #[test]
    fn repeat_cycles_off_all_one() {
        let mut q = Queue::new(1);
        assert_eq!(q.repeat(), Repeat::Off);
        assert_eq!(q.cycle_repeat(), Repeat::All);
        assert_eq!(q.cycle_repeat(), Repeat::One);
        assert_eq!(q.cycle_repeat(), Repeat::Off);
    }

    #[test]
    fn next_and_prev_walk_linear_order() {
        let mut q = queue(4, Repeat::Off);
        assert!(q.select(1, &mut rng()));
        assert_eq!(q.next(Advance::Manual), Some(2));
        assert_eq!(q.next(Advance::Ended), Some(3));
        assert_eq!(q.prev(), Some(2));
        assert_eq!(q.prev(), Some(1));
        assert_eq!(q.prev(), Some(0));
        assert_eq!(q.current(), Some(0));
    }

    #[test]
    fn next_with_nothing_current_starts_at_first() {
        let mut q = queue(3, Repeat::Off);
        assert_eq!(q.current(), None);
        assert_eq!(q.prev(), None);
        assert_eq!(q.next(Advance::Manual), Some(0));
    }

    #[test]
    fn repeat_off_stops_at_ends() {
        let mut q = queue(3, Repeat::Off);
        q.select(2, &mut rng());
        assert_eq!(q.next(Advance::Ended), None);
        assert_eq!(q.next(Advance::Manual), None);
        assert_eq!(q.next(Advance::Failed), None);
        assert_eq!(
            q.current(),
            Some(2),
            "current is kept when there is no next"
        );
        q.select(0, &mut rng());
        assert_eq!(q.prev(), None);
        assert_eq!(q.current(), Some(0));
    }

    #[test]
    fn repeat_all_wraps_both_ways() {
        let mut q = queue(3, Repeat::All);
        q.select(2, &mut rng());
        assert_eq!(q.next(Advance::Ended), Some(0));
        assert_eq!(q.prev(), Some(2));
    }

    #[test]
    fn repeat_one_replays_on_end_only() {
        let mut q = queue(3, Repeat::One);
        q.select(1, &mut rng());
        assert_eq!(q.next(Advance::Ended), Some(1));
        assert_eq!(q.next(Advance::Ended), Some(1));
        assert_eq!(q.next(Advance::Manual), Some(2));
        assert_eq!(q.next(Advance::Manual), Some(0), "manual next wraps");
    }

    #[test]
    fn failed_track_always_moves_on() {
        let mut q = queue(2, Repeat::One);
        q.select(0, &mut rng());
        assert_eq!(q.next(Advance::Failed), Some(1));
        assert_eq!(
            q.next(Advance::Failed),
            Some(0),
            "wraps unless repeat is off"
        );
    }

    #[test]
    fn empty_queue_has_nothing_to_play() {
        let mut q = queue(0, Repeat::All);
        assert!(q.is_empty());
        assert!(!q.select(0, &mut rng()));
        assert_eq!(q.next(Advance::Manual), None);
        assert_eq!(q.next(Advance::Ended), None);
        assert_eq!(q.prev(), None);
        q.set_shuffle(true, &mut rng());
        assert_eq!(q.current(), None);
    }

    #[test]
    fn select_out_of_range_changes_nothing() {
        let mut q = queue(3, Repeat::Off);
        q.select(1, &mut rng());
        assert!(!q.select(3, &mut rng()));
        assert_eq!(q.current(), Some(1));
    }

    #[test]
    fn shuffle_visits_every_track_once() {
        let len = 50;
        let mut q = queue(len, Repeat::Off);
        q.select(17, &mut rng());
        q.set_shuffle(true, &mut rng());
        assert_eq!(q.current(), Some(17), "current track stays first");

        let mut seen = vec![17];
        while let Some(t) = q.next(Advance::Ended) {
            seen.push(t);
        }
        assert_eq!(seen.len(), len);
        assert_eq!(seen.iter().copied().collect::<HashSet<_>>().len(), len);
        assert_ne!(seen, (0..len).collect::<Vec<_>>(), "order is shuffled");
    }

    #[test]
    fn shuffle_select_puts_track_first() {
        let mut q = queue(10, Repeat::Off);
        q.set_shuffle(true, &mut rng());
        assert_eq!(q.current(), None);
        assert!(q.select(4, &mut rng()));
        assert_eq!(q.order()[0], 4);
        assert_eq!(q.current(), Some(4));
        assert_eq!(q.prev(), None, "nothing precedes the chosen track");
    }

    #[test]
    fn shuffle_with_repeat_all_wraps_to_first() {
        let mut q = queue(5, Repeat::All);
        q.set_shuffle(true, &mut rng());
        q.select(3, &mut rng());
        for _ in 0..4 {
            q.next(Advance::Ended);
        }
        assert_eq!(q.next(Advance::Ended), Some(3));
    }

    #[test]
    fn unshuffle_restores_library_order() {
        let mut q = queue(10, Repeat::Off);
        q.select(6, &mut rng());
        assert!(q.toggle_shuffle(&mut rng()));
        q.next(Advance::Manual);
        let playing = q.current();
        assert!(!q.toggle_shuffle(&mut rng()));
        assert_eq!(q.order(), (0..10).collect::<Vec<_>>());
        assert_eq!(q.current(), playing);
    }
}
