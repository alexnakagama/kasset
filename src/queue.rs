//! Play order: which track comes next, honouring shuffle and repeat.
//!
//! Pure logic with no audio or terminal dependencies. The queue plays a list
//! of **items**, each the index of a track in the library; the same track may
//! appear more than once (a playlist with duplicates). Items are addressed by
//! their position in that list ("item positions"); the queue never looks at
//! the tracks themselves.
//!
//! The list can be edited while it plays ([`Queue::remove_item`],
//! [`Queue::move_item`], [`Queue::append_items`], [`Queue::replace_items`]).
//! Removing the item that is playing leaves the queue *detached*: the removed
//! track stays current until the queue moves on, and it moves on to the item
//! that followed it in play order, never back to the removed track.

use rand::seq::SliceRandom;
use rand::{Rng, RngExt};

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

/// The current track after its item was removed from the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Detached {
    /// Library index of the track that is still current.
    track: usize,
    /// Slot of `order` that holds the item to play next; `order.len()` when
    /// the removed item was the last one in play order.
    next_slot: usize,
}

/// The order items play in and the position of the current one.
///
/// Invariants: `order` is a permutation of `0..items.len()` (the identity
/// unless shuffle is on); `pos`, when set, is `< order.len()`; `pos` and
/// `detached` are never both set; `detached.next_slot <= order.len()`.
#[derive(Debug, Clone)]
pub struct Queue {
    /// Library indices in list order; duplicates allowed.
    items: Vec<usize>,
    /// Item positions in play order.
    order: Vec<usize>,
    /// Index into `order` of the current item.
    pos: Option<usize>,
    /// Set while the current track's item has been removed.
    detached: Option<Detached>,
    shuffle: bool,
    repeat: Repeat,
}

impl Queue {
    /// A linear queue over the library tracks `0..len` with nothing current.
    #[must_use]
    pub fn new(len: usize) -> Self {
        Self::from_items((0..len).collect())
    }

    /// A linear queue over `items` (library indices, duplicates allowed)
    /// with nothing current.
    #[must_use]
    pub fn from_items(items: Vec<usize>) -> Self {
        Self {
            order: (0..items.len()).collect(),
            items,
            pos: None,
            detached: None,
            shuffle: false,
            repeat: Repeat::Off,
        }
    }

    /// Replaces the whole list with `items` and clears the current track,
    /// keeping the shuffle and repeat modes. Unlike
    /// [`Queue::replace_items`], this is a fresh start, not an edit.
    pub fn reset(&mut self, items: Vec<usize>) {
        self.order = (0..items.len()).collect();
        self.items = items;
        self.pos = None;
        self.detached = None;
    }

