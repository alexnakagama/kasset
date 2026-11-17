//! Discovering audio files under the library root and reading their metadata.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::Metadata;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use lofty::config::ParseOptions;
use lofty::file::{AudioFile, TaggedFile, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::{Accessor, Tag};
use walkdir::WalkDir;

use crate::panic_guard::quietly;
use crate::text::{display_path, sanitize};

/// File extensions (lower case) the player can decode.
///
/// `.opus` is deliberately absent: symphonia cannot decode it.
pub const SUPPORTED_EXTENSIONS: &[&str] = &["mp3", "flac", "ogg", "oga", "wav", "m4a", "aac"];

/// Shown for a missing artist or album.
pub const UNKNOWN: &str = "Unknown";

/// Upper bound on directory nesting, as a guard against pathological trees.
const MAX_SCAN_DEPTH: usize = 64;

/// Upper bound on filesystem entries visited in one scan, so a hostile or
/// enormous tree cannot stall startup indefinitely.
const MAX_SCAN_ENTRIES: usize = 2_000_000;

/// One playable file in the library. All text fields are sanitised for display;
/// `path` is raw and must go through [`display_path`] before being shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub path: PathBuf,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub number: Option<u32>,
    pub duration: Option<Duration>,
}

/// Result of walking the library root.
#[derive(Debug, Default)]
pub struct Discovery {
    /// Supported audio files, each underlying file at most once.
    pub paths: Vec<PathBuf>,
    /// Entries that could not be read (permissions, broken links, loops).
    pub skipped: usize,
    /// `true` when the walk stopped at [`MAX_SCAN_ENTRIES`].
    pub truncated: bool,
}

/// The scanned, sorted library plus what the scan had to leave out.
#[derive(Debug, Default)]
pub struct Library {
    pub tracks: Vec<Track>,
    pub skipped: usize,
    pub truncated: bool,
}

/// Returns `true` when `path` has a supported audio extension (case-insensitive).
#[must_use]
pub fn is_supported(path: &Path) -> bool {
    path.extension().and_then(OsStr::to_str).is_some_and(|ext| {
        SUPPORTED_EXTENSIONS
            .iter()
            .any(|s| ext.eq_ignore_ascii_case(s))
    })
}

/// Identity of a filesystem object, used to visit each directory and list each
/// file once even when symlinks reach it through several paths.
#[cfg(unix)]
type FileId = (u64, u64);
#[cfg(not(unix))]
type FileId = PathBuf;

#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "signature shared with the non-unix variant"
)]
fn file_id(_path: &Path, meta: &Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_id(path: &Path, _meta: &Metadata) -> Option<FileId> {
    std::fs::canonicalize(path).ok()
}

/// Recursively finds supported audio files under `root`, following symlinks.
///
/// Every directory and file is visited at most once (by device/inode), so
/// symlinks that reach the same folder twice neither duplicate tracks nor
/// multiply the work. Entries that cannot be read are counted in
/// [`Discovery::skipped`] and the rest of the library still loads.
///
/// # Errors
///
/// Fails when `root` itself cannot be read.
pub fn discover(root: &Path) -> Result<Discovery> {
    std::fs::read_dir(root)
        .with_context(|| format!("cannot read music folder '{}'", display_path(root)))?;

    let mut visited_dirs: HashSet<FileId> = HashSet::new();
    let mut seen_files: HashSet<FileId> = HashSet::new();
    let mut skipped = 0usize;
    let mut out = Discovery::default();

    let walker = WalkDir::new(root)
        .follow_links(true)
        .max_depth(MAX_SCAN_DEPTH)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|entry| {
            if !entry.file_type().is_dir() {
                return true;
            }
            // With `follow_links`, metadata describes the link target.
            if let Ok(meta) = entry.metadata() {
                file_id(entry.path(), &meta).is_some_and(|id| visited_dirs.insert(id))
            } else {
                skipped += 1;
                false
            }
        });

    for (visited, entry) in walker.enumerate() {
        if visited >= MAX_SCAN_ENTRIES {
            out.truncated = true;
            break;
        }
        let Ok(entry) = entry else {
            out.skipped += 1;
            continue;
        };
        if !entry.file_type().is_file() || !is_supported(entry.path()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            out.skipped += 1;
            continue;
        };
        // Keyed by (dev, ino): hard links to one file are deliberately listed once,
        // under the path that sorts first by file name.
        if file_id(entry.path(), &meta).is_some_and(|id| seen_files.insert(id)) {
            out.paths.push(entry.into_path());
        }
    }

    out.skipped += skipped;
    Ok(out)
}

