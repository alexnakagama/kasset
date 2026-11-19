//! Finding and reading the lyrics of one track: a `.lrc` sidecar file next to
//! it, else the lyrics in its tags.
//!
//! Rules (spec *Lyrics sources*, *Lyrics are untrusted input*):
//! - **Sidecar first.** The candidates are `<stem>.lrc`, then `<stem>.LRC`,
//!   then, only if both miss, any `<stem>.<lrc in any case>` found by listing
//!   the track's folder (at most [`MAX_DIR_ENTRIES`] entries, matches tried in
//!   name order). The stem is compared exactly; only the extension ignores case.
//! - A candidate that cannot be read — missing, a directory, a FIFO, a device,
//!   a dangling link, permission denied — counts as absent. The **first
//!   readable** one is used even if it holds no lyrics, so an empty `.lrc`
//!   deliberately hides the tag lyrics.
//! - **Tags otherwise**, through [`crate::library::lyrics_tag`].
//! - Sidecars are read through [`crate::bounded_read`]: never blocking, regular
//!   files only, at most [`MAX_LYRICS_BYTES`]. Tag text is cut to the same size
//!   (at a character boundary). A truncated sidecar shows what was read, minus
//!   its last, possibly cut, line.
//! - The tag fallback opens the track the same non-blocking, regular-file-only
//!   way (see `library::read_tagged`).
//! - Sidecar bytes are decoded as UTF-8 (BOM stripped), as UTF-16 LE/BE when
//!   they start with that byte-order mark, and lossily as UTF-8 otherwise, so
//!   undecodable bytes become U+FFFD.
//! - Everything runs inside [`panic_guard::quietly`]: a panic becomes "no
//!   lyrics". Nothing here writes anything.

use std::char::REPLACEMENT_CHARACTER;
use std::ffi::OsStr;
use std::fs;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use super::{Lyrics, parse};
use crate::bounded_read::read_limited;
use crate::library::lyrics_tag;
use crate::panic_guard;

/// Largest lyrics text read, from a sidecar file or a tag, in bytes (1 MiB).
pub const MAX_LYRICS_BYTES: u64 = 1024 * 1024;

/// Most folder entries looked at when searching for a sidecar whose extension
/// is in mixed case.
pub const MAX_DIR_ENTRIES: usize = 10_000;

/// The extension of a sidecar file, compared ignoring ASCII case.
const SIDECAR_EXTENSION: &str = "lrc";

/// The lyrics of the track at `track`, from its sidecar file or its tags.
/// `None` when it has none, or they cannot be read or parsed.
#[must_use]
pub fn load(track: &Path) -> Option<Lyrics> {
    // The closure only reads `track` and owns everything else, so no broken
    // invariant can be observed after an unwind. Requires `panic = "unwind"`.
    panic_guard::quietly(AssertUnwindSafe(|| load_unguarded(track)))
        .ok()
        .flatten()
}

fn load_unguarded(track: &Path) -> Option<Lyrics> {
    match read_sidecar(track) {
        Some((bytes, truncated)) => parse(&decode_sidecar(&bytes, truncated)),
        None => parse(cap(&lyrics_tag(track)?)),
    }
}

/// Decodes sidecar bytes; for a truncated file, drops the last line, which
/// may be cut inside a character.
fn decode_sidecar(bytes: &[u8], truncated: bool) -> String {
    let mut text = decode(bytes);
    // `\r` too, for files with classic Mac (lone CR) line endings.
    if truncated && let Some(end) = text.rfind(['\n', '\r']) {
        text.truncate(end);
    }
    text
}

/// The bytes of the first readable sidecar of `track` (see the module doc),
/// and whether it was longer than [`MAX_LYRICS_BYTES`].
fn read_sidecar(track: &Path) -> Option<(Vec<u8>, bool)> {
    let direct = [
        track.with_extension(SIDECAR_EXTENSION),
        track.with_extension(SIDECAR_EXTENSION.to_ascii_uppercase()),
    ];
    if let Some(bytes) = direct.iter().find_map(|p| read(p)) {
        return Some(bytes);
    }
    // Compared by name: the listing may spell the folder differently
    // (`./song.lrc` for `song.lrc`).
    let tried: Vec<_> = direct.iter().filter_map(|p| p.file_name()).collect();
    mixed_case_sidecars(track, MAX_DIR_ENTRIES)
        .into_iter()
        .filter(|p| p.file_name().is_none_or(|name| !tried.contains(&name)))
        .find_map(|p| read(&p))
}

