//! Inferring missing track metadata from a track's file name and folders.
//!
//! Pure logic with no I/O: [`infer`] looks only at the text of a path, never at
//! the file system. Its input is the track's path **relative to the library
//! root** (or just the file name when no root is known), because only folders
//! inside the library say anything about the music.
//!
//! Invariants:
//! - file and folder names are untrusted input: every returned string has gone
//!   through [`sanitize`], so it is printable, at most
//!   [`MAX_TEXT_CHARS`](crate::text::MAX_TEXT_CHARS) characters, and `None`
//!   means "missing", never "empty";
//! - inference never overrides tags: [`Inferred`] only holds candidates, and the
//!   caller uses each one only when the matching tag is missing or empty;
//! - scanning is hand-written, linear and slice-safe (every split is at an ASCII
//!   byte or at a `char_indices` position), so crafted names can't panic or stall.
//!
//! Rules (design D2–D4 of `infer-metadata-from-paths`):
//! - **leading number**: 1–3 ASCII digits then a spaced dash (`-`, `–`, `—`),
//!   `". "`, or — only when the digits start with `0` — a single space, with
//!   something after it. `0` is no number. A stem of digits only gives nothing;
//! - **artist / title**: the rest is split at the first spaced dash; the left
//!   side is the artist, the right side the title. No split when a side is empty;
//! - **labels**: the title loses every `(…)` / `[…]` group whose words all come
//!   from [`LABEL_WORDS`], and the run of those words at its end from its
//!   first `official` on (a run without `official` stays, so titles ending in
//!   "Music" or "Video" survive); separators left dangling at either end are
//!   trimmed. Labels are stripped after the stem is capped at
//!   [`MAX_TEXT_CHARS`](crate::text::MAX_TEXT_CHARS), like the plain
//!   file-name title, so a label cut by the cap stays. A title that
//!   would become empty is kept as it was;
//! - **folders**: with at least two parent folders (after skipping a disc folder
//!   such as `CD1` or `Disc 2` as the immediate parent), the nearest is the
//!   album and the one above it the artist. An artist from the file name wins.

use std::path::{Component, Path};

use crate::text::sanitize;

/// Words that make up video-release labels such as "(Official Music Video)".
/// Compared ASCII case-insensitively.
const LABEL_WORDS: &[&str] = &[
    "official",
    "music",
    "lyric",
    "lyrics",
    "audio",
    "video",
    "visualizer",
    "visualiser",
    "hd",
    "hq",
    "4k",
];

/// Dashes that separate artist from title (and a track number from the rest)
/// when they have a space on both sides: hyphen, en dash, em dash.
const DASHES: [char; 3] = ['-', '\u{2013}', '\u{2014}'];

/// Characters trimmed from both ends of a title after a label was removed, so
/// `Song - Official Video` becomes `Song`, not `Song -`, and `[HD] - Song`
/// becomes `Song`.
const DANGLING: [char; 5] = [' ', '-', '\u{2013}', '\u{2014}', '|'];

/// Metadata candidates read from a path. Every string is sanitised; `None`
/// means nothing could be inferred.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inferred {
    /// From the file name if it has `Artist - Title`, else from the folder above
    /// the album folder.
    pub artist: Option<String>,
    /// From the nearest folder that is not a disc folder.
    pub album: Option<String>,
    /// From the file name, with the number, artist and labels removed.
    pub title: Option<String>,
    /// Leading track number of the file name; never 0.
    pub number: Option<u32>,
}

/// Infers metadata from `relative`, the track's path relative to the library
/// root (or just its file name). See the module docs for the rules.
#[must_use]
pub fn infer(relative: &Path) -> Inferred {
    let mut components = relative.components();
    let Some(Component::Normal(name)) = components.next_back() else {
        return Inferred::default();
    };

    let mut out = Path::new(name)
        .file_stem()
        .and_then(|stem| sanitize(&stem.to_string_lossy()))
        .map(|stem| from_stem(&stem))
        .unwrap_or_default();

    // Only plain folder names count; stop at anything else (`..`, a root).
    let mut parents = components.rev().map_while(|c| match c {
        Component::Normal(part) => Some(sanitize(&part.to_string_lossy())),
        _ => None,
    });
    let mut album = parents.next();
    if album
        .as_ref()
        .is_some_and(|name| name.as_deref().is_some_and(is_disc_folder))
    {
        album = parents.next();
    }
    if let (Some(album), Some(artist)) = (album, parents.next()) {
        out.album = album;
        out.artist = out.artist.or(artist);
    }
    out
}

