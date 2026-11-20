//! Saved session state: the model and the on-disk format, with no I/O.
//!
//! kasset remembers the player [`Settings`] (shared by every library folder)
//! and, per library folder, a [`FolderEntry`]: the [`Resume`] point (track,
//! queue item, position, play source) and the playlist shown in the main pane.
//! [`parse`] reads the state file, [`render`] writes it and [`merge`] folds one
//! run's [`Session`] into what is on disk. Reading and writing the file itself
//! is the job of [`store`]; this module is pure.
//!
//! Format (D3 of the `persist-session-state` design), one `key value` per line:
//!
//! ```text
//! kasset-state 1
//! volume 60
//! shuffle on
//! repeat all
//! visualizer off
//! lyrics on
//! folder /home/user/M%C3%BAsica
//! track Band/LP/01%20Song.flac
//! item 3
//! position-ms 135000
//! source playlist Running
//! open Running
//! folder /mnt/other
//! ```
//!
//! - Line 1 is the header. A missing or malformed header, or version 0, is
//!   [`Parsed::Invalid`]; a version above [`FORMAT_VERSION`] is
//!   [`Parsed::Newer`].
//! - Settings come before the first `folder` line; each `folder` line starts
//!   an entry that owns the lines after it. Folders are most recently used
//!   first, at most [`MAX_FOLDERS`].
//! - Paths and names are percent-encoded: every byte outside `0x21..=0x7E`,
//!   and `%`, is written `%XX`, so any value fits on one printable line. On
//!   Unix paths are encoded from their raw bytes; elsewhere from UTF-8, and a
//!   path that is not valid Unicode is not saved.
//!
//! Invariants:
//! - the file is **untrusted**: parsing is line by line and forgiving. An
//!   unknown key, a bad value or bad escape is ignored on its own, a repeated
//!   key keeps its first valid value, a trailing `\r` is ignored, settings
//!   after the first `folder` line are ignored, and a second entry for a root
//!   already seen is ignored. Parsing is linear in the input, every number is
//!   parsed with checked arithmetic, and at most [`MAX_FOLDERS`] entries are
//!   kept, so crafted input can never panic, wrap or stall;
//! - a resume point names its track only as a [`RelPath`]: plain components
//!   relative to the library root, never empty, `.`, `..`, a root or a
//!   prefix, and never containing a separator. It is only ever compared with
//!   the relative paths of scanned tracks, never joined onto a path;
//! - playlist names (`open`, `source playlist`) are UTF-8 of at most
//!   [`MAX_NAME_BYTES`] bytes that could be one file name component (see
//!   [`valid_name`]); they may hold control characters, as file names can.
//!   They are only ever matched exactly against the playlist listing, never
//!   used as paths, and sanitised before they are shown;
//! - a position is at most [`MAX_POSITION`]; a larger one is ignored on read
//!   and clamped on write;
//! - a [`RootKey`] is the canonical root's bytes and is compared exactly;
//! - [`render`] never returns more than [`MAX_STATE_BYTES`], the read cap, and
//!   only writes values [`parse`] accepts, so what is written is always read
//!   back whole: an entry that does not fit is left out and later (older)
//!   ones that still fit are kept, and a first entry too large on its own
//!   loses its resume point, then its playlist, then is left out entirely.

pub mod store;

use std::path::{Component, Path};
use std::time::Duration;

use crate::player::Volume;
use crate::queue::Repeat;

/// The format version this build writes and understands.
pub const FORMAT_VERSION: u32 = 1;

/// Most library folders remembered; the least recently used is dropped first.
pub const MAX_FOLDERS: usize = 20;

/// Largest state file read, and therefore the most [`render`] ever writes.
pub const MAX_STATE_BYTES: usize = 64 * 1024;

/// Longest playlist name kept, in bytes (a file name limit on common filesystems).
pub const MAX_NAME_BYTES: usize = 255;

/// Latest resume position kept (one week), far beyond any real track.
pub const MAX_POSITION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// First word of the header line.
const HEADER: &[u8] = b"kasset-state";

/// The player settings shared by every library folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub volume: Volume,
    pub shuffle: bool,
    pub repeat: Repeat,
    pub visualizer: bool,
    pub lyrics: bool,
}

impl Default for Settings {
    /// The first-run settings: full volume, shuffle and repeat off, both panes on.
    fn default() -> Self {
        Self {
            volume: Volume::MAX,
            shuffle: false,
            repeat: Repeat::Off,
            visualizer: true,
            lyrics: true,
        }
    }
}

/// Where the resumed track was played from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SavedSource {
    /// The whole library (also how a detached play order is saved).
    #[default]
    Library,
    /// The playlist with exactly this name.
    Playlist(String),
}

/// The track to resume in one library folder, and how it was being played.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resume {
    /// The track, relative to the library root.
    pub track: RelPath,
    /// Its position in the source's item list, if known.
    pub item: Option<usize>,
    /// Where to resume; stored in whole milliseconds, at most
    /// [`MAX_POSITION`]. Build it in whole milliseconds, so a value read back
    /// compares equal to the one saved.
    pub position: Duration,
    /// What the play order was built from.
    pub source: SavedSource,
}

/// What is remembered for one library folder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FolderEntry {
    /// The track to resume, if one was current.
    pub resume: Option<Resume>,
    /// The playlist shown in the main pane, if any.
    pub open: Option<String>,
}

/// What one run saves: the settings, and the entry for its own folder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Session {
    pub settings: Settings,
    pub folder: FolderEntry,
}

/// A library root as saved: the canonical root path's bytes, compared exactly.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RootKey(Vec<u8>);

impl RootKey {
    /// The key of the canonical root `root`, or `None` for an empty path, or
    /// (outside Unix) one that is not valid Unicode.
    #[must_use]
    pub fn new(root: &Path) -> Option<Self> {
        let bytes = os_bytes(root.as_os_str())?;
        (!bytes.is_empty()).then(|| Self(bytes.to_vec()))
    }
}

