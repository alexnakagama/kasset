//! Reading and writing the per-user state file (D1, D2 and D7 of the
//! `persist-session-state` design).
//!
//! [`StateStore`] is what the app talks to: [`FsStateStore`] keeps the real
//! file, [`NoState`] (`--no-state`, no known state folder, and every test that
//! builds an `App` directly) reads and writes nothing. [`location`] picks the
//! file from the folders `dirs` reports, without calling `dirs` itself, so it
//! and every store built in tests stay hermetic.
//!
//! Invariants:
//! - **Reading** goes through [`read_limited`]: regular files only (a FIFO
//!   never blocks), at most [`MAX_STATE_BYTES`]. A truncated read drops its
//!   last, possibly cut, line before [`parse`], so a cut `volume 100` never
//!   reads as `volume 10`. A file that cannot be read or parsed loads as empty.
//! - **Saving** is read-merge-write: the file is read again and the session
//!   merged in, so folders another kasset saved meanwhile survive. The result
//!   is written to `.state.tmp-<pid>-<random>` in the same folder, created with
//!   `create_new` (Unix: mode `0600`), flushed with `sync_all`, then renamed
//!   over the file, so a crash leaves the old file or the new one, and a
//!   symbolic link at the file's path is replaced, never written through. A
//!   taken temp name is retried once with a new random part; the temp file is
//!   removed on failure, and `.state.tmp-*` leftovers older than
//!   [`STALE_TEMP_AGE`] are swept, best effort, before each write.
//! - The state folder is created on first save. On Unix a newly created
//!   `kasset` folder gets mode `0700`; an existing one keeps its mode.
//! - A file of a **newer** format version is never written: once the store
//!   has seen one (at load or at save time) every save is
//!   `Ok(Saved::Skipped)`.
//! - Nothing is written anywhere but the state folder (and the missing
//!   parents of that folder, created on first save), and the store never
//!   reads or writes inside the library folder.
//! - A file that exists but cannot be read (permission denied, I/O error, a
//!   link loop) is never replaced: the save fails instead, so other folders'
//!   resume points are not lost to a transient error.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rand::RngExt as _;

use super::{
    FolderEntry, MAX_STATE_BYTES, Parsed, Resume, RootKey, Session, Settings, StateFile, merge,
    parse, render,
};
use crate::bounded_read::read_limited;
use crate::text::display_path;

/// Prefix of the temporary files a save writes next to the state file.
const TEMP_PREFIX: &str = ".state.tmp-";

/// A leftover temporary file older than this is assumed abandoned.
pub const STALE_TEMP_AGE: Duration = Duration::from_secs(10 * 60);

/// At most this many folder entries are looked at when sweeping leftovers.
const MAX_SWEEP_ENTRIES: usize = 10_000;

/// What a load found for this run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Loaded {
    /// The saved settings, if the file had any.
    pub settings: Option<Settings>,
    /// This library folder's entry (empty if it has none).
    pub folder: FolderEntry,
    /// The file is from a newer kasset: it was not read, and is never written.
    pub newer: bool,
}

/// The outcome of a successful [`StateStore::save`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Saved {
    /// The file was replaced.
    Written,
    /// Nothing was written, on purpose (no store, or a newer file).
    Skipped,
}

/// Where the app's saved state lives.
pub trait StateStore {
    /// Reads the settings and this folder's entry. Never fails: a missing,
    /// unreadable or damaged file loads as empty.
    fn load(&mut self) -> Loaded;

    /// Saves `session` for this store's folder, keeping `keep` as the resume
    /// point when the session has none (see [`merge`]).
    ///
    /// # Errors
    ///
    /// A message naming the state file and the reason, ready to show.
    fn save(&mut self, session: &Session, keep: Option<&Resume>) -> Result<Saved, String>;

    /// Whether this store reads or writes anything at all; `false` lets the
    /// app skip building sessions it would never save.
    fn persists(&self) -> bool {
        true
    }
}

/// The state file under the per-user state folder `state`, else under the
/// local data folder `local` (D2): `<folder>/kasset/state`. A relative folder
/// is ignored, so the file never lands relative to the working directory.
#[must_use]
pub fn location(state: Option<PathBuf>, local: Option<PathBuf>) -> Option<PathBuf> {
    [state, local]
        .into_iter()
        .flatten()
        .find(|dir| dir.is_absolute())
        .map(|dir| dir.join("kasset").join("state"))
}

/// The store used when nothing may be read or saved.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoState;