/// Number, artist and title from a sanitised file stem.
fn from_stem(stem: &str) -> Inferred {
    // A bare number (`01`) is neither a track number nor a useful title; the
    // caller falls back to the stem itself.
    if stem.bytes().all(|b| b.is_ascii_digit()) {
        return Inferred::default();
    }
    let (number, rest) = split_number(stem);
    let (artist, title) = match split_artist(rest) {
        Some((artist, title)) => (sanitize(artist), title),
        None => (None, rest),
    };
    Inferred {
        artist,
        album: None,
        title: sanitize(&strip_labels(title)),
        number,
    }
}

/// Splits a leading track number off `stem`. Returns the number (`None` for 0)
/// and the text after the separator, or `(None, stem)` when there is no number.
fn split_number(stem: &str) -> (Option<u32>, &str) {
    let digits = stem.bytes().take_while(u8::is_ascii_digit).count();
    if !(1..=3).contains(&digits) {
        return (None, stem);
    }
    // ASCII digits, so `digits` is a char boundary.
    let (num, after) = stem.split_at(digits);
    let rest = strip_spaced_dash(after)
        .or_else(|| after.strip_prefix(". "))
        // A bare space only after a zero-padded number: `01 Song`, but not
        // `50 Cent - …` or `99 Problems`.
        // A dash right after it (`01 -x`) is not a separator, so the stem stays.
        .or_else(|| {
            after
                .strip_prefix(' ')
                .filter(|rest| num.starts_with('0') && !rest.starts_with(DASHES))
        })
        .map(str::trim_start)
        .filter(|rest| !rest.is_empty());
    match rest {
        Some(rest) => (num.parse().ok().filter(|&n: &u32| n > 0), rest),
        None => (None, stem),
    }
}

/// `text` without a leading `" - "` (or en/em dash), if it starts with one.
fn strip_spaced_dash(text: &str) -> Option<&str> {
    text.strip_prefix(' ')?
        .strip_prefix(DASHES)?
        .strip_prefix(' ')
}

/// Splits at the first spaced dash into `(artist, title)`, both trimmed and
/// non-empty; `None` when there is no such dash or a side would be empty.
fn split_artist(text: &str) -> Option<(&str, &str)> {
    let (at, dash) = text.char_indices().find(|&(i, c)| {
        DASHES.contains(&c) && text[..i].ends_with(' ') && text[i + c.len_utf8()..].starts_with(' ')
    })?;
    let artist = text[..at].trim();
    let title = text[at + dash.len_utf8()..].trim();
    (!artist.is_empty() && !title.is_empty()).then_some((artist, title))
}

fn is_label_word(word: &str) -> bool {
    LABEL_WORDS
        .iter()
        .any(|label| word.eq_ignore_ascii_case(label))
}

/// `true` when the text inside a bracket pair has words and all are labels.
fn is_label_group(inner: &str) -> bool {
    let mut words = inner.split_whitespace().peekable();
    words.peek().is_some() && words.all(is_label_word)
}

/// Removes video-release labels from `title` (see the module docs). Returns the
/// title unchanged when nothing matches or when removing would empty it.
fn strip_labels(title: &str) -> String {
    let (without_groups, mut removed) = remove_label_groups(title);
    // Re-sanitise to collapse the spaces left where groups were.
    let Some(mut text) = sanitize(&without_groups) else {
        return title.to_owned();
    };
    if let Some(cut) = trailing_label_start(&text) {
        text.truncate(cut);
        removed = true;
    }
    if removed {
        text.truncate(text.trim_end_matches(DANGLING).len());
        let leading = text.len() - text.trim_start_matches(DANGLING).len();
        text.drain(..leading);
    }
    if text.is_empty() {
        title.to_owned()
    } else {
        text
    }
}

