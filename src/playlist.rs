//! Playlists: `.m3u8` files kept in the `Playlists` folder of the library.
//!
//! A playlist is held as the raw lines of its file, each with its own line
//! ending, so that saving after an edit changes only the lines the edit
//! touched: comments, extended-M3U tags, duplicates, entries that do not match
//! the library, a byte order mark and a missing final newline all survive.
//! Files that cannot be rewritten without losing data (not UTF-8, or over the
//! size limits) are still parsed for playback but every edit is refused.
//!
//! Playlist files are untrusted input: their size and entry count are bounded,
//! entry paths are only ever resolved (never opened here) and names are
//! validated before they become file names. Text derived from them must go
//! through [`crate::text::sanitize`] before it is displayed.

mod name;
mod paths;
mod store;

use std::collections::HashSet;
use std::fmt;
use std::fs::Metadata;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::library::Track;
use crate::text::sanitize;

pub use name::{MAX_NAME_BYTES, MAX_NAME_CHARS, NameError, check_new_name, validate_name};
pub use paths::{Matching, Resolver};
pub use store::{FsStore, PlaylistInfo, PlaylistStore, SaveOutcome, StoreError};

/// Folder inside the library root that holds the playlists.
pub const PLAYLISTS_DIR: &str = "Playlists";

/// Extension of playlist files (without the dot).
pub const EXTENSION: &str = "m3u8";

/// Files larger than this are read only up to here and are read-only.
pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// Playlists with more entries are read only up to here and are read-only;
/// additions that would go past it are refused.
pub const MAX_ENTRIES: usize = 100_000;

const BOM: &[u8] = b"\xEF\xBB\xBF";
const HEADER: &[u8] = b"#EXTM3U";
const EXTINF: &[u8] = b"#EXTINF";

/// How a line ends in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Lf,
    CrLf,
    /// Only the last line of a file without a final newline.
    None,
}

impl Ending {
    const fn bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
            Self::None => b"",
        }
    }
}

/// What a line means to the player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A path, with the library track it matched, if any.
    Entry(Option<usize>),
    /// An `#EXTINF` tag, describing the entry on the next line.
    ExtInf,
    /// Blank lines, comments and every other tag.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    /// The line without its ending.
    bytes: Vec<u8>,
    ending: Ending,
    kind: Kind,
}

impl Line {
    const fn new(bytes: Vec<u8>, kind: Kind) -> Self {
        Self {
            bytes,
            // Set to the file's ending when the line is inserted.
            ending: Ending::None,
            kind,
        }
    }
}

/// Why a playlist cannot be edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyReason {
    /// The file is not valid UTF-8; rewriting it could corrupt its text.
    NotUtf8,
    /// The file is over [`MAX_FILE_BYTES`] or [`MAX_ENTRIES`]; it was read only
    /// in part, so rewriting it would drop the rest.
    TooLarge,
}

impl fmt::Display for ReadOnlyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotUtf8 => "the playlist is not UTF-8 and cannot be edited",
            Self::TooLarge => "the playlist is over 16 MiB or 100 000 entries and cannot be edited",
        })
    }
}

/// Why an edit was refused. The playlist is unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditError {
    ReadOnly(ReadOnlyReason),
    /// The addition would take the playlist past [`MAX_ENTRIES`].
    TooManyEntries,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadOnly(reason) => reason.fmt(f),
            Self::TooManyEntries => write!(
                f,
                "the playlist would exceed {} entries",
                group_thousands(MAX_ENTRIES)
            ),
        }
    }
}

impl std::error::Error for EditError {}

/// What an addition did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AddOutcome {
    pub added: usize,
    /// Tracks already in the playlist (or given twice).
    pub skipped: usize,
    /// Tracks whose path cannot be written into a UTF-8 playlist line (not
    /// UTF-8, or containing a line break).
    pub unrepresentable: usize,
}