impl StateStore for NoState {
    fn load(&mut self) -> Loaded {
        Loaded::default()
    }

    fn save(&mut self, _session: &Session, _keep: Option<&Resume>) -> Result<Saved, String> {
        Ok(Saved::Skipped)
    }

    fn persists(&self) -> bool {
        false
    }
}

/// The state file on disk, for one library folder.
#[derive(Debug)]
pub struct FsStateStore {
    /// The state file.
    file: PathBuf,
    /// This run's library root; `None` if it cannot be saved (outside Unix,
    /// a root that is not valid Unicode), so only the settings are.
    root: Option<RootKey>,
    /// A newer file was seen: never write.
    newer: bool,
}

impl FsStateStore {
    /// The store for the state file `file` and the canonical library root
    /// `root`. Touches nothing until [`StateStore::load`] or
    /// [`StateStore::save`].
    #[must_use]
    pub fn new(file: PathBuf, root: &Path) -> Self {
        Self {
            file,
            root: RootKey::new(root),
            newer: false,
        }
    }

    /// Reads and parses the file; anything unreadable is [`Parsed::Invalid`].
    ///
    /// A missing file, and something that is not a regular file (a FIFO, a
    /// socket), hold no state: they read as [`Parsed::Invalid`] and a save
    /// replaces them. Any other error (permission denied, I/O error, too many
    /// open files, a link loop) is returned, so a save never replaces a file
    /// that may hold other folders' state just because it could not be read.
    fn read(&self) -> io::Result<Parsed> {
        match read_limited(&self.file, MAX_STATE_BYTES as u64) {
            Ok((bytes, truncated, _)) => Ok(parse(whole_lines(&bytes, truncated))),
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
                ) =>
            {
                Ok(Parsed::Invalid)
            }
            Err(err) => Err(err),
        }
    }

    /// Creates the state folder if needed (see the module docs).
    fn ensure_dir(&self) -> io::Result<&Path> {
        let dir = self
            .file
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no parent folder"))?;
        if !dir.is_dir() {
            if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
                fs::create_dir_all(parent)?;
            }
            create_private_dir(dir)?;
        }
        Ok(dir)
    }

    /// Writes `bytes` atomically over the state file in `dir`.
    fn write(&self, dir: &Path, bytes: &[u8]) -> io::Result<()> {
        sweep_stale_temps(dir);
        let (tmp, mut file) = match create_temp(dir) {
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => create_temp(dir)?,
            other => other?,
        };
        let written = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, &self.file)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        } else {
            sync_dir(dir);
        }
        written
    }

    fn save_inner(&mut self, session: &Session, keep: Option<&Resume>) -> io::Result<Saved> {
        if self.newer {
            return Ok(Saved::Skipped);
        }
        let current = match self.read()? {
            Parsed::Valid(file) => file,
            Parsed::Newer => {
                self.newer = true;
                return Ok(Saved::Skipped);
            }
            // Damaged, missing or not a regular file: replaced by a valid file.
            Parsed::Invalid => StateFile::default(),
        };
        let merged = match &self.root {
            Some(root) => merge(current, root, session, keep),
            None => StateFile {
                settings: Some(session.settings),
                ..current
            },
        };
        let dir = self.ensure_dir()?;
        self.write(dir, &render(&merged))?;
        Ok(Saved::Written)
    }
}

impl StateStore for FsStateStore {
    fn load(&mut self) -> Loaded {
        // Unreadable: start with the defaults (a save will report the error).
        match self.read().unwrap_or(Parsed::Invalid) {
            Parsed::Valid(file) => {
                let folder = self
                    .root
                    .as_ref()
                    .and_then(|root| file.folders.into_iter().find(|(key, _)| key == root))
                    .map(|(_, entry)| entry)
                    .unwrap_or_default();
                Loaded {
                    settings: file.settings,
                    folder,
                    newer: false,
                }
            }
            Parsed::Newer => {
                self.newer = true;
                Loaded {
                    newer: true,
                    ..Loaded::default()
                }
            }
            Parsed::Invalid => Loaded::default(),
        }
    }

    fn save(&mut self, session: &Session, keep: Option<&Resume>) -> Result<Saved, String> {
        self.save_inner(session, keep)
            .map_err(|err| format!("cannot save state to {}: {err}", display_path(&self.file)))
    }
}

/// `bytes` up to and including its last line break when the read was
/// `truncated` (the last line may be cut), else all of it.
fn whole_lines(bytes: &[u8], truncated: bool) -> &[u8] {
    if !truncated {
        return bytes;
    }
    bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(&[][..], |end| &bytes[..=end])
}

