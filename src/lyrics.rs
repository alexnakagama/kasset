//! Lyrics in the LRC format: parsing and finding the current line.
//!
//! Pure logic with no I/O: [`parse`] turns the text of an `.lrc` file (or of a
//! lyrics tag) into [`Lyrics`], and [`Lyrics::current`] / [`Lyrics::next_change`]
//! answer "which line is sung at this position" and "when does that change".
//! All of them are pure functions of their inputs. Finding and reading a
//! track's lyrics (a `.lrc` sidecar, else its tags) is the I/O-doing
//! [`source`] submodule.
//!
//! Format, as read here:
//! - a line starts with zero or more `[...]` groups. `[mm:ss]`, `[mm:ss.x]`,
//!   `[mm:ss.xx]` and `[mm:ss.xxx]` (1 to 6 minute digits, seconds below 60)
//!   are timestamps, and a line gets one entry per timestamp;
//! - `[tag:value]`, with a tag of ASCII letters, digits, `_`, `-` or `#`, is a
//!   header. `[offset:±N]` shifts every time by N ms (positive N shows lines
//!   earlier), clamped to ±[`MAX_OFFSET_MS`]; other headers are ignored;
//! - a line without a timestamp is dropped when it holds only headers whose
//!   tags are all standard LRC tags ([`KNOWN_TAGS`], e.g. `[ar:Band]`). Any
//!   other untimed line is plain text as written, so section markers such as
//!   `[Chorus]`, `[Verse 1]` or `[Chorus: Singer] la la` are kept;
//! - a malformed stamp (`[01:75.00]`, `[00:12.0000]`) is not a timestamp: on a
//!   timed line it reads as a header, on an untimed line it stays as text;
//! - `<mm:ss.xx>` word timings are removed from the text;
//! - leading whitespace and invisible format characters (BOM, zero-width
//!   space, bidi marks) before the first group are skipped.
//!
//! Invariants:
//! - every text has gone through [`text::sanitize`]: no control character or
//!   escape sequence survives, and a blank line is an empty string;
//! - [`Lyrics::Synced`] lines are sorted by time, lines with equal times in
//!   file order; untimed lines are dropped as soon as one timestamp exists, and
//!   no synced line is trimmed (a blank timed line ends the previous line);
//! - [`Lyrics::Plain`] keeps file order, without leading or trailing blank lines;
//! - neither variant is empty or all blank (`parse` returns `None` instead; a
//!   file whose timed lines are all blank is `None` even if it has untimed
//!   text), and neither holds more than [`MAX_LINES`] lines;
//! - [`Lyrics::current`] returns the last of several lines with the same time;
//! - all time arithmetic is checked or saturating, and scanning is linear in
//!   the input length, so crafted input can never panic, wrap or stall.

pub mod source;

use std::time::Duration;

use crate::text;

/// Most lines kept from one file; later ones are ignored.
pub const MAX_LINES: usize = 10_000;

/// Largest `[offset:]` honoured, either way, in milliseconds (one hour).
pub const MAX_OFFSET_MS: i64 = 3_600_000;

/// Standard LRC header tags, compared case-insensitively (`#` is a comment).
/// An untimed line made only of these is metadata, not lyrics.
pub const KNOWN_TAGS: &[&str] = &[
    "#", "al", "ar", "au", "by", "id", "la", "lang", "length", "offset", "re", "ti", "tool", "ve",
];

/// Most minute digits in a timestamp; longer stamps are not timestamps.
const MAX_MINUTE_DIGITS: usize = 6;

/// Longest timestamp text, `mmmmmm:ss.xxx`, in bytes.
const MAX_STAMP_LEN: usize = MAX_MINUTE_DIGITS + ":ss.xxx".len();

/// One timed lyric line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// When the line starts, from the start of the track, offset applied.
    pub at: Duration,
    /// Sanitised text; empty for a blank timed line.
    pub text: String,
}

/// The parsed lyrics of one track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lyrics {
    /// Timed lines, sorted by `at` (stable for equal times).
    Synced(Vec<Line>),
    /// Untimed lines in file order; blank lines inside are empty strings.
    Plain(Vec<String>),
}

