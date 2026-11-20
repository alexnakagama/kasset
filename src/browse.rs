//! Grouping the library into artists and albums for browsing (D1 of
//! add-artist-album-browsing).
//!
//! Pure, no I/O: [`Catalog::build`] groups the scanned tracks once per run.
//! Every index it hands out (artist, album, library track) is only valid
//! within that run, for the track list it was built from: indices are never
//! persisted (the state file stores names, D8).
//!
//! Invariants, for a catalog built from `tracks`:
//!
//! - Every track belongs to exactly one album, keyed by (album artist key,
//!   album title key). A track's album artist is its non-empty
//!   [`Track::album_artist`], else its [`Track::artist`]. Keys are
//!   `to_lowercase()` of the text, so case differences merge; sorting is by
//!   those keys (code-point order), not by locale collation.
//! - Display text (artist name, album title) is the spelling of the first
//!   track in library order. It comes from [`Track`] fields, which the
//!   library has already sanitised; this module adds nothing of its own.
//! - Albums are sorted by (artist key, title key), artists by key, and an
//!   artist's albums by title key. Each artist's albums are a contiguous run
//!   of the album list. Keys are unique at their level, so the order is total.
//! - An album's tracks are in album order: disc (missing = 1), number
//!   (unnumbered after numbered), title lower-cased, path, then library index.
//! - Only album artists are listed: an artist who is only a track artist on
//!   someone else's album has no entry.
//! - Search keys are lower-cased: an artist's name, an album's title and
//!   artist joined by `\n` (so a query never matches across the two).

use std::collections::BTreeMap;

use crate::library::Track;

/// One album: tracks sharing an album artist and album title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    title: String,
    artist: usize,
    tracks: Vec<usize>,
    search_key: String,
}

impl Album {
    /// The album title, as spelled by its first track in library order.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Index of the album artist in [`Catalog::artists`].
    pub const fn artist(&self) -> usize {
        self.artist
    }

    /// Library track indices, in album order. Never empty.
    pub fn tracks(&self) -> &[usize] {
        &self.tracks
    }

    /// Lower-cased title and album artist, joined by `\n`, for filtering.
    pub fn search_key(&self) -> &str {
        &self.search_key
    }
}

/// One album artist and their albums.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artist {
    name: String,
    albums: Vec<usize>,
    track_count: usize,
    search_key: String,
}

impl Artist {
    /// The artist name, as spelled by their first track in library order.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Indices into [`Catalog::albums`], sorted by title key. Never empty.
    #[must_use]
    pub fn albums(&self) -> &[usize] {
        &self.albums
    }

    /// Number of tracks over all of this artist's albums.
    pub const fn track_count(&self) -> usize {
        self.track_count
    }

    /// Lower-cased name, for filtering.
    pub fn search_key(&self) -> &str {
        &self.search_key
    }
}

/// The library grouped into album artists and albums. See the module docs
/// for its invariants.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    artists: Vec<Artist>,
    albums: Vec<Album>,
}

/// Albums of one artist while building: title key → (title, tracks).
type AlbumsByTitle = BTreeMap<String, (String, Vec<usize>)>;

impl Catalog {
    /// Groups `tracks` (in library order) into albums and artists.
    ///
    /// One pass over the tracks plus one sort per album, so `O(n log n)`.
    #[must_use]
    pub fn build(tracks: &[Track]) -> Self {
        // artist key → (name, albums). BTreeMaps give the sorted orders.
        let mut grouped: BTreeMap<String, (String, AlbumsByTitle)> = BTreeMap::new();
        for (index, track) in tracks.iter().enumerate() {
            let artist = album_artist_of(track);
            let (_, albums) = grouped
                .entry(artist.to_lowercase())
                .or_insert_with(|| (artist.to_owned(), BTreeMap::new()));
            albums
                .entry(track.album.to_lowercase())
                .or_insert_with(|| (track.album.clone(), Vec::new()))
                .1
                .push(index);
        }

        let mut catalog = Self::default();
        for (artist_key, (name, albums_by_title)) in grouped {
            let artist = catalog.artists.len();
            let mut albums = Vec::with_capacity(albums_by_title.len());
            let mut track_count = 0usize;
            for (title_key, (title, mut members)) in albums_by_title {
                sort_album_tracks(&mut members, tracks);
                track_count = track_count.saturating_add(members.len());
                albums.push(catalog.albums.len());
                catalog.albums.push(Album {
                    title,
                    artist,
                    tracks: members,
                    // Track text is sanitised (no control characters), so
                    // `\n` never occurs in either part.
                    search_key: format!("{title_key}\n{artist_key}"),
                });
            }
            catalog.artists.push(Artist {
                name,
                albums,
                track_count,
                search_key: artist_key,
            });
        }
        catalog
    }