/// Reads a track's metadata, falling back to the file name for the title and
/// [`UNKNOWN`] for artist/album when tags are missing or unreadable.
///
/// Each field is taken from the first tag that has it, primary tag first.
#[must_use]
pub fn read_track(path: PathBuf) -> Track {
    let tagged = read_tagged(&path);
    let tags = tagged.as_ref().map(ordered_tags).unwrap_or_default();

    let title = tag_text(&tags, Accessor::title).unwrap_or_else(|| title_from_path(&path));
    let artist = tag_text(&tags, Accessor::artist).unwrap_or_else(|| UNKNOWN.to_owned());
    let album = tag_text(&tags, Accessor::album).unwrap_or_else(|| UNKNOWN.to_owned());
    let number = tags.iter().find_map(|t| t.track().filter(|&n| n > 0));
    let duration = tagged
        .as_ref()
        .map(|t| t.properties().duration())
        .filter(|d| !d.is_zero());

    Track {
        path,
        title,
        artist,
        album,
        number,
        duration,
    }
}

/// Scans `root` and returns every supported track, sorted for display.
///
/// # Errors
///
/// Fails when `root` itself cannot be read.
pub fn scan(root: &Path) -> Result<Library> {
    let found = discover(root)?;
    let mut tracks: Vec<Track> = found.paths.into_iter().map(read_track).collect();
    sort_tracks(&mut tracks);
    Ok(Library {
        tracks,
        skipped: found.skipped,
        truncated: found.truncated,
    })
}

/// Sorts by artist, album, track number, then title (text compared
/// case-insensitively). Untagged track numbers sort after numbered ones and the
/// path breaks remaining ties so the order is deterministic.
pub fn sort_tracks(tracks: &mut [Track]) {
    tracks.sort_by_cached_key(|t| {
        (
            t.artist.to_lowercase(),
            t.album.to_lowercase(),
            t.number.map_or((1, 0), |n| (0, n)),
            t.title.to_lowercase(),
            t.path.clone(),
        )
    });
}

/// Parses `path` with lofty. Any failure — including a panic inside the parser
/// on a malformed file — is treated as "no metadata".
fn read_tagged(path: &Path) -> Option<TaggedFile> {
    // Cover art is never displayed; skipping it avoids large allocations.
    // Lofty additionally caps every allocation (16 MiB by default).
    let options = ParseOptions::new().read_cover_art(false);
    let parse = || {
        Probe::open(path)
            .ok()?
            .options(options)
            .guess_file_type()
            .ok()?
            .read()
            .ok()
    };
    // The closure only touches `path` and owned locals, so no broken invariant
    // can be observed after an unwind. Requires `panic = "unwind"` (Cargo.toml).
    quietly(AssertUnwindSafe(parse)).ok().flatten()
}

/// All tags of the file, primary tag first.
fn ordered_tags(tagged: &TaggedFile) -> Vec<&Tag> {
    let primary = tagged.primary_tag();
    primary
        .into_iter()
        .chain(
            tagged
                .tags()
                .iter()
                .filter(move |t| primary.is_none_or(|p| !std::ptr::eq(*t, p))),
        )
        .collect()
}

fn tag_text(
    tags: &[&Tag],
    get: impl for<'a> Fn(&'a Tag) -> Option<std::borrow::Cow<'a, str>>,
) -> Option<String> {
    tags.iter().find_map(|t| get(t).and_then(|s| sanitize(&s)))
}