/// Reads `path` if it is a readable regular file, up to the size cap; also
/// tells whether the file was longer.
fn read(path: &Path) -> Option<(Vec<u8>, bool)> {
    read_limited(path, MAX_LYRICS_BYTES)
        .ok()
        .map(|(bytes, truncated, _)| (bytes, truncated))
}

/// Paths in `track`'s folder named `<stem>.<lrc in any case>`, sorted, from
/// the first `max_entries` entries the listing yields (unreadable entries
/// count too, so the work stays bounded).
fn mixed_case_sidecars(track: &Path, max_entries: usize) -> Vec<PathBuf> {
    let Some(stem) = track.file_stem() else {
        return Vec::new();
    };
    let dir = match track.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .take(max_entries)
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_sidecar_of(path, stem))
        .collect();
    found.sort();
    found
}

fn is_sidecar_of(path: &Path, stem: &OsStr) -> bool {
    path.file_stem() == Some(stem)
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(SIDECAR_EXTENSION))
}

/// Decodes sidecar bytes: UTF-8 with or without BOM, UTF-16 with a BOM, else
/// lossy UTF-8.
fn decode(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        String::from_utf8_lossy(rest).into_owned()
    } else if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        decode_utf16(rest, u16::from_le_bytes)
    } else if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        decode_utf16(rest, u16::from_be_bytes)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// Decodes UTF-16 code units built by `unit`; unpaired surrogates and a
/// trailing odd byte become U+FFFD.
fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let (pairs, rest) = bytes.as_chunks::<2>();
    let mut text: String = char::decode_utf16(pairs.iter().map(|&pair| unit(pair)))
        .map(|c| c.unwrap_or(REPLACEMENT_CHARACTER))
        .collect();
    if !rest.is_empty() {
        text.push(REPLACEMENT_CHARACTER);
    }
    text
}