impl Lyrics {
    /// Index of the line sung at `position`: the last synced line whose time is
    /// at or before it. `None` before the first line and for plain lyrics.
    #[must_use]
    pub fn current(&self, position: Duration) -> Option<usize> {
        match self {
            Self::Synced(lines) => lines
                .partition_point(|line| line.at <= position)
                .checked_sub(1),
            Self::Plain(_) => None,
        }
    }

    /// The first line time strictly after `position`, when the current line
    /// changes next. `None` after the last line and for plain lyrics.
    #[must_use]
    pub fn next_change(&self, position: Duration) -> Option<Duration> {
        match self {
            Self::Synced(lines) => lines
                .get(lines.partition_point(|line| line.at <= position))
                .map(|line| line.at),
            Self::Plain(_) => None,
        }
    }
}

/// Parses LRC (or plain) lyrics text. Returns `None` when no non-blank lyric
/// line remains (empty input, headers only, whitespace only).
#[must_use]
pub fn parse(text: &str) -> Option<Lyrics> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);

    let mut synced: Vec<Line> = Vec::new();
    let mut plain: Vec<String> = Vec::new();
    let mut offset_ms: i64 = 0;

    for raw in split_lines(text) {
        let parsed = parse_line(raw, MAX_LINES.saturating_sub(synced.len()));
        if let Some(offset) = parsed.offset {
            offset_ms = offset;
        }
        if !parsed.stamps.is_empty() {
            let line_text = clean(parsed.rest);
            for at in parsed.stamps {
                if synced.len() >= MAX_LINES {
                    break;
                }
                synced.push(Line {
                    at,
                    text: line_text.clone(),
                });
            }
        } else if synced.is_empty() && plain.len() < MAX_LINES {
            // Only needed while no timestamp has been seen; once one has,
            // untimed lines are dropped anyway.
            if parsed.known_headers_only() {
                continue;
            }
            let line_text = clean(raw);
            if !(plain.is_empty() && line_text.is_empty()) {
                plain.push(line_text);
            }
        }
    }

    if synced.is_empty() {
        while plain.last().is_some_and(String::is_empty) {
            plain.pop();
        }
        return (!plain.is_empty()).then_some(Lyrics::Plain(plain));
    }

    if synced.iter().all(|line| line.text.is_empty()) {
        return None;
    }
    for line in &mut synced {
        line.at = apply_offset(line.at, offset_ms);
    }
    synced.sort_by_key(|line| line.at);
    Some(Lyrics::Synced(synced))
}

/// Splits on `\n`, `\r\n` and lone `\r` line endings. The `\r` of a `\r\n` is
/// stripped first: splitting on it would add an empty line after every line.
fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n')
        .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
}

/// What the leading bracket groups of one line held.
struct ParsedLine<'a> {
    /// Timestamps, in the order they appear (at most the cap passed in).
    stamps: Vec<Duration>,
    /// At least one `[tag:value]` header was found.
    header: bool,
    /// At least one header's tag is not in [`KNOWN_TAGS`].
    unknown_header: bool,
    /// The value of the last valid `[offset:]` header on the line.
    offset: Option<i64>,
    /// The text after the last timestamp or header group.
    rest: &'a str,
}

impl ParsedLine<'_> {
    /// The line is LRC metadata: no timestamp, only known headers, no text.
    fn known_headers_only(&self) -> bool {
        self.stamps.is_empty()
            && self.header
            && !self.unknown_header
            && text::sanitize(self.rest).is_none()
    }
}

/// Reads the leading timestamp and header groups of `line`, stopping at the
/// first group that is neither, or at the first text. Keeps at most
/// `max_stamps` timestamps, so a line of repeated stamps stays bounded.
fn parse_line(line: &str, max_stamps: usize) -> ParsedLine<'_> {
    let mut parsed = ParsedLine {
        stamps: Vec::new(),
        header: false,
        unknown_header: false,
        offset: None,
        rest: line.trim_start_matches(|c: char| c.is_whitespace() || text::is_invisible_format(c)),
    };
    while let Some(after) = parsed.rest.strip_prefix('[') {
        let Some(end) = after.find(']') else { break };
        let inner = &after[..end];
        if let Some(at) = parse_stamp(inner) {
            if parsed.stamps.len() < max_stamps {
                parsed.stamps.push(at);
            }
        } else if let Some((tag, value)) = header_parts(inner) {
            parsed.header = true;
            if !KNOWN_TAGS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(tag))
            {
                parsed.unknown_header = true;
            }
            if tag.eq_ignore_ascii_case("offset")
                && let Some(offset) = parse_offset(value)
            {
                parsed.offset = Some(offset);
            }
        } else {
            break;
        }
        parsed.rest = &after[end + 1..];
    }
    parsed
}

