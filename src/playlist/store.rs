//! Reading and writing playlist files in the `Playlists` folder.
//!
//! Saves are atomic (temporary file in the same folder, then `rename`), never
//! overwrite a file another program changed since it was read, and keep the
//! file's permission bits and, for a symlinked playlist, the link. Creating and
//! renaming never replace an existing file: they go through `hard_link`, which
//! fails if the name is taken, falling back to check-then-rename only on
//! filesystems without hard links.
//!
//! Nothing is ever written outside the playlists folder: a playlist that is a
//! symbolic link to a file outside it (judged by where it resolves, so `..`
//! and link chains count) is read-only. This is checked when the playlist is
//! listed or loaded, and again right before each save. A dangling link is not
//! a playlist: it is not listed, cannot be loaded, and a save through a link
//! that became dangling fails as `NotFound`.
//!
//! Replacing the file gives it a new inode: owner and group are restored on a
//! best-effort basis, but extended attributes, ACLs and other hard links to the
//! old file are not kept.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use super::name::{NameError, check_new_name};
use super::{
    EXTENSION, FileStamp, MAX_FILE_BYTES, Matching, PLAYLISTS_DIR, Playlist, ReadOnlyReason,
    Resolver,
};
use crate::bounded_read::read_limited;
use crate::text::{display_path, is_invisible_format};

/// At most this many playlists are listed, so a folder flooded with files
/// cannot stall the interface (each listed playlist is read to count tracks).
const MAX_LISTED: usize = 1_000;

/// At most this many folder entries are looked at when listing.
const MAX_DIR_ENTRIES: usize = 100_000;

/// At most this many bytes are read in total to count tracks when listing;
/// playlists past the budget are listed without a count.
const LIST_BYTE_BUDGET: u64 = 64 * 1024 * 1024;

/// A leftover temporary file older than this is assumed abandoned.
const STALE_TEMP_AGE: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// File names longer than this get a short temporary name, so that saving a
/// playlist with a long name (made outside kasset) does not fail on the
/// temporary file's name.
const MAX_TEMP_BASE_BYTES: usize = 220;

/// A playlist as shown in the playlists list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistInfo {
    pub name: String,
    /// Tracks matched in the library without filesystem access (an entry that
    /// only matches through a symlink counts once the playlist is opened);
    /// `None` when the file could not be read or the listing's read budget ran out.
    pub tracks: Option<usize>,
    /// Why the playlist cannot be edited, if it cannot.
    pub read_only: Option<ReadOnlyReason>,
}

/// Result of a save that did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    Saved,
    /// Another program changed the file since it was read; nothing was written.
    ChangedOnDisk,
}

/// What a store operation was doing when it failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    List,
    Read,
    Save,
    Create,
    Rename,
    Delete,
}

impl Action {
    const fn verb(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Read => "read",
            Self::Save => "save",
            Self::Create => "create",
            Self::Rename => "rename",
            Self::Delete => "delete",
        }
    }
}

/// Why a store operation failed. Nothing was changed on disk.
#[derive(Debug)]
pub enum StoreError {
    /// The name breaks a naming rule or is taken.
    Name(NameError),
    /// The playlist file does not exist (any more).
    NotFound,
    ReadOnly(ReadOnlyReason),
    Io {
        action: Action,
        source: io::Error,
    },
}

impl StoreError {
    fn io(action: Action) -> impl FnOnce(io::Error) -> Self {
        move |source| {
            if source.kind() == io::ErrorKind::NotFound && action != Action::Create {
                Self::NotFound
            } else {
                Self::Io { action, source }
            }
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(err) => err.fmt(f),
            Self::NotFound => f.write_str("the playlist no longer exists"),
            Self::ReadOnly(reason) => reason.fmt(f),
            Self::Io { action, source } => write!(f, "cannot {}: {source}", action.verb()),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Name(err) => Some(err),
            Self::Io { source, .. } => Some(source),
            Self::NotFound | Self::ReadOnly(_) => None,
        }
    }
}

impl From<NameError> for StoreError {
    fn from(err: NameError) -> Self {
        Self::Name(err)
    }
}

/// Where playlists live. `App` talks to this, never to the filesystem.
pub trait PlaylistStore: fmt::Debug {
    /// Every playlist, sorted by name ignoring case. A missing folder is an
    /// empty list.
    ///
    /// # Errors
    ///
    /// Fails when the folder exists but cannot be read.
    fn list(&self, resolver: &Resolver) -> Result<Vec<PlaylistInfo>, StoreError>;

    /// Reads and parses a playlist.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when it does not exist, else an I/O error.
    fn load(&self, name: &str, resolver: &Resolver) -> Result<Playlist, StoreError>;

    /// `true` when the file on disk differs from what `playlist` was read from.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] when the file is gone, else an I/O error.
    fn changed(&self, playlist: &Playlist) -> Result<bool, StoreError>;

    /// A new, empty playlist (not yet on disk) to fill and pass to [`Self::create`].
    ///
    /// # Errors
    ///
    /// Fails when `name` breaks a naming rule.
    fn blank(&self, name: &str) -> Result<Playlist, StoreError>;

    /// Writes a new playlist file, creating the folder if needed. Never
    /// replaces an existing file.
    ///
    /// # Errors
    ///
    /// [`NameError::Taken`] when the name is used (ignoring case), else an I/O error.
    fn create(&self, playlist: &mut Playlist) -> Result<(), StoreError>;

    /// Saves an edited playlist atomically unless the file changed on disk.
    ///
    /// # Errors
    ///
    /// Fails when the playlist is read-only, gone, or cannot be written.
    fn save(&self, playlist: &mut Playlist) -> Result<SaveOutcome, StoreError>;

    /// Renames a playlist file. Never replaces another playlist.
    ///
    /// # Errors
    ///
    /// Fails when `to` breaks a naming rule or is taken, `from` is gone, or
    /// the rename fails.
    fn rename(&self, from: &str, to: &str) -> Result<(), StoreError>;