/// A track's path relative to the library root: one or more plain components
/// joined with `/` (on every platform), each valid by [`valid_component`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RelPath(Vec<u8>);

impl RelPath {
    /// The relative path made of `path`'s components, or `None` if `path` is
    /// empty, absolute, has a prefix, `.` or `..`, or a component that cannot
    /// be saved (see [`valid_component`]).
    #[must_use]
    pub fn new(path: &Path) -> Option<Self> {
        let mut out = Vec::new();
        for component in path.components() {
            let Component::Normal(name) = component else {
                return None;
            };
            let bytes = os_bytes(name)?;
            if !valid_component(bytes) {
                return None;
            }
            if !out.is_empty() {
                out.push(b'/');
            }
            out.extend_from_slice(bytes);
        }
        (!out.is_empty()).then_some(Self(out))
    }

    /// Parses the encoded form, checking every component after decoding it, so
    /// an encoded separator (`%2F`) never splits or joins components.
    fn decode(encoded: &[u8]) -> Option<Self> {
        let mut out = Vec::with_capacity(encoded.len());
        for (i, part) in encoded.split(|&b| b == b'/').enumerate() {
            let bytes = decode(part)?;
            if !valid_component(&bytes) {
                return None;
            }
            if i > 0 {
                out.push(b'/');
            }
            out.extend_from_slice(&bytes);
        }
        Some(Self(out))
    }
}

/// The raw bytes of an OS string: exact on Unix, its UTF-8 form elsewhere
/// (`None` if it is not valid Unicode).
#[cfg_attr(
    unix,
    expect(clippy::unnecessary_wraps, reason = "fallible outside Unix")
)]
fn os_bytes(s: &std::ffi::OsStr) -> Option<&[u8]> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(s.as_bytes())
    }
    #[cfg(not(unix))]
    {
        s.to_str().map(str::as_bytes)
    }
}

/// Whether `bytes` can be one component of a [`RelPath`]: not empty, `.` or
/// `..`, and free of `/` and NUL. Outside Unix it must also be valid UTF-8
/// without `\` or `:` (a separator, or a drive or stream prefix on Windows).
fn valid_component(bytes: &[u8]) -> bool {
    let plain = !bytes.is_empty()
        && bytes != b"."
        && bytes != b".."
        && !bytes.iter().any(|&b| b == b'/' || b == 0);
    #[cfg(not(unix))]
    let plain = plain
        && std::str::from_utf8(bytes).is_ok()
        && !bytes.iter().any(|&b| b == b'\\' || b == b':');
    plain
}

/// Everything in the state file, as parsed or as about to be written.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StateFile {
    /// The settings, if the file had any valid settings line.
    pub settings: Option<Settings>,
    /// One entry per library root, most recently used first, roots unique.
    pub folders: Vec<(RootKey, FolderEntry)>,
}

/// The result of [`parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// A file of this format version.
    Valid(StateFile),
    /// A file from a newer kasset; it must not be overwritten.
    Newer,
    /// Not a state file (no or a malformed header); it may be replaced.
    Invalid,
}

/// Parses the state file's bytes. Never panics; see the module invariants.
#[must_use]
pub fn parse(bytes: &[u8]) -> Parsed {
    let mut lines = bytes
        .split(|&b| b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line));
    match lines.next().map(header_version) {
        Some(Version::Current) => {}
        Some(Version::Newer) => return Parsed::Newer,
        Some(Version::Invalid) | None => return Parsed::Invalid,
    }

    let mut settings = PartialSettings::default();
    let mut folders: Vec<(RootKey, FolderEntry)> = Vec::new();
    let mut section = Section::Settings;
    for line in lines {
        // A bare `folder` still ends the entry before it.
        let (key, value) = if line == b"folder" {
            (line, &[][..])
        } else if let Some(pair) = split_key(line) {
            pair
        } else {
            continue;
        };
        if key == b"folder" {
            section.finish(&mut folders);
            if folders.len() >= MAX_FOLDERS {
                section = Section::Skipped;
                break;
            }
            section = match decode(value) {
                Some(root) if !root.is_empty() && !folders.iter().any(|(r, _)| r.0 == root) => {
                    Section::Folder(RootKey(root), PartialEntry::default())
                }
                _ => Section::Skipped,
            };
            continue;
        }
        match &mut section {
            Section::Settings => settings.apply(key, value),
            Section::Folder(_, entry) => entry.apply(key, value),
            Section::Skipped => {}
        }
    }
    section.finish(&mut folders);

    Parsed::Valid(StateFile {
        settings: settings.finish(),
        folders,
    })
}

/// What a header line says about the file's version.
enum Version {
    Current,
    Newer,
    Invalid,
}

/// Reads `kasset-state <N>`: exactly that word, one space and decimal digits.
fn header_version(line: &[u8]) -> Version {
    let Some(digits) = line
        .strip_prefix(HEADER)
        .and_then(|rest| rest.strip_prefix(b" "))
    else {
        return Version::Invalid;
    };
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return Version::Invalid;
    }
    // Too many digits for a `u64` is still a (much) newer version.
    match parse_number(digits) {
        Some(0) => Version::Invalid,
        Some(n) if n <= u64::from(FORMAT_VERSION) => Version::Current,
        _ => Version::Newer,
    }
}

/// Splits `key value` at the first space; `None` without one.
fn split_key(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let space = line.iter().position(|&b| b == b' ')?;
    Some((&line[..space], &line[space + 1..]))
}

/// The part of the file being read.
enum Section {
    /// Before the first `folder` line.
    Settings,
    /// Inside the entry for this root.
    Folder(RootKey, PartialEntry),
    /// Inside an entry that is ignored (bad or repeated root).
    Skipped,
}

impl Section {
    /// Stores a finished entry.
    fn finish(&mut self, folders: &mut Vec<(RootKey, FolderEntry)>) {
        if let Self::Folder(root, entry) = std::mem::replace(self, Self::Skipped) {
            folders.push((root, entry.finish()));
        }
    }
}