    /// Every album artist, sorted case-insensitively.
    #[must_use]
    pub fn artists(&self) -> &[Artist] {
        &self.artists
    }

    /// Every album, sorted case-insensitively by album artist, then title.
    pub fn albums(&self) -> &[Album] {
        &self.albums
    }

    /// The artist at `index`, if any.
    #[must_use]
    pub fn artist(&self, index: usize) -> Option<&Artist> {
        self.artists.get(index)
    }

    /// The album at `index`, if any.
    #[must_use]
    pub fn album(&self, index: usize) -> Option<&Album> {
        self.albums.get(index)
    }

    /// The artist's tracks: their albums' tracks concatenated, album by album
    /// in the artist's album order. Empty for an unknown `artist`. Built on
    /// each call (when an artist is opened or played), not stored.
    #[must_use]
    pub fn artist_tracks(&self, artist: usize) -> Vec<usize> {
        let Some(artist) = self.artist(artist) else {
            return Vec::new();
        };
        let mut tracks = Vec::with_capacity(artist.track_count);
        for album in artist.albums.iter().filter_map(|&b| self.album(b)) {
            tracks.extend_from_slice(&album.tracks);
        }
        tracks
    }
}

/// The name a track's album is grouped under: its album-artist tag when
/// present and non-empty, else its track artist.
fn album_artist_of(track: &Track) -> &str {
    track
        .album_artist
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(&track.artist)
}