/// Creates the folder `dir` (its parent must exist); on Unix with mode
/// `0700`, set explicitly so a narrow umask cannot leave it unusable. The
/// mode is set through a handle opened with `O_NOFOLLOW | O_DIRECTORY`, so a
/// link swapped in for the new folder is never followed. A folder that
/// appeared meanwhile is accepted as it is.
fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let created = {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .and_then(|()| {
                use rustix::fs::{Mode, OFlags};
                let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
                let handle = rustix::fs::open(dir, flags, Mode::empty())?;
                rustix::fs::fchmod(&handle, Mode::from_raw_mode(0o700))?;
                Ok(())
            })
    };
    #[cfg(not(unix))]
    let created = fs::create_dir(dir);
    match created {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        other => other,
    }
}

/// Creates a new temporary file in `dir` with a fresh random name; on Unix
/// owner-only (`0600`, set explicitly so the umask cannot change it).
fn create_temp(dir: &Path) -> io::Result<(PathBuf, File)> {
    let name = format!(
        "{TEMP_PREFIX}{}-{:016x}",
        std::process::id(),
        rand::rng().random::<u64>()
    );
    let tmp = dir.join(name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(&tmp)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(err) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
            drop(file);
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
    }
    Ok((tmp, file))
}

/// Removes regular `.state.tmp-*` files in `dir` untouched for
/// [`STALE_TEMP_AGE`]: leftovers of killed runs. Best effort.
fn sweep_stale_temps(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.take(MAX_SWEEP_ENTRIES).flatten() {
        let name = entry.file_name();
        if !name.as_encoded_bytes().starts_with(TEMP_PREFIX.as_bytes()) {
            continue;
        }
        // `DirEntry::metadata` does not follow links.
        let stale = entry.metadata().is_ok_and(|meta| {
            meta.file_type().is_file()
                && meta
                    .modified()
                    .ok()
                    .and_then(|time| time.elapsed().ok())
                    .is_some_and(|age| age >= STALE_TEMP_AGE)
        });
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Flushes the rename in `dir` to disk (Unix; best effort).
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use anyhow::Result;

    use super::*;
    use crate::player::Volume;
    use crate::queue::Repeat;
    use crate::state::{RelPath, SavedSource};

    fn settings(volume: u8) -> Settings {
        Settings {
            volume: Volume::new(volume),
            shuffle: true,
            repeat: Repeat::All,
            visualizer: false,
            lyrics: true,
        }
    }

    fn resume(track: &str, position_ms: u64) -> Resume {
        Resume {
            track: RelPath::new(Path::new(track))
                .unwrap_or_else(|| panic!("invalid relative path {track:?}")),
            item: Some(2),
            position: Duration::from_millis(position_ms),
            source: SavedSource::Playlist("Running".to_owned()),
        }
    }

    fn session(volume: u8, track: Option<&str>) -> Session {
        Session {
            settings: settings(volume),
            folder: FolderEntry {
                resume: track.map(|t| resume(t, 135_000)),
                open: Some("Running".to_owned()),
            },
        }
    }

    /// A state file path in a fresh temp folder: `<tmp>/state/kasset/state`.
    fn state_path(tmp: &tempfile::TempDir) -> PathBuf {
        tmp.path().join("state").join("kasset").join("state")
    }

    fn temp_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with(TEMP_PREFIX))
                    .collect()
            })
            .unwrap_or_default()
    }

    // --- location ---

    #[test]
    fn location_prefers_the_state_folder() {
        let state = PathBuf::from("/s");
        let local = PathBuf::from("/l");
        assert_eq!(
            location(Some(state.clone()), Some(local.clone())),
            Some(state.join("kasset").join("state"))
        );
        assert_eq!(
            location(None, Some(local.clone())),
            Some(local.join("kasset").join("state"))
        );
        assert_eq!(
            location(Some(state.clone()), None),
            Some(state.join("kasset").join("state"))
        );
    }

    #[test]
    fn no_known_folder_gives_no_location() {
        assert_eq!(location(None, None), None);
    }

    #[test]
    fn relative_folders_are_ignored() {
        let local = std::env::temp_dir();
        assert_eq!(
            location(Some(PathBuf::from("relative")), Some(local.clone())),
            Some(local.join("kasset").join("state"))
        );
        assert_eq!(location(Some(PathBuf::from("rel")), None), None);
    }

    // --- load / save ---

    #[test]
    fn save_then_load_gives_back_the_session() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        let saved = session(60, Some("Band/01 Song.flac"));
        let mut store = FsStateStore::new(path.clone(), Path::new("/music"));
        assert_eq!(store.save(&saved, None), Ok(Saved::Written));
        assert!(path.is_file());

        let mut again = FsStateStore::new(path, Path::new("/music"));
        let loaded = again.load();
        assert_eq!(loaded.settings, Some(saved.settings));
        assert_eq!(loaded.folder, saved.folder);
        assert!(!loaded.newer);
        Ok(())
    }

    #[test]
    fn missing_file_loads_as_empty() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut store = FsStateStore::new(state_path(&tmp), Path::new("/music"));
        assert_eq!(store.load(), Loaded::default());
        Ok(())
    }

    #[test]
    fn another_folder_entry_is_not_loaded() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        FsStateStore::new(path.clone(), Path::new("/a"))
            .save(&session(40, Some("x.flac")), None)
            .map_err(anyhow::Error::msg)?;
        let loaded = FsStateStore::new(path, Path::new("/b")).load();
        assert_eq!(loaded.settings, Some(settings(40)));
        assert_eq!(loaded.folder, FolderEntry::default());
        Ok(())
    }

    #[test]
    fn damaged_file_loads_as_empty_and_is_replaced() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        fs::create_dir_all(path.parent().unwrap_or(tmp.path()))?;
        let junk: Vec<u8> = (0..4096u32)
            .map(|i| u8::try_from(i.wrapping_mul(2_654_435_761) >> 24).unwrap_or(0))
            .collect();
        fs::write(&path, &junk)?;

        let mut store = FsStateStore::new(path.clone(), Path::new("/music"));
        assert_eq!(store.load(), Loaded::default());
        assert_eq!(store.save(&session(60, None), None), Ok(Saved::Written));
        let bytes = fs::read(&path)?;
        assert!(bytes.starts_with(b"kasset-state 1\n"));
        assert!(matches!(parse(&bytes), Parsed::Valid(_)));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn file_and_new_folder_are_owner_only() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        FsStateStore::new(path.clone(), Path::new("/music"))
            .save(&session(60, None), None)
            .map_err(anyhow::Error::msg)?;
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        let dir = path.parent().unwrap_or(tmp.path());
        assert_eq!(fs::metadata(dir)?.permissions().mode() & 0o777, 0o700);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn existing_folder_keeps_its_mode() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        let dir = path.parent().unwrap_or(tmp.path()).to_path_buf();
        fs::create_dir_all(&dir)?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))?;
        FsStateStore::new(path.clone(), Path::new("/music"))
            .save(&session(60, None), None)
            .map_err(anyhow::Error::msg)?;
        assert_eq!(fs::metadata(&dir)?.permissions().mode() & 0o777, 0o755);
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        Ok(())
    }

    #[test]
    fn two_instances_keep_both_folders() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        let mut music = FsStateStore::new(path.clone(), Path::new("/home/u/Music"));
        let mut other = FsStateStore::new(path.clone(), Path::new("/mnt/other"));
        // Both loaded before either saved, as two running instances would.
        music.load();
        other.load();
        music
            .save(&session(60, Some("a.flac")), None)
            .map_err(anyhow::Error::msg)?;
        other
            .save(&session(40, Some("b.flac")), None)
            .map_err(anyhow::Error::msg)?;

        let Parsed::Valid(file) = parse(&fs::read(&path)?) else {
            panic!("the state file does not parse");
        };
        let roots: Vec<_> = file.folders.iter().map(|(root, _)| root.clone()).collect();
        assert_eq!(
            roots,
            [
                RootKey::new(Path::new("/mnt/other")),
                RootKey::new(Path::new("/home/u/Music")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
        );
        assert_eq!(
            FsStateStore::new(path, Path::new("/home/u/Music"))
                .load()
                .folder
                .resume,
            Some(resume("a.flac", 135_000))
        );
        Ok(())
    }

    #[test]
    fn newer_file_seen_at_load_is_never_written() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        fs::create_dir_all(path.parent().unwrap_or(tmp.path()))?;
        let newer = b"kasset-state 2\nvolume 10\nwhatever\n";
        fs::write(&path, newer)?;

        let mut store = FsStateStore::new(path.clone(), Path::new("/music"));
        let loaded = store.load();
        assert!(loaded.newer);
        assert_eq!(loaded.settings, None);
        assert_eq!(store.save(&session(60, None), None), Ok(Saved::Skipped));
        assert_eq!(fs::read(&path)?, newer);
        // Still skipped once the file is valid again: no writes this run.
        fs::write(&path, b"kasset-state 1\n")?;
        assert_eq!(store.save(&session(60, None), None), Ok(Saved::Skipped));
        assert_eq!(fs::read(&path)?, b"kasset-state 1\n");
        Ok(())
    }

    #[test]
    fn newer_file_appearing_after_load_is_not_written() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        let mut store = FsStateStore::new(path.clone(), Path::new("/music"));
        assert!(!store.load().newer);
        fs::create_dir_all(path.parent().unwrap_or(tmp.path()))?;
        let newer = b"kasset-state 99\n";
        fs::write(&path, newer)?;
        assert_eq!(store.save(&session(60, None), None), Ok(Saved::Skipped));
        assert_eq!(fs::read(&path)?, newer);
        assert_eq!(store.save(&session(60, None), None), Ok(Saved::Skipped));
        assert_eq!(fs::read(&path)?, newer);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fifo_loads_as_empty_without_blocking() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        fs::create_dir_all(path.parent().unwrap_or(tmp.path()))?;
        if !crate::test_support::make_fifo(&path) {
            return Ok(()); // mkfifo unavailable
        }
        let mut store = FsStateStore::new(path, Path::new("/music"));
        assert_eq!(store.load(), Loaded::default());
        Ok(())
    }

    #[test]
    fn oversized_file_drops_its_cut_last_line() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        fs::create_dir_all(path.parent().unwrap_or(tmp.path()))?;
        // Pad with an ignored key so `volume 100` is cut after `volume 10`.
        let head = "kasset-state 1\n";
        let line = "volume 100\n";
        let cut_at = MAX_STATE_BYTES - "volume 10".len();
        let pad = cut_at - head.len() - "pad \n".len();
        let text = format!("{head}pad {}\n{line}", "x".repeat(pad));
        assert_eq!(text.len(), cut_at + line.len());
        fs::write(&path, text)?;

        let loaded = FsStateStore::new(path, Path::new("/music")).load();
        assert_eq!(loaded.settings, None, "a cut line must not be read");
        Ok(())
    }

    #[test]
    fn whole_lines_cuts_only_truncated_reads() {
        assert_eq!(whole_lines(b"a\nb", false), b"a\nb");
        assert_eq!(whole_lines(b"a\nb", true), b"a\n");
        assert_eq!(whole_lines(b"a\n", true), b"a\n");
        assert_eq!(whole_lines(b"abc", true), b"");
    }

    #[test]
    fn unwritable_folder_fails_naming_the_path() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        // The state folder is a regular file.
        let dir = tmp.path().join("kasset");
        fs::write(&dir, b"not a folder")?;
        let path = dir.join("state");
        let mut store = FsStateStore::new(path.clone(), Path::new("/music"));
        let err = store
            .save(&session(60, None), None)
            .err()
            .unwrap_or_else(|| panic!("the save must fail"));
        assert!(
            err.starts_with(&format!("cannot save state to {}: ", display_path(&path))),
            "{err}"
        );
        assert_eq!(fs::read(&dir)?, b"not a folder");
        assert_eq!(temp_files(tmp.path()), Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn failed_rename_removes_the_temp_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        // A non-empty folder at the state path: the rename over it fails.
        fs::create_dir_all(path.join("blocker"))?;
        let mut store = FsStateStore::new(path.clone(), Path::new("/music"));
        assert!(store.save(&session(60, None), None).is_err());
        assert_eq!(
            temp_files(path.parent().unwrap_or(tmp.path())),
            Vec::<String>::new()
        );
        assert!(path.join("blocker").is_dir());
        Ok(())
    }

    #[test]
    fn stale_temp_files_are_swept_and_fresh_ones_kept() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        let dir = path.parent().unwrap_or(tmp.path()).to_path_buf();
        fs::create_dir_all(&dir)?;
        let stale = dir.join(".state.tmp-1-00000000000000aa");
        let fresh = dir.join(".state.tmp-2-00000000000000bb");
        let other = dir.join("unrelated");
        for file in [&stale, &fresh, &other] {
            fs::write(file, b"x")?;
        }
        let old = SystemTime::now() - (STALE_TEMP_AGE + Duration::from_secs(60));
        for file in [&stale, &other] {
            File::options().write(true).open(file)?.set_modified(old)?;
        }

        FsStateStore::new(path.clone(), Path::new("/music"))
            .save(&session(60, None), None)
            .map_err(anyhow::Error::msg)?;
        assert!(!stale.exists(), "a stale leftover is removed");
        assert!(fresh.exists(), "a recent temp file may be another run's");
        assert!(other.exists(), "only temp files are swept");
        assert_eq!(temp_files(&dir), [".state.tmp-2-00000000000000bb"]);
        assert!(path.is_file());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_file_is_never_replaced() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        fs::create_dir_all(path.parent().unwrap_or(tmp.path()))?;
        // A link loop: it exists, but reading it fails (not `NotFound`).
        std::os::unix::fs::symlink(&path, &path)?;
        let mut store = FsStateStore::new(path.clone(), Path::new("/music"));
        assert_eq!(store.load(), Loaded::default());
        let err = store
            .save(&session(60, None), None)
            .err()
            .unwrap_or_else(|| panic!("an unreadable file must not be replaced"));
        assert!(err.starts_with("cannot save state to "), "{err}");
        assert!(fs::symlink_metadata(&path)?.file_type().is_symlink());
        assert_eq!(
            temp_files(path.parent().unwrap_or(tmp.path())),
            Vec::<String>::new()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_replaced_by_a_save() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        fs::create_dir_all(path.parent().unwrap_or(tmp.path()))?;
        if !crate::test_support::make_fifo(&path) {
            return Ok(()); // mkfifo unavailable
        }
        let mut store = FsStateStore::new(path.clone(), Path::new("/music"));
        assert_eq!(store.save(&session(60, None), None), Ok(Saved::Written));
        assert!(fs::symlink_metadata(&path)?.file_type().is_file());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn sweep_never_follows_links_or_removes_folders() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        let dir = path.parent().unwrap_or(tmp.path()).to_path_buf();
        fs::create_dir_all(&dir)?;
        let old = SystemTime::now() - (STALE_TEMP_AGE + Duration::from_secs(60));
        let outside = tmp.path().join("outside");
        fs::write(&outside, b"keep me")?;
        File::options()
            .write(true)
            .open(&outside)?
            .set_modified(old)?;
        let link = dir.join(".state.tmp-3-00000000000000cc");
        std::os::unix::fs::symlink(&outside, &link)?;
        let folder = dir.join(".state.tmp-4-00000000000000dd");
        fs::create_dir(&folder)?;
        fs::write(folder.join("inner"), b"x")?;
        File::open(&folder)?.set_modified(old)?;

        FsStateStore::new(path, Path::new("/music"))
            .save(&session(60, None), None)
            .map_err(anyhow::Error::msg)?;
        assert_eq!(fs::read(&outside)?, b"keep me");
        assert!(fs::symlink_metadata(&link)?.file_type().is_symlink());
        assert!(folder.join("inner").is_file());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_state_file_is_replaced() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        fs::create_dir_all(path.parent().unwrap_or(tmp.path()))?;
        let target = tmp.path().join("target");
        fs::write(&target, b"kasset-state 1\nvolume 20\n")?;
        std::os::unix::fs::symlink(&target, &path)?;

        let mut store = FsStateStore::new(path.clone(), Path::new("/music"));
        // Reading through the link is harmless.
        assert_eq!(
            store.load().settings.map(|s| s.volume),
            Some(Volume::new(20))
        );
        store
            .save(&session(60, None), None)
            .map_err(anyhow::Error::msg)?;
        assert!(fs::symlink_metadata(&path)?.file_type().is_file());
        assert_eq!(fs::read(&target)?, b"kasset-state 1\nvolume 20\n");
        Ok(())
    }

    #[test]
    fn keep_is_written_without_a_current_track() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = state_path(&tmp);
        let kept = resume("gone/away.flac", 5_000);
        FsStateStore::new(path.clone(), Path::new("/music"))
            .save(&session(60, None), Some(&kept))
            .map_err(anyhow::Error::msg)?;
        let loaded = FsStateStore::new(path, Path::new("/music")).load();
        assert_eq!(loaded.folder.resume, Some(kept));
        Ok(())
    }

    #[test]
    fn no_state_loads_nothing_and_writes_nothing() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let mut store = NoState;
        assert_eq!(store.load(), Loaded::default());
        assert_eq!(
            store.save(&session(60, Some("a.flac")), None),
            Ok(Saved::Skipped)
        );
        assert!(!store.persists());
        assert!(FsStateStore::new(state_path(&tmp), Path::new("/m")).persists());
        assert_eq!(fs::read_dir(tmp.path())?.count(), 0);
        Ok(())
    }
}