/// Parses `mm:ss`, `mm:ss.x`, `mm:ss.xx` or `mm:ss.xxx` (the inside of a
/// `[...]` or `<...>` group). Anything else is `None`.
fn parse_stamp(inner: &str) -> Option<Duration> {
    let (minutes, rest) = inner.split_once(':')?;
    let (seconds, fraction) = match rest.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (rest, None),
    };
    if !(1..=MAX_MINUTE_DIGITS).contains(&minutes.len()) || seconds.len() != 2 {
        return None;
    }
    let minutes = digits(minutes)?;
    let seconds = digits(seconds)?;
    if seconds >= 60 {
        return None;
    }
    let millis = match fraction {
        None => 0,
        Some(fraction) => {
            let scale = match fraction.len() {
                1 => 100,
                2 => 10,
                3 => 1,
                _ => return None,
            };
            digits(fraction)?.checked_mul(scale)?
        }
    };
    let total = minutes
        .checked_mul(60_000)?
        .checked_add(seconds.checked_mul(1_000)?)?
        .checked_add(millis)?;
    Some(Duration::from_millis(total))
}

/// The value of a non-empty run of ASCII digits (no sign, no spaces).
fn digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Splits `tag:value` when the tag looks like an LRC header tag.
fn header_parts(inner: &str) -> Option<(&str, &str)> {
    let (tag, value) = inner.split_once(':')?;
    let is_tag_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '#');
    (!tag.is_empty() && tag.chars().all(is_tag_char)).then_some((tag, value))
}