/// Identity of the file contents as last read or written, to detect changes
/// made by other programs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    /// `(device, inode)`: catches replacements by editors that save atomically.
    id: Option<(u64, u64)>,
}

impl FileStamp {
    fn of(meta: &Metadata) -> Self {
        #[cfg(unix)]
        let id = {
            use std::os::unix::fs::MetadataExt;
            Some((meta.dev(), meta.ino()))
        };
        #[cfg(not(unix))]
        let id = None;
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
            id,
        }
    }
}

/// One playlist file, as read from disk (or about to be created).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    name: String,
    /// The playlist's own path in the playlists folder (a symlink is not resolved).
    path: PathBuf,
    bom: bool,
    lines: Vec<Line>,
    editable: Result<(), ReadOnlyReason>,
    /// `None` until the file exists on disk.
    stamp: Option<FileStamp>,
}

impl Playlist {
    /// An empty, editable playlist that does not exist on disk yet.
    fn blank(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            bom: false,
            lines: Vec::new(),
            editable: Ok(()),
            stamp: None,
        }
    }

    /// Parses file contents. `truncated` says the read stopped at
    /// [`MAX_FILE_BYTES`]; entries past [`MAX_ENTRIES`] are dropped. Either way
    /// the playlist becomes read-only.
    fn parse(
        name: String,
        path: PathBuf,
        content: &[u8],
        truncated: bool,
        resolver: &Resolver,
        matching: Matching,
    ) -> Self {
        Self::parse_with_limit(
            name,
            path,
            content,
            truncated,
            resolver,
            matching,
            MAX_ENTRIES,
        )
    }

    fn parse_with_limit(
        name: String,
        path: PathBuf,
        content: &[u8],
        mut truncated: bool,
        resolver: &Resolver,
        matching: Matching,
        max_entries: usize,
    ) -> Self {
        let (bom, body) = match content.strip_prefix(BOM) {
            Some(rest) => (true, rest),
            None => (false, content),
        };
        let utf8 = std::str::from_utf8(body).is_ok();

        let mut lines = Vec::new();
        let mut entries = 0usize;
        let mut rest = body;
        while !rest.is_empty() {
            let (raw, ending, next) = match rest.iter().position(|&b| b == b'\n') {
                Some(i) => {
                    let (line, ending) = match rest[..i].strip_suffix(b"\r") {
                        Some(line) => (line, Ending::CrLf),
                        None => (&rest[..i], Ending::Lf),
                    };
                    (line, ending, &rest[i + 1..])
                }
                None => (rest, Ending::None, &rest[rest.len()..]),
            };
            let kind = classify(raw);
            if matches!(kind, Kind::Entry(_)) {
                if entries == max_entries {
                    truncated = true;
                    break;
                }
                entries += 1;
            }
            lines.push(Line {
                bytes: raw.to_vec(),
                ending,
                kind,
            });
            rest = next;
        }
        // A read cut at the byte limit ends mid-line: that fragment is not an entry.
        if truncated && lines.last().is_some_and(|l| l.ending == Ending::None) {
            lines.pop();
        }

        let editable = if truncated {
            Err(ReadOnlyReason::TooLarge)
        } else if utf8 {
            Ok(())
        } else {
            Err(ReadOnlyReason::NotUtf8)
        };

        let mut playlist = Self {
            name,
            path,
            bom,
            lines,
            editable,
            stamp: None,
        };
        playlist.resolve(resolver, matching);
        playlist
    }

    /// Matches every entry against the library.
    fn resolve(&mut self, resolver: &Resolver, matching: Matching) {
        let folder = self.folder().to_owned();
        for line in &mut self.lines {
            if let Kind::Entry(resolved) = &mut line.kind {
                *resolved = resolver.resolve_with(&folder, &line.bytes, matching);
            }
        }
    }

    /// The file contents: BOM, then every line with its own ending.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let len = self.lines.iter().map(|l| l.bytes.len() + 2).sum::<usize>() + BOM.len();
        let mut out = Vec::with_capacity(len);
        if self.bom {
            out.extend_from_slice(BOM);
        }
        for line in &self.lines {
            out.extend_from_slice(&line.bytes);
            out.extend_from_slice(line.ending.bytes());
        }
        out
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The folder entries are relative to.
    fn folder(&self) -> &Path {
        self.path.parent().unwrap_or_else(|| Path::new(""))
    }

    /// `Ok` when the playlist can be edited, else why not.
    ///
    /// # Errors
    ///
    /// Returns the reason the playlist is read-only.
    pub const fn editable(&self) -> Result<(), ReadOnlyReason> {
        self.editable
    }

    #[must_use]
    pub const fn stamp(&self) -> Option<FileStamp> {
        self.stamp
    }

    /// Library indices of the matched entries, in playlist order (duplicates kept).
    #[must_use]
    pub fn tracks(&self) -> Vec<usize> {
        self.lines
            .iter()
            .filter_map(|l| match l.kind {
                Kind::Entry(resolved) => resolved,
                _ => None,
            })
            .collect()
    }

    /// Entries that match no library track.
    #[must_use]
    pub fn unmatched(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| l.kind == Kind::Entry(None))
            .count()
    }

    /// Every entry, matched or not.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l.kind, Kind::Entry(_)))
            .count()
    }

    fn check_editable(&self) -> Result<(), EditError> {
        self.editable.map_err(EditError::ReadOnly)
    }

    /// Line indices of matched entries, i.e. of the rows the user sees.
    fn matched_lines(&self) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| matches!(l.kind, Kind::Entry(Some(_))))
            .map(|(i, _)| i)
            .collect()
    }

    /// The entry at `line` plus the `#EXTINF` right before it, if any.
    fn block(&self, line: usize) -> Range<usize> {
        let start = if line > 0 && self.lines[line - 1].kind == Kind::ExtInf {
            line - 1
        } else {
            line
        };
        start..line + 1
    }

    /// The ending of the first line, used for new lines.
    fn default_ending(&self) -> Ending {
        match self.lines.first().map(|l| l.ending) {
            Some(Ending::CrLf) => Ending::CrLf,
            _ => Ending::Lf,
        }
    }

    fn has_final_newline(&self) -> bool {
        self.lines.last().is_none_or(|l| l.ending != Ending::None)
    }

    /// Restores the invariant that only the last line lacks an ending, and
    /// gives the last line a newline or not as requested.
    fn fix_endings(&mut self, default: Ending, final_newline: bool) {
        let last = self.lines.len().saturating_sub(1);
        for (i, line) in self.lines.iter_mut().enumerate() {
            if i < last || final_newline {
                if line.ending == Ending::None {
                    line.ending = default;
                }
            } else {
                line.ending = Ending::None;
            }
        }
    }

    /// Appends the given library tracks, each with an `#EXTINF` line. Tracks
    /// already in the playlist are skipped; the whole addition is refused if
    /// it would exceed [`MAX_ENTRIES`].
    ///
    /// # Errors
    ///
    /// Fails, changing nothing, when the playlist is read-only or would grow
    /// past the entry limit.
    pub fn add(&mut self, indices: &[usize], library: &[Track]) -> Result<AddOutcome, EditError> {
        self.check_editable()?;
        let present: HashSet<usize> = self.tracks().into_iter().collect();
        let mut seen = HashSet::new();
        let mut outcome = AddOutcome::default();
        let mut new_lines = Vec::new();
        let folder = self.folder().to_owned();

        for &index in indices {
            let Some(track) = library.get(index) else {
                continue;
            };
            if present.contains(&index) || !seen.insert(index) {
                outcome.skipped += 1;
                continue;
            }
            let Some(entry) = paths::entry_for(&folder, &track.path) else {
                outcome.unrepresentable += 1;
                continue;
            };
            new_lines.push(Line::new(extinf(track), Kind::ExtInf));
            new_lines.push(Line::new(entry, Kind::Entry(Some(index))));
            outcome.added += 1;
        }

        if self.entry_count().saturating_add(outcome.added) > MAX_ENTRIES {
            return Err(EditError::TooManyEntries);
        }
        if outcome.added == 0 {
            return Ok(outcome);
        }
        if self.lines.is_empty() {
            self.lines.push(Line::new(HEADER.to_vec(), Kind::Other));
        }
        let default = self.default_ending();
        self.lines.extend(new_lines);
        self.fix_endings(default, true);
        Ok(outcome)
    }

    /// Removes the matched entry at `row` (an index into [`Self::tracks`]),
    /// with its `#EXTINF` line. Returns `false` when `row` is out of range.
    ///
    /// # Errors
    ///
    /// Fails, changing nothing, when the playlist is read-only.
    pub fn remove(&mut self, row: usize) -> Result<bool, EditError> {
        self.check_editable()?;
        let Some(&line) = self.matched_lines().get(row) else {
            return Ok(false);
        };
        let default = self.default_ending();
        let final_newline = self.has_final_newline();
        let block = self.block(line);
        self.lines.drain(block);
        self.fix_endings(default, final_newline);
        Ok(true)
    }

    /// Swaps the matched entry at `row` with the next (`down`) or previous
    /// matched entry; unmatched entries and other lines in between stay where
    /// they are. Returns `false` when there is no entry to swap with.
    ///
    /// # Errors
    ///
    /// Fails, changing nothing, when the playlist is read-only.
    pub fn move_entry(&mut self, row: usize, down: bool) -> Result<bool, EditError> {
        self.check_editable()?;
        let matched = self.matched_lines();
        let other = if down {
            row.checked_add(1)
        } else {
            row.checked_sub(1)
        };
        let (Some(&a), Some(&b)) = (matched.get(row), other.and_then(|o| matched.get(o))) else {
            return Ok(false);
        };
        let (first, second) = if a < b { (a, b) } else { (b, a) };
        let default = self.default_ending();
        let final_newline = self.has_final_newline();
        let (first, second) = (self.block(first), self.block(second));

        let mut tail = self.lines.split_off(first.start);
        let after = tail.split_off(second.end - first.start);
        let second_block = tail.split_off(second.start - first.start);
        let middle = tail.split_off(first.end - first.start);
        let first_block = tail;
        self.lines.extend(second_block);
        self.lines.extend(middle);
        self.lines.extend(first_block);
        self.lines.extend(after);

        self.fix_endings(default, final_newline);
        Ok(true)
    }
}