/// Sorts library indices (all valid for `tracks`) into album order.
fn sort_album_tracks(members: &mut [usize], tracks: &[Track]) {
    members.sort_by_cached_key(|&index| {
        let track = tracks.get(index);
        (
            track.and_then(|t| t.disc).unwrap_or(1),
            track.and_then(|t| t.number).map_or((1, 0), |n| (0, n)),
            track.map(|t| t.title.to_lowercase()),
            track.map(|t| t.path.as_path()),
            index,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::track;

    fn with(
        title: &str,
        artist: &str,
        album: &str,
        album_artist: Option<&str>,
        disc: Option<u32>,
        number: Option<u32>,
    ) -> Track {
        Track {
            album_artist: album_artist.map(str::to_owned),
            disc,
            number,
            ..track(title, artist, album)
        }
    }

    fn names(catalog: &Catalog) -> Vec<&str> {
        catalog.artists().iter().map(Artist::name).collect()
    }

    fn album_list(catalog: &Catalog) -> Vec<(&str, &str, usize)> {
        catalog
            .albums()
            .iter()
            .map(|b| {
                let artist = catalog.artist(b.artist()).map_or("?", Artist::name);
                (b.title(), artist, b.tracks().len())
            })
            .collect()
    }

    fn titles<'a>(tracks: &'a [Track], indices: &[usize]) -> Vec<&'a str> {
        indices
            .iter()
            .filter_map(|&i| tracks.get(i).map(|t| t.title.as_str()))
            .collect()
    }

    /// Every track is in exactly one album, every album in exactly one
    /// artist's list (and only that artist's), and the counts add up.
    fn assert_partition(catalog: &Catalog, tracks: &[Track]) {
        let mut seen = vec![0usize; tracks.len()];
        for album in catalog.albums() {
            assert_ne!(album.tracks().len(), 0);
            for &t in album.tracks() {
                if let Some(count) = seen.get_mut(t) {
                    *count += 1;
                } else {
                    panic!("track index {t} out of range");
                }
            }
        }
        assert!(seen.iter().all(|&n| n == 1), "{seen:?}");
        let mut owners = vec![0usize; catalog.albums().len()];
        for (a, artist) in catalog.artists().iter().enumerate() {
            assert_ne!(artist.albums().len(), 0);
            let sum: usize = artist
                .albums()
                .iter()
                .filter_map(|&b| catalog.album(b))
                .inspect(|b| assert_eq!(b.artist(), a))
                .map(|b| b.tracks().len())
                .sum();
            assert_eq!(artist.track_count(), sum);
            for &b in artist.albums() {
                if let Some(count) = owners.get_mut(b) {
                    *count += 1;
                }
            }
        }
        assert!(owners.iter().all(|&n| n == 1), "{owners:?}");
    }

    #[test]
    fn empty_library_gives_an_empty_catalog() {
        let catalog = Catalog::build(&[]);
        assert_eq!(catalog, Catalog::default());
        assert_eq!(catalog.artist(0), None);
        assert_eq!(catalog.album(0), None);
        assert_eq!(catalog.artist_tracks(0), Vec::<usize>::new());
    }

    /// Spec *Compilation kept together*.
    #[test]
    fn compilation_kept_together() {
        let tracks: Vec<Track> = ["A", "B", "C"]
            .iter()
            .map(|&artist| with(artist, artist, "Hits", Some("Various Artists"), None, None))
            .collect();
        let catalog = Catalog::build(&tracks);
        assert_eq!(album_list(&catalog), [("Hits", "Various Artists", 3)]);
        assert_eq!(names(&catalog), ["Various Artists"]);
        let artists: Vec<&str> = tracks.iter().map(|t| t.artist.as_str()).collect();
        assert_eq!(artists, ["A", "B", "C"], "each track keeps its own artist");
        assert_partition(&catalog, &tracks);
    }

    /// Spec *Same title, different artists*.
    #[test]
    fn same_title_different_artists() {
        let tracks = [
            track("x", "Band", "Greatest Hits"),
            track("y", "Other", "Greatest Hits"),
        ];
        let catalog = Catalog::build(&tracks);
        assert_eq!(
            album_list(&catalog),
            [("Greatest Hits", "Band", 1), ("Greatest Hits", "Other", 1)]
        );
        assert_partition(&catalog, &tracks);
    }

    /// Spec *Untagged files without folders* at the catalog level: the
    /// library gives such files artist and album "Unknown".
    #[test]
    fn unknown_tracks_form_one_unknown_album() {
        let tracks: Vec<Track> = (0..5)
            .map(|i| track(&format!("t{i}"), "Unknown", "Unknown"))
            .collect();
        let catalog = Catalog::build(&tracks);
        assert_eq!(album_list(&catalog), [("Unknown", "Unknown", 5)]);
    }

    /// Spec *Case differences merge*, with the first spelling shown.
    #[test]
    fn case_differences_merge() {
        let tracks = [
            track("a", "Air", "Moon Safari"),
            track("b", "AIR", "moon safari"),
        ];
        let catalog = Catalog::build(&tracks);
        assert_eq!(album_list(&catalog), [("Moon Safari", "Air", 2)]);
        assert_eq!(names(&catalog), ["Air"]);
        assert_partition(&catalog, &tracks);
    }

    /// The album-artist tag is compared case-insensitively with plain artists.
    #[test]
    fn album_artist_tag_merges_with_matching_track_artist() {
        let tracks = [
            track("a", "Air", "Moon Safari"),
            with("b", "Guest", "moon SAFARI", Some("AIR"), None, None),
        ];
        let catalog = Catalog::build(&tracks);
        assert_eq!(album_list(&catalog), [("Moon Safari", "Air", 2)]);
    }

    /// An empty album-artist tag (never produced by the scan) counts as missing.
    #[test]
    fn empty_album_artist_falls_back_to_the_artist() {
        let tracks = [with("a", "Air", "Moon Safari", Some(""), None, None)];
        let catalog = Catalog::build(&tracks);
        assert_eq!(names(&catalog), ["Air"]);
    }

    /// Spec *Guest on a compilation*.
    #[test]
    fn guest_on_a_compilation_is_not_listed() {
        let tracks = [
            with("a", "A", "Hits", Some("Various Artists"), None, None),
            with("c", "C", "Hits", Some("Various Artists"), None, None),
            track("z", "Z", "Solo"),
        ];
        let catalog = Catalog::build(&tracks);
        assert_eq!(names(&catalog), ["Various Artists", "Z"]);
    }

    /// Spec *Artists listed*: album and track counts, sorted case-insensitively.
    #[test]
    fn artists_listed_with_counts() {
        let mut tracks = Vec::new();
        for i in 0..10 {
            tracks.push(track(&format!("d{i}"), "Björk", "Debut"));
        }
        for i in 0..7 {
            tracks.push(track(&format!("m{i}"), "air", "Moon Safari"));
        }
        for i in 0..5 {
            tracks.push(track(&format!("t{i}"), "Air", "Talkie Walkie"));
        }
        let catalog = Catalog::build(&tracks);
        let listed: Vec<(&str, usize, usize)> = catalog
            .artists()
            .iter()
            .map(|a| (a.name(), a.albums().len(), a.track_count()))
            .collect();
        assert_eq!(listed, [("air", 2, 12), ("Björk", 1, 10)]);
        assert_partition(&catalog, &tracks);
    }

    /// Spec *Albums listed* and *Open an artist*: albums by artist then title,
    /// an artist's albums by title.
    #[test]
    fn albums_listed_in_order() {
        let tracks = [
            track("1", "Björk", "Debut"),
            track("2", "Air", "Talkie Walkie"),
            track("3", "Air", "moon safari"),
        ];
        let catalog = Catalog::build(&tracks);
        assert_eq!(
            album_list(&catalog),
            [
                ("moon safari", "Air", 1),
                ("Talkie Walkie", "Air", 1),
                ("Debut", "Björk", 1)
            ]
        );
        let air = catalog.artist(0).map(Artist::albums).unwrap_or_default();
        let air_titles: Vec<&str> = air
            .iter()
            .filter_map(|&b| catalog.album(b).map(Album::title))
            .collect();
        assert_eq!(air_titles, ["moon safari", "Talkie Walkie"]);
        assert_partition(&catalog, &tracks);
    }

    /// Spec *Two discs*, plus unnumbered tracks after the numbered ones of
    /// their disc, then title and path order.
    #[test]
    fn album_order_is_disc_number_title_path() {
        let tracks = [
            with("d2t2", "A", "X", None, Some(2), Some(2)),
            with("d1t2", "A", "X", None, None, Some(2)),
            with("d2t1", "A", "X", None, Some(2), Some(1)),
            with("d1 untitled b", "A", "X", None, Some(1), None),
            with("D1 untitled a", "A", "X", None, None, None),
            with("d1t1", "A", "X", None, Some(1), Some(1)),
            with("d2 untitled", "A", "X", None, Some(2), None),
        ];
        let catalog = Catalog::build(&tracks);
        let album = catalog.album(0).map(Album::tracks).unwrap_or_default();
        assert_eq!(
            titles(&tracks, album),
            [
                "d1t1",
                "d1t2",
                "D1 untitled a",
                "d1 untitled b",
                "d2t1",
                "d2t2",
                "d2 untitled"
            ]
        );
    }

    /// Equal title, number and disc: the path decides, then the index.
    #[test]
    fn ties_break_on_path_then_index() {
        let mut b = track("same", "A", "X");
        b.path = "/music/b.mp3".into();
        let mut a = track("same", "A", "X");
        a.path = "/music/a.mp3".into();
        let dup = a.clone();
        let tracks = [b, a, dup];
        let catalog = Catalog::build(&tracks);
        assert_eq!(catalog.album(0).map(Album::tracks), Some(&[1, 2, 0][..]));
    }

    /// Spec *All tracks*: album by album in the artist's album order.
    #[test]
    fn all_tracks_concatenates_albums() {
        let tracks = [
            with("t2", "Air", "Talkie Walkie", None, None, Some(2)),
            with("m2", "Air", "Moon Safari", None, None, Some(2)),
            with("t1", "Air", "Talkie Walkie", None, None, Some(1)),
            with("m1", "Air", "Moon Safari", None, None, Some(1)),
            track("other", "Björk", "Debut"),
        ];
        let catalog = Catalog::build(&tracks);
        assert_eq!(
            titles(&tracks, &catalog.artist_tracks(0)),
            ["m1", "m2", "t1", "t2"]
        );
        assert_eq!(titles(&tracks, &catalog.artist_tracks(1)), ["other"]);
        assert_eq!(catalog.artist_tracks(2), Vec::<usize>::new());
    }

    #[test]
    fn search_keys_are_lower_cased() {
        let tracks = [with("a", "Guest", "Moon SAFARI", Some("ÄIR"), None, None)];
        let catalog = Catalog::build(&tracks);
        assert_eq!(catalog.artist(0).map(Artist::search_key), Some("äir"));
        assert_eq!(
            catalog.album(0).map(Album::search_key),
            Some("moon safari\näir")
        );
    }

    /// Measures [`Catalog::build`] on a large library (design risk: start-up
    /// time): `cargo test --release catalog_build_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing measurement, run by hand"]
    fn catalog_build_cost() {
        let tracks: Vec<Track> = (0..100_000u32)
            .map(|i| {
                with(
                    &format!("Title {i}"),
                    &format!("Artist {}", i % 500),
                    &format!("Album {}", i % 37),
                    None,
                    None,
                    Some(i % 20 + 1),
                )
            })
            .collect();
        let start = std::time::Instant::now();
        let catalog = Catalog::build(&tracks);
        println!(
            "Catalog::build on 100 000 tracks: {:?}; {} artists, {} albums",
            start.elapsed(),
            catalog.artists().len(),
            catalog.albums().len()
        );
    }

    /// Only tagged tracks move: an album where some tracks carry the
    /// album-artist tag splits by grouping artist (spec *Album grouping*).
    #[test]
    fn partly_tagged_album_splits_by_grouping_artist() {
        let tracks = [
            with("a", "Guest", "Hits", Some("Air"), None, None),
            track("b", "Air", "Hits"),
            track("c", "Guest", "Hits"),
        ];
        let catalog = Catalog::build(&tracks);
        assert_eq!(
            album_list(&catalog),
            [("Hits", "Air", 2), ("Hits", "Guest", 1)]
        );
        assert_partition(&catalog, &tracks);
    }

    /// A mix of artists, compilations, case variants and discs, in a
    /// scrambled order (not library order), keeps every invariant.
    #[test]
    fn mixed_library_is_a_partition() {
        let mut tracks = Vec::new();
        for n in 0..200u32 {
            let i = n * 37 % 200;
            let artist = format!("Artist {}", i % 7);
            let album = if i % 3 == 0 {
                format!("ALBUM {}", i % 5)
            } else {
                format!("album {}", i % 5)
            };
            let album_artist = (i % 11 == 0).then_some("Various");
            tracks.push(with(
                &format!("t{i}"),
                &artist,
                &album,
                album_artist,
                Some(i % 2 + 1),
                Some(i % 13),
            ));
        }
        let catalog = Catalog::build(&tracks);
        assert_partition(&catalog, &tracks);
        let keys: Vec<(String, String)> = catalog
            .albums()
            .iter()
            .map(|b| {
                let artist = catalog.artist(b.artist()).map_or("", Artist::search_key);
                (artist.to_owned(), b.title().to_lowercase())
            })
            .collect();
        assert!(
            keys.windows(2).all(|w| w[0] < w[1]),
            "albums strictly sorted"
        );
        assert!(
            catalog
                .artists()
                .windows(2)
                .all(|w| w[0].search_key() < w[1].search_key())
        );
    }
}