    /// Deletes a playlist file (a symlink itself, never its target).
    ///
    /// # Errors
    ///
    /// Fails when the file is gone or cannot be removed.
    fn delete(&self, name: &str) -> Result<(), StoreError>;
}

/// Playlists stored as files in `<library root>/Playlists`.
#[derive(Debug, Clone)]
pub struct FsStore {
    dir: PathBuf,
}

impl FsStore {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            dir: root.join(PLAYLISTS_DIR),
        }
    }

    /// The playlists folder (it may not exist yet).
    #[cfg(test)]
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file for `name`, refusing anything that is not a single plain file
    /// name in the playlists folder.
    fn path_for(&self, name: &str) -> Result<PathBuf, StoreError> {
        if !is_listable(name) {
            return Err(StoreError::Name(NameError::Invisible));
        }
        let file = format!("{name}.{EXTENSION}");
        let mut components = Path::new(&file).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(_)), None) => Ok(self.dir.join(file)),
            _ => Err(StoreError::Name(NameError::Forbidden('/'))),
        }
    }

    /// The file to replace when saving `path`: the file itself or, for a
    /// symbolic link, its resolved target, which must lie inside the
    /// playlists folder (both resolved, so `..`, link chains and a symlinked
    /// `Playlists` folder are all judged by where they really lead).
    fn write_target(&self, path: &Path) -> Result<PathBuf, StoreError> {
        if !fs::symlink_metadata(path)
            .map_err(StoreError::io(Action::Save))?
            .file_type()
            .is_symlink()
        {
            return Ok(path.to_owned());
        }
        let target = fs::canonicalize(path).map_err(StoreError::io(Action::Save))?;
        let dir = fs::canonicalize(&self.dir).map_err(StoreError::io(Action::Save))?;
        if target.starts_with(&dir) {
            Ok(target)
        } else {
            Err(StoreError::ReadOnly(ReadOnlyReason::LinksOutside))
        }
    }

    /// Whether `path` is a symbolic link to a file outside the playlists
    /// folder, which saving refuses to write.
    fn links_outside(&self, path: &Path) -> bool {
        matches!(
            self.write_target(path),
            Err(StoreError::ReadOnly(ReadOnlyReason::LinksOutside))
        )
    }

    /// Names of `.m3u8` files in the folder, unfiltered and unsorted.
    fn file_stems(&self) -> io::Result<Vec<String>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let suffix = format!(".{EXTENSION}");
        Ok(entries
            .take(MAX_DIR_ENTRIES)
            .filter_map(Result::ok)
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|n| n.strip_suffix(&suffix).map(str::to_owned))
            .collect())
    }

    /// Refuses `name` if a playlist file with the same name ignoring case
    /// exists (other than `current`, for a case-only rename).
    fn ensure_free<'a>(
        &self,
        name: &'a str,
        current: Option<&str>,
        action: Action,
    ) -> Result<&'a str, StoreError> {
        let stems = self
            .file_stems()
            .map_err(|source| StoreError::Io { action, source })?;
        Ok(check_new_name(
            name,
            stems.iter().map(String::as_str),
            current,
        )?)
    }

    /// [`PlaylistStore::list`] reading at most `budget` bytes in total.
    fn list_within(
        &self,
        resolver: &Resolver,
        mut budget: u64,
    ) -> Result<Vec<PlaylistInfo>, StoreError> {
        let mut names: Vec<String> = self
            .file_stems()
            .map_err(|source| StoreError::Io {
                action: Action::List,
                source,
            })?
            .into_iter()
            .filter(|n| is_listable(n))
            // Directories and dangling links are not playlists.
            .filter(|n| {
                fs::metadata(self.dir.join(format!("{n}.{EXTENSION}"))).is_ok_and(|m| m.is_file())
            })
            .collect();
        names.sort_by_cached_key(|n| (n.to_lowercase(), n.clone()));
        names.truncate(MAX_LISTED);

        Ok(names
            .into_iter()
            .map(|name| {
                let loaded = self
                    .path_for(&name)
                    .ok()
                    .and_then(|path| {
                        Some((fs::metadata(&path).ok()?.len().min(MAX_FILE_BYTES), path))
                    })
                    .filter(|(len, _)| *len <= budget)
                    .and_then(|(len, path)| {
                        budget -= len;
                        let outside = self.links_outside(&path);
                        let (content, truncated, _) = read_limited(&path, MAX_FILE_BYTES).ok()?;
                        let mut playlist = Playlist::parse(
                            name.clone(),
                            path,
                            &content,
                            truncated,
                            resolver,
                            Matching::Lexical,
                        );
                        if outside {
                            playlist.editable = Err(ReadOnlyReason::LinksOutside);
                        }
                        Some(playlist)
                    });
                PlaylistInfo {
                    tracks: loaded.as_ref().map(|p| p.tracks().len()),
                    read_only: loaded.as_ref().and_then(|p| p.editable().err()),
                    name,
                }
            })
            .collect())
    }

    fn create_dir(&self) -> Result<(), StoreError> {
        match fs::create_dir(&self.dir) {
            Ok(()) => Ok(()),
            // If it exists as a file, writing into it reports the error.
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            Err(source) => Err(StoreError::Io {
                action: Action::Create,
                source,
            }),
        }
    }
}

impl PlaylistStore for FsStore {
    fn list(&self, resolver: &Resolver) -> Result<Vec<PlaylistInfo>, StoreError> {
        self.list_within(resolver, LIST_BYTE_BUDGET)
    }

    fn load(&self, name: &str, resolver: &Resolver) -> Result<Playlist, StoreError> {
        let path = self.path_for(name)?;
        let (content, truncated, meta) =
            read_limited(&path, MAX_FILE_BYTES).map_err(StoreError::io(Action::Read))?;
        let mut playlist = Playlist::parse(
            name.to_owned(),
            path,
            &content,
            truncated,
            resolver,
            Matching::Full,
        );
        if self.links_outside(&playlist.path) {
            playlist.editable = Err(ReadOnlyReason::LinksOutside);
        }
        playlist.stamp = Some(FileStamp::of(&meta));
        Ok(playlist)
    }