/// Classifies a line (without its ending).
fn classify(raw: &[u8]) -> Kind {
    let trimmed = raw.trim_ascii();
    match trimmed.first() {
        None => Kind::Other,
        Some(b'#') => {
            if trimmed.len() >= EXTINF.len() && trimmed[..EXTINF.len()].eq_ignore_ascii_case(EXTINF)
            {
                Kind::ExtInf
            } else {
                Kind::Other
            }
        }
        Some(_) => Kind::Entry(None),
    }
}

/// `#EXTINF:<seconds>,<artist> - <title>` for a new entry.
fn extinf(track: &Track) -> Vec<u8> {
    let secs = track
        .duration
        .map_or_else(|| "-1".to_owned(), |d| d.as_secs().to_string());
    // Track text is sanitised at scan time; sanitising again guarantees that no
    // line break can reach the file whatever the caller passed.
    let artist = sanitize(&track.artist).unwrap_or_default();
    let title = sanitize(&track.title).unwrap_or_default();
    format!("#EXTINF:{secs},{artist} - {title}").into_bytes()
}

/// `100000` → `"100 000"`.
fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const ROOT: &str = "/music";

    fn track(rel: &str) -> Track {
        Track {
            path: Path::new(ROOT).join(rel),
            title: rel.to_owned(),
            artist: "Band".to_owned(),
            album: "LP".to_owned(),
            number: None,
            duration: Some(Duration::from_secs(185)),
        }
    }

    fn library() -> Vec<Track> {
        ["A.flac", "B.flac", "C.flac", "D.flac"]
            .into_iter()
            .map(track)
            .collect()
    }

    fn playlist_path() -> PathBuf {
        Path::new(ROOT).join(PLAYLISTS_DIR).join("Running.m3u8")
    }

    fn parse(content: &[u8], library: &[Track]) -> Playlist {
        Playlist::parse(
            "Running".to_owned(),
            playlist_path(),
            content,
            false,
            &Resolver::new(library),
            Matching::Full,
        )
    }

    fn titles(p: &Playlist, library: &[Track]) -> Vec<String> {
        p.tracks()
            .into_iter()
            .map(|i| library[i].title.clone())
            .collect()
    }

    const LF_FILE: &[u8] = b"#EXTM3U\n# a comment\n#EXTINF:1,Band - A\n../A.flac\n../missing.flac\n\n../B.flac\n../A.flac\n";

    #[test]
    fn round_trip_is_byte_identical() {
        let lib = library();
        let crlf_bom =
            b"\xEF\xBB\xBF#EXTM3U\r\n#EXTINF:1,x\r\n../A.flac\r\n../gone.mp3\r\n../A.flac\r\n";
        let no_final = b"#EXTM3U\n../B.flac\n# trailing comment\n../nope.flac";
        let mixed = b"../A.flac\r\n../B.flac\n\r\n  \n../C.flac";
        for content in [
            LF_FILE,
            &crlf_bom[..],
            &no_final[..],
            &mixed[..],
            b"",
            b"\n",
            b"\xEF\xBB\xBF",
        ] {
            let p = parse(content, &lib);
            assert_eq!(
                p.to_bytes(),
                content,
                "{:?}",
                String::from_utf8_lossy(content)
            );
            assert_eq!(p.editable(), Ok(()));
        }
    }

    #[test]
    fn parses_entries_duplicates_and_unmatched() {
        let lib = library();
        let p = parse(LF_FILE, &lib);
        assert_eq!(titles(&p, &lib), ["A.flac", "B.flac", "A.flac"]);
        assert_eq!(p.unmatched(), 1);
        assert_eq!(p.entry_count(), 4);
    }

    #[test]
    fn classify_lines() {
        assert_eq!(classify(b""), Kind::Other);
        assert_eq!(classify(b"   \t"), Kind::Other);
        assert_eq!(classify(b"#EXTM3U"), Kind::Other);
        assert_eq!(classify(b"#extinf:3,a"), Kind::ExtInf);
        assert_eq!(classify(b"# comment"), Kind::Other);
        assert_eq!(classify(b"song.mp3"), Kind::Entry(None));
    }

    #[test]
    fn append_to_crlf_file_without_final_newline_uses_crlf_everywhere() -> Result<(), EditError> {
        let lib = library();
        let mut p = parse(b"\xEF\xBB\xBF#EXTM3U\r\n../A.flac", &lib);
        let out = p.add(&[1], &lib)?;
        assert_eq!(out.added, 1);
        let bytes = p.to_bytes();
        assert!(bytes.starts_with(BOM));
        assert_eq!(
            bytes,
            b"\xEF\xBB\xBF#EXTM3U\r\n../A.flac\r\n#EXTINF:185,Band - B.flac\r\n../B.flac\r\n"
        );
        Ok(())
    }

    #[test]
    fn add_to_new_playlist_writes_header_and_relative_entries() -> Result<(), EditError> {
        let lib = vec![track("Band/LP/01 Song.flac")];
        let mut p = Playlist::blank("Running".to_owned(), playlist_path());
        p.add(&[0], &lib)?;
        assert_eq!(
            p.to_bytes(),
            b"#EXTM3U\n#EXTINF:185,Band - Band/LP/01 Song.flac\n../Band/LP/01 Song.flac\n"
        );
        Ok(())
    }

    #[test]
    fn add_skips_present_and_repeated_tracks() -> Result<(), EditError> {
        let lib = library();
        let mut p = parse(b"../A.flac\n", &lib);
        let out = p.add(&[0, 2, 2, 3], &lib)?;
        assert_eq!(
            out,
            AddOutcome {
                added: 2,
                skipped: 2,
                unrepresentable: 0
            }
        );
        assert_eq!(titles(&p, &lib), ["A.flac", "C.flac", "D.flac"]);

        let before = p.to_bytes();
        let out = p.add(&[0], &lib)?;
        assert_eq!(out.added, 0);
        assert_eq!(out.skipped, 1);
        assert_eq!(p.to_bytes(), before);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn add_refuses_paths_that_cannot_be_written() -> Result<(), EditError> {
        use std::os::unix::ffi::OsStrExt;
        let mut lib = library();
        lib.push(Track {
            path: Path::new(ROOT).join(std::ffi::OsStr::from_bytes(b"caf\xE9.mp3")),
            ..track("x")
        });
        lib.push(track("line\nbreak.mp3"));
        let mut p = parse(b"", &lib);
        let out = p.add(&[4, 5, 0], &lib)?;
        assert_eq!(out.added, 1);
        assert_eq!(out.unrepresentable, 2);
        assert!(std::str::from_utf8(&p.to_bytes()).is_ok());
        Ok(())
    }

    #[test]
    fn entry_limit_refuses_whole_addition() {
        let lib = library();
        let content = b"../x.flac\n".repeat(MAX_ENTRIES - 1);
        let mut p = parse(&content, &lib);
        assert_eq!(p.editable(), Ok(()));
        let before = p.to_bytes();
        assert_eq!(p.add(&[0, 1], &lib), Err(EditError::TooManyEntries));
        assert_eq!(p.to_bytes(), before);
        // Exactly up to the limit is fine.
        assert_eq!(p.add(&[0], &lib).map(|o| o.added), Ok(1));
        assert_eq!(p.entry_count(), MAX_ENTRIES);
    }

    #[test]
    fn over_entry_limit_is_read_only() {
        let lib = library();
        let content = b"../x.flac\n".repeat(MAX_ENTRIES + 1);
        let mut p = parse(&content, &lib);
        assert_eq!(p.editable(), Err(ReadOnlyReason::TooLarge));
        assert_eq!(p.entry_count(), MAX_ENTRIES);
        assert_eq!(
            p.add(&[0], &lib),
            Err(EditError::ReadOnly(ReadOnlyReason::TooLarge))
        );
    }

    #[test]
    fn byte_truncated_read_is_read_only_and_drops_partial_line() {
        let lib = library();
        let p = Playlist::parse(
            "R".to_owned(),
            playlist_path(),
            b"../A.flac\n../B.fl",
            true,
            &Resolver::new(&lib),
            Matching::Full,
        );
        assert_eq!(p.editable(), Err(ReadOnlyReason::TooLarge));
        assert_eq!(titles(&p, &lib), ["A.flac"]);
        assert_eq!(p.entry_count(), 1);
    }

    #[test]
    fn small_entry_limit_truncates() {
        let lib = library();
        let p = Playlist::parse_with_limit(
            "R".to_owned(),
            playlist_path(),
            b"../A.flac\n#c\n../B.flac\n../C.flac\n",
            false,
            &Resolver::new(&lib),
            Matching::Full,
            2,
        );
        assert_eq!(p.editable(), Err(ReadOnlyReason::TooLarge));
        assert_eq!(titles(&p, &lib), ["A.flac", "B.flac"]);
    }

    #[test]
    fn latin1_file_parses_and_refuses_every_edit() {
        let lib = library();
        let content = b"#EXTINF:1,Caf\xE9\n../A.flac\n../B.flac\n";
        let mut p = parse(content, &lib);
        assert_eq!(p.editable(), Err(ReadOnlyReason::NotUtf8));
        assert_eq!(titles(&p, &lib), ["A.flac", "B.flac"]);
        let err = Err(EditError::ReadOnly(ReadOnlyReason::NotUtf8));
        assert_eq!(p.add(&[2], &lib), err.map(|()| AddOutcome::default()));
        assert_eq!(p.remove(0), err.map(|()| false));
        assert_eq!(p.move_entry(0, true), err.map(|()| false));
        assert_eq!(p.move_entry(1, false), err.map(|()| false));
        assert_eq!(p.to_bytes(), content);
        assert!(
            EditError::ReadOnly(ReadOnlyReason::NotUtf8)
                .to_string()
                .contains("not UTF-8")
        );
    }

    #[test]
    fn remove_takes_extinf_and_keeps_unmatched_in_place() -> Result<(), EditError> {
        let lib = library();
        let mut p = parse(LF_FILE, &lib);
        assert!(p.remove(0)?);
        assert_eq!(
            p.to_bytes(),
            b"#EXTM3U\n# a comment\n../missing.flac\n\n../B.flac\n../A.flac\n"
        );
        assert_eq!(titles(&p, &lib), ["B.flac", "A.flac"]);
        assert_eq!(p.unmatched(), 1);
        assert!(!p.remove(5)?);
        Ok(())
    }

    #[test]
    fn remove_last_line_keeps_missing_final_newline() -> Result<(), EditError> {
        let lib = library();
        let mut p = parse(b"../A.flac\r\n../B.flac", &lib);
        assert!(p.remove(1)?);
        assert_eq!(p.to_bytes(), b"../A.flac");
        let mut p = parse(b"../A.flac\r\n../B.flac\r\n", &lib);
        assert!(p.remove(1)?);
        assert_eq!(p.to_bytes(), b"../A.flac\r\n");
        Ok(())
    }

    #[test]
    fn remove_only_the_selected_duplicate() -> Result<(), EditError> {
        let lib = library();
        let mut p = parse(b"../A.flac\n../B.flac\n../A.flac\n", &lib);
        assert!(p.remove(2)?);
        assert_eq!(p.to_bytes(), b"../A.flac\n../B.flac\n");
        Ok(())
    }

    #[test]
    fn move_swaps_blocks_and_leaves_unmatched_in_place() -> Result<(), EditError> {
        let lib = library();
        let mut p = parse(LF_FILE, &lib);
        // Rows: A(with EXTINF), B, A. Move the first A down past B.
        assert!(p.move_entry(0, true)?);
        assert_eq!(
            p.to_bytes(),
            b"#EXTM3U\n# a comment\n../B.flac\n../missing.flac\n\n#EXTINF:1,Band - A\n../A.flac\n../A.flac\n"
        );
        assert_eq!(titles(&p, &lib), ["B.flac", "A.flac", "A.flac"]);
        // And back up again restores the file.
        assert!(p.move_entry(1, false)?);
        assert_eq!(p.to_bytes(), LF_FILE);
        Ok(())
    }

    #[test]
    fn move_past_either_end_does_nothing() -> Result<(), EditError> {
        let lib = library();
        let mut p = parse(b"../A.flac\n../B.flac\n", &lib);
        assert!(!p.move_entry(0, false)?);
        assert!(!p.move_entry(1, true)?);
        assert!(!p.move_entry(9, true)?);
        assert_eq!(p.to_bytes(), b"../A.flac\n../B.flac\n");
        Ok(())
    }

    #[test]
    fn move_of_last_line_without_newline_keeps_file_well_formed() -> Result<(), EditError> {
        let lib = library();
        let mut p = parse(b"../A.flac\r\n../B.flac", &lib);
        assert!(p.move_entry(1, false)?);
        assert_eq!(p.to_bytes(), b"../B.flac\r\n../A.flac");
        Ok(())
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(100_000), "100 000");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1_000), "1 000");
    }
}