/// Parses an `[offset:]` value: optional sign, then digits, surrounding spaces
/// allowed. Values beyond ±[`MAX_OFFSET_MS`] (however many digits) are clamped;
/// anything else is `None`.
fn parse_offset(value: &str) -> Option<i64> {
    let value = value.trim();
    let (negative, number) = match value.as_bytes().first()? {
        b'-' => (true, &value[1..]),
        b'+' => (false, &value[1..]),
        _ => (false, value),
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut magnitude: i64 = 0;
    for b in number.bytes() {
        magnitude = magnitude
            .saturating_mul(10)
            .saturating_add(i64::from(b - b'0'))
            .min(MAX_OFFSET_MS);
    }
    Some(if negative { -magnitude } else { magnitude })
}

/// Shifts `at` back by `offset_ms` (forward for a negative offset), saturating
/// at zero.
fn apply_offset(at: Duration, offset_ms: i64) -> Duration {
    let shift = Duration::from_millis(offset_ms.unsigned_abs());
    if offset_ms >= 0 {
        at.saturating_sub(shift)
    } else {
        at.saturating_add(shift)
    }
}

/// The display text of a lyric: word timings removed, then sanitised; blank
/// becomes the empty string.
fn clean(raw: &str) -> String {
    text::sanitize(&strip_word_timings(raw)).unwrap_or_default()
}

/// Removes `<mm:ss.xx>` word timings. Other `<...>` text is kept.
///
/// The closing `>` is looked for only within [`MAX_STAMP_LEN`] bytes of each
/// `<`, so the work stays linear in the line length whatever it contains.
fn strip_word_timings(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        // `>` is ASCII, so its byte index is a char boundary.
        let close = after
            .bytes()
            .take(MAX_STAMP_LEN + 1)
            .position(|b| b == b'>');
        match close {
            Some(close) if parse_stamp(&after[..close]).is_some() => {
                rest = &after[close + 1..];
            }
            _ => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    fn ms(millis: u64) -> Duration {
        Duration::from_millis(millis)
    }

    fn synced(text: &str) -> Vec<Line> {
        match parse(text) {
            Some(Lyrics::Synced(lines)) => lines,
            other => panic!("expected synced lyrics, got {other:?}"),
        }
    }

    fn plain(text: &str) -> Vec<String> {
        match parse(text) {
            Some(Lyrics::Plain(lines)) => lines,
            other => panic!("expected plain lyrics, got {other:?}"),
        }
    }

    fn line(at: u64, text: &str) -> Line {
        Line {
            at: ms(at),
            text: text.to_owned(),
        }
    }

    #[test]
    fn timestamp_precisions() {
        assert_eq!(parse_stamp("01:02"), Some(ms(62_000)));
        assert_eq!(parse_stamp("01:02.3"), Some(ms(62_300)));
        assert_eq!(parse_stamp("01:02.34"), Some(ms(62_340)));
        assert_eq!(parse_stamp("01:02.345"), Some(ms(62_345)));
        assert_eq!(parse_stamp("0:00"), Some(ms(0)));
        assert_eq!(parse_stamp("123:04.50"), Some(ms(123 * 60_000 + 4_500)));
        assert_eq!(
            parse_stamp("999999:59.999"),
            Some(ms(999_999 * 60_000 + 59_999))
        );
    }

    #[test]
    fn malformed_stamps_are_not_timestamps() {
        for bad in [
            "01:75.00",
            "01:60",
            "1:2",
            "01:002",
            "01:02.",
            "01:02.3456",
            ":02",
            "aa:bb",
            "01:0x",
            "+1:02",
            "01:-2",
            "01:02.-1",
            "01 :02",
            "1234567:00.00",
            "01.02",
            "01:02:03",
            "",
        ] {
            assert_eq!(parse_stamp(bad), None, "{bad:?}");
        }
        // A malformed stamp is a header-shaped group ("01" is a tag), so the
        // line is dropped rather than shown with a bogus time.
        let lines = synced("[01:75.00]bad\n[00:01.00]good");
        assert_eq!(lines, vec![line(1_000, "good")]);
    }

    #[test]
    fn line_with_several_timestamps() {
        let lyrics = parse("[00:12.00][01:30.00]Chorus").unwrap_or_else(|| panic!("no lyrics"));
        assert_eq!(
            lyrics,
            Lyrics::Synced(vec![line(12_000, "Chorus"), line(90_000, "Chorus")])
        );
        assert_eq!(lyrics.current(ms(12_000)), Some(0));
        assert_eq!(lyrics.current(ms(90_000)), Some(1));
    }

    #[test]
    fn out_of_order_lines_are_sorted_stably() {
        let lines = synced("[00:20.00]twenty\n[00:10.00]ten\n[00:10.00]ten again\n[00:05]five");
        assert_eq!(
            lines,
            vec![
                line(5_000, "five"),
                line(10_000, "ten"),
                line(10_000, "ten again"),
                line(20_000, "twenty"),
            ]
        );
    }

    #[test]
    fn offset_shifts_and_saturates() {
        assert_eq!(
            synced("[offset:+500]\n[00:10.00]a\n[00:00.20]b"),
            vec![line(0, "b"), line(9_500, "a")]
        );
        assert_eq!(synced("[offset:500]\n[00:10.00]a"), vec![line(9_500, "a")]);
        assert_eq!(
            synced("[offset:-500]\n[00:10.00]a"),
            vec![line(10_500, "a")]
        );
        // The offset applies wherever the header is, and the last one wins.
        assert_eq!(
            synced("[00:10.00]a\n[offset:100]\n[OFFSET: -250 ]"),
            vec![line(10_250, "a")]
        );
        // An unparsable offset is ignored.
        assert_eq!(
            synced("[offset:soon]\n[00:10.00]a"),
            vec![line(10_000, "a")]
        );
    }

    #[test]
    fn headers_and_word_timings_are_removed() {
        let lines = synced(
            "[ar:Band]\n[ti:Song]\n[length: 03:20]\n[#:comment]\n\
             [00:05.00]<00:05.00>Hello <00:05.50>world",
        );
        assert_eq!(lines, vec![line(5_000, "Hello world")]);
        // Angle brackets that are not word timings stay.
        assert_eq!(
            synced("[00:01.00]a <3 b <tag>"),
            vec![line(1_000, "a <3 b <tag>")]
        );
    }

    #[test]
    fn headers_only_or_blank_input_is_none() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("   \n\t\n\r\n"), None);
        assert_eq!(parse("[ar:Band]\n[ti:Song]\n[offset:100]"), None);
        assert_eq!(parse("[00:01.00]\n[00:02.00]  "), None);
        assert_eq!(parse("\u{FEFF}"), None);
    }

    #[test]
    fn section_markers_stay_as_text() {
        assert_eq!(
            plain("[Chorus]\n[Verse 1] la la\n[Verse 2: Singer] da"),
            vec!["[Chorus]", "[Verse 1] la la", "[Verse 2: Singer] da"]
        );
        // Markers whose name looks like a header tag are kept too, alone or
        // followed by text; only standard LRC metadata lines are dropped.
        assert_eq!(
            plain(
                "[ar:Band]\n[TI: Song]\n[Chorus: Drake] la la\n[Intro: Name]\n\
                 [Hook:x2][Pre-Chorus:Bob]\nnext"
            ),
            vec![
                "[Chorus: Drake] la la",
                "[Intro: Name]",
                "[Hook:x2][Pre-Chorus:Bob]",
                "next"
            ]
        );
        // Known tags followed by text are not metadata either.
        assert_eq!(plain("[ar:Band] hello"), vec!["[ar:Band] hello"]);
        // On an untimed line a malformed stamp stays as text.
        assert_eq!(plain("[01:75.00]odd"), vec!["[01:75.00]odd"]);
        // After a timestamp a bracket group is part of the text.
        assert_eq!(
            synced("[00:01.00][Chorus] la"),
            vec![line(1_000, "[Chorus] la")]
        );
    }

    #[test]
    fn blank_timed_lines_end_the_previous_line() {
        let text = "[00:10.00]one\n[00:30.00]\n[00:40.00]two\n[03:40.00]";
        let lyrics = parse(text).unwrap_or_else(|| panic!("no lyrics"));
        let Lyrics::Synced(lines) = &lyrics else {
            panic!("expected synced lyrics");
        };
        assert_eq!(lines.len(), 4, "the trailing blank line is kept");
        assert_eq!(lines[1], line(30_000, ""));
        assert_eq!(lines[3], line(220_000, ""));
        assert_eq!(lyrics.current(ms(35_000)), Some(1));
        assert_eq!(lyrics.current(ms(225_000)), Some(3));
    }

    #[test]
    fn equal_times_make_the_last_line_current() {
        let lyrics = parse("[00:10.00]one\n[00:10.00]\n[00:20.00]two")
            .unwrap_or_else(|| panic!("no lyrics"));
        assert_eq!(lyrics.current(ms(10_000)), Some(1));
        assert_eq!(lyrics.current(ms(15_000)), Some(1));
    }

    #[test]
    fn invisible_characters_before_the_first_group_are_skipped() {
        assert_eq!(
            synced("\u{200B}[00:01.00]a\n\u{FEFF}\u{200E} [00:02.00]b"),
            vec![line(1_000, "a"), line(2_000, "b")]
        );
    }

    #[test]
    fn multibyte_text_next_to_brackets_is_safe() {
        assert_eq!(
            synced("[00:01.00]<é>héllo <日本>語 <00:02.00>🎵\n[é:x]dropped untimed"),
            vec![line(1_000, "<é>héllo <日本>語 🎵")]
        );
        assert_eq!(plain("[é:x]a"), vec!["[é:x]a"]);
        assert_eq!(plain("[日本:語]"), vec!["[日本:語]"]);
        // `<` followed by multibyte characters and no `>` within reach.
        assert_eq!(plain("a<ééééééééééé"), vec!["a<ééééééééééé"]);
        assert_eq!(plain("a<日日日日日>b"), vec!["a<日日日日日>b"]);
        assert_eq!(synced("[00:01.00]🎵"), vec![line(1_000, "🎵")]);
        assert_eq!(plain("[🎵]"), vec!["[🎵]"]);
        assert_eq!(plain("[🎵"), vec!["[🎵"]);
    }

    #[test]
    fn untimed_lines_are_dropped_once_a_timestamp_exists() {
        assert_eq!(
            synced("Credits: someone\n[00:01.00]a\nstray\n[00:02.00]b"),
            vec![line(1_000, "a"), line(2_000, "b")]
        );
    }

    #[test]
    fn plain_lyrics_keep_order_and_trim_blank_edges() {
        assert_eq!(
            plain("\n\n  \nfirst\n\nsecond\r\nthird\rfourth\n\n \n"),
            vec!["first", "", "second", "third", "fourth"]
        );
        assert_eq!(plain("\u{FEFF}only"), vec!["only"]);
    }

    #[test]
    fn absurd_timestamps_neither_panic_nor_wrap() {
        let lines = synced(
            "[99999999999999999999:00.00]x\n[1234567:00.00]y\n\
             [offset:99999999999999999999]\n[00:10.00]kept\n[999999:00.00]late",
        );
        assert_eq!(
            lines,
            vec![line(0, "kept"), line(999_999 * 60_000 - 3_600_000, "late"),]
        );
        assert_eq!(
            synced("[offset:-99999999999999999999999]\n[999999:59.999]a"),
            vec![line(999_999 * 60_000 + 59_999 + 3_600_000, "a")]
        );
        assert_eq!(parse_offset("99999999999999999999"), Some(MAX_OFFSET_MS));
        assert_eq!(parse_offset("-3600001"), Some(-MAX_OFFSET_MS));
        assert_eq!(parse_offset("+"), None);
        assert_eq!(parse_offset("1e3"), None);
        assert_eq!(parse_offset(""), None);
    }

    #[test]
    fn escape_sequences_are_sanitised() {
        let lines = synced("[00:01.00]\u{1b}[31mred\u{1b}[0m\u{7}\u{202E}x");
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0]
                .text
                .chars()
                .all(|c| !c.is_control() && c != '\u{202E}')
        );
        assert_eq!(lines[0].text, "[31mred [0m x");
        let plain = plain("a\u{1b}]0;title\u{7}b");
        assert!(plain[0].chars().all(|c| !c.is_control()));
    }

    #[test]
    fn current_and_next_change() {
        let lyrics =
            parse("[00:12.00]a\n[00:18.00]b\n[00:30.00]c").unwrap_or_else(|| panic!("no lyrics"));
        assert_eq!(lyrics.current(ms(0)), None);
        assert_eq!(lyrics.current(ms(11_999)), None);
        assert_eq!(lyrics.current(ms(12_000)), Some(0));
        assert_eq!(lyrics.current(ms(15_000)), Some(0));
        assert_eq!(lyrics.current(ms(18_000)), Some(1));
        assert_eq!(lyrics.current(ms(600_000)), Some(2));
        assert_eq!(lyrics.next_change(ms(0)), Some(ms(12_000)));
        assert_eq!(lyrics.next_change(ms(12_000)), Some(ms(18_000)));
        assert_eq!(lyrics.next_change(ms(29_999)), Some(ms(30_000)));
        assert_eq!(lyrics.next_change(ms(30_000)), None);

        let plain = parse("a\nb").unwrap_or_else(|| panic!("no lyrics"));
        assert_eq!(plain.current(ms(1_000)), None);
        assert_eq!(plain.next_change(ms(0)), None);
    }

    #[test]
    fn longest_word_timing_is_removed() {
        assert_eq!(
            synced("[00:01.00]<999999:59.999>a<0:00>b"),
            vec![line(1_000, "ab")]
        );
    }

    #[test]
    fn crafted_long_lines_stay_linear() {
        // Quadratic scanning would take minutes on these; linear takes milliseconds.
        let opens = format!("[00:01.00]{}>", "<".repeat(1 << 20));
        assert_eq!(synced(&opens)[0].text.chars().count(), text::MAX_TEXT_CHARS);
        let brackets = "[".repeat(1 << 20) + "]";
        assert_eq!(plain(&brackets)[0].chars().count(), text::MAX_TEXT_CHARS);
        let headers = "[ar:b]".repeat(1 << 18);
        assert_eq!(parse(&headers), None);
    }

    #[test]
    fn line_count_is_capped() {
        let numbered = |prefix: &str| {
            (0..MAX_LINES + 50).fold(String::new(), |mut out, i| {
                let _ = writeln!(out, "{prefix}line {i}");
                out
            })
        };
        let many = numbered("[00:00.00]");
        assert_eq!(synced(&many).len(), MAX_LINES);

        let stamps = "[00:00.00]".repeat(MAX_LINES * 2) + "x";
        assert_eq!(synced(&stamps).len(), MAX_LINES);

        let lines = plain(&numbered(""));
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[0], "line 0");
    }
}