    fn changed(&self, playlist: &Playlist) -> Result<bool, StoreError> {
        let meta = fs::metadata(&playlist.path).map_err(StoreError::io(Action::Read))?;
        Ok(playlist.stamp != Some(FileStamp::of(&meta)))
    }

    fn blank(&self, name: &str) -> Result<Playlist, StoreError> {
        let name = super::validate_name(name)?;
        let path = self.path_for(name)?;
        Ok(Playlist::blank(name.to_owned(), path))
    }

    fn create(&self, playlist: &mut Playlist) -> Result<(), StoreError> {
        let name = self
            .ensure_free(&playlist.name, None, Action::Create)?
            .to_owned();
        let path = self.path_for(&name)?;
        if playlist.path != path || playlist.stamp.is_some() {
            return Err(StoreError::Io {
                action: Action::Create,
                source: io::Error::new(io::ErrorKind::InvalidInput, "not a new playlist"),
            });
        }
        self.create_dir()?;
        let io_err = |source| StoreError::Io {
            action: Action::Create,
            source,
        };
        let tmp = temp_path(&path, "tmp").map_err(io_err)?;
        let stamp = write_temp(&tmp, &playlist.to_bytes(), None).map_err(io_err)?;
        let linked = link_no_clobber(&tmp, &path);
        // After a successful link the content is in place under both names.
        let _ = fs::remove_file(&tmp);
        linked?;
        sync_dir(&self.dir);
        playlist.stamp = Some(stamp);
        Ok(())
    }

    fn save(&self, playlist: &mut Playlist) -> Result<SaveOutcome, StoreError> {
        playlist.editable().map_err(StoreError::ReadOnly)?;
        let Some(stamp) = playlist.stamp else {
            // A programming error: new playlists go through `create`.
            return Err(StoreError::Io {
                action: Action::Save,
                source: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the playlist has not been created yet",
                ),
            });
        };
        // Checked again here, not only on load: the link may have been
        // repointed since.
        let target = self.write_target(&playlist.path)?;
        let meta = fs::metadata(&target).map_err(StoreError::io(Action::Save))?;
        if FileStamp::of(&meta) != stamp {
            return Ok(SaveOutcome::ChangedOnDisk);
        }
        let io_err = |source| StoreError::Io {
            action: Action::Save,
            source,
        };
        if meta.permissions().readonly() {
            return Err(io_err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the file is read-only",
            )));
        }
        let tmp = temp_path(&target, "tmp").map_err(io_err)?;
        // `write_temp` cleans up after itself, and never touches a file it did
        // not create; only the temp file it wrote is removed here.
        let new_stamp = write_temp(&tmp, &playlist.to_bytes(), Some(&meta)).map_err(io_err)?;
        if let Err(err) = fs::rename(&tmp, &target) {
            let _ = fs::remove_file(&tmp);
            return Err(io_err(err));
        }
        if let Some(dir) = target.parent() {
            sync_dir(dir);
        }
        playlist.stamp = Some(new_stamp);
        Ok(SaveOutcome::Saved)
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), StoreError> {
        let from_path = self.path_for(from)?;
        let to = self.ensure_free(to, Some(from), Action::Rename)?;
        let to_path = self.path_for(to)?;
        let io_err = |source| StoreError::Io {
            action: Action::Rename,
            source,
        };
        let meta = fs::symlink_metadata(&from_path).map_err(StoreError::io(Action::Rename))?;
        if from == to {
            return Ok(());
        }
        if from.to_lowercase() == to.to_lowercase() {
            // Case-insensitive filesystems see `to` as existing (it is `from`),
            // so go through a temporary name.
            let tmp = temp_path(&from_path, "rename").map_err(io_err)?;
            if fs::symlink_metadata(&tmp).is_ok() {
                return Err(io_err(io::Error::from(io::ErrorKind::AlreadyExists)));
            }
            fs::rename(&from_path, &tmp).map_err(io_err)?;
            // `hard_link` follows symlinks on some systems (macOS): move a link itself.
            let moved = if meta.file_type().is_symlink() {
                rename_checked(&tmp, &to_path)
            } else {
                link_no_clobber(&tmp, &to_path)
            };
            if let Err(err) = moved {
                let _ = fs::rename(&tmp, &from_path);
                return Err(err);
            }
            // Still there when linking worked; gone after the fallback rename.
            let _ = fs::remove_file(&tmp);
        } else if meta.file_type().is_symlink() {
            // `hard_link` on a symlink is platform-dependent; move the link itself.
            rename_checked(&from_path, &to_path)?;
        } else {
            match fs::hard_link(&from_path, &to_path) {
                Ok(()) => {
                    if let Err(err) = fs::remove_file(&from_path) {
                        let _ = fs::remove_file(&to_path);
                        return Err(io_err(err));
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(NameError::Taken.into());
                }
                Err(_) => rename_checked(&from_path, &to_path)?,
            }
        }
        sync_dir(&self.dir);
        Ok(())
    }

    fn delete(&self, name: &str) -> Result<(), StoreError> {
        let path = self.path_for(name)?;
        let meta = fs::symlink_metadata(&path).map_err(StoreError::io(Action::Delete))?;
        if meta.is_dir() {
            return Err(StoreError::Io {
                action: Action::Delete,
                source: io::Error::new(io::ErrorKind::InvalidInput, "not a playlist file"),
            });
        }
        fs::remove_file(&path).map_err(StoreError::io(Action::Delete))?;
        sync_dir(&self.dir);
        Ok(())
    }
}

/// Names shown as playlists: non-empty, not hidden, nothing that could fool
/// the eye or the terminal, no path separator.
fn is_listable(name: &str) -> bool {
    let separator = |c: char| c == '/' || (cfg!(windows) && c == '\\');
    !name.is_empty()
        && !name.starts_with('.')
        && !name
            .chars()
            .any(|c| separator(c) || c.is_control() || is_invisible_format(c))
}