/// Settings as read so far; each field keeps its first valid value.
#[derive(Default)]
struct PartialSettings {
    volume: Option<Volume>,
    shuffle: Option<bool>,
    repeat: Option<Repeat>,
    visualizer: Option<bool>,
    lyrics: Option<bool>,
}

impl PartialSettings {
    fn apply(&mut self, key: &[u8], value: &[u8]) {
        match key {
            b"volume" => fill(&mut self.volume, || parse_volume(value)),
            b"shuffle" => fill(&mut self.shuffle, || parse_switch(value)),
            b"repeat" => fill(&mut self.repeat, || parse_repeat(value)),
            b"visualizer" => fill(&mut self.visualizer, || parse_switch(value)),
            b"lyrics" => fill(&mut self.lyrics, || parse_switch(value)),
            _ => {}
        }
    }

    /// The settings, missing values at their defaults; `None` if none was read.
    fn finish(self) -> Option<Settings> {
        let any = self.volume.is_some()
            || self.shuffle.is_some()
            || self.repeat.is_some()
            || self.visualizer.is_some()
            || self.lyrics.is_some();
        let default = Settings::default();
        any.then(|| Settings {
            volume: self.volume.unwrap_or(default.volume),
            shuffle: self.shuffle.unwrap_or(default.shuffle),
            repeat: self.repeat.unwrap_or(default.repeat),
            visualizer: self.visualizer.unwrap_or(default.visualizer),
            lyrics: self.lyrics.unwrap_or(default.lyrics),
        })
    }
}

/// One folder entry as read so far; each field keeps its first valid value.
#[derive(Default)]
struct PartialEntry {
    track: Option<RelPath>,
    item: Option<usize>,
    position: Option<Duration>,
    source: Option<SavedSource>,
    open: Option<String>,
}

impl PartialEntry {
    fn apply(&mut self, key: &[u8], value: &[u8]) {
        match key {
            b"track" => fill(&mut self.track, || RelPath::decode(value)),
            b"item" => fill(&mut self.item, || {
                parse_number(value).and_then(|n| usize::try_from(n).ok())
            }),
            b"position-ms" => fill(&mut self.position, || {
                parse_number(value)
                    .map(Duration::from_millis)
                    .filter(|position| *position <= MAX_POSITION)
            }),
            b"source" => fill(&mut self.source, || parse_source(value)),
            b"open" => fill(&mut self.open, || decode_name(value)),
            _ => {}
        }
    }

    /// The entry; item, position and source only count with a track.
    fn finish(self) -> FolderEntry {
        FolderEntry {
            resume: self.track.map(|track| Resume {
                track,
                item: self.item,
                position: self.position.unwrap_or_default(),
                source: self.source.unwrap_or_default(),
            }),
            open: self.open,
        }
    }
}

/// Sets `slot` from `value` unless it already holds a value.
fn fill<T>(slot: &mut Option<T>, value: impl FnOnce() -> Option<T>) {
    if slot.is_none() {
        *slot = value();
    }
}

/// A non-empty run of ASCII digits as a `u64`; `None` on anything else or
/// overflow.
fn parse_number(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u64, |acc, &b| {
        let digit = b.is_ascii_digit().then(|| u64::from(b - b'0'))?;
        acc.checked_mul(10)?.checked_add(digit)
    })
}

/// A volume of 0–100, rounded down to a whole step.
fn parse_volume(value: &[u8]) -> Option<Volume> {
    let percent = u8::try_from(parse_number(value)?).ok()?;
    (percent <= Volume::MAX.percent()).then(|| Volume::new(percent - percent % Volume::STEP))
}

fn parse_switch(value: &[u8]) -> Option<bool> {
    match value {
        b"on" => Some(true),
        b"off" => Some(false),
        _ => None,
    }
}

fn parse_repeat(value: &[u8]) -> Option<Repeat> {
    [Repeat::Off, Repeat::All, Repeat::One]
        .into_iter()
        .find(|repeat| repeat.label().as_bytes() == value)
}

/// `library` or `playlist <name>`.
fn parse_source(value: &[u8]) -> Option<SavedSource> {
    if value == b"library" {
        return Some(SavedSource::Library);
    }
    let name = value.strip_prefix(b"playlist ")?;
    decode_name(name).map(SavedSource::Playlist)
}

/// A playlist name that passes [`valid_name`].
fn decode_name(value: &[u8]) -> Option<String> {
    String::from_utf8(decode(value)?)
        .ok()
        .filter(|name| valid_name(name))
}

/// Whether `name` can be saved as a playlist name: at most
/// [`MAX_NAME_BYTES`] bytes, and a valid path component by
/// [`valid_component`] (so no separator, NUL, `.` or `..`).
fn valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME_BYTES && valid_component(name.as_bytes())
}

/// Whether `b` is written as itself rather than escaped.
const fn is_plain(b: u8) -> bool {
    matches!(b, 0x21..=0x7E) && b != b'%'
}

/// Percent-encodes `bytes` (see the module docs). The result is printable
/// ASCII without spaces.
fn encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if is_plain(b) {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0x0F)]));
        }
    }
    out
}

/// Reverses [`encode`]; `None` on a byte that should have been escaped or a
/// malformed escape. Either hex case is accepted.
fn decode(encoded: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(encoded.len());
    let mut bytes = encoded.iter();
    while let Some(&b) = bytes.next() {
        if b == b'%' {
            let high = hex_value(*bytes.next()?)?;
            let low = hex_value(*bytes.next()?)?;
            out.push(high << 4 | low);
        } else if is_plain(b) {
            out.push(b);
        } else {
            return None;
        }
    }
    Some(out)
}

const fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Writes the state file. The output never exceeds [`MAX_STATE_BYTES`]: see
/// the module invariants for what is left out when it would.
#[must_use]
pub fn render(file: &StateFile) -> Vec<u8> {
    let mut out = format!("{} {FORMAT_VERSION}\n", String::from_utf8_lossy(HEADER));
    if let Some(settings) = file.settings {
        render_settings(&mut out, settings);
    }
    for (index, (root, entry)) in file.folders.iter().take(MAX_FOLDERS).enumerate() {
        let full = render_entry(root, entry.resume.as_ref(), entry.open.as_deref());
        if fits(&out, &full) {
            out.push_str(&full);
            continue;
        }
        if index == 0 {
            // The current folder alone is too large: keep as much of it as fits.
            let fallback = [
                render_entry(root, None, entry.open.as_deref()),
                render_entry(root, None, None),
            ]
            .into_iter()
            .find(|text| fits(&out, text));
            if let Some(text) = fallback {
                out.push_str(&text);
            }
        }
        // An older, smaller entry may still fit.
    }
    out.into_bytes()
}

/// Whether `extra` can follow `out` within [`MAX_STATE_BYTES`].
fn fits(out: &str, extra: &str) -> bool {
    out.len().saturating_add(extra.len()) <= MAX_STATE_BYTES
}

/// Appends the line `key value`.
fn push_line(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push(' ');
    out.push_str(value);
    out.push('\n');
}

fn render_settings(out: &mut String, settings: Settings) {
    let switch = |on: bool| if on { "on" } else { "off" };
    push_line(out, "volume", &settings.volume.percent().to_string());
    push_line(out, "shuffle", switch(settings.shuffle));
    push_line(out, "repeat", settings.repeat.label());
    push_line(out, "visualizer", switch(settings.visualizer));
    push_line(out, "lyrics", switch(settings.lyrics));
}

fn render_entry(root: &RootKey, resume: Option<&Resume>, open: Option<&str>) -> String {
    let mut out = String::new();
    push_line(&mut out, "folder", &encode(&root.0));
    if let Some(resume) = resume {
        // Components never hold `/`, which `encode` keeps as is.
        push_line(&mut out, "track", &encode(&resume.track.0));
        if let Some(item) = resume.item {
            push_line(&mut out, "item", &item.to_string());
        }
        let millis = resume.position.min(MAX_POSITION).as_millis();
        push_line(&mut out, "position-ms", &millis.to_string());
        match &resume.source {
            // A name `parse` would refuse reads back as the library anyway.
            SavedSource::Library => push_line(&mut out, "source", "library"),
            SavedSource::Playlist(name) if !valid_name(name) => {
                push_line(&mut out, "source", "library");
            }
            SavedSource::Playlist(name) => {
                push_line(
                    &mut out,
                    "source",
                    &format!("playlist {}", encode(name.as_bytes())),
                );
            }
        }
    }
    if let Some(open) = open.filter(|name| valid_name(name)) {
        push_line(&mut out, "open", &encode(open.as_bytes()));
    }
    out
}

