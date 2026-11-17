//! Command-line interface and library root resolution.

use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, ValueHint};

use crate::keys;
use crate::text::display_path;

/// A keyboard-driven terminal music player for your local music folder.
#[derive(Debug, Parser)]
#[command(name = "kasset", version, about, after_help = keys::help_text())]
pub struct Cli {
    /// Music folder to open [default: your music directory, else the current directory]
    #[arg(value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub dir: Option<PathBuf>,
}

/// Resolves the library root: the given folder, else the user's music
/// directory (the `XDG_MUSIC_DIR` entry of the XDG user-dirs config, i.e.
/// `~/.config/user-dirs.dirs`, on Linux; else `~/Music`), else the current
/// directory.
///
/// The returned path is canonical (absolute, symlinks resolved).
///
/// # Errors
///
/// Fails when the given folder does not exist, is not a directory or cannot be
/// accessed, or when no default folder can be determined.
pub fn resolve_root(dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = dir {
        return validate_dir(dir);
    }
    let candidates = [
        dirs::audio_dir(),
        dirs::home_dir().map(|home| home.join("Music")),
    ];
    if let Some(music) = candidates.into_iter().flatten().find(|p| p.is_dir()) {
        return validate_dir(&music);
    }
    let cwd = std::env::current_dir().context("cannot determine the current directory")?;
    validate_dir(&cwd)
}

/// Checks that `dir` is an accessible directory and returns its canonical form.
fn validate_dir(dir: &Path) -> Result<PathBuf> {
    let shown = display_path(dir);
    let meta = match std::fs::metadata(dir) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            bail!("music folder '{shown}' does not exist")
        }
        Err(err) => {
            return Err(err).with_context(|| format!("cannot access music folder '{shown}'"));
        }
    };
    if !meta.is_dir() {
        bail!("'{shown}' is not a folder");
    }
    std::fs::canonicalize(dir).with_context(|| format!("cannot resolve music folder '{shown}'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn help_includes_keybindings() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("Usage: kasset [DIR]"));
        assert!(help.contains("pause / resume"));
        assert!(help.contains("move focus between the sidebar and the main pane"));
        assert!(help.contains("add the selected / every listed track to a playlist"));
        assert!(help.contains("<DIR>/Playlists"));
    }

    /// Spec: *Help lists playlist keys* (and `?`).
    #[test]
    fn help_lists_playlist_keys() {
        let help = Cli::command().render_long_help().to_string();
        for (keys, action) in [
            ("Tab, Shift+Tab", "move focus"),
            ("a / A", "add the selected"),
            ("d", "remove the selected entry"),
            ("J / K", "move the selected entry down / up"),
            ("R", "rename the selected playlist"),
            ("D", "delete the selected playlist"),
            ("?", "show every key"),
        ] {
            let listed = help.lines().any(|line| {
                let line = line.trim_start();
                line.strip_prefix(keys)
                    .is_some_and(|rest| rest.starts_with("  ") && rest.contains(action))
            });
            assert!(listed, "--help does not list {keys:?} ({action})");
        }
    }

    #[test]
    fn parses_optional_dir() {
        let cli = Cli::try_parse_from(["kasset"]).map(|c| c.dir);
        assert!(matches!(cli, Ok(None)));
        let cli = Cli::try_parse_from(["kasset", "/music"]).map(|c| c.dir);
        assert!(matches!(cli, Ok(Some(p)) if p == Path::new("/music")));
    }

    #[test]
    fn existing_dir_is_canonicalised() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let nested = tmp.path().join("a");
        std::fs::create_dir(&nested)?;
        let root = resolve_root(Some(&nested.join("..").join("a")))?;
        assert_eq!(root, std::fs::canonicalize(&nested)?);
        Ok(())
    }

    #[test]
    fn missing_dir_error_names_folder() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let missing = tmp.path().join("no-such-dir");
        let err = resolve_root(Some(&missing)).map_err(|e| format!("{e:#}"));
        let Err(msg) = err else {
            bail!("expected an error")
        };
        assert!(msg.contains("no-such-dir"), "{msg}");
        assert!(msg.contains("does not exist"), "{msg}");
        Ok(())
    }

    #[test]
    fn file_is_rejected() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let file = tmp.path().join("song.mp3");
        std::fs::write(&file, b"")?;
        let err = resolve_root(Some(&file)).map_err(|e| format!("{e:#}"));
        let Err(msg) = err else {
            bail!("expected an error")
        };
        assert!(msg.contains("is not a folder"), "{msg}");
        Ok(())
    }
}
