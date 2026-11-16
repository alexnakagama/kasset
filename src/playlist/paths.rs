//! Turning playlist entries into library tracks and library tracks into entries.
//!
//! Entries are matched lexically (no filesystem access) first: literally, then,
//! on systems where `\` is not a separator, with `\` read as one (playlists
//! written on Windows). Only entries still unmatched pay for `canonicalize`.
//!
//! Matching through `canonicalize` also needs the canonical path of library
//! tracks reached through a symlink. Those are computed all at once, on the
//! first entry that exists on disk without a lexical match, and only for
//! libraries up to [`CANONICAL_MAP_MAX_TRACKS`] tracks so untrusted playlist
//! content cannot make the interface stall on a huge library. Above that, such
//! entries are reported as unmatched (and kept in the file).

use std::cell::OnceCell;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::library::Track;

/// Largest library for which the canonical path of every track is computed.
pub const CANONICAL_MAP_MAX_TRACKS: usize = 20_000;

/// How hard to try matching an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matching {
    /// Lexical, then `canonicalize` for misses.
    Full,
    /// Lexical only: no filesystem access (used to count tracks when listing).
    Lexical,
}

/// Matches playlist entries to library tracks. Built once per library.
#[derive(Debug)]
pub struct Resolver {
    /// Lexically normalised track path → library index.
    by_path: HashMap<PathBuf, usize>,
    track_paths: Vec<PathBuf>,
    /// Canonical track path → library index, built on the first entry that
    /// exists on disk but has no lexical match.
    canonical: OnceCell<HashMap<PathBuf, usize>>,
}

impl Resolver {
    #[must_use]
    pub fn new(tracks: &[Track]) -> Self {
        let mut by_path = HashMap::with_capacity(tracks.len());
        for (i, t) in tracks.iter().enumerate() {
            by_path.entry(normalize(&t.path)).or_insert(i);
        }
        Self {
            by_path,
            track_paths: tracks.iter().map(|t| t.path.clone()).collect(),
            canonical: OnceCell::new(),
        }
    }

    /// The library index `raw` (an entry line) refers to, read relative to
    /// `folder` unless absolute.
    #[cfg(test)]
    #[must_use]
    pub fn resolve(&self, folder: &Path, raw: &[u8]) -> Option<usize> {
        self.resolve_with(folder, raw, Matching::Full)
    }

    /// [`Self::resolve`] with a choice of [`Matching`].
    #[must_use]
    pub fn resolve_with(&self, folder: &Path, raw: &[u8], matching: Matching) -> Option<usize> {
        let candidates = candidates(folder, raw);
        if let Some(&i) = candidates.iter().find_map(|p| self.by_path.get(p)) {
            return Some(i);
        }
        if matching == Matching::Lexical {
            return None;
        }
        candidates.iter().find_map(|p| {
            let real = std::fs::canonicalize(p).ok()?;
            self.by_path
                .get(&real)
                .or_else(|| self.canonical_map().get(&real))
                .copied()
        })
    }

    fn canonical_map(&self) -> &HashMap<PathBuf, usize> {
        self.canonical.get_or_init(|| {
            let mut map = HashMap::new();
            if self.track_paths.len() > CANONICAL_MAP_MAX_TRACKS {
                return map;
            }
            for (i, path) in self.track_paths.iter().enumerate() {
                if let Ok(real) = std::fs::canonicalize(path) {
                    map.entry(real).or_insert(i);
                }
            }
            map
        })
    }
}

/// Paths an entry may mean, most literal first, without duplicates.
fn candidates(folder: &Path, raw: &[u8]) -> Vec<PathBuf> {
    let mut forms: Vec<&[u8]> = vec![raw];
    let trimmed = raw.trim_ascii();
    if trimmed.len() != raw.len() {
        forms.push(trimmed);
    }
    #[cfg(not(windows))]
    let swapped: Vec<Vec<u8>> = forms
        .iter()
        .filter(|f| f.contains(&b'\\'))
        .map(|f| {
            f.iter()
                .map(|&b| if b == b'\\' { b'/' } else { b })
                .collect()
        })
        .collect();
    #[cfg(not(windows))]
    forms.extend(swapped.iter().map(Vec::as_slice));

    let mut out: Vec<PathBuf> = Vec::with_capacity(forms.len());
    for form in forms {
        if let Some(path) = entry_path(folder, form)
            && !out.contains(&path)
        {
            out.push(path);
        }
    }
    out
}