fn title_from_path(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| sanitize(&stem.to_string_lossy()))
        .or_else(|| {
            path.file_name()
                .and_then(|name| sanitize(&name.to_string_lossy()))
        })
        .unwrap_or_else(|| UNKNOWN.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::wav_bytes;
    use anyhow::Result;
    use std::fs;

    /// Paths relative to `root`, joined with `/` on every platform.
    fn names(paths: &[PathBuf], root: &Path) -> Vec<String> {
        let mut v: Vec<String> = paths
            .iter()
            .filter_map(|p| p.strip_prefix(root).ok())
            .map(|p| {
                p.components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect();
        v.sort();
        v
    }

    fn track(artist: &str, album: &str, no: Option<u32>, title: &str) -> Track {
        Track {
            path: PathBuf::from(format!("/m/{title}.mp3")),
            title: title.to_owned(),
            artist: artist.to_owned(),
            album: album.to_owned(),
            number: no,
            duration: None,
        }
    }

    #[test]
    fn extension_filter_is_case_insensitive() {
        assert!(is_supported(Path::new("a/b.MP3")));
        assert!(is_supported(Path::new("b.Flac")));
        assert!(is_supported(Path::new("c.oga")));
        assert!(!is_supported(Path::new("cover.jpg")));
        assert!(!is_supported(Path::new("song.opus")));
        assert!(!is_supported(Path::new("mp3")));
        assert!(!is_supported(Path::new("noext")));
    }

    #[test]
    fn discovers_nested_files_and_ignores_others() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::create_dir_all(root.join("Artist/Album"))?;
        fs::write(root.join("Artist/Album/01.flac"), b"")?;
        fs::write(root.join("single.MP3"), b"")?;
        fs::write(root.join("cover.jpg"), b"")?;
        fs::write(root.join("notes.txt"), b"")?;
        fs::create_dir(root.join("folder.mp3"))?; // a directory, not a track

        let found = discover(root)?;
        assert_eq!(
            names(&found.paths, root),
            ["Artist/Album/01.flac", "single.MP3"]
        );
        assert_eq!(found.skipped, 0);
        assert!(!found.truncated);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_subfolder_is_skipped() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        let locked = root.join("locked");
        fs::create_dir(&locked)?;
        fs::write(locked.join("hidden.mp3"), b"")?;
        fs::write(root.join("visible.mp3"), b"")?;
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000))?;

        // Root (or CAP_DAC_OVERRIDE) can still read the folder; detect that.
        let enforced = fs::read_dir(&locked).is_err();
        let found = discover(root);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755))?;
        let found = found?;

        let names = names(&found.paths, root);
        assert!(names.contains(&"visible.mp3".to_owned()));
        if enforced {
            assert!(
                !names.contains(&"locked/hidden.mp3".to_owned()),
                "{names:?}"
            );
            assert_eq!(found.skipped, 1);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_root_is_an_error() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("music");
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o000))?;
        let enforced = fs::read_dir(&root).is_err();
        let result = discover(&root).map_err(|e| format!("{e:#}"));
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755))?;

        if enforced {
            let Err(msg) = result else {
                anyhow::bail!("expected an error")
            };
            assert!(msg.contains("cannot read music folder"), "{msg}");
            assert!(msg.contains("music"), "{msg}");
        }
        Ok(())
    }

    #[test]
    fn missing_root_is_an_error() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        assert!(discover(&tmp.path().join("gone")).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_folder_is_listed_once() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::create_dir(root.join("real"))?;
        fs::write(root.join("real/song.mp3"), b"")?;
        std::os::unix::fs::symlink(root.join("real"), root.join("alias"))?;
        std::os::unix::fs::symlink(root.join("real/song.mp3"), root.join("song-link.mp3"))?;

        let found = discover(root)?;
        assert_eq!(found.paths.len(), 1, "{:?}", found.paths);
        Ok(())
    }

    /// d0..dN each hold two links to d(N+1): without dedup the walk is 2^N.
    #[cfg(unix)]
    #[test]
    fn symlink_dag_is_not_exponential() -> Result<()> {
        const LEVELS: usize = 24;
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        for i in 0..=LEVELS {
            fs::create_dir(root.join(format!("d{i}")))?;
        }
        for i in 0..LEVELS {
            let next = root.join(format!("d{}", i + 1));
            std::os::unix::fs::symlink(&next, root.join(format!("d{i}/a")))?;
            std::os::unix::fs::symlink(&next, root.join(format!("d{i}/b")))?;
        }
        fs::write(root.join(format!("d{LEVELS}/song.flac")), b"")?;

        let start = std::time::Instant::now();
        let found = discover(root)?;
        assert_eq!(found.paths.len(), 1);
        assert!(start.elapsed() < Duration::from_secs(5));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loop_does_not_hang() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path();
        fs::create_dir(root.join("a"))?;
        fs::write(root.join("a/song.ogg"), b"")?;
        std::os::unix::fs::symlink(root, root.join("a/loop"))?;

        let found = discover(root)?;
        assert_eq!(names(&found.paths, root), ["a/song.ogg"]);
        Ok(())
    }

    #[test]
    fn untagged_file_uses_filename_and_unknown() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("my track.mp3");
        fs::write(&path, b"not really audio")?;

        let t = read_track(path.clone());
        assert_eq!(t.title, "my track");
        assert_eq!(t.artist, UNKNOWN);
        assert_eq!(t.album, UNKNOWN);
        assert_eq!(t.number, None);
        assert_eq!(t.duration, None);
        assert_eq!(t.path, path);
        Ok(())
    }

    #[test]
    fn garbage_and_truncated_files_fall_back_for_every_extension() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let wav = wav_bytes();
        let samples: [(&str, &[u8]); 9] = [
            ("empty", b""),
            ("garbage", b"\xff\xfb\x90\x00garbage\x00\x00ID3"),
            ("id3", b"ID3\x04\x00\x00\x7f\x7f\x7f\x7fTIT2"),
            ("flac", b"fLaC\x00\x00\x00\x22\x00"),
            ("ogg", b"OggS\x00\x02\x00\x00\x00\x00"),
            ("mp4", b"\x00\x00\x00\x18ftypM4A \x00\x00\x00\x00"),
            ("riff", b"RIFF\xff\xff\xff\xffWAVEfmt "),
            ("wav-half", &wav[..30]),
            ("adts", b"\xff\xf1\x50\x80\x00\x1f\xfc"),
        ];
        for ext in SUPPORTED_EXTENSIONS {
            for (name, bytes) in samples {
                let path = tmp.path().join(format!("{name}.{ext}"));
                fs::write(&path, bytes)?;
                let t = read_track(path);
                assert_eq!(t.title, name, "{name}.{ext}");
            }
        }
        Ok(())
    }

    #[test]
    fn untagged_valid_wav_uses_filename_and_reads_duration() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("plain.wav");
        fs::write(&path, wav_bytes())?;

        let t = read_track(path);
        assert_eq!(t.title, "plain");
        assert_eq!(t.artist, UNKNOWN);
        assert_eq!(t.duration, Some(Duration::from_millis(100)));
        Ok(())
    }

    #[test]
    fn tagged_file_reads_tags_and_sanitises_them() -> Result<()> {
        use lofty::config::WriteOptions;
        use lofty::tag::{TagExt, TagType};

        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("file.wav");
        fs::write(&path, wav_bytes())?;

        let mut tag = Tag::new(TagType::Id3v2);
        tag.set_title("Song".to_owned());
        tag.set_artist("Ba\u{1b}[31mnd".to_owned());
        tag.set_album("LP".to_owned());
        tag.set_track(7);
        tag.save_to_path(&path, WriteOptions::default())?;

        let t = read_track(path);
        assert_eq!(t.title, "Song");
        assert_eq!(t.artist, "Ba [31mnd");
        assert_eq!(t.album, "LP");
        assert_eq!(t.number, Some(7));
        Ok(())
    }

    #[test]
    fn track_numbers_sort_numerically() {
        let mut tracks = vec![
            track("A", "LP", Some(10), "ten"),
            track("A", "LP", Some(1), "one"),
            track("A", "LP", Some(2), "two"),
        ];
        sort_tracks(&mut tracks);
        let order: Vec<_> = tracks.iter().map(|t| t.number).collect();
        assert_eq!(order, [Some(1), Some(2), Some(10)]);
    }

    #[test]
    fn sorts_by_artist_album_number_title_case_insensitively() {
        let mut tracks = vec![
            track("beta", "x", Some(1), "b1"),
            track("Alpha", "Zed", Some(1), "a-z1"),
            track("alpha", "able", None, "B untagged"),
            track("alpha", "able", None, "a untagged"),
            track("ALPHA", "Able", Some(3), "a3"),
        ];
        sort_tracks(&mut tracks);
        let titles: Vec<_> = tracks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["a3", "a untagged", "B untagged", "a-z1", "b1"]);
    }

    #[test]
    fn scan_returns_sorted_tracks() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        fs::write(tmp.path().join("b.mp3"), b"")?;
        fs::write(tmp.path().join("A.flac"), b"")?;
        fs::write(tmp.path().join("c.txt"), b"")?;
        let titles: Vec<_> = scan(tmp.path())?
            .tracks
            .into_iter()
            .map(|t| t.title)
            .collect();
        assert_eq!(titles, ["A", "b"]);
        Ok(())
    }
}