/// `.<file name>.<kind>-<pid>` next to `path`.
fn temp_path(path: &Path, kind: &str) -> io::Result<PathBuf> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            display_path(path),
        ));
    };
    let mut tmp = std::ffi::OsString::from(".");
    if name.len() <= MAX_TEMP_BASE_BYTES {
        tmp.push(name);
    } else {
        tmp.push("kasset-playlist");
    }
    tmp.push(format!(".{kind}-{}", std::process::id()));
    Ok(dir.join(tmp))
}

/// Writes `bytes` to a new file at `tmp`, copies the owner (best effort) and
/// permission bits of `like`, and flushes it to disk. Removes `tmp` on failure.
///
/// A regular file already at `tmp` and untouched for [`STALE_TEMP_AGE`] is a
/// leftover of a crashed run with the same process id (temporary names are per
/// process): it is removed and creation is retried once. Anything else there,
/// including a recent file that may belong to another kasset with the same pid
/// in another pid namespace sharing the folder, makes the write fail.
fn write_temp(tmp: &Path, bytes: &[u8], like: Option<&fs::Metadata>) -> io::Result<FileStamp> {
    let create = || OpenOptions::new().write(true).create_new(true).open(tmp);
    let mut file = match create() {
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && is_stale(tmp) => {
            fs::remove_file(tmp)?;
            create()?
        }
        other => other?,
    };
    let written = (|| {
        file.write_all(bytes)?;
        if let Some(meta) = like {
            // Before the mode: changing the owner can clear permission bits.
            copy_owner(&file, meta);
            file.set_permissions(permission_bits(meta))?;
        }
        file.sync_all()?;
        // Renaming and linking keep size, mtime and inode: this is the stamp
        // of the final file.
        Ok(FileStamp::of(&file.metadata()?))
    })();
    drop(file);
    if written.is_err() {
        let _ = fs::remove_file(tmp);
    }
    written
}

/// `true` when `path` is a regular file not modified for [`STALE_TEMP_AGE`].
fn is_stale(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| {
        m.file_type().is_file()
            && m.modified()
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age >= STALE_TEMP_AGE)
    })
}

/// Gives `file` the owner and group of `meta`. Best effort: only root may
/// give a file away, and a group change needs membership; when the owner
/// cannot be kept, the group still is if possible.
#[cfg(unix)]
fn copy_owner(file: &File, meta: &fs::Metadata) {
    use std::os::unix::fs::{MetadataExt, fchown};
    if fchown(file, Some(meta.uid()), Some(meta.gid())).is_err() {
        let _ = fchown(file, None, Some(meta.gid()));
    }
}

#[cfg(not(unix))]
fn copy_owner(_file: &File, _meta: &fs::Metadata) {}