/// Folds one run's `session` for `root` into `file`: the settings are
/// replaced, every entry for `root` is removed and the new one put first,
/// other entries keep their order, and the list is cut to [`MAX_FOLDERS`].
///
/// When the session has no resume point, `keep` (a resume point loaded at
/// start that could not be resumed) is saved in its place.
#[must_use]
pub fn merge(
    mut file: StateFile,
    root: &RootKey,
    session: &Session,
    keep: Option<&Resume>,
) -> StateFile {
    let mut entry = session.folder.clone();
    if entry.resume.is_none() {
        entry.resume = keep.cloned();
    }
    file.settings = Some(session.settings);
    file.folders.retain(|(key, _)| key != root);
    file.folders.insert(0, (root.clone(), entry));
    file.folders.truncate(MAX_FOLDERS);
    file
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::path::PathBuf;

    use super::*;

    fn rel(path: &str) -> RelPath {
        RelPath::new(Path::new(path)).unwrap_or_else(|| panic!("invalid relative path {path:?}"))
    }

    fn root(path: &str) -> RootKey {
        RootKey::new(Path::new(path)).unwrap_or_else(|| panic!("invalid root {path:?}"))
    }

    fn valid(bytes: &[u8]) -> StateFile {
        match parse(bytes) {
            Parsed::Valid(file) => file,
            other => panic!("expected a valid file, got {other:?}"),
        }
    }

    fn settings_of(text: &str) -> Settings {
        valid(text.as_bytes())
            .settings
            .unwrap_or_else(|| panic!("no settings in {text:?}"))
    }

    fn resume(track: &str, position_ms: u64) -> Resume {
        Resume {
            track: rel(track),
            item: Some(3),
            position: Duration::from_millis(position_ms),
            source: SavedSource::Playlist("Running".to_owned()),
        }
    }

    fn session(track: Option<&str>, open: Option<&str>) -> Session {
        Session {
            settings: Settings {
                volume: Volume::new(60),
                shuffle: true,
                repeat: Repeat::All,
                visualizer: false,
                lyrics: true,
            },
            folder: FolderEntry {
                resume: track.map(|t| resume(t, 135_000)),
                open: open.map(str::to_owned),
            },
        }
    }

    fn sample_file() -> StateFile {
        StateFile {
            settings: Some(session(None, None).settings),
            folders: vec![
                (
                    root("/home/user/Música"),
                    FolderEntry {
                        resume: Some(resume("Band/LP/01 Song.flac", 135_000)),
                        open: Some("Running".to_owned()),
                    },
                ),
                (root("/mnt/other"), FolderEntry::default()),
            ],
        }
    }

    // --- render / parse round trip ---

    #[test]
    fn renders_the_documented_format() {
        let text = String::from_utf8(render(&sample_file())).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            text,
            "kasset-state 1\nvolume 60\nshuffle on\nrepeat all\nvisualizer off\nlyrics on\n\
             folder /home/user/M%C3%BAsica\ntrack Band/LP/01%20Song.flac\nitem 3\n\
             position-ms 135000\nsource playlist Running\nopen Running\nfolder /mnt/other\n"
        );
    }

    #[test]
    fn round_trips_the_sample() {
        let file = sample_file();
        assert_eq!(valid(&render(&file)), file);
    }

    #[test]
    fn round_trips_awkward_names() {
        // Names hold no `/` (they are file names); roots and tracks may.
        let awkward = "Sí señor 100% \n\r\t\u{1b}[31m 日本語 % %41 x";
        let file = StateFile {
            settings: None,
            folders: vec![(
                root(&format!("/music/{awkward}/sub")),
                FolderEntry {
                    resume: Some(Resume {
                        track: rel("Ünïcode dir/a b%c/\u{7f}\u{1}name\n.mp3"),
                        item: None,
                        position: Duration::ZERO,
                        source: SavedSource::Playlist(awkward.to_owned()),
                    }),
                    open: Some(awkward.to_owned()),
                },
            )],
        };
        let rendered = render(&file);
        assert!(
            rendered
                .iter()
                .all(|&b| b == b'\n' || (0x20..=0x7E).contains(&b))
        );
        assert_eq!(valid(&rendered), file);
    }

    #[cfg(unix)]
    #[test]
    fn round_trips_non_utf8_paths() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let root_path = Path::new(OsStr::from_bytes(b"/music/\xff\xfe\x80"));
        let track = Path::new(OsStr::from_bytes(b"dir\xc3/\xe9t\xe9.flac"));
        let file = StateFile {
            settings: Some(Settings::default()),
            folders: vec![(
                RootKey::new(root_path).unwrap_or_else(|| panic!("no root key")),
                FolderEntry {
                    resume: Some(Resume {
                        track: RelPath::new(track).unwrap_or_else(|| panic!("no rel path")),
                        item: Some(0),
                        position: Duration::from_millis(1),
                        source: SavedSource::Library,
                    }),
                    open: None,
                },
            )],
        };
        assert_eq!(valid(&render(&file)), file);
    }

    #[test]
    fn round_trips_every_setting() {
        for repeat in [Repeat::Off, Repeat::All, Repeat::One] {
            for (shuffle, visualizer, lyrics) in [(false, true, false), (true, false, true)] {
                let settings = Settings {
                    volume: Volume::new(35),
                    shuffle,
                    repeat,
                    visualizer,
                    lyrics,
                };
                let file = StateFile {
                    settings: Some(settings),
                    folders: Vec::new(),
                };
                assert_eq!(valid(&render(&file)), file);
            }
        }
    }

    #[test]
    fn positions_are_bounded() {
        let mut file = sample_file();
        if let Some(resume) = file.folders[0].1.resume.as_mut() {
            resume.position = Duration::MAX;
        }
        let parsed = valid(&render(&file));
        let position = parsed.folders[0].1.resume.as_ref().map(|r| r.position);
        assert_eq!(position, Some(MAX_POSITION));

        let at = |millis: u128| {
            let text = format!("kasset-state 1\nfolder /m\ntrack a.mp3\nposition-ms {millis}\n");
            valid(text.as_bytes()).folders[0]
                .1
                .resume
                .as_ref()
                .map(|r| r.position)
        };
        let max = MAX_POSITION.as_millis();
        assert_eq!(at(max), Some(MAX_POSITION));
        assert_eq!(at(max + 1), Some(Duration::ZERO));
        assert_eq!(at(u128::from(u64::MAX)), Some(Duration::ZERO));
    }

    #[test]
    fn playlist_names_must_be_file_name_components() {
        let too_long = "x".repeat(MAX_NAME_BYTES + 1);
        let longest = "é".repeat(MAX_NAME_BYTES / 2);
        for (name, ok) in [
            ("Gym", true),
            ("Gym\u{1b}[2J\n", true),
            (longest.as_str(), true),
            (too_long.as_str(), false),
            ("", false),
            ("a/b", false),
            ("../secret", false),
            ("..", false),
            (".", false),
            ("a\0b", false),
        ] {
            let file = StateFile {
                settings: None,
                folders: vec![(
                    root("/m"),
                    FolderEntry {
                        resume: Some(Resume {
                            track: rel("a.mp3"),
                            item: None,
                            position: Duration::ZERO,
                            source: SavedSource::Playlist(name.to_owned()),
                        }),
                        open: Some(name.to_owned()),
                    },
                )],
            };
            let parsed = valid(&render(&file));
            let entry = &parsed.folders[0].1;
            let expected = ok.then(|| name.to_owned());
            assert_eq!(entry.open, expected, "{name:?}");
            let source = entry.resume.as_ref().map(|r| r.source.clone());
            let expected = expected.map_or(SavedSource::Library, SavedSource::Playlist);
            assert_eq!(source, Some(expected), "{name:?}");

            // The same name written by someone else is refused on read.
            let text = format!(
                "kasset-state 1\nfolder /m\nopen {}\n",
                encode(name.as_bytes())
            );
            let entry = &valid(text.as_bytes()).folders[0].1;
            assert_eq!(entry.open.is_some(), ok, "{name:?}");
        }
        let entry = &valid(b"kasset-state 1\nfolder /m\nopen %2E%2E%2Fsecret\n").folders[0].1;
        assert_eq!(entry.open, None);
    }

    #[test]
    fn a_bare_folder_line_ends_the_entry_before_it() {
        let file =
            valid(b"kasset-state 1\nfolder /a\ntrack a.mp3\nfolder\nopen X\nitem 5\nfolder /b\n");
        assert_eq!(
            file.folders,
            [
                (
                    root("/a"),
                    FolderEntry {
                        resume: Some(Resume {
                            track: rel("a.mp3"),
                            item: None,
                            position: Duration::ZERO,
                            source: SavedSource::Library,
                        }),
                        open: None,
                    }
                ),
                (root("/b"), FolderEntry::default()),
            ]
        );
    }

    // --- forgiving parsing ---

    #[test]
    fn damaged_files_are_invalid() {
        let mut noise = Vec::new();
        let mut x: u32 = 0x1234_5678;
        for _ in 0..4096 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            noise.push(x.to_le_bytes()[0]);
        }
        for bytes in [
            &noise[..],
            b"",
            b"\n",
            b"volume 60\nshuffle on\n",
            b"kasset-state\nvolume 60\n",
            b"kasset-state  1\n",
            b"kasset-state 1 \n",
            b" kasset-state 1\n",
            b"kasset-state -1\n",
            b"kasset-state +1\n",
            b"KASSET-STATE 1\n",
        ] {
            assert_eq!(parse(bytes), Parsed::Invalid, "{bytes:?}");
        }
    }

    #[test]
    fn versions() {
        assert_eq!(parse(b"kasset-state 2\nvolume 60\n"), Parsed::Newer);
        assert_eq!(parse(b"kasset-state 4294967296\n"), Parsed::Newer);
        assert_eq!(
            parse(&[b"kasset-state ".as_slice(), &[b'9'; 40]].concat()),
            Parsed::Newer
        );
        assert_eq!(parse(b"kasset-state 0\n"), Parsed::Invalid);
        assert_eq!(parse(b"kasset-state 000\n"), Parsed::Invalid);
        assert_eq!(parse(b"kasset-state x\n"), Parsed::Invalid);
        assert_eq!(parse(b"kasset-state 1x\n"), Parsed::Invalid);
        assert_eq!(
            parse(b"kasset-state 1"),
            Parsed::Valid(StateFile::default())
        );
        assert_eq!(
            parse(b"kasset-state 01\n"),
            Parsed::Valid(StateFile::default())
        );
    }

    #[test]
    fn invalid_value_ignored() {
        let settings = settings_of("kasset-state 1\nvolume 250\nrepeat all\n");
        assert_eq!(settings.volume, Volume::MAX);
        assert_eq!(settings.repeat, Repeat::All);
        assert!(!settings.shuffle);
        assert!(settings.visualizer && settings.lyrics);
    }

    #[test]
    fn volume_between_steps_rounds_down() {
        assert_eq!(
            settings_of("kasset-state 1\nvolume 63\n").volume,
            Volume::new(60)
        );
        assert_eq!(
            settings_of("kasset-state 1\nvolume 4\n").volume,
            Volume::new(0)
        );
        assert_eq!(
            settings_of("kasset-state 1\nvolume 100\n").volume,
            Volume::MAX
        );
    }

    #[test]
    fn bad_values_and_unknown_keys_are_ignored_line_by_line() {
        let settings = settings_of(
            "kasset-state 1\n\
             colour blue\n\
             volume\n\
             volume -5\n\
             volume 6 0\n\
             volume 0x10\n\
             volume 99999999999999999999999999999999999999999\n\
             shuffle yes\n\
             shuffle ON\n\
             repeat forever\n\
             visualizer\n\
             \n\
             garbage line without key\n\
             volume 45\n\
             shuffle on\n\
             visualizer off\n",
        );
        assert_eq!(settings.volume, Volume::new(45));
        assert!(settings.shuffle);
        assert_eq!(settings.repeat, Repeat::Off);
        assert!(!settings.visualizer);
        assert!(settings.lyrics);
    }

    #[test]
    fn duplicate_keys_keep_the_first_value() {
        let file = valid(
            b"kasset-state 1\nvolume 40\nvolume 80\nrepeat one\nrepeat all\n\
              folder /m\ntrack a.mp3\ntrack b.mp3\nitem 1\nitem 2\n\
              position-ms 5\nposition-ms 6\nsource library\nsource playlist X\n\
              open A\nopen B\n",
        );
        let settings = file.settings.unwrap_or_else(|| panic!("no settings"));
        assert_eq!(settings.volume, Volume::new(40));
        assert_eq!(settings.repeat, Repeat::One);
        let entry = &file.folders[0].1;
        assert_eq!(
            entry.resume,
            Some(Resume {
                track: rel("a.mp3"),
                item: Some(1),
                position: Duration::from_millis(5),
                source: SavedSource::Library,
            })
        );
        assert_eq!(entry.open.as_deref(), Some("A"));
    }

    #[test]
    fn bad_escapes_are_ignored_line_by_line() {
        let file = valid(
            b"kasset-state 1\nfolder /m\n\
              track a%2\ntrack a%zz.mp3\ntrack a%\ntrack a b.mp3\ntrack a\xc3\xa9.mp3\n\
              track ok%2emp3\n\
              open bad%G1\nopen %FF\nopen \nopen Gym%20Day\n\
              source playlist\nsource playlist \nsource Library\nsource playlist %C3%A9\n",
        );
        let entry = &file.folders[0].1;
        let resume = entry.resume.as_ref().unwrap_or_else(|| panic!("no resume"));
        assert_eq!(resume.track, rel("ok.mp3"));
        assert_eq!(resume.source, SavedSource::Playlist("é".to_owned()));
        assert_eq!(resume.item, None);
        assert_eq!(resume.position, Duration::ZERO);
        assert_eq!(entry.open.as_deref(), Some("Gym Day"));
    }

    #[test]
    fn item_position_and_source_need_a_track() {
        let file =
            valid(b"kasset-state 1\nfolder /m\nitem 3\nposition-ms 9\nsource library\nopen X\n");
        assert_eq!(
            file.folders,
            vec![(
                root("/m"),
                FolderEntry {
                    resume: None,
                    open: Some("X".to_owned()),
                }
            )]
        );
    }

    #[test]
    fn crlf_parses_like_lf() {
        let lf = render(&sample_file());
        let crlf: Vec<u8> = lf
            .split(|&b| b == b'\n')
            .collect::<Vec<_>>()
            .join(b"\r\n".as_slice());
        assert_eq!(valid(&crlf), sample_file());
    }

    #[test]
    fn settings_after_the_first_folder_are_ignored() {
        let file = valid(b"kasset-state 1\nvolume 40\nfolder /m\nvolume 80\nshuffle on\n");
        let settings = file.settings.unwrap_or_else(|| panic!("no settings"));
        assert_eq!(settings.volume, Volume::new(40));
        assert!(!settings.shuffle);
        assert_eq!(
            valid(b"kasset-state 1\nfolder /m\nvolume 80\n").settings,
            None
        );
    }

    #[test]
    fn a_second_entry_for_the_same_root_is_ignored() {
        let file = valid(
            b"kasset-state 1\nfolder /a\ntrack one.mp3\nfolder /b\nfolder /a\ntrack two.mp3\nopen X\nfolder /c\n",
        );
        let roots: Vec<&RootKey> = file.folders.iter().map(|(r, _)| r).collect();
        assert_eq!(roots, [&root("/a"), &root("/b"), &root("/c")]);
        assert_eq!(
            file.folders[0].1.resume.as_ref().map(|r| &r.track),
            Some(&rel("one.mp3"))
        );
        assert_eq!(file.folders[0].1.open, None);

        let merged = merge(file, &root("/a"), &session(Some("three.mp3"), None), None);
        let count = merged
            .folders
            .iter()
            .filter(|(r, _)| *r == root("/a"))
            .count();
        assert_eq!(count, 1);
        assert_eq!(merged.folders[0].0, root("/a"));
        assert_eq!(
            merged.folders[0].1.resume.as_ref().map(|r| &r.track),
            Some(&rel("three.mp3"))
        );
    }

    #[test]
    fn bad_folder_lines_drop_their_entry_only() {
        let file = valid(
            b"kasset-state 1\nfolder\nfolder \ntrack x.mp3\nfolder a b\ntrack y.mp3\nfolder /ok\ntrack z.mp3\n",
        );
        assert_eq!(file.folders.len(), 1);
        assert_eq!(file.folders[0].0, root("/ok"));
        assert_eq!(
            file.folders[0].1.resume.as_ref().map(|r| &r.track),
            Some(&rel("z.mp3"))
        );
    }

    #[test]
    fn paths_outside_the_library_are_rejected() {
        for track in [
            "../../etc/passwd",
            "..",
            ".",
            "/etc/passwd",
            "a//b",
            "a/./b",
            "a/../b",
            "a/",
            "/",
            "a%2Fb",
            "a%2fb",
            "..%2Fetc",
            "a%00b",
            "%2E%2E/etc/passwd",
        ] {
            let text = format!("kasset-state 1\nfolder /m\ntrack {track}\n");
            let file = valid(text.as_bytes());
            assert_eq!(file.folders[0].1.resume, None, "{track:?}");
        }
    }

    #[test]
    fn rel_path_takes_only_plain_components() {
        assert!(RelPath::new(Path::new("")).is_none());
        assert!(RelPath::new(Path::new("/etc/passwd")).is_none());
        assert!(RelPath::new(Path::new("../x")).is_none());
        assert!(RelPath::new(Path::new("a/../x")).is_none());
        assert_eq!(rel("a/b/c.mp3").0, b"a/b/c.mp3");
        // `Path::components` folds `.` and repeated separators.
        assert_eq!(rel("a//./b").0, b"a/b");
        assert!(RootKey::new(Path::new("")).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_components_have_no_prefix_or_stream() {
        assert!(RelPath::new(Path::new(r"C:\x")).is_none());
        assert!(RelPath::new(Path::new(r"\\server\share\x")).is_none());
        let file = valid(b"kasset-state 1\nfolder C:\\m\ntrack C:x\n");
        assert_eq!(file.folders[0].1.resume, None);
        let file = valid(b"kasset-state 1\nfolder C:\\m\ntrack a%5Cb\n");
        assert_eq!(file.folders[0].1.resume, None);
    }

    // --- merge ---

    #[test]
    fn merge_puts_the_current_folder_first_and_keeps_the_rest_in_order() {
        let file = StateFile {
            settings: Some(Settings::default()),
            folders: vec![
                (root("/a"), FolderEntry::default()),
                (root("/b"), FolderEntry::default()),
                (root("/c"), FolderEntry::default()),
            ],
        };
        let new = session(Some("x.mp3"), Some("Gym"));
        let merged = merge(file, &root("/b"), &new, None);
        let roots: Vec<&RootKey> = merged.folders.iter().map(|(r, _)| r).collect();
        assert_eq!(roots, [&root("/b"), &root("/a"), &root("/c")]);
        assert_eq!(merged.settings, Some(new.settings));
        assert_eq!(merged.folders[0].1, new.folder);
    }

    #[test]
    fn oldest_folder_forgotten() {
        let file = StateFile {
            settings: None,
            folders: (0..MAX_FOLDERS)
                .map(|i| (root(&format!("/f{i}")), FolderEntry::default()))
                .collect(),
        };
        let merged = merge(file, &root("/new"), &session(None, None), None);
        assert_eq!(merged.folders.len(), MAX_FOLDERS);
        assert_eq!(merged.folders[0].0, root("/new"));
        let expected: Vec<RootKey> = (0..MAX_FOLDERS - 1)
            .map(|i| root(&format!("/f{i}")))
            .collect();
        let kept: Vec<RootKey> = merged.folders[1..].iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(kept, expected);
    }

    #[test]
    fn merge_keeps_an_unresumed_point_only_without_a_track() {
        let kept = resume("old.mp3", 42_000);

        let merged = merge(
            StateFile::default(),
            &root("/m"),
            &session(None, Some("Gym")),
            Some(&kept),
        );
        assert_eq!(
            merged.folders[0].1,
            FolderEntry {
                resume: Some(kept.clone()),
                open: Some("Gym".to_owned()),
            }
        );

        let new = session(Some("new.mp3"), None);
        let merged = merge(StateFile::default(), &root("/m"), &new, Some(&kept));
        assert_eq!(merged.folders[0].1, new.folder);
    }

    // --- size limits ---

    /// A path of `len` bytes of two-byte UTF-8, in components of at most 200 bytes.
    fn long_path(prefix: &str, len: usize) -> String {
        let mut out = String::from(prefix);
        let mut written = 0;
        while written < len {
            if written > 0 && written % 200 == 0 {
                out.push('/');
            }
            out.push('é');
            written += 2;
        }
        out
    }

    #[test]
    fn long_paths_keep_the_newest_entries_that_fit() {
        let file = StateFile {
            settings: Some(Settings::default()),
            folders: (0..MAX_FOLDERS)
                .map(|i| {
                    let entry = FolderEntry {
                        resume: Some(Resume {
                            track: rel(&long_path("", 4096)),
                            item: Some(i),
                            position: Duration::from_millis(1000),
                            source: SavedSource::Library,
                        }),
                        open: Some(format!("Playlist {i}")),
                    };
                    (root(&long_path(&format!("/{i}/"), 4096)), entry)
                })
                .collect(),
        };
        let rendered = render(&file);
        assert!(rendered.len() <= MAX_STATE_BYTES, "{}", rendered.len());
        let parsed = valid(&rendered);
        assert_eq!(parsed.settings, file.settings);
        assert_ne!(parsed.folders.len(), 0);
        assert!(parsed.folders.len() < MAX_FOLDERS);
        assert_eq!(parsed.folders[..], file.folders[..parsed.folders.len()]);
    }

    #[test]
    fn an_entry_too_large_alone_loses_its_track() {
        let entry = FolderEntry {
            resume: Some(Resume {
                track: rel(&long_path("", 30_000)),
                item: None,
                position: Duration::ZERO,
                source: SavedSource::Library,
            }),
            open: Some("Gym".to_owned()),
        };
        let file = StateFile {
            settings: Some(Settings::default()),
            folders: vec![
                (root("/music"), entry),
                (root("/other"), FolderEntry::default()),
            ],
        };
        let rendered = render(&file);
        assert!(rendered.len() <= MAX_STATE_BYTES);
        let parsed = valid(&rendered);
        assert_eq!(parsed.settings, file.settings);
        assert_eq!(
            parsed.folders,
            vec![
                (
                    root("/music"),
                    FolderEntry {
                        resume: None,
                        open: Some("Gym".to_owned()),
                    }
                ),
                (root("/other"), FolderEntry::default())
            ]
        );
    }

    #[test]
    fn a_folder_line_too_large_is_left_out() {
        let file = StateFile {
            settings: Some(Settings::default()),
            folders: vec![
                (root(&long_path("/", 30_000)), FolderEntry::default()),
                (root("/other"), FolderEntry::default()),
            ],
        };
        let rendered = render(&file);
        assert!(rendered.len() <= MAX_STATE_BYTES);
        let parsed = valid(&rendered);
        assert_eq!(parsed.settings, file.settings);
        assert_eq!(parsed.folders, [(root("/other"), FolderEntry::default())]);
    }

    #[test]
    fn an_entry_whose_playlist_cannot_fit_keeps_only_its_folder() {
        let settings = Some(Settings::default());
        let head = render(&StateFile {
            settings,
            folders: Vec::new(),
        })
        .len();
        // Leave 50 bytes after the `folder` line: enough for the resume lines
        // (42 bytes) but not with the 200-byte `open` line, nor for `open` alone.
        let len = MAX_STATE_BYTES - head - "folder \n".len() - 50;
        let file = StateFile {
            settings,
            folders: vec![(
                root(&format!("/{}", "a".repeat(len - 1))),
                FolderEntry {
                    resume: Some(Resume {
                        track: rel("a.mp3"),
                        item: None,
                        position: Duration::ZERO,
                        source: SavedSource::Library,
                    }),
                    open: Some("p".repeat(200)),
                },
            )],
        };
        let rendered = render(&file);
        assert_eq!(rendered.len(), MAX_STATE_BYTES - 50);
        let parsed = valid(&rendered);
        assert_eq!(
            parsed.folders,
            [(file.folders[0].0.clone(), FolderEntry::default())]
        );
    }

    #[test]
    fn older_entries_that_fit_are_kept_after_one_that_does_not() {
        let big = |name: &str| {
            (
                root(&long_path(&format!("/{name}/"), 12_000)),
                FolderEntry::default(),
            )
        };
        let file = StateFile {
            settings: Some(Settings::default()),
            folders: vec![
                (root("/current"), FolderEntry::default()),
                big("a"),
                big("b"),
                (root("/small"), FolderEntry::default()),
            ],
        };
        let rendered = render(&file);
        assert!(rendered.len() <= MAX_STATE_BYTES);
        let parsed = valid(&rendered);
        let roots: Vec<&RootKey> = parsed.folders.iter().map(|(r, _)| r).collect();
        assert_eq!(
            roots,
            [&file.folders[0].0, &file.folders[1].0, &file.folders[3].0]
        );
    }

    #[test]
    fn hostile_input_neither_panics_nor_stalls() {
        // A 64 KiB file of very long lines.
        let mut long = b"kasset-state 1\nvolume ".to_vec();
        long.resize(MAX_STATE_BYTES / 2, b'9');
        long.extend_from_slice(b"\nfolder /");
        long.resize(MAX_STATE_BYTES, b'%');
        assert!(matches!(parse(&long), Parsed::Valid(_)));

        // 100 000 folder lines, most of them distinct.
        let mut many = String::from("kasset-state 1\n");
        for i in 0..100_000 {
            let _ = writeln!(many, "folder /f{i}\ntrack t{i}.mp3");
        }
        let file = valid(many.as_bytes());
        assert_eq!(file.folders.len(), MAX_FOLDERS);
        assert_eq!(file.folders[0].0, root("/f0"));

        // 100 000 repeats of one folder.
        let same = format!("kasset-state 1\n{}", "folder /x\n".repeat(100_000));
        assert_eq!(valid(same.as_bytes()).folders.len(), 1);

        // A 40-digit number anywhere.
        let digits = "9".repeat(40);
        let text = format!(
            "kasset-state 1\nvolume {digits}\nfolder /m\ntrack a.mp3\nitem {digits}\nposition-ms {digits}\n"
        );
        let file = valid(text.as_bytes());
        assert_eq!(file.settings, None);
        let resume = file.folders[0]
            .1
            .resume
            .as_ref()
            .unwrap_or_else(|| panic!("no resume"));
        assert_eq!((resume.item, resume.position), (None, Duration::ZERO));
    }

    #[test]
    fn root_keys_compare_exactly() {
        assert_ne!(root("/music"), root("/music/"));
        assert_ne!(root("/Music"), root("/music"));
        let path = PathBuf::from("/music");
        assert_eq!(RootKey::new(&path), Some(root("/music")));
    }
}