/// The absolute, normalised path an entry names; `None` for an empty entry or
/// one that cannot be a path.
fn entry_path(folder: &Path, raw: &[u8]) -> Option<PathBuf> {
    if raw.is_empty() || raw.contains(&0) {
        return None;
    }
    let path = bytes_to_path(raw);
    if path.is_absolute() {
        Some(normalize(&path))
    } else {
        Some(normalize(&folder.join(path)))
    }
}

#[cfg(unix)]
fn bytes_to_path(raw: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(raw))
}

#[cfg(not(unix))]
fn bytes_to_path(raw: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(raw).into_owned())
}

/// Removes `.` and resolves `..` lexically (`..` at the root stays at the root).
#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                None | Some(Component::ParentDir | Component::CurDir) => out.push(".."),
            },
            other => out.push(other),
        }
    }
    out
}

/// The line to write for `track` in a playlist stored in `folder`: relative to
/// `folder` with `/` separators, or absolute if no relative path exists.
/// `None` when the path cannot be stored in a UTF-8 line (not UTF-8, or
/// containing a line break or NUL).
pub fn entry_for(folder: &Path, track: &Path) -> Option<Vec<u8>> {
    let base = normalize(folder);
    let target = normalize(track);
    let mut line = relative(&base, &target)
        .or_else(|| component_str(target.as_os_str()).map(str::to_owned))?;
    if line.is_empty() || line.contains(['\n', '\r', '\0']) {
        return None;
    }
    // Keep the line an entry: not a comment, not lost to whitespace trimming.
    if line.starts_with('#') || line.starts_with(|c: char| c.is_ascii_whitespace()) {
        line.insert_str(0, "./");
    }
    Some(line.into_bytes())
}

/// `target` relative to `base`, joined with `/`. Both must be absolute and
/// share a root.
fn relative(base: &Path, target: &Path) -> Option<String> {
    if !base.is_absolute() || !target.is_absolute() {
        return None;
    }
    let base: Vec<Component> = base.components().collect();
    let target: Vec<Component> = target.components().collect();
    let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
    // No shared root (e.g. another drive on Windows).
    if common == 0 || target.len() == common {
        return None;
    }
    let mut parts: Vec<&str> = vec![".."; base.len() - common];
    for component in &target[common..] {
        let Component::Normal(name) = component else {
            return None;
        };
        parts.push(component_str(name)?);
    }
    Some(parts.join("/"))
}

#[cfg(unix)]
fn component_str(name: &std::ffi::OsStr) -> Option<&str> {
    use std::os::unix::ffi::OsStrExt;
    std::str::from_utf8(name.as_bytes()).ok()
}