/// Replaces each `(…)` / `[…]` label group with a space. Brackets don't nest:
/// a closer pairs with the latest unpaired opener of its kind, so the scan is
/// linear and unbalanced brackets are simply left alone. Also returns whether
/// anything was removed.
fn remove_label_groups(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let (mut round, mut square) = (None, None);
    for (i, b) in text.bytes().enumerate() {
        let open = match b {
            b'(' => {
                round = Some(i);
                continue;
            }
            b'[' => {
                square = Some(i);
                continue;
            }
            b')' => round.take(),
            b']' => square.take(),
            _ => continue,
        };
        // Brackets are ASCII, so `open + 1` and `i` are char boundaries. A label
        // group holds no brackets, so it never overlaps one already removed.
        if let Some(open) = open.filter(|&open| open >= copied)
            && is_label_group(&text[open + 1..i])
        {
            out.push_str(&text[copied..open]);
            out.push(' ');
            copied = i + 1;
        }
    }
    let removed = copied > 0;
    out.push_str(&text[copied..]);
    (out, removed)
}

/// Byte offset where a trailing run of label words starting with `official`
/// begins, in sanitised text (single spaces, trimmed). Within the run of label
/// words at the end, the cut is at its first `official`; a run without one
/// gives `None`.
fn trailing_label_start(text: &str) -> Option<usize> {
    let mut cut = None;
    let mut end = text.len();
    loop {
        let start = text[..end].rfind(' ').map_or(0, |space| space + 1);
        let word = &text[start..end];
        if !is_label_word(word) {
            break;
        }
        if word.eq_ignore_ascii_case("official") {
            cut = Some(start);
        }
        if start == 0 {
            break;
        }
        end = start - 1;
    }
    cut
}

