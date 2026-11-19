//! Reading small untrusted files (playlists, lyrics) safely.
//!
//! Rules, shared by every caller so they live in one place:
//! - **Regular files only.** Anything else — a FIFO, a socket, a device, a
//!   directory — is refused with [`io::ErrorKind::InvalidInput`]. The type is
//!   judged from the *opened* handle, not from the path, so a file swapped for
//!   a FIFO between a check and the open cannot slip through.
//! - **Never blocks on open.** On unix the file is opened with
//!   `O_NONBLOCK | O_NOCTTY | O_CLOEXEC`: opening a FIFO nobody writes to
//!   returns at once (and is then refused), and a terminal device never becomes
//!   the controlling terminal. Elsewhere, the path is checked to be a regular
//!   file before opening it.
//! - **Size-capped.** At most `limit` bytes are read, whatever the file claims
//!   its size is or grows to while being read; the caller learns whether more
//!   was there.
//! - **Read-only.** Nothing here creates, changes or deletes a file.

use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::path::Path;

/// Reads at most `limit` bytes of the regular file at `path` (symbolic links
/// are followed). Returns the bytes, whether the file was longer than `limit`,
/// and the metadata of the file that was read.
///
/// # Errors
///
/// Fails if the file cannot be opened or read, or is not a regular file.
pub fn read_limited(path: &Path, limit: u64) -> io::Result<(Vec<u8>, bool, Metadata)> {
    let file = open_regular(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(not_a_file());
    }
    // Pre-allocate only what the file says it holds, capped: a lying or
    // growing file costs at most `limit + 1` bytes in total.
    let cap = usize::try_from(meta.len().min(limit)).unwrap_or(0);
    let mut content = Vec::with_capacity(cap);
    file.take(limit.saturating_add(1))
        .read_to_end(&mut content)?;
    let truncated = content.len() as u64 > limit;
    if truncated {
        content.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    Ok((content, truncated, meta))
}

/// Opens `path` for reading without blocking if it is a FIFO or a device. The
/// caller must still reject anything that is not a regular file, from the
/// handle's metadata.
#[cfg(unix)]
pub fn open_regular(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    Ok(File::from(rustix::fs::open(path, flags, Mode::empty())?))
}

/// Opens `path` for reading if it is a regular file (checked before opening,
/// so a named pipe is never opened).
#[cfg(not(unix))]
pub fn open_regular(path: &Path) -> io::Result<File> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(not_a_file());
    }
    File::open(path)
}

/// The error for a path that exists but is not a regular file.
pub fn not_a_file() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::fs;

    #[test]
    fn reads_whole_small_file() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("a.txt");
        fs::write(&path, b"hello")?;
        let (content, truncated, meta) = read_limited(&path, 5)?;
        assert_eq!(content, b"hello");
        assert!(!truncated);
        assert_eq!(meta.len(), 5);
        Ok(())
    }

    #[test]
    fn caps_at_limit_and_reports_truncation() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("a.txt");
        fs::write(&path, b"hello world")?;
        let (content, truncated, _) = read_limited(&path, 4)?;
        assert_eq!(content, b"hell");
        assert!(truncated);
        let (content, truncated, _) = read_limited(&path, 0)?;
        assert_eq!(content, Vec::<u8>::new());
        assert!(truncated);
        Ok(())
    }

    #[test]
    fn empty_file_is_not_truncated() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("empty");
        fs::write(&path, b"")?;
        let (content, truncated, _) = read_limited(&path, 0)?;
        assert_eq!(content, Vec::<u8>::new());
        assert!(!truncated);
        Ok(())
    }

    #[test]
    fn directory_is_refused() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let err = read_limited(tmp.path(), 10).err();
        assert!(err.is_some(), "a directory must not be read");
        Ok(())
    }

    #[test]
    fn missing_file_is_not_found() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let err = read_limited(&tmp.path().join("nope"), 10).err();
        assert_eq!(err.map(|e| e.kind()), Some(io::ErrorKind::NotFound));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fifo_is_refused_without_blocking() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("pipe");
        if !crate::test_support::make_fifo(&path) {
            return Ok(()); // mkfifo unavailable
        }
        let err = read_limited(&path, 10).err();
        assert_eq!(err.map(|e| e.kind()), Some(io::ErrorKind::InvalidInput));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_regular_file_is_followed() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let target = tmp.path().join("target");
        fs::write(&target, b"x")?;
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&target, &link)?;
        let (content, _, _) = read_limited(&link, 10)?;
        assert_eq!(content, b"x");
        Ok(())
    }
}