#[cfg(not(unix))]
fn component_str(name: &std::ffi::OsStr) -> Option<&str> {
    name.to_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn track(path: impl Into<PathBuf>) -> Track {
        Track {
            path: path.into(),
            title: "t".to_owned(),
            artist: "a".to_owned(),
            album: "l".to_owned(),
            number: None,
            duration: Some(Duration::from_secs(1)),
        }
    }

    #[test]
    fn normalizes_dots() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), Path::new("/a/c"));
        assert_eq!(normalize(Path::new("/../a")), Path::new("/a"));
        assert_eq!(normalize(Path::new("a/../../b")), Path::new("../b"));
    }

    #[cfg(unix)]
    #[test]
    fn writes_relative_entries_with_slashes() {
        let line = entry_for(
            Path::new("/music/Playlists"),
            Path::new("/music/Band/LP/01 Song.flac"),
        );
        assert_eq!(line.as_deref(), Some(&b"../Band/LP/01 Song.flac"[..]));
        // Inside the playlists folder itself.
        let line = entry_for(
            Path::new("/music/Playlists"),
            Path::new("/music/Playlists/x.mp3"),
        );
        assert_eq!(line.as_deref(), Some(&b"x.mp3"[..]));
        // A name that would read as a comment or lose leading spaces.
        let line = entry_for(
            Path::new("/music/Playlists"),
            Path::new("/music/Playlists/#1.mp3"),
        );
        assert_eq!(line.as_deref(), Some(&b"./#1.mp3"[..]));
        let line = entry_for(
            Path::new("/music/Playlists"),
            Path::new("/music/Playlists/ x.mp3"),
        );
        assert_eq!(line.as_deref(), Some(&b"./ x.mp3"[..]));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_unwritable_paths() {
        use std::os::unix::ffi::OsStrExt;
        let folder = Path::new("/music/Playlists");
        assert_eq!(entry_for(folder, Path::new("/music/a\nb.mp3")), None);
        assert_eq!(entry_for(folder, Path::new("/music/a\rb.mp3")), None);
        let latin1 = Path::new(std::ffi::OsStr::from_bytes(b"/music/caf\xE9.mp3"));
        assert_eq!(entry_for(folder, latin1), None);
    }

    #[cfg(unix)]
    #[test]
    fn backslash_file_name_round_trips_and_matches() {
        let folder = Path::new("/music/Playlists");
        let lib = vec![track("/music/AC\\DC - Song.mp3")];
        let line = entry_for(folder, &lib[0].path).unwrap_or_default();
        assert_eq!(line, b"../AC\\DC - Song.mp3");
        assert_eq!(Resolver::new(&lib).resolve(folder, &line), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn windows_separators_match_when_literal_fails() {
        let folder = Path::new("/music/Playlists");
        let lib = vec![track("/music/Band/LP/01.flac")];
        let r = Resolver::new(&lib);
        assert_eq!(r.resolve(folder, b"..\\Band\\LP\\01.flac"), Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn literal_backslash_name_wins_over_separator_reading() {
        let folder = Path::new("/music/Playlists");
        let lib = vec![track("/music/A/B.mp3"), track("/music/A\\B.mp3")];
        let r = Resolver::new(&lib);
        assert_eq!(r.resolve(folder, b"../A\\B.mp3"), Some(1));
    }

    #[cfg(unix)]
    #[test]
    fn absolute_and_dotted_entries_match() {
        let folder = Path::new("/music/Playlists");
        let lib = vec![track("/music/Band/x.flac")];
        let r = Resolver::new(&lib);
        assert_eq!(r.resolve(folder, b"/music/Band/x.flac"), Some(0));
        assert_eq!(r.resolve(folder, b"./../Band/./x.flac"), Some(0));
        assert_eq!(r.resolve(folder, b"../Band/x.flac  "), Some(0));
        assert_eq!(r.resolve(folder, b"../Band/y.flac"), None);
        assert_eq!(r.resolve(folder, b""), None);
        assert_eq!(r.resolve(folder, b"../Band/x\0.flac"), None);
    }

    #[cfg(unix)]
    #[test]
    fn canonicalize_matches_through_symlinks() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(tmp.path())?;
        std::fs::create_dir_all(root.join("real"))?;
        std::fs::create_dir_all(root.join("Playlists"))?;
        std::fs::write(root.join("real/s.mp3"), b"")?;
        std::os::unix::fs::symlink(root.join("real"), root.join("alias"))?;
        let folder = root.join("Playlists");

        // Library path canonical, entry through the link.
        let r = Resolver::new(&[track(root.join("real/s.mp3"))]);
        assert_eq!(r.resolve(&folder, b"../alias/s.mp3"), Some(0));
        // Library path through the link, entry canonical.
        let r = Resolver::new(&[track(root.join("alias/s.mp3"))]);
        assert_eq!(r.resolve(&folder, b"../real/s.mp3"), Some(0));
        // Existing file outside the library stays unmatched.
        std::fs::write(root.join("other.mp3"), b"")?;
        assert_eq!(r.resolve(&folder, b"../other.mp3"), None);
        // Lexical matching never touches the filesystem.
        assert_eq!(
            r.resolve_with(&folder, b"../real/s.mp3", Matching::Lexical),
            None
        );
        assert_eq!(
            r.resolve_with(&folder, b"../alias/s.mp3", Matching::Lexical),
            Some(0)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn huge_library_skips_canonical_map() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(tmp.path())?;
        std::fs::create_dir_all(root.join("real"))?;
        std::fs::write(root.join("real/s.mp3"), b"")?;
        std::os::unix::fs::symlink(root.join("real"), root.join("alias"))?;
        let mut lib: Vec<Track> = (0..CANONICAL_MAP_MAX_TRACKS)
            .map(|i| track(root.join(format!("missing/{i}.mp3"))))
            .collect();
        lib.push(track(root.join("alias/s.mp3")));
        let r = Resolver::new(&lib);
        let start = std::time::Instant::now();
        assert_eq!(r.resolve(&root, b"real/s.mp3"), None);
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        Ok(())
    }
}