#[cfg(unix)]
fn permission_bits(meta: &fs::Metadata) -> fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    // Read/write/execute bits only: never carry setuid/setgid/sticky over.
    fs::Permissions::from_mode(meta.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn permission_bits(meta: &fs::Metadata) -> fs::Permissions {
    meta.permissions()
}

/// Gives the file at `tmp` the name `path` unless `path` exists. `tmp` is left
/// in place when linking worked (the caller removes it).
fn link_no_clobber(tmp: &Path, path: &Path) -> Result<(), StoreError> {
    match fs::hard_link(tmp, path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Err(NameError::Taken.into()),
        // No hard links here (FAT, exFAT, some network filesystems).
        Err(_) => rename_checked(tmp, path),
    }
}

/// `rename` that refuses to replace an existing `to`. The check and the rename
/// are not atomic; only used where hard links are unavailable.
fn rename_checked(from: &Path, to: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(to) {
        Ok(_) => return Err(NameError::Taken.into()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(StoreError::Io {
                action: Action::Rename,
                source,
            });
        }
    }
    fs::rename(from, to).map_err(StoreError::io(Action::Rename))
}

/// Makes a rename or new file in `dir` durable. Best effort.
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Track;
    #[cfg(unix)]
    use crate::playlist::EditError;
    use anyhow::{Result, bail};
    use std::time::Duration;

    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        store: FsStore,
        library: Vec<Track>,
        resolver: Resolver,
    }

    fn fixture() -> Result<Fixture> {
        let tmp = tempfile::tempdir()?;
        let root = fs::canonicalize(tmp.path())?;
        let library: Vec<Track> = ["A", "B", "C"]
            .into_iter()
            .map(|t| Track {
                path: root.join(format!("Band/{t}.flac")),
                title: t.to_owned(),
                artist: "Band".to_owned(),
                album: "LP".to_owned(),
                number: None,
                album_artist: None,
                disc: None,
                duration: Some(Duration::from_secs(60)),
            })
            .collect();
        Ok(Fixture {
            store: FsStore::new(&root),
            resolver: Resolver::new(&library),
            _tmp: tmp,
            root,
            library,
        })
    }

    impl Fixture {
        fn create(&self, name: &str, tracks: &[usize]) -> Result<Playlist> {
            let mut p = self.store.blank(name)?;
            p.add(tracks, &self.library)?;
            self.store.create(&mut p)?;
            Ok(p)
        }

        fn file(&self, name: &str) -> PathBuf {
            self.store.dir().join(format!("{name}.m3u8"))
        }

        fn names(&self) -> Result<Vec<String>> {
            Ok(self
                .store
                .list(&self.resolver)?
                .into_iter()
                .map(|i| i.name)
                .collect())
        }
    }

    #[test]
    fn folder_absent_until_first_playlist() -> Result<()> {
        let f = fixture()?;
        assert_eq!(f.store.list(&f.resolver)?, []);
        let p = f.store.blank("Running")?;
        assert!(!f.store.dir().exists(), "blank must not write");
        drop(p);
        f.create("Running", &[0])?;
        assert_eq!(
            fs::read(f.file("Running"))?,
            b"#EXTM3U\n#EXTINF:60,Band - A\n../Band/A.flac\n"
        );
        assert_eq!(f.names()?, ["Running"]);
        Ok(())
    }

    #[test]
    fn lists_only_m3u8_sorted_case_insensitively() -> Result<()> {
        let f = fixture()?;
        fs::create_dir(f.store.dir())?;
        for name in [
            "Old.m3u",
            "notes.txt",
            "Gym.m3u8",
            "abba.m3u8",
            "Zed.m3u8",
            ".hidden.m3u8",
            ".x.m3u8.tmp-1",
        ] {
            fs::write(f.store.dir().join(name), b"../Band/A.flac\n../gone.flac\n")?;
        }
        fs::create_dir(f.store.dir().join("dir.m3u8"))?;
        let list = f.store.list(&f.resolver)?;
        let names: Vec<_> = list.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, ["abba", "Gym", "Zed"]);
        assert_eq!(list[0].tracks, Some(1));
        assert_eq!(list[0].read_only, None);
        Ok(())
    }

    #[test]
    fn list_marks_read_only_playlists() -> Result<()> {
        let f = fixture()?;
        fs::create_dir(f.store.dir())?;
        fs::write(f.file("Old"), b"../Band/A.flac\n#\xE9\n")?;
        let list = f.store.list(&f.resolver)?;
        assert_eq!(list[0].read_only, Some(ReadOnlyReason::NotUtf8));
        assert_eq!(list[0].tracks, Some(1));
        Ok(())
    }

    #[test]
    fn load_round_trips_and_save_is_byte_identical() -> Result<()> {
        let f = fixture()?;
        fs::create_dir(f.store.dir())?;
        let content = b"\xEF\xBB\xBF#EXTM3U\r\n../Band/A.flac\r\n../gone.flac\r\n../Band/A.flac";
        fs::write(f.file("Mix"), content)?;
        let mut p = f.store.load("Mix", &f.resolver)?;
        assert_eq!(p.tracks(), [0, 0]);
        assert_eq!(p.unmatched(), 1);
        assert_eq!(f.store.save(&mut p)?, SaveOutcome::Saved);
        assert_eq!(fs::read(f.file("Mix"))?, content);
        Ok(())
    }

    #[test]
    fn load_missing_is_not_found() -> Result<()> {
        let f = fixture()?;
        assert!(matches!(
            f.store.load("Nope", &f.resolver),
            Err(StoreError::NotFound)
        ));
        Ok(())
    }

    #[test]
    fn rejects_path_like_names() -> Result<()> {
        let f = fixture()?;
        for name in ["../x", "a/b", "", ".", "..", ".hidden", "a\u{1b}b"] {
            assert!(
                matches!(f.store.load(name, &f.resolver), Err(StoreError::Name(_))),
                "{name:?}"
            );
            assert!(
                matches!(f.store.delete(name), Err(StoreError::Name(_))),
                "{name:?}"
            );
        }
        assert!(matches!(
            f.store.blank("a:b"),
            Err(StoreError::Name(NameError::Forbidden(':')))
        ));
        Ok(())
    }

    #[test]
    fn oversized_file_is_read_only() -> Result<()> {
        let f = fixture()?;
        let path = f.root.join("big.m3u8");
        fs::write(&path, b"../a\n../b\n../c\n")?;
        let (content, truncated, _) = read_limited(&path, 7)?;
        assert!(truncated);
        assert_eq!(content, b"../a\n..");
        let (_, truncated, _) = read_limited(&path, 15)?;
        assert!(!truncated);
        Ok(())
    }

    #[test]
    fn external_write_is_detected_and_nothing_written() -> Result<()> {
        let f = fixture()?;
        let mut p = f.create("Running", &[0, 1])?;
        assert!(!f.store.changed(&p)?);
        let mut external = fs::read(f.file("Running"))?;
        external.extend_from_slice(b"../Band/C.flac\n");
        fs::write(f.file("Running"), &external)?;
        assert!(f.store.changed(&p)?);

        p.remove(0)?;
        assert_eq!(f.store.save(&mut p)?, SaveOutcome::ChangedOnDisk);
        assert_eq!(fs::read(f.file("Running"))?, external);
        Ok(())
    }

    #[test]
    fn consecutive_saves_succeed() -> Result<()> {
        let f = fixture()?;
        let mut p = f.create("Running", &[0, 1, 2])?;
        p.remove(0)?;
        assert_eq!(f.store.save(&mut p)?, SaveOutcome::Saved);
        p.remove(0)?;
        assert_eq!(f.store.save(&mut p)?, SaveOutcome::Saved);
        assert!(!f.store.changed(&p)?);
        assert_eq!(f.store.load("Running", &f.resolver)?.tracks(), [2]);
        Ok(())
    }

    #[test]
    fn save_of_deleted_playlist_is_not_found() -> Result<()> {
        let f = fixture()?;
        let mut p = f.create("Running", &[0])?;
        fs::remove_file(f.file("Running"))?;
        assert!(matches!(f.store.save(&mut p), Err(StoreError::NotFound)));
        assert!(matches!(f.store.changed(&p), Err(StoreError::NotFound)));
        assert!(!f.file("Running").exists());
        Ok(())
    }

    #[test]
    fn read_only_playlist_is_never_saved() -> Result<()> {
        let f = fixture()?;
        fs::create_dir(f.store.dir())?;
        fs::write(f.file("Old"), b"../Band/A.flac\n#\xE9\n")?;
        let mut p = f.store.load("Old", &f.resolver)?;
        assert!(matches!(
            f.store.save(&mut p),
            Err(StoreError::ReadOnly(ReadOnlyReason::NotUtf8))
        ));
        Ok(())
    }

    #[test]
    fn create_refuses_a_name_that_appeared_meanwhile() -> Result<()> {
        let f = fixture()?;
        let mut p = f.store.blank("Road trip")?;
        p.add(&[0], &f.library)?;
        fs::create_dir(f.store.dir())?;
        fs::write(f.file("Road trip"), b"theirs")?;
        assert!(matches!(
            f.store.create(&mut p),
            Err(StoreError::Name(NameError::Taken))
        ));
        assert_eq!(fs::read(f.file("Road trip"))?, b"theirs");
        Ok(())
    }

    #[test]
    fn link_no_clobber_refuses_existing_file() -> Result<()> {
        // The race itself: the name check passed, then the file appeared.
        let f = fixture()?;
        let tmp = f.root.join(".new.tmp");
        let path = f.root.join("taken.m3u8");
        fs::write(&tmp, b"ours")?;
        fs::write(&path, b"theirs")?;
        assert!(matches!(
            link_no_clobber(&tmp, &path),
            Err(StoreError::Name(NameError::Taken))
        ));
        assert!(matches!(
            rename_checked(&tmp, &path),
            Err(StoreError::Name(NameError::Taken))
        ));
        assert_eq!(fs::read(&path)?, b"theirs");
        Ok(())
    }

    #[test]
    fn create_refuses_names_taken_ignoring_case() -> Result<()> {
        let f = fixture()?;
        f.create("Running", &[0])?;
        let Err(err) = f.create("running", &[1]) else {
            bail!("expected an error")
        };
        assert!(matches!(
            err.downcast_ref::<StoreError>(),
            Some(StoreError::Name(NameError::Taken))
        ));
        assert_eq!(f.names()?, ["Running"]);
        Ok(())
    }

    #[test]
    fn rename_moves_the_file() -> Result<()> {
        let f = fixture()?;
        f.create("Running", &[0])?;
        let before = fs::read(f.file("Running"))?;
        f.store.rename("Running", "Morning run")?;
        assert!(!f.file("Running").exists());
        assert_eq!(fs::read(f.file("Morning run"))?, before);
        assert_eq!(f.names()?, ["Morning run"]);
        Ok(())
    }

    #[test]
    fn rename_refuses_taken_and_invalid_names() -> Result<()> {
        let f = fixture()?;
        f.create("Running", &[0])?;
        f.create("Gym", &[1])?;
        assert!(matches!(
            f.store.rename("Running", "GYM"),
            Err(StoreError::Name(NameError::Taken))
        ));
        assert!(matches!(
            f.store.rename("Running", "a/b"),
            Err(StoreError::Name(NameError::Forbidden('/')))
        ));
        assert!(matches!(
            f.store.rename("Nope", "Other"),
            Err(StoreError::NotFound)
        ));
        assert_eq!(f.names()?, ["Gym", "Running"]);
        Ok(())
    }

    #[test]
    fn rename_may_change_only_case() -> Result<()> {
        let f = fixture()?;
        f.create("Running", &[0])?;
        f.store.rename("Running", "RUNNING")?;
        assert_eq!(f.names()?, ["RUNNING"]);
        f.store.rename("RUNNING", "RUNNING")?;
        assert_eq!(f.names()?, ["RUNNING"]);
        Ok(())
    }

    #[test]
    fn delete_removes_only_the_playlist() -> Result<()> {
        let f = fixture()?;
        fs::create_dir_all(f.root.join("Band"))?;
        fs::write(&f.library[0].path, b"audio")?;
        f.create("Gym", &[0])?;
        f.store.delete("Gym")?;
        assert!(!f.file("Gym").exists());
        assert_eq!(fs::read(&f.library[0].path)?, b"audio");
        assert!(matches!(f.store.delete("Gym"), Err(StoreError::NotFound)));
        Ok(())
    }

    /// D9: the playlists folder is a regular file, so nothing can be created.
    #[test]
    fn create_fails_when_folder_is_a_file() -> Result<()> {
        let f = fixture()?;
        fs::write(f.store.dir(), b"not a folder")?;
        let mut p = f.store.blank("Running")?;
        p.add(&[0], &f.library)?;
        let err = f.store.create(&mut p);
        assert!(
            matches!(
                err,
                Err(StoreError::Io {
                    action: Action::Create,
                    ..
                })
            ),
            "{err:?}"
        );
        assert_eq!(fs::read(f.store.dir())?, b"not a folder");
        assert!(f.store.list(&f.resolver).is_err());
        Ok(())
    }

    /// D9: the temporary path is occupied by a directory, so the save fails.
    #[test]
    fn failed_save_leaves_file_unchanged() -> Result<()> {
        let f = fixture()?;
        let mut p = f.create("Running", &[0, 1])?;
        let before = fs::read(f.file("Running"))?;
        let blocker = temp_path(&f.file("Running"), "tmp")?;
        fs::create_dir(&blocker)?;
        p.remove(0)?;
        let err = f.store.save(&mut p);
        assert!(
            matches!(
                err,
                Err(StoreError::Io {
                    action: Action::Save,
                    ..
                })
            ),
            "{err:?}"
        );
        assert_eq!(fs::read(f.file("Running"))?, before);
        assert!(blocker.is_dir(), "the blocker is not ours to remove");
        let msg = err
            .map(|_| ())
            .map_err(|e| e.to_string())
            .err()
            .unwrap_or_default();
        assert!(msg.starts_with("cannot save"), "{msg}");
        Ok(())
    }

    #[test]
    fn stale_temp_file_is_replaced() -> Result<()> {
        let f = fixture()?;
        let mut p = f.create("Running", &[0, 1])?;
        let stale = temp_path(&f.file("Running"), "tmp")?;
        fs::write(&stale, b"left by a crash")?;
        let old = std::time::SystemTime::now() - STALE_TEMP_AGE - Duration::from_secs(1);
        File::options()
            .write(true)
            .open(&stale)?
            .set_modified(old)?;
        p.remove(0)?;
        assert_eq!(f.store.save(&mut p)?, SaveOutcome::Saved);
        assert!(!stale.exists());
        assert_eq!(f.store.load("Running", &f.resolver)?.tracks(), [1]);
        Ok(())
    }

    #[test]
    fn recent_temp_file_is_left_alone() -> Result<()> {
        let f = fixture()?;
        let mut p = f.create("Running", &[0, 1])?;
        let before = fs::read(f.file("Running"))?;
        let busy = temp_path(&f.file("Running"), "tmp")?;
        fs::write(&busy, b"another process is writing")?;
        p.remove(0)?;
        assert!(matches!(
            f.store.save(&mut p),
            Err(StoreError::Io {
                action: Action::Save,
                ..
            })
        ));
        assert_eq!(fs::read(&busy)?, b"another process is writing");
        assert_eq!(fs::read(f.file("Running"))?, before);
        Ok(())
    }

    #[test]
    fn long_external_name_saves_through_short_temp_name() -> Result<()> {
        let f = fixture()?;
        fs::create_dir(f.store.dir())?;
        let name = "n".repeat(240);
        fs::write(f.file(&name), b"../Band/A.flac\n../Band/B.flac\n")?;
        let mut p = f.store.load(&name, &f.resolver)?;
        p.remove(0)?;
        assert_eq!(f.store.save(&mut p)?, SaveOutcome::Saved);
        assert_eq!(fs::read(f.file(&name))?, b"../Band/B.flac\n");
        Ok(())
    }

    #[test]
    fn save_of_uncreated_playlist_is_refused() -> Result<()> {
        let f = fixture()?;
        let mut p = f.store.blank("New")?;
        let Err(err) = f.store.save(&mut p) else {
            bail!("expected an error")
        };
        assert!(err.to_string().contains("not been created"), "{err}");
        assert!(!f.store.dir().exists());
        Ok(())
    }

    #[test]
    fn list_stops_counting_when_budget_runs_out() -> Result<()> {
        let f = fixture()?;
        fs::create_dir(f.store.dir())?;
        let content = b"../Band/A.flac\n../Band/B.flac\n";
        for name in ["a", "b", "c"] {
            fs::write(f.file(name), content)?;
        }
        let budget = 2 * content.len() as u64;
        let list = f.store.list_within(&f.resolver, budget)?;
        let tracks: Vec<_> = list.iter().map(|i| i.tracks).collect();
        assert_eq!(tracks, [Some(2), Some(2), None]);
        assert_eq!(list[2].name, "c");
        Ok(())
    }

    #[test]
    fn no_temp_files_left_behind() -> Result<()> {
        let f = fixture()?;
        let mut p = f.create("Running", &[0, 1])?;
        p.remove(0)?;
        f.store.save(&mut p)?;
        f.store.rename("Running", "running")?;
        let entries: Vec<_> = fs::read_dir(f.store.dir())?
            .filter_map(Result::ok)
            .map(|e| e.file_name())
            .collect();
        assert_eq!(entries, ["running.m3u8"]);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_playlist_stays_a_link() -> Result<()> {
        let f = fixture()?;
        f.create("Gym", &[0])?;
        // A link to a file elsewhere in the playlists folder, spelled with
        // `..` so only its resolved location keeps it inside.
        let archive = f.store.dir().join("archive");
        fs::create_dir(&archive)?;
        let target = archive.join("real.m3u8");
        fs::write(&target, b"#EXTM3U\n../Band/A.flac\n")?;
        std::os::unix::fs::symlink("../Playlists/archive/real.m3u8", f.file("Running"))?;

        let mut p = f.store.load("Running", &f.resolver)?;
        assert_eq!(p.editable(), Ok(()));
        p.add(&[1], &f.library)?;
        assert_eq!(f.store.save(&mut p)?, SaveOutcome::Saved);
        assert!(
            fs::symlink_metadata(f.file("Running"))?
                .file_type()
                .is_symlink()
        );
        let saved = fs::read(&target)?;
        assert!(
            saved.ends_with(b"../Band/B.flac\n"),
            "{}",
            String::from_utf8_lossy(&saved)
        );

        // Rename moves the link; delete removes the link, never the target.
        f.store.rename("Running", "Run")?;
        assert!(
            fs::symlink_metadata(f.file("Run"))?
                .file_type()
                .is_symlink()
        );
        f.store.delete("Run")?;
        assert!(target.exists());
        Ok(())
    }

    /// A link planted in a shared music folder must not let an edit rewrite
    /// whatever it points to (here a stand-in for a shell start-up file).
    #[cfg(unix)]
    #[test]
    fn playlist_linked_outside_the_folder_is_read_only() -> Result<()> {
        let f = fixture()?;
        f.create("Gym", &[0])?;
        let outside = f.root.join("bashrc");
        let original = b"#EXTM3U\n../Band/A.flac\n";
        fs::write(&outside, original)?;
        std::os::unix::fs::symlink("../bashrc", f.file("Mix"))?;

        let listed = f.store.list(&f.resolver)?;
        let mix = listed.iter().find(|i| i.name == "Mix");
        assert_eq!(
            mix.and_then(|i| i.read_only),
            Some(ReadOnlyReason::LinksOutside),
            "{listed:?}"
        );

        let mut p = f.store.load("Mix", &f.resolver)?;
        assert_eq!(p.editable(), Err(ReadOnlyReason::LinksOutside));
        assert_eq!(p.tracks().len(), 1, "still readable and playable");
        assert!(matches!(
            p.add(&[1], &f.library),
            Err(EditError::ReadOnly(ReadOnlyReason::LinksOutside))
        ));
        assert!(matches!(
            f.store.save(&mut p),
            Err(StoreError::ReadOnly(ReadOnlyReason::LinksOutside))
        ));
        assert_eq!(fs::read(&outside)?, original);

        // Renaming and deleting only touch the link inside the folder.
        f.store.rename("Mix", "Old mix")?;
        f.store.delete("Old mix")?;
        assert_eq!(fs::read(&outside)?, original);
        Ok(())
    }

    /// The check is repeated when saving: a link repointed after the playlist
    /// was loaded is not followed.
    #[cfg(unix)]
    #[test]
    fn link_repointed_outside_after_load_is_not_written() -> Result<()> {
        let f = fixture()?;
        f.create("Gym", &[0])?;
        let inside = f.store.dir().join("real.m3u8.bak");
        fs::write(&inside, b"../Band/A.flac\n")?;
        std::os::unix::fs::symlink(&inside, f.file("Mix"))?;
        let mut p = f.store.load("Mix", &f.resolver)?;
        p.add(&[1], &f.library)?;

        let outside = f.root.join("bashrc");
        fs::write(&outside, b"../Band/A.flac\n")?;
        fs::remove_file(f.file("Mix"))?;
        std::os::unix::fs::symlink(&outside, f.file("Mix"))?;
        assert!(matches!(
            f.store.save(&mut p),
            Err(StoreError::ReadOnly(ReadOnlyReason::LinksOutside))
        ));
        assert_eq!(fs::read(&outside)?, b"../Band/A.flac\n");
        assert_eq!(fs::read(&inside)?, b"../Band/A.flac\n");
        Ok(())
    }

    /// A chain is judged by its end: `A -> B` stays inside the folder, but
    /// `B` leads out of it.
    #[cfg(unix)]
    #[test]
    fn link_chain_leading_outside_is_read_only() -> Result<()> {
        let f = fixture()?;
        f.create("Gym", &[0])?;
        let outside = f.root.join("bashrc");
        fs::write(&outside, b"../Band/A.flac\n")?;
        std::os::unix::fs::symlink("../bashrc", f.store.dir().join("hop.m3u8.bak"))?;
        std::os::unix::fs::symlink("hop.m3u8.bak", f.file("Mix"))?;

        let mut p = f.store.load("Mix", &f.resolver)?;
        assert_eq!(p.editable(), Err(ReadOnlyReason::LinksOutside));
        p.editable = Ok(()); // as if the check on load had been missed
        p.add(&[1], &f.library)?;
        assert!(matches!(
            f.store.save(&mut p),
            Err(StoreError::ReadOnly(ReadOnlyReason::LinksOutside))
        ));
        assert_eq!(fs::read(&outside)?, b"../Band/A.flac\n");
        Ok(())
    }

    /// A dangling link is not a playlist, and a link that becomes dangling
    /// after loading is reported as gone; nothing is written either way.
    #[cfg(unix)]
    #[test]
    fn dangling_links_are_not_written() -> Result<()> {
        let f = fixture()?;
        f.create("Gym", &[0])?;
        let missing = f.root.join("missing.m3u8");
        std::os::unix::fs::symlink(&missing, f.file("Ghost"))?;
        assert_eq!(f.names()?, ["Gym"]);
        assert!(matches!(
            f.store.load("Ghost", &f.resolver),
            Err(StoreError::NotFound)
        ));

        let inside = f.store.dir().join("real.m3u8.bak");
        fs::write(&inside, b"../Band/A.flac\n")?;
        std::os::unix::fs::symlink(&inside, f.file("Mix"))?;
        let mut p = f.store.load("Mix", &f.resolver)?;
        p.add(&[1], &f.library)?;
        fs::remove_file(f.file("Mix"))?;
        std::os::unix::fs::symlink(&missing, f.file("Mix"))?;
        assert!(matches!(f.store.save(&mut p), Err(StoreError::NotFound)));
        assert!(!missing.exists());
        assert_eq!(fs::read(&inside)?, b"../Band/A.flac\n");
        Ok(())
    }

    /// When `Playlists` itself is a link (e.g. to a synced folder), a playlist
    /// linking to another file in that folder is inside it, not outside.
    #[cfg(unix)]
    #[test]
    fn links_within_a_symlinked_playlists_folder_are_editable() -> Result<()> {
        let f = fixture()?;
        let synced = f.root.join("synced");
        fs::create_dir(&synced)?;
        fs::write(synced.join("real.m3u8.bak"), b"../Band/A.flac\n")?;
        std::os::unix::fs::symlink(&synced, f.store.dir())?;
        std::os::unix::fs::symlink(synced.join("real.m3u8.bak"), f.file("Mix"))?;

        let mut p = f.store.load("Mix", &f.resolver)?;
        assert_eq!(p.editable(), Ok(()));
        p.add(&[1], &f.library)?;
        assert_eq!(f.store.save(&mut p)?, SaveOutcome::Saved);
        let saved = fs::read(synced.join("real.m3u8.bak"))?;
        assert!(saved.ends_with(b"../Band/B.flac\n"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn save_keeps_permission_bits() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture()?;
        let mut p = f.create("Running", &[0, 1])?;
        // chmod changes neither size, mtime nor inode: no false conflict.
        fs::set_permissions(f.file("Running"), fs::Permissions::from_mode(0o640))?;
        p.remove(0)?;
        assert_eq!(f.store.save(&mut p)?, SaveOutcome::Saved);
        let mode = fs::metadata(f.file("Running"))?.permissions().mode() & 0o7777;
        assert_eq!(mode, 0o640);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn read_only_file_is_not_replaced() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture()?;
        let mut p = f.create("Running", &[0, 1])?;
        let before = fs::read(f.file("Running"))?;
        // chmod changes neither size, mtime nor inode: no false conflict.
        fs::set_permissions(f.file("Running"), fs::Permissions::from_mode(0o444))?;
        p.remove(0)?;
        assert!(matches!(
            f.store.save(&mut p),
            Err(StoreError::Io {
                action: Action::Save,
                ..
            })
        ));
        assert_eq!(fs::read(f.file("Running"))?, before);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_not_opened() -> Result<()> {
        let f = fixture()?;
        fs::create_dir(f.store.dir())?;
        let status = std::process::Command::new("mkfifo")
            .arg(f.file("Pipe"))
            .status();
        if !status.is_ok_and(|s| s.success()) {
            return Ok(()); // mkfifo unavailable
        }
        assert!(f.store.load("Pipe", &f.resolver).is_err());
        assert_eq!(f.names()?, Vec::<String>::new());
        Ok(())
    }
}