/// `text` cut to at most [`MAX_LYRICS_BYTES`], at a character boundary.
fn cap(text: &str) -> &str {
    let limit = usize::try_from(MAX_LYRICS_BYTES).unwrap_or(usize::MAX);
    if text.len() <= limit {
        return text;
    }
    // A UTF-8 character is at most 4 bytes, so a boundary is at most 3 back.
    let end = (limit.saturating_sub(3)..=limit)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    text.get(..end).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lyrics::Line;
    use crate::test_support::flac_bytes;
    use anyhow::Result;
    use std::time::Duration;

    const SYNCED: &str = "[00:01.00]From the sidecar\n";

    fn synced(text: &str) -> Lyrics {
        Lyrics::Synced(vec![Line {
            at: Duration::from_secs(1),
            text: text.to_owned(),
        }])
    }

    fn plain(lines: &[&str]) -> Lyrics {
        Lyrics::Plain(lines.iter().map(|&l| l.to_owned()).collect())
    }

    /// A FLAC track `song.flac` in a fresh folder, with `lyrics` in its
    /// Vorbis comments if given.
    fn flac_track(lyrics: Option<&str>) -> Result<(tempfile::TempDir, PathBuf)> {
        use lofty::config::WriteOptions;
        use lofty::tag::{ItemKey, Tag, TagExt, TagType};

        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("song.flac");
        fs::write(&path, flac_bytes())?;
        if let Some(text) = lyrics {
            let mut tag = Tag::new(TagType::VorbisComments);
            tag.insert_text(ItemKey::Lyrics, text.to_owned());
            tag.save_to_path(&path, WriteOptions::default())?;
        }
        Ok((tmp, path))
    }

    const TAGGED: &str = "From the tags";

    #[test]
    fn sidecar_is_found() -> Result<()> {
        let (tmp, track) = flac_track(None)?;
        fs::write(tmp.path().join("song.lrc"), SYNCED)?;
        assert_eq!(load(&track), Some(synced("From the sidecar")));
        Ok(())
    }

    #[test]
    fn upper_and_mixed_case_extensions_are_found() -> Result<()> {
        for name in ["song.LRC", "song.Lrc", "song.lRc"] {
            let (tmp, track) = flac_track(None)?;
            fs::write(tmp.path().join(name), SYNCED)?;
            assert_eq!(load(&track), Some(synced("From the sidecar")), "{name}");
        }
        Ok(())
    }

    #[test]
    fn other_stems_are_not_sidecars() -> Result<()> {
        let (tmp, track) = flac_track(None)?;
        for name in ["song.flac.lrc", "song2.lrc", "song.lrc.txt", "song.lrcx"] {
            fs::write(tmp.path().join(name), SYNCED)?;
        }
        assert_eq!(load(&track), None);
        Ok(())
    }

    #[test]
    fn sidecar_wins_over_tags() -> Result<()> {
        let (tmp, track) = flac_track(Some(TAGGED))?;
        fs::write(tmp.path().join("song.lrc"), SYNCED)?;
        assert_eq!(load(&track), Some(synced("From the sidecar")));
        Ok(())
    }

    #[test]
    fn tag_lyrics_without_sidecar() -> Result<()> {
        let (_tmp, track) = flac_track(Some("[00:01.00]From the tags"))?;
        assert_eq!(load(&track), Some(synced(TAGGED)));
        Ok(())
    }

    #[test]
    fn empty_sidecar_hides_tag_lyrics() -> Result<()> {
        let (tmp, track) = flac_track(Some(TAGGED))?;
        fs::write(tmp.path().join("song.lrc"), "")?;
        assert_eq!(load(&track), None);
        Ok(())
    }

    #[test]
    fn directory_named_like_a_sidecar_is_ignored() -> Result<()> {
        let (tmp, track) = flac_track(Some(TAGGED))?;
        fs::create_dir(tmp.path().join("song.lrc"))?;
        assert_eq!(load(&track), Some(plain(&[TAGGED])));
        Ok(())
    }

    #[test]
    fn unreadable_sidecar_falls_through_to_the_next_candidate() -> Result<()> {
        // A directory `song.lrc` is skipped and `song.LRC` is used instead.
        let (tmp, track) = flac_track(Some(TAGGED))?;
        fs::create_dir(tmp.path().join("song.lrc"))?;
        let upper = tmp.path().join("song.LRC");
        if !upper.exists() {
            // Case-sensitive filesystem: both names can coexist.
            fs::write(&upper, SYNCED)?;
            assert_eq!(load(&track), Some(synced("From the sidecar")));
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_sidecar_falls_back_to_tags() -> Result<()> {
        let (tmp, track) = flac_track(Some(TAGGED))?;
        std::os::unix::fs::symlink(tmp.path().join("gone"), tmp.path().join("song.lrc"))?;
        assert_eq!(load(&track), Some(plain(&[TAGGED])));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fifo_sidecar_returns_promptly_and_falls_back() -> Result<()> {
        let (tmp, track) = flac_track(Some(TAGGED))?;
        if !crate::test_support::make_fifo(&tmp.path().join("song.lrc")) {
            return Ok(()); // mkfifo unavailable
        }
        let start = std::time::Instant::now();
        assert_eq!(load(&track), Some(plain(&[TAGGED])));
        assert!(start.elapsed() < Duration::from_secs(5));
        Ok(())
    }

    #[test]
    fn latin1_shows_replacement_characters() -> Result<()> {
        let (tmp, track) = flac_track(None)?;
        fs::write(tmp.path().join("song.lrc"), b"[00:01.00]caf\xe9\n")?;
        assert_eq!(load(&track), Some(synced("caf\u{FFFD}")));
        Ok(())
    }

    #[test]
    fn utf8_bom_is_stripped() -> Result<()> {
        let (tmp, track) = flac_track(None)?;
        fs::write(
            tmp.path().join("song.lrc"),
            b"\xEF\xBB\xBF[00:01.00]From the sidecar\n",
        )?;
        assert_eq!(load(&track), Some(synced("From the sidecar")));
        Ok(())
    }

    #[test]
    fn utf16_with_bom_decodes() -> Result<()> {
        let text = "[00:01.00]Caf\u{e9} \u{6b4c}\u{1F3B5}\n";
        for big_endian in [false, true] {
            let (tmp, track) = flac_track(None)?;
            let mut bytes = if big_endian {
                vec![0xFE, 0xFF]
            } else {
                vec![0xFF, 0xFE]
            };
            for unit in text.encode_utf16() {
                let pair = if big_endian {
                    unit.to_be_bytes()
                } else {
                    unit.to_le_bytes()
                };
                bytes.extend_from_slice(&pair);
            }
            fs::write(tmp.path().join("song.lrc"), bytes)?;
            assert_eq!(
                load(&track),
                Some(synced("Caf\u{e9} \u{6b4c}\u{1F3B5}")),
                "big endian: {big_endian}"
            );
        }
        Ok(())
    }

    #[test]
    fn broken_utf16_is_replaced_not_rejected() {
        // An unpaired surrogate and an odd trailing byte.
        let bytes = [0xFF, 0xFE, b'a', 0, 0x00, 0xD8, b'b', 0, b'c'];
        assert_eq!(decode(&bytes), "a\u{FFFD}b\u{FFFD}");
    }

    #[test]
    fn large_sidecar_is_read_up_to_the_cap() -> Result<()> {
        let (tmp, track) = flac_track(None)?;
        let line = format!("{}\n", "x".repeat(199));
        let mut text = String::from("HEAD\n");
        while text.len() < 2 * 1024 * 1024 {
            text.push_str(&line);
        }
        text.push_str("TAIL\n");
        let path = tmp.path().join("song.lrc");
        fs::write(&path, &text)?;

        let (read, truncated) = read_sidecar(&track).unwrap_or_default();
        assert_eq!(read.len() as u64, MAX_LYRICS_BYTES);
        assert!(truncated);
        let Some(Lyrics::Plain(lines)) = load(&track) else {
            anyhow::bail!("expected plain lyrics");
        };
        assert_eq!(lines.first().map(String::as_str), Some("HEAD"));
        assert!(!lines.iter().any(|l| l == "TAIL"));
        Ok(())
    }

    #[test]
    fn truncated_sidecar_drops_its_cut_last_line() {
        // Cut inside a 3-byte character.
        let bytes = b"[00:01.00]One\n[00:02.00]Tw\xE6\xAD";
        assert_eq!(decode_sidecar(bytes, true), "[00:01.00]One");
        assert_eq!(
            decode_sidecar(bytes, false),
            "[00:01.00]One\n[00:02.00]Tw\u{FFFD}"
        );
        // Lone CR (classic Mac) and CRLF endings.
        assert_eq!(
            decode_sidecar(b"[00:01.00]One\r[00:02.00]Tw", true),
            "[00:01.00]One"
        );
        assert_eq!(
            decode_sidecar(b"[00:01.00]One\r\n[00:02.00]Tw", true),
            "[00:01.00]One\r",
            "the parser drops the CR of a CRLF ending"
        );
        // A single cut line is kept as it is.
        assert_eq!(decode_sidecar(b"abc", true), "abc");
    }

    #[test]
    fn directory_scan_stops_after_the_entry_limit() -> Result<()> {
        let (tmp, track) = flac_track(None)?;
        for i in 0..20 {
            fs::write(tmp.path().join(format!("other{i:02}.txt")), "")?;
        }
        fs::write(tmp.path().join("song.Lrc"), SYNCED)?;
        // 22 entries: the track, the sidecar and 20 others.
        assert_eq!(
            mixed_case_sidecars(&track, 22),
            vec![tmp.path().join("song.Lrc")]
        );
        // With a limit of 1 the listing may stop before the sidecar; it must
        // never find more than the entries it looked at allow.
        assert!(mixed_case_sidecars(&track, 1).len() <= 1);
        assert_eq!(mixed_case_sidecars(&track, 0), Vec::<PathBuf>::new());
        Ok(())
    }

    #[test]
    fn several_mixed_case_sidecars_are_tried_in_name_order() -> Result<()> {
        let (tmp, track) = flac_track(None)?;
        let lower = tmp.path().join("song.lrc");
        fs::write(tmp.path().join("song.Lrc"), "[00:01.00]A\n")?;
        fs::write(tmp.path().join("song.lRc"), "[00:01.00]B\n")?;
        if lower.exists() {
            return Ok(()); // case-insensitive filesystem: only one can exist
        }
        assert_eq!(load(&track), Some(synced("A")));
        Ok(())
    }

    #[test]
    fn cap_cuts_at_a_char_boundary() {
        let limit = usize::try_from(MAX_LYRICS_BYTES).unwrap_or(usize::MAX);
        let short = "abc";
        assert_eq!(cap(short), short);
        // A 3-byte character straddling the limit is dropped whole.
        let long = format!("{}\u{6b4c}tail", "a".repeat(limit - 1));
        let capped = cap(&long);
        assert_eq!(capped.len(), limit - 1);
        assert!(capped.bytes().all(|b| b == b'a'));
        let exact = "a".repeat(limit + 10);
        assert_eq!(cap(&exact).len(), limit);
    }

    #[test]
    fn track_without_lyrics_or_folder_gives_none() -> Result<()> {
        let (_tmp, track) = flac_track(None)?;
        assert_eq!(load(&track), None);
        assert_eq!(load(Path::new("/nonexistent/kasset/song.flac")), None);
        assert_eq!(load(Path::new("")), None);
        Ok(())
    }
}