/// `true` for disc folders: `cd`, `disc` or `disk` (any case), an optional
/// single ` `, `-` or `_`, then 1–2 ASCII digits and nothing else.
fn is_disc_folder(name: &str) -> bool {
    let bytes = name.as_bytes();
    let Some(rest) = ["disc", "disk", "cd"].iter().find_map(|prefix| {
        bytes
            .get(..prefix.len())
            .filter(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
            .map(|_| &bytes[prefix.len()..])
    }) else {
        return false;
    };
    let digits = match rest.first() {
        Some(b' ' | b'-' | b'_') => &rest[1..],
        _ => rest,
    };
    (1..=2).contains(&digits.len()) && digits.iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn inf(path: &str) -> Inferred {
        infer(Path::new(path))
    }

    fn title(path: &str) -> Option<String> {
        inf(path).title
    }

    fn want(
        artist: Option<&str>,
        album: Option<&str>,
        title: Option<&str>,
        number: Option<u32>,
    ) -> Inferred {
        Inferred {
            artist: artist.map(str::to_owned),
            album: album.map(str::to_owned),
            title: title.map(str::to_owned),
            number,
        }
    }

    // --- Spec: Artist and title from the file name ---

    #[test]
    fn artist_dash_title() {
        assert_eq!(
            inf("Kendrick Lamar - dodger blue.mp3"),
            want(Some("Kendrick Lamar"), None, Some("dodger blue"), None)
        );
    }

    #[test]
    fn only_the_first_dash_splits() {
        assert_eq!(
            inf("A - B - C.mp3"),
            want(Some("A"), None, Some("B - C"), None)
        );
    }

    #[test]
    fn unspaced_hyphen_is_not_a_separator() {
        assert_eq!(
            inf("Spider-Man Theme.mp3"),
            want(None, None, Some("Spider-Man Theme"), None)
        );
    }

    #[test]
    fn title_tag_present_artist_missing_gives_file_name_artist() {
        // The caller keeps the title tag; the artist candidate is "Band".
        assert_eq!(inf("Band - whatever.mp3").artist.as_deref(), Some("Band"));
    }

    #[test]
    fn artist_tag_present_title_missing_gives_file_name_title() {
        // The caller keeps the artist tag; the title candidate is "Name".
        assert_eq!(title("Other - Name.mp3").as_deref(), Some("Name"));
    }

    #[test]
    fn empty_side_does_not_split() {
        for stem in [" - Song", "Band - ", " - ", "- Song", "Band -"] {
            let got = inf(&format!("{stem}.mp3"));
            assert_eq!(got.artist, None, "{stem:?}");
            assert_eq!(got.title, sanitize(stem), "{stem:?}");
        }
    }

    #[test]
    fn en_and_em_dashes_split() {
        assert_eq!(
            inf("Björk – Jóga.mp3"),
            want(Some("Björk"), None, Some("Jóga"), None)
        );
        assert_eq!(
            inf("坂本龍一 — energy flow.flac"),
            want(Some("坂本龍一"), None, Some("energy flow"), None)
        );
    }

    // --- Spec: Track number from the file name ---

    #[test]
    fn numbered_file() {
        assert_eq!(
            inf("03 - Song.flac"),
            want(None, None, Some("Song"), Some(3))
        );
    }

    #[test]
    fn numbered_file_with_artist() {
        assert_eq!(
            inf("01. Band - Song.mp3"),
            want(Some("Band"), None, Some("Song"), Some(1))
        );
    }

    #[test]
    fn number_that_belongs_to_the_artist() {
        assert_eq!(
            inf("50 Cent - In Da Club.mp3"),
            want(Some("50 Cent"), None, Some("In Da Club"), None)
        );
        assert_eq!(
            inf("2 Chainz - Song.mp3"),
            want(Some("2 Chainz"), None, Some("Song"), None)
        );
    }

    #[test]
    fn number_that_belongs_to_the_title() {
        assert_eq!(
            inf("99 Problems.mp3"),
            want(None, None, Some("99 Problems"), None)
        );
        assert_eq!(inf("10 Song.mp3"), want(None, None, Some("10 Song"), None));
        assert_eq!(inf("7 Rings.mp3"), want(None, None, Some("7 Rings"), None));
    }

    #[test]
    fn zero_is_not_a_track_number() {
        assert_eq!(inf("00 - Intro.mp3"), want(None, None, Some("Intro"), None));
    }

    #[test]
    fn bare_number_name_gives_nothing() {
        // The caller falls back to the stem, so the title shows as "01".
        assert_eq!(inf("01.mp3"), Inferred::default());
        assert_eq!(inf("1999.mp3"), Inferred::default());
    }

    #[test]
    fn number_separators() {
        assert_eq!(
            inf("10 - Song.mp3"),
            want(None, None, Some("Song"), Some(10))
        );
        assert_eq!(
            inf("10. Song.mp3"),
            want(None, None, Some("Song"), Some(10))
        );
        assert_eq!(
            inf("01 – Song.mp3"),
            want(None, None, Some("Song"), Some(1))
        );
        assert_eq!(
            inf("01 — Song.mp3"),
            want(None, None, Some("Song"), Some(1))
        );
        assert_eq!(inf("01 Song.mp3"), want(None, None, Some("Song"), Some(1)));
        assert_eq!(
            inf("01 Band - Song.mp3"),
            want(Some("Band"), None, Some("Song"), Some(1))
        );
    }

    #[test]
    fn four_digits_are_not_a_track_number() {
        assert_eq!(
            inf("1999 - Song.mp3"),
            want(Some("1999"), None, Some("Song"), None)
        );
    }

    #[test]
    fn number_without_separator_is_kept() {
        assert_eq!(inf("01.Song.mp3"), want(None, None, Some("01.Song"), None));
        assert_eq!(inf("01-Song.mp3"), want(None, None, Some("01-Song"), None));
        assert_eq!(inf("01 -.mp3"), want(None, None, Some("01 -"), None));
        assert_eq!(
            inf("01 -Song.mp3"),
            want(None, None, Some("01 -Song"), None)
        );
    }

    // --- Spec: Video suffixes removed from inferred titles ---

    #[test]
    fn bracketed_label() {
        assert_eq!(
            inf("Kendrick Lamar - dodger blue (Official Audio).mp3"),
            want(Some("Kendrick Lamar"), None, Some("dodger blue"), None)
        );
    }

    #[test]
    fn unbracketed_label() {
        assert_eq!(
            title("Self Love by Metro Boomin Official Lyric Video.mp3").as_deref(),
            Some("Self Love by Metro Boomin")
        );
    }

    #[test]
    fn other_brackets_are_kept() {
        assert_eq!(
            inf("Calling (Spider-Man Across the Spider-Verse).mp3"),
            want(
                None,
                None,
                Some("Calling (Spider-Man Across the Spider-Verse)"),
                None
            )
        );
    }

    #[test]
    fn label_in_the_middle() {
        assert_eq!(
            inf("Miguel - All I Want Is You (Official Video) ft. J. Cole.mp3"),
            want(
                Some("Miguel"),
                None,
                Some("All I Want Is You ft. J. Cole"),
                None
            )
        );
    }

    #[test]
    fn unbracketed_words_without_official_are_kept() {
        assert_eq!(
            title("Band - Born to the Music.mp3").as_deref(),
            Some("Born to the Music")
        );
        assert_eq!(
            title("Band - Music Video.mp3").as_deref(),
            Some("Music Video")
        );
    }

    #[test]
    fn dangling_dash_before_a_trailing_label() {
        assert_eq!(
            inf("Band - Song - Official Video.mp3"),
            want(Some("Band"), None, Some("Song"), None)
        );
        assert_eq!(strip_labels("Song - Official Video"), "Song");
        // Through `infer` the first dash splits before labels are stripped, and
        // a title that would become empty is kept (design: accepted trade-off).
        assert_eq!(
            inf("Song - Official Video.mp3"),
            want(Some("Song"), None, Some("Official Video"), None)
        );
        assert_eq!(title("Band - [HD] - Song.mp3").as_deref(), Some("Song"));
        assert_eq!(strip_labels("Song | Official Music Video"), "Song");
        assert_eq!(strip_labels("Song – (Official Video)"), "Song");
    }

    #[test]
    fn label_only_name_is_kept() {
        assert_eq!(
            title("Official Video.mp3").as_deref(),
            Some("Official Video")
        );
        assert_eq!(
            title("(Official Video).mp3").as_deref(),
            Some("(Official Video)")
        );
        assert_eq!(
            title("Band - Official Video.mp3").as_deref(),
            Some("Official Video")
        );
    }

    #[test]
    fn label_groups_any_case_and_kind() {
        assert_eq!(title("Song [Official Video].mp3").as_deref(), Some("Song"));
        assert_eq!(title("Song (official AUDIO).mp3").as_deref(), Some("Song"));
        assert_eq!(
            title("Song (Official Video) [HD].mp3").as_deref(),
            Some("Song")
        );
        assert_eq!(title("Song (Lyrics)(4K).mp3").as_deref(), Some("Song"));
        assert_eq!(title("(Official Video) Song.mp3").as_deref(), Some("Song"));
    }

    #[test]
    fn non_label_groups_stay() {
        assert_eq!(
            title("Band - Song (Live).mp3").as_deref(),
            Some("Song (Live)")
        );
        assert_eq!(
            title("Band - Song (Remix).mp3").as_deref(),
            Some("Song (Remix)")
        );
        assert_eq!(
            title("Song (Live) (Official Video).mp3").as_deref(),
            Some("Song (Live)")
        );
        assert_eq!(title("Song ().mp3").as_deref(), Some("Song ()"));
        assert_eq!(
            title("Song (Official Video Edit).mp3").as_deref(),
            Some("Song (Official Video Edit)")
        );
    }

    #[test]
    fn earliest_official_in_trailing_run_is_cut() {
        assert_eq!(strip_labels("Song Music Official Video"), "Song Music");
        assert_eq!(strip_labels("Song Official HD Official Video"), "Song");
        assert_eq!(
            strip_labels("Born to the Music Official Video"),
            "Born to the Music"
        );
    }

    // --- Spec: Album and artist from folders ---

    #[test]
    fn artist_album_layout() {
        assert_eq!(
            inf("Band/LP/01 Song.flac"),
            want(Some("Band"), Some("LP"), Some("Song"), Some(1))
        );
    }

    #[test]
    fn deeper_layout_uses_the_nearest_two_folders() {
        assert_eq!(
            inf("Rock/Band/LP/Song.mp3"),
            want(Some("Band"), Some("LP"), Some("Song"), None)
        );
    }

    #[test]
    fn file_name_artist_wins_over_folder() {
        assert_eq!(
            inf("Various/Mix/Guest - Song.mp3"),
            want(Some("Guest"), Some("Mix"), Some("Song"), None)
        );
    }

    #[test]
    fn one_folder_deep_gives_no_folder_values() {
        assert_eq!(
            inf("Downloads/Song.mp3"),
            want(None, None, Some("Song"), None)
        );
    }

    #[test]
    fn folders_apply_to_a_bare_number_name() {
        assert_eq!(
            inf("Band/LP/01.mp3"),
            want(Some("Band"), Some("LP"), None, None)
        );
    }

    #[test]
    fn non_normal_components_end_the_folder_walk() {
        assert_eq!(inf("../LP/Song.mp3"), want(None, None, Some("Song"), None));
        assert_eq!(inf("/LP/Song.mp3"), want(None, None, Some("Song"), None));
        assert_eq!(inf("Band/LP/.."), Inferred::default());
        assert_eq!(inf(""), Inferred::default());
    }

    // --- Spec: Disc folders skipped for folder inference ---

    #[test]
    fn disc_folder_is_skipped() {
        assert_eq!(
            inf("Band/LP/CD1/01 Song.flac"),
            want(Some("Band"), Some("LP"), Some("Song"), Some(1))
        );
    }

    #[test]
    fn disc_folder_alone_is_not_enough() {
        assert_eq!(
            inf("LP/Disc 2/Song.mp3"),
            want(None, None, Some("Song"), None)
        );
    }

    #[test]
    fn disc_folder_names() {
        for name in ["CD1", "cd 1", "Disc 2", "DISK-02", "disc_3", "cd09"] {
            assert!(is_disc_folder(name), "{name:?}");
            assert_eq!(
                inf(&format!("Band/LP/{name}/Song.mp3")).album.as_deref(),
                Some("LP"),
                "{name:?}"
            );
        }
        for name in [
            "CD",
            "CD123",
            "Discography",
            "CDs",
            "cd  1",
            "cd-_1",
            "ＣＤ1",
        ] {
            assert!(!is_disc_folder(name), "{name:?}");
        }
        for name in ["CD", "CD123", "Discography", "CDs"] {
            assert_eq!(
                inf(&format!("Band/{name}/Song.mp3")),
                want(Some("Band"), Some(name), Some("Song"), None),
                "{name:?}"
            );
        }
    }

    #[test]
    fn only_the_immediate_parent_is_checked_for_a_disc() {
        assert_eq!(
            inf("CD1/Band/LP/x.mp3"),
            want(Some("Band"), Some("LP"), Some("x"), None)
        );
    }

    #[test]
    fn components_are_platform_independent() {
        // Built from components, so it holds whatever the native separator is;
        // `\\` parsing itself is covered by running this suite on Windows CI.
        let path = PathBuf::from_iter(["Band", "LP", "CD2", "02 Song.flac"]);
        assert_eq!(
            infer(&path),
            want(Some("Band"), Some("LP"), Some("Song"), Some(2))
        );
    }

    // --- Spec: Inferred text is untrusted ---

    #[test]
    fn control_characters_are_sanitised() {
        let got = inf("Ba\u{1b}[31mnd - So\u{7}ng\u{202E}.mp3");
        assert_eq!(got.artist.as_deref(), Some("Ba [31mnd"));
        assert_eq!(got.title.as_deref(), Some("So ng"));

        let got = inf("Ro\u{1b}ck/\u{9B}LP\u{0}/Song.mp3");
        assert_eq!(got.artist.as_deref(), Some("Ro ck"));
        assert_eq!(got.album.as_deref(), Some("LP"));
    }

    #[test]
    fn blank_parts_count_as_missing() {
        assert_eq!(
            inf(" /\u{200B}/Song.mp3"),
            want(None, None, Some("Song"), None)
        );
        assert_eq!(inf("\u{1b}.mp3"), Inferred::default());
    }

    // --- Robustness ---

    #[test]
    fn long_multibyte_stems_neither_panic_nor_split_characters() {
        let stem = format!("{} - {}", "ü".repeat(150), "日".repeat(150));
        let got = inf(&format!("{stem}.mp3"));
        assert_eq!(got.artist, Some("ü".repeat(150)));
        // The stem is capped at 256 characters before splitting.
        assert_eq!(got.title, Some("日".repeat(256 - 153)));

        let stem = "é".repeat(300);
        assert_eq!(title(&format!("{stem}.mp3")), Some("é".repeat(256)));
    }

    #[test]
    fn long_and_bracket_heavy_stems_are_handled() {
        let long = format!("{}(Official Video)", "a ".repeat(300));
        assert!(title(&format!("{long}.mp3")).is_some());

        for stem in [
            "((((",
            "[[[[",
            "Song [Official Video",
            "Song Official Video)",
            "((((Official Audio)",
            "([)]",
            ")(",
            "][",
            "(Official [Video)]",
        ] {
            let got = title(&format!("{stem}.mp3"));
            assert!(got.is_some(), "{stem:?}");
        }
        assert_eq!(
            title("Song [Official Video.mp3").as_deref(),
            Some("Song [Official Video")
        );
        assert_eq!(title("((((Official Audio).mp3").as_deref(), Some("((("));

        let brackets = "([".repeat(200);
        assert!(title(&format!("{brackets}.mp3")).is_some());
        let groups = "(HD)".repeat(50);
        assert_eq!(
            title(&format!("Song {groups}.mp3")).as_deref(),
            Some("Song")
        );
    }
}