    /// Number of items in the play order.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the play order holds no items.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Library index of the current track, if any. While detached this is
    /// the removed track that is still playing.
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        match self.detached {
            Some(detached) => Some(detached.track),
            None => self.current_item().map(|item| self.items[item]),
        }
    }

    /// Whether the current track's item was removed from the list while it
    /// played (the track is still current, but not part of the list).
    #[must_use]
    pub const fn is_detached(&self) -> bool {
        self.detached.is_some()
    }

    /// Item position of the current track, or `None` when nothing is current
    /// or the current track's item was removed.
    #[must_use]
    pub fn current_item(&self) -> Option<usize> {
        self.pos.map(|p| self.order[p])
    }

    /// The list as library indices, in list (not play) order.
    #[must_use]
    pub fn items(&self) -> &[usize] {
        &self.items
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

    /// The play order as item positions (library indices for a queue made
    /// with [`Queue::new`]).
    #[cfg(test)]
    #[must_use]
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// Makes the item at position `item` current. With shuffle on, the order
    /// is reshuffled with `item` first so every other item still plays once
    /// before any repeats. Returns `false` (and changes nothing) when `item`
    /// is out of range.
    pub fn select<R: Rng + ?Sized>(&mut self, item: usize, rng: &mut R) -> bool {
        if item >= self.items.len() {
            return false;
        }
        self.detached = None;
        if self.shuffle {
            self.reshuffle(Some(item), rng);
        } else {
            self.pos = Some(item);
        }
        true
    }

    /// Advances the play order and returns the new current track, or `None`
    /// when there is no next track (the current track is left unchanged).
    ///
    /// With nothing current, starts from the first item in the order. While
    /// detached, every cause (even [`Advance::Ended`] with repeat one) moves
    /// to the item that followed the removed one, or behaves as at the end of
    /// the order when there is none.
    pub fn next(&mut self, cause: Advance) -> Option<usize> {
        let len = self.order.len();
        let wraps = match cause {
            Advance::Ended => self.repeat == Repeat::All,
            Advance::Manual | Advance::Failed => self.repeat != Repeat::Off,
        };
        if let Some(detached) = self.detached {
            let slot = if detached.next_slot < len {
                detached.next_slot
            } else if self.repeat != Repeat::Off && len > 0 {
                // The removed track cannot be replayed, so repeat one wraps
                // like repeat all.
                0
            } else {
                return None;
            };
            self.detached = None;
            self.pos = Some(slot);
            return self.current();
        }
        let Some(pos) = self.pos else {
            self.pos = (len > 0).then_some(0);
            return self.current();
        };
        if cause == Advance::Ended && self.repeat == Repeat::One {
            return self.current();
        }
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
    /// current). Any repeat mode other than off wraps to the last item. While
    /// detached, steps to the item that preceded the removed one.
    pub fn prev(&mut self) -> Option<usize> {
        let len = self.order.len();
        let target = if let Some(detached) = self.detached {
            detached.next_slot.checked_sub(1)
        } else {
            self.pos?.checked_sub(1)
        };
        let slot = match target {
            Some(slot) => slot,
            None if self.repeat != Repeat::Off && len > 0 => len - 1,
            None => return None,
        };
        self.detached = None;
        self.pos = Some(slot);
        self.current()
    }

    /// Turns shuffle on or off, keeping the current track current.
    ///
    /// Turning it on builds a fresh random order (Fisher–Yates) with the
    /// current item first; turning it off restores list order. While
    /// detached, the next item becomes the first of the new order.
    pub fn set_shuffle<R: Rng + ?Sized>(&mut self, on: bool, rng: &mut R) {
        let current = self.current_item();
        self.shuffle = on;
        if on {
            self.reshuffle(current, rng);
        } else {
            self.order = (0..self.items.len()).collect();
            self.pos = current;
        }
        if let Some(detached) = &mut self.detached {
            detached.next_slot = 0;
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

    /// Sets the repeat mode; the order and the current item are unchanged.
    pub const fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    /// Random permutation of all items with `first` (if any) moved to the
    /// front, which becomes current.
    fn reshuffle<R: Rng + ?Sized>(&mut self, first: Option<usize>, rng: &mut R) {
        // `SliceRandom::shuffle` is a Fisher–Yates shuffle. Swapping one element
        // to the front afterwards leaves the rest uniformly permuted.
        self.order.shuffle(rng);
        self.pos = first.and_then(|item| {
            let at = self.order.iter().position(|&i| i == item)?;
            self.order.swap(0, at);
            Some(0)
        });
    }
}

/// Editing the list while it plays.
impl Queue {
    /// Removes the item at position `item`. If it is the current one, the
    /// queue becomes detached: its track stays current and the queue moves
    /// on to the item that followed it in play order. Returns `false` (and
    /// changes nothing) when `item` is out of range.
    pub fn remove_item(&mut self, item: usize) -> bool {
        if item >= self.items.len() {
            return false;
        }
        let track = self.items.remove(item);
        let Some(slot) = self.order.iter().position(|&i| i == item) else {
            // Unreachable while `order` is a permutation of the items; keep
            // the invariants anyway rather than index out of bounds later.
            self.order = (0..self.items.len()).collect();
            self.pos = self.pos.filter(|&p| p < self.order.len());
            return true;
        };
        self.order.remove(slot);
        for i in &mut self.order {
            if *i > item {
                *i -= 1;
            }
        }
        match self.pos {
            Some(pos) if pos == slot => {
                self.pos = None;
                self.detached = Some(Detached {
                    track,
                    next_slot: slot,
                });
            }
            Some(pos) if pos > slot => self.pos = Some(pos - 1),
            _ => {}
        }
        if let Some(detached) = &mut self.detached
            && detached.next_slot > slot
        {
            detached.next_slot -= 1;
        }
        true
    }

    /// Moves the item at position `from` to position `to` in the list (the
    /// positions of the items in between shift by one). The current item
    /// stays current. In linear mode play order follows the new list order;
    /// with shuffle on the shuffled sequence is unchanged. Returns `false`
    /// (and changes nothing) when either position is out of range.
    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        let len = self.items.len();
        if from >= len || to >= len {
            return false;
        }
        if from == to {
            return true;
        }
        let track = self.items.remove(from);
        self.items.insert(to, track);
        if self.shuffle {
            // Same slots, relabelled items: the sequence does not change.
            for i in &mut self.order {
                *i = moved(*i, from, to);
            }
        } else {
            // `order` stays the identity; the current item is followed.
            self.pos = self.pos.map(|p| moved(p, from, to));
            if let Some(detached) = &mut self.detached {
                detached.next_slot = moved_gap(detached.next_slot, from, to);
            }
        }
        true
    }

    /// Appends library tracks to the list. In linear mode they play after the
    /// current last item; with shuffle on each lands at a uniformly random
    /// slot among those not yet played.
    pub fn append_items<R: Rng + ?Sized>(&mut self, tracks: &[usize], rng: &mut R) {
        let first_new = self.items.len();
        self.items.extend_from_slice(tracks);
        let new = first_new..self.items.len();
        if !self.shuffle {
            self.order.extend(new);
            return;
        }
        // The slots still to play: after the current one, or from the next
        // one while detached.
        let start = match (self.pos, self.detached) {
            (Some(pos), _) => pos + 1,
            (None, Some(detached)) => detached.next_slot,
            (None, None) => 0,
        };
        // Inserting each new item at a uniform random slot of the tail, one
        // after another, gives a uniformly random interleaving of the tail
        // with a uniformly shuffled run of new items. Building that directly
        // is linear instead of quadratic in the number of items.
        let tail = self.order.split_off(start);
        let mut added: Vec<usize> = new.collect();
        added.shuffle(rng);
        let (mut old_left, mut new_left) = (tail.len(), added.len());
        let (mut old, mut fresh) = (tail.into_iter(), added.into_iter());
        self.order.reserve(old_left + new_left);
        while old_left + new_left > 0 {
            let take_new = rng.random_range(0..old_left + new_left) < new_left;
            let next = if take_new {
                new_left -= 1;
                fresh.next()
            } else {
                old_left -= 1;
                old.next()
            };
            self.order.extend(next);
        }
    }

    /// Replaces the list after it was reloaded. The current track stays
    /// current: at the same item position if that still holds the same
    /// track, else at its first occurrence; if it is no longer in the list,
    /// the queue becomes detached so it finishes and the queue moves on.
    /// With shuffle on, the order is reshuffled with the current item first.
    pub fn replace_items<R: Rng + ?Sized>(&mut self, tracks: Vec<usize>, rng: &mut R) {
        let track = self.current();
        let old_item = self.current_item();
        let old_slot = self
            .pos
            .or_else(|| self.detached.map(|detached| detached.next_slot));
        self.items = tracks;
        self.order = (0..self.items.len()).collect();
        self.pos = None;
        self.detached = None;

        let Some(track) = track else {
            if self.shuffle {
                self.reshuffle(None, rng);
            }
            return;
        };
        let item = old_item
            .filter(|&i| self.items.get(i) == Some(&track))
            .or_else(|| self.items.iter().position(|&t| t == track));
        if let Some(item) = item {
            self.select(item, rng);
            return;
        }
        let next_slot = if self.shuffle {
            self.reshuffle(None, rng);
            0
        } else {
            old_slot.unwrap_or(0).min(self.order.len())
        };
        self.detached = Some(Detached { track, next_slot });
    }
}

/// Where the item at position `p` ends up when the item at `from` is moved to
/// `to`.
const fn moved(p: usize, from: usize, to: usize) -> usize {
    if p == from {
        to
    } else if from < to && p > from && p <= to {
        p - 1
    } else if to < from && p >= to && p < from {
        p + 1
    } else {
        p
    }
}

/// Where a gap before position `gap` (between items `gap - 1` and `gap`)
/// ends up when the item at `from` is moved to `to`. An item only changes
/// side when it moves past another item on the other side: one that lands
/// right at the gap stays on the side it came from, so a played item is not
/// replayed and an unplayed one is not skipped.
const fn moved_gap(gap: usize, from: usize, to: usize) -> usize {
    // Take the item out, then put it back in front of the item that is at
    // position `to` once it was taken out.
    let played = from < gap;
    let gap = if played { gap - 1 } else { gap };
    if to < gap || (to == gap && played) {
        gap + 1
    } else {
        gap
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

    // ---- Item lists (playlists) ----

    /// Library indices of the items in play order, from the current one on.
    fn play_all(q: &mut Queue) -> Vec<usize> {
        let mut seen: Vec<usize> = q.current().into_iter().collect();
        while let Some(t) = q.next(Advance::Ended) {
            seen.push(t);
            assert!(seen.len() <= 1000, "runaway play order");
        }
        seen
    }

    fn list(items: &[usize], repeat: Repeat) -> Queue {
        let mut q = Queue::from_items(items.to_vec());
        q.repeat = repeat;
        q
    }

    /// Invariants that every operation must keep.
    fn check(q: &Queue) {
        let mut sorted = q.order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..q.items.len()).collect::<Vec<_>>(), "{q:?}");
        if !q.shuffle {
            assert_eq!(q.order, sorted, "linear order is the identity");
        }
        assert!(q.pos.is_none_or(|p| p < q.order.len()), "{q:?}");
        assert!(!(q.pos.is_some() && q.detached.is_some()), "{q:?}");
        assert!(
            q.detached.is_none_or(|d| d.next_slot <= q.order.len()),
            "{q:?}"
        );
    }

    #[test]
    fn new_is_the_identity_list() {
        let q = Queue::new(4);
        assert_eq!(q.items(), [0, 1, 2, 3]);
        assert_eq!(q.current_item(), None);
        check(&q);
    }

    #[test]
    fn duplicates_play_as_separate_items() {
        // Library tracks 7, 3, 7, 5: track 7 appears twice.
        let mut q = list(&[7, 3, 7, 5], Repeat::Off);
        assert!(q.select(0, &mut rng()));
        assert_eq!(q.current(), Some(7));
        assert_eq!(play_all(&mut q), [7, 3, 7, 5]);
        assert_eq!(q.current_item(), Some(3));
        assert!(!q.select(4, &mut rng()), "positions, not library indices");
    }

    #[test]
    fn duplicates_under_shuffle_each_play_once() {
        let items = [7, 3, 7, 5, 3, 3];
        let mut q = list(&items, Repeat::Off);
        q.select(2, &mut rng());
        q.set_shuffle(true, &mut rng());
        assert_eq!(q.current_item(), Some(2), "same item, not just same track");
        let mut played = play_all(&mut q);
        played.sort_unstable();
        let mut expected = items.to_vec();
        expected.sort_unstable();
        assert_eq!(played, expected);
        check(&q);
    }

    #[test]
    fn duplicates_with_repeat_all_and_one() {
        let mut q = list(&[4, 4, 9], Repeat::All);
        q.select(2, &mut rng());
        assert_eq!(q.next(Advance::Ended), Some(4));
        assert_eq!(q.current_item(), Some(0));
        assert_eq!(q.next(Advance::Ended), Some(4));
        assert_eq!(q.current_item(), Some(1));
        q.repeat = Repeat::One;
        assert_eq!(q.next(Advance::Ended), Some(4));
        assert_eq!(q.current_item(), Some(1), "repeat one keeps the item");
        assert_eq!(q.next(Advance::Manual), Some(9));

        q.set_shuffle(true, &mut rng());
        q.repeat = Repeat::All;
        let start = q.current_item();
        let mut seen = HashSet::new();
        for _ in 0..3 {
            seen.insert(q.current_item());
            q.next(Advance::Ended);
        }
        assert_eq!(seen.len(), 3, "every item once per round");
        assert_eq!(q.current_item(), start, "then wraps");
    }

    #[test]
    fn reset_keeps_modes() {
        let mut q = list(&[1, 2, 3], Repeat::All);
        q.set_shuffle(true, &mut rng());
        q.select(1, &mut rng());
        q.reset(vec![8, 9]);
        assert_eq!(q.current(), None);
        assert!(q.shuffle());
        assert_eq!(q.repeat(), Repeat::All);
        assert_eq!(q.items(), [8, 9]);
        check(&q);
    }

    #[test]
    fn set_repeat_keeps_order_and_current() {
        let mut q = list(&[1, 2, 3], Repeat::Off);
        q.set_shuffle(true, &mut rng());
        q.select(2, &mut rng());
        let order = q.order().to_vec();
        for repeat in [Repeat::One, Repeat::All, Repeat::Off] {
            q.set_repeat(repeat);
            assert_eq!(q.repeat(), repeat);
            assert_eq!(q.order(), order);
            assert_eq!(q.current_item(), Some(2));
            check(&q);
        }
    }

    // A, B, C, D are library tracks 10, 11, 12, 13.
    const A: usize = 10;
    const B: usize = 11;
    const C: usize = 12;
    const D: usize = 13;

    #[test]
    fn removing_playing_entry_continues_with_follower() {
        let mut q = list(&[A, B, C], Repeat::Off);
        q.select(1, &mut rng());
        assert!(q.remove_item(1));
        check(&q);
        assert_eq!(q.items(), [A, C]);
        assert_eq!(q.current(), Some(B), "B keeps playing");
        assert_eq!(q.current_item(), None);
        assert_eq!(q.next(Advance::Ended), Some(C));
        assert_eq!(q.current_item(), Some(1));
        assert_eq!(q.next(Advance::Ended), None);
    }

    #[test]
    fn removed_entry_is_not_replayed_with_repeat_one() {
        let mut q = list(&[A, B, C], Repeat::One);
        q.select(1, &mut rng());
        q.remove_item(1);
        assert_eq!(q.next(Advance::Ended), Some(C));
        assert_eq!(q.next(Advance::Ended), Some(C), "then C repeats");
    }

    #[test]
    fn removing_last_playing_entry() {
        let mut q = list(&[A, B, C], Repeat::Off);
        q.select(2, &mut rng());
        q.remove_item(2);
        assert_eq!(q.next(Advance::Ended), None, "repeat off stops");
        assert_eq!(
            q.current(),
            Some(C),
            "current is kept when there is no next"
        );

        let mut q = list(&[A, B, C], Repeat::All);
        q.select(2, &mut rng());
        q.remove_item(2);
        assert_eq!(q.next(Advance::Ended), Some(A), "repeat all wraps");

        let mut q = list(&[A, B, C], Repeat::One);
        q.select(2, &mut rng());
        q.remove_item(2);
        assert_eq!(q.next(Advance::Ended), Some(A), "nothing to replay: wraps");
    }

    #[test]
    fn prev_after_removal_goes_to_preceding_entry() {
        let mut q = list(&[A, B, C], Repeat::Off);
        q.select(1, &mut rng());
        q.remove_item(1);
        assert_eq!(q.prev(), Some(A));
        assert_eq!(q.current_item(), Some(0));

        let mut q = list(&[A, B, C], Repeat::Off);
        q.select(0, &mut rng());
        q.remove_item(0);
        assert_eq!(q.prev(), None, "nothing precedes with repeat off");
        assert_eq!(q.current(), Some(A));
        q.repeat = Repeat::All;
        assert_eq!(q.prev(), Some(C), "repeat wraps to the last entry");
    }

    /// A queue over A, B, C, D in shuffled play order C, B, D, A with B current.
    fn shuffled_cbda() -> Queue {
        let mut q = list(&[A, B, C, D], Repeat::Off);
        q.shuffle = true;
        q.order = vec![2, 1, 3, 0];
        q.pos = Some(1);
        check(&q);
        assert_eq!(q.current(), Some(B));
        q
    }

    #[test]
    fn removal_in_shuffle_follows_play_order() {
        let mut q = shuffled_cbda();
        q.remove_item(1);
        check(&q);
        assert_eq!(q.items(), [A, C, D]);
        assert_eq!(q.current(), Some(B));
        assert_eq!(q.next(Advance::Ended), Some(D), "not C, the list successor");
        assert_eq!(q.next(Advance::Ended), Some(A));

        let mut q = shuffled_cbda();
        q.remove_item(1);
        assert_eq!(q.prev(), Some(C), "not A, the list predecessor");
    }

    #[test]
    fn shuffle_toggle_while_detached() {
        let mut q = list(&[A, B, C, D], Repeat::Off);
        q.select(1, &mut rng());
        q.remove_item(1);
        q.set_shuffle(true, &mut rng());
        check(&q);
        assert_eq!(q.current(), Some(B), "B keeps playing");
        let mut rest = play_all(&mut q);
        assert_eq!(rest.remove(0), B);
        rest.sort_unstable();
        assert_eq!(rest, [A, C, D], "the whole new order plays");

        let mut q = shuffled_cbda();
        q.remove_item(1);
        q.set_shuffle(false, &mut rng());
        check(&q);
        assert_eq!(q.current(), Some(B));
        assert_eq!(play_all(&mut q), [B, A, C, D], "from the start of the list");
    }

    #[test]
    fn edits_while_detached_adjust_next_slot() {
        let mut q = list(&[A, B, C, D], Repeat::Off);
        q.select(1, &mut rng());
        q.remove_item(1); // A | C D
        q.remove_item(0); // | C D
        check(&q);
        assert_eq!(q.next(Advance::Ended), Some(C));

        let mut q = list(&[A, B, C, D], Repeat::Off);
        q.select(1, &mut rng());
        q.remove_item(1); // A | C D
        q.remove_item(1); // A | D
        assert_eq!(q.next(Advance::Ended), Some(D), "the next one moves up");
    }

    #[test]
    fn removing_other_entries_keeps_current() {
        let mut q = list(&[A, B, C, D], Repeat::Off);
        q.select(2, &mut rng());
        q.remove_item(0);
        check(&q);
        assert_eq!(q.current(), Some(C));
        assert_eq!(q.current_item(), Some(1));
        q.remove_item(2);
        assert_eq!(play_all(&mut q), [C]);
        assert!(!q.remove_item(5), "out of range");
    }

    #[test]
    fn removing_from_shuffle_keeps_sequence() {
        let mut q = shuffled_cbda();
        q.remove_item(3); // D
        check(&q);
        assert_eq!(q.current(), Some(B));
        assert_eq!(play_all(&mut q), [B, A]);
    }

    #[test]
    fn moving_an_entry_reorders_linear_play() {
        // A, B, C with A playing; move C above B.
        let mut q = list(&[A, B, C], Repeat::Off);
        q.select(0, &mut rng());
        assert!(q.move_item(2, 1));
        check(&q);
        assert_eq!(q.items(), [A, C, B]);
        assert_eq!(play_all(&mut q), [A, C, B]);
    }

    #[test]
    fn moving_the_current_entry_keeps_it_current() {
        let mut q = list(&[A, B, C], Repeat::Off);
        q.select(0, &mut rng());
        q.move_item(0, 1);
        check(&q);
        assert_eq!(q.current(), Some(A));
        assert_eq!(q.current_item(), Some(1));
        assert_eq!(play_all(&mut q), [A, C]);
        assert!(!q.move_item(0, 3), "out of range");
        assert!(q.move_item(1, 1), "no-op move");
    }

    #[test]
    fn moving_in_shuffle_keeps_sequence() {
        let mut q = shuffled_cbda();
        q.move_item(3, 0); // list D, A, B, C
        check(&q);
        assert_eq!(q.items(), [D, A, B, C]);
        assert_eq!(q.current(), Some(B));
        assert_eq!(play_all(&mut q), [B, D, A]);
        let mut q = shuffled_cbda();
        q.move_item(1, 3); // the current one
        assert_eq!(q.current(), Some(B));
        assert_eq!(play_all(&mut q), [B, D, A]);
    }

    #[test]
    fn moving_while_detached_in_linear_mode() {
        // A | C D after removing playing B.
        let detached = || {
            let mut q = list(&[A, B, C, D], Repeat::Off);
            q.select(1, &mut rng());
            q.remove_item(1);
            q
        };
        let mut q = detached();
        q.move_item(1, 2); // A | D C
        check(&q);
        assert_eq!(play_all(&mut q), [B, D, C]);

        let mut q = detached();
        q.move_item(1, 0); // C A | D: C moved above the gap
        assert_eq!(play_all(&mut q), [B, D]);

        let mut q = detached();
        q.move_item(0, 1); // | C A D: A moved down past the gap plays again
        assert_eq!(play_all(&mut q), [B, C, A, D]);

        let mut q = detached();
        q.move_item(2, 0); // D A | C
        assert_eq!(play_all(&mut q), [B, C]);
    }

    #[test]
    fn moving_to_the_gap_keeps_the_items_side() {
        // A B | D after removing playing C from A, B, C, D.
        let detached = || {
            let mut q = list(&[A, B, C, D], Repeat::Off);
            q.select(2, &mut rng());
            q.remove_item(2);
            q
        };
        let mut q = detached();
        q.move_item(0, 1); // B A | D: A stays played
        check(&q);
        assert_eq!(play_all(&mut q), [C, D]);

        let mut q = detached();
        q.move_item(1, 2); // A | D B: B moved past D plays again
        assert_eq!(play_all(&mut q), [C, D, B]);

        let mut q = detached();
        q.append_items(&[20], &mut rng()); // A B | D 20
        q.move_item(3, 2); // A B | 20 D: unplayed 20 is not skipped
        assert_eq!(play_all(&mut q), [C, 20, D]);
    }

    #[test]
    fn appended_items_play_in_linear_mode() {
        let mut q = list(&[A, B], Repeat::Off);
        q.select(1, &mut rng());
        q.append_items(&[D, D], &mut rng());
        check(&q);
        assert_eq!(q.items(), [A, B, D, D]);
        assert_eq!(play_all(&mut q), [B, D, D]);

        let mut q = list(&[A, B], Repeat::Off);
        q.select(1, &mut rng());
        q.remove_item(1);
        q.append_items(&[C], &mut rng());
        assert_eq!(play_all(&mut q), [B, C], "after a removed last entry too");
    }

    #[test]
    fn appended_items_land_after_current_in_shuffle() {
        for seed in 0..50 {
            let mut r = StdRng::seed_from_u64(seed);
            let mut q = shuffled_cbda();
            q.next(Advance::Manual); // D current: C, B already played
            q.append_items(&[20, 21, 22], &mut r);
            check(&q);
            assert_eq!(&q.order[..3], [2, 1, 3], "played slots untouched");
            assert_eq!(q.current(), Some(D));
            let mut rest = play_all(&mut q);
            assert_eq!(rest.remove(0), D);
            rest.sort_unstable();
            assert_eq!(rest, [A, 20, 21, 22]);
        }
    }

    #[test]
    fn append_in_shuffle_is_spread_over_unplayed_slots() {
        // One new item after the current one with three unplayed: each of
        // the four possible slots must come up.
        let mut slots = HashSet::new();
        for seed in 0..200 {
            let mut r = StdRng::seed_from_u64(seed);
            let mut q = list(&[0, 1, 2, 3, 4], Repeat::Off);
            q.shuffle = true;
            q.pos = Some(1);
            q.append_items(&[9], &mut r);
            slots.insert(q.order.iter().position(|&i| i == 5));
        }
        assert_eq!(slots, (2..=5).map(Some).collect());
    }

    #[test]
    fn append_while_detached_in_shuffle_stays_unplayed() {
        for seed in 0..50 {
            let mut r = StdRng::seed_from_u64(seed);
            let mut q = shuffled_cbda();
            q.remove_item(1); // C | D A
            q.append_items(&[20], &mut r);
            check(&q);
            assert_eq!(q.order[0], 1, "C (now item 1) stays played");
            let mut rest = play_all(&mut q);
            assert_eq!(rest.remove(0), B);
            rest.sort_unstable();
            assert_eq!(rest, [A, D, 20]);
        }
    }

    #[test]
    fn reload_keeps_playing_track_and_follows_new_order() {
        // Running plays B; the file becomes A, B, X.
        let x = 30;
        let mut q = list(&[A, B], Repeat::Off);
        q.select(1, &mut rng());
        q.replace_items(vec![A, B, x], &mut rng());
        check(&q);
        assert_eq!(q.current_item(), Some(1));
        assert_eq!(play_all(&mut q), [B, x]);
    }

    #[test]
    fn reload_prefers_same_position_then_first_occurrence() {
        let mut q = list(&[A, B, A], Repeat::Off);
        q.select(2, &mut rng());
        q.replace_items(vec![C, B, A], &mut rng());
        assert_eq!(q.current_item(), Some(2), "same position, same track");

        let mut q = list(&[A, B, A], Repeat::Off);
        q.select(2, &mut rng());
        q.replace_items(vec![A, B], &mut rng());
        assert_eq!(q.current_item(), Some(0), "first occurrence");
    }

    #[test]
    fn reload_without_playing_track_detaches() {
        let mut q = list(&[A, B, C], Repeat::Off);
        q.select(1, &mut rng());
        q.replace_items(vec![A, C, D], &mut rng());
        check(&q);
        assert_eq!(q.current(), Some(B));
        assert_eq!(play_all(&mut q), [B, C, D]);

        let mut q = list(&[A, B, C], Repeat::Off);
        q.select(2, &mut rng());
        q.replace_items(vec![A], &mut rng());
        check(&q);
        assert_eq!(play_all(&mut q), [C], "position clamped to the end");

        let mut q = list(&[A, B], Repeat::Off);
        q.replace_items(vec![C], &mut rng());
        assert_eq!(q.current(), None, "nothing current stays so");
    }

    #[test]
    fn reload_in_shuffle_puts_current_first() {
        let mut q = shuffled_cbda();
        q.replace_items(vec![D, C, B, A, 40], &mut rng());
        check(&q);
        assert_eq!(q.current(), Some(B));
        assert_eq!(q.order[0], 2);
        let mut rest = play_all(&mut q);
        assert_eq!(rest.remove(0), B);
        rest.sort_unstable();
        assert_eq!(rest, [A, C, D, 40]);

        let mut q = shuffled_cbda();
        q.replace_items(vec![A, C], &mut rng());
        check(&q);
        assert_eq!(q.current(), Some(B), "detached");
        assert_eq!(play_all(&mut q).len(), 3, "then the whole new list");
    }

    #[test]
    fn reload_while_detached_reattaches_if_track_returns() {
        let mut q = list(&[A, B, C], Repeat::Off);
        q.select(1, &mut rng());
        q.remove_item(1);
        q.replace_items(vec![A, B, C], &mut rng());
        assert_eq!(q.current_item(), Some(1));
        assert_eq!(play_all(&mut q), [B, C]);
    }

    #[test]
    fn random_edit_sequences_keep_invariants() {
        let mut r = StdRng::seed_from_u64(7);
        for _ in 0..200 {
            let mut q = list(&[0, 1, 2, 3, 4, 5], Repeat::All);
            for _ in 0..40 {
                let len = q.len();
                match r.random_range(0..9) {
                    0 if len > 0 => {
                        q.select(r.random_range(0..len), &mut r);
                    }
                    1 if len > 0 => {
                        q.remove_item(r.random_range(0..len));
                    }
                    2 if len > 0 => {
                        q.move_item(r.random_range(0..len), r.random_range(0..len));
                    }
                    3 => q.append_items(&[r.random_range(0..9)], &mut r),
                    4 => {
                        q.toggle_shuffle(&mut r);
                    }
                    5 => {
                        q.next(Advance::Failed);
                    }
                    6 => {
                        q.prev();
                    }
                    7 => {
                        let n = r.random_range(0..6);
                        q.replace_items((0..n).map(|_| r.random_range(0..9)).collect(), &mut r);
                    }
                    _ => {
                        q.next(Advance::Ended);
                    }
                }
                check(&q);
                if let Some(item) = q.current_item() {
                    assert_eq!(q.current(), Some(q.items()[item]));
                }
            }
        }
    }
}
