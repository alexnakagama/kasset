//! `kasset` — a keyboard-driven terminal music player for a local music folder.
//!
//! `main` wires the pieces together in order: resolve the canonical root,
//! check the terminal, open the audio device, scan the library, build the
//! [`App`], restore the saved session ([`App::restore`], from the per-user
//! state file unless `--no-state` or no state folder is known), run the UI,
//! then write the final session ([`finish`]) for every way the UI ended.
//! The exit status is decided by how the UI ended only: a failed final save
//! is reported on stderr but never changes it.

mod app;
mod bounded_read;
mod browse;
mod cli;
mod keys;
mod library;
mod lyrics;
mod panic_guard;
mod playback;
mod player;
mod playlist;
mod queue;
mod state;
mod terminal;
#[cfg(test)]
mod test_support;
mod text;
mod theme;
mod ui;
mod visualizer;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::app::App;
use crate::cli::Cli;
use crate::library::Library;
use crate::playback::Notice;
use crate::player::{Audio, Player};
use crate::playlist::FsStore;
use crate::state::store::{FsStateStore, NoState, StateStore, location};
use crate::terminal::Exit;
use crate::text::display_path;

fn main() -> ExitCode {
    // `parse` prints help/version/usage errors itself and exits with the right status.
    let cli = Cli::parse();
    terminal::install_panic_hook();
    match run(&cli) {
        Ok(Exit::Quit) => ExitCode::SUCCESS,
        // The shell convention for "terminated by signal N".
        Ok(Exit::Signal(signal)) => ExitCode::from(u8::try_from(128 + signal).unwrap_or(u8::MAX)),
        Err(err) => {
            eprintln!("kasset: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<Exit> {
    let root = cli::resolve_root(cli.dir.as_deref())?;
    // Checked before the scan so a misuse fails fast.
    terminal::ensure_terminal()?;
    // Fail before a potentially long scan when there is nothing to play on.
    let player = Player::open()?;
    let shown = display_path(&root);
    eprintln!("Scanning {shown}…");
    let library = library::scan(&root)?;
    let notice = scan_notice(&library);
    // Playlists live in `<root>/Playlists`; nothing is written there until
    // the user creates or edits a playlist.
    let store = Box::new(FsStore::new(&root));
    let mut app = App::new(library.tracks, player, store, shown, notice);
    // The state file lives in the per-user state folder, never in the
    // library; `root` is the one the tracks were scanned from, so the saved
    // track path is matched against exactly what the scan found.
    let state_file = location(dirs::state_dir(), dirs::data_local_dir());
    app.restore(make_store(cli.no_state, state_file, &root), &root);
    let result = terminal::run(&mut app);
    finish(&mut app, result)
}

/// The state store for this run: none with `--no-state` or when no state
/// folder is known (D8), else the file `location` for the library `root`.
fn make_store(no_state: bool, location: Option<PathBuf>, root: &Path) -> Box<dyn StateStore> {
    match location {
        Some(file) if !no_state => Box::new(FsStateStore::new(file, root)),
        _ => Box::new(NoState),
    }
}

/// Writes the final session (taken by [`App::shutdown`] when the UI ended)
/// for every outcome of [`terminal::run`], reports a failed save on stderr,
/// and returns `result` unchanged.
fn finish<A: Audio>(app: &mut App<A>, result: Result<Exit>) -> Result<Exit> {
    finish_to(app, result, &mut io::stderr().lock())
}

/// [`finish`], reporting to `err`.
fn finish_to<A: Audio>(
    app: &mut App<A>,
    result: Result<Exit>,
    err: &mut dyn Write,
) -> Result<Exit> {
    if let Some(message) = app.save_final() {
        // The store already shows the path through `display_path`; control
        // characters are still replaced here so nothing in the message can
        // drive the terminal. Not cut, unlike `text::sanitize`: the whole
        // path and reason are useful on stderr.
        let message: String = message
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        // Best effort: a closed stderr must neither panic (as `eprintln!`
        // would) nor change the exit status.
        let _ = writeln!(err, "kasset: {message}");
    }
    result
}

/// A message about what the scan had to leave out, if anything.
fn scan_notice(library: &Library) -> Option<Notice> {
    let skipped = match library.skipped {
        0 => None,
        1 => Some("1 entry skipped (unreadable, broken link or loop)".to_owned()),
        n => Some(format!(
            "{n} entries skipped (unreadable, broken link or loop)"
        )),
    };
    let truncated = library
        .truncated
        .then_some("scan stopped early: the folder has too many entries");
    match (skipped, truncated) {
        (None, None) => None,
        (Some(s), None) => Some(Notice::info(s)),
        (None, Some(t)) => Some(Notice::error(t)),
        (Some(s), Some(t)) => Some(Notice::error(format!("{t}; {s}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use anyhow::{Context as _, anyhow, bail};

    use super::*;
    use crate::state::store::{Loaded, Saved};
    use crate::state::{Resume, Session, Settings, parse};
    use crate::test_support::{SilentAudio, no_store, track};

    /// A store that counts saves, and fails them if `fail` is set.
    #[derive(Debug)]
    struct Recording {
        saves: Rc<Cell<usize>>,
        fail: bool,
    }

    impl StateStore for Recording {
        fn load(&mut self) -> Loaded {
            Loaded::default()
        }

        fn save(&mut self, _session: &Session, _keep: Option<&Resume>) -> Result<Saved, String> {
            self.saves.set(self.saves.get() + 1);
            if self.fail {
                Err("cannot save state to /state:\x1b[2J disk\nfull".to_owned())
            } else {
                Ok(Saved::Written)
            }
        }
    }

    /// An app restored from a [`Recording`] store and shut down, as
    /// `terminal::run` leaves it; returns it with the store's save counter.
    fn shut_down_app(fail: bool) -> (App<SilentAudio>, Rc<Cell<usize>>) {
        let saves = Rc::new(Cell::new(0));
        let store = Recording {
            saves: Rc::clone(&saves),
            fail,
        };
        let tracks = vec![track("Song", "Band", "LP")];
        let mut app = App::new(
            tracks,
            SilentAudio::default(),
            no_store(),
            "music".to_owned(),
            None,
        );
        app.restore(Box::new(store), Path::new("/music"));
        app.shutdown();
        (app, saves)
    }

    fn outcomes() -> [Result<Exit>; 3] {
        [
            Ok(Exit::Quit),
            Ok(Exit::Signal(15)),
            Err(anyhow!("cannot draw to the terminal")),
        ]
    }

    fn describe(result: &Result<Exit>) -> String {
        match result {
            Ok(exit) => format!("{exit:?}"),
            Err(err) => format!("error: {err:#}"),
        }
    }

    /// `finish` saves exactly once for every outcome and returns it unchanged.
    #[test]
    fn finish_saves_once_for_every_outcome() {
        for result in outcomes() {
            let expected = describe(&result);
            let (mut app, saves) = shut_down_app(false);
            let mut err = Vec::new();
            let returned = finish_to(&mut app, result, &mut err);
            assert_eq!(describe(&returned), expected);
            assert_eq!(saves.get(), 1, "{expected}");
            assert!(
                err.is_empty(),
                "{expected}: {}",
                String::from_utf8_lossy(&err)
            );
            // The final session is written once only.
            let again = finish_to(&mut app, Ok(Exit::Quit), &mut err);
            assert!(matches!(again, Ok(Exit::Quit)));
            assert_eq!(saves.get(), 1, "{expected}");
        }
    }

    /// A failed final save is reported, and the result is still unchanged.
    #[test]
    fn finish_reports_a_failed_save_and_keeps_the_result() {
        for result in outcomes() {
            let expected = describe(&result);
            let (mut app, saves) = shut_down_app(true);
            let mut err = Vec::new();
            let returned = finish_to(&mut app, result, &mut err);
            assert_eq!(describe(&returned), expected);
            assert_eq!(saves.get(), 1, "{expected}");
            assert_eq!(
                String::from_utf8_lossy(&err),
                "kasset: cannot save state to /state: [2J disk full\n"
            );
        }
    }

    /// A writer that always fails, like a closed stderr.
    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn finish_survives_a_closed_stderr() {
        let (mut app, saves) = shut_down_app(true);
        let returned = finish_to(&mut app, Ok(Exit::Signal(2)), &mut Closed);
        assert!(matches!(returned, Ok(Exit::Signal(2))));
        assert_eq!(saves.get(), 1);
    }

    /// Without `shutdown` (the UI never started) there is nothing to write.
    #[test]
    fn finish_without_shutdown_saves_nothing() {
        let saves = Rc::new(Cell::new(0));
        let store = Recording {
            saves: Rc::clone(&saves),
            fail: true,
        };
        let mut app = App::new(
            vec![track("Song", "Band", "LP")],
            SilentAudio::default(),
            no_store(),
            "music".to_owned(),
            None,
        );
        app.restore(Box::new(store), Path::new("/music"));
        let mut err = Vec::new();
        let returned = finish_to(&mut app, Err(anyhow!("no terminal")), &mut err);
        assert_eq!(describe(&returned), "error: no terminal");
        assert_eq!(saves.get(), 0);
        assert_eq!(String::from_utf8_lossy(&err), "");
    }

    /// A session with non-default settings, so a save has something to say.
    fn session() -> Session {
        Session {
            settings: Settings {
                shuffle: true,
                ..Settings::default()
            },
            ..Session::default()
        }
    }

    /// Spec: *Private session* and *No state folder known*.
    #[test]
    fn make_store_without_state_writes_nothing() -> Result<()> {
        let state = tempfile::tempdir()?;
        let root = tempfile::tempdir()?;
        let file = state.path().join("kasset").join("state");
        for mut store in [
            make_store(true, Some(file.clone()), root.path()),
            make_store(false, None, root.path()),
        ] {
            assert!(!store.persists());
            assert_eq!(store.load(), Loaded::default());
            let saved = store.save(&session(), None).map_err(|e| anyhow!(e))?;
            assert_eq!(saved, Saved::Skipped);
        }
        assert_eq!(fs_entries(state.path())?, Vec::<PathBuf>::new());
        assert_eq!(fs_entries(root.path())?, Vec::<PathBuf>::new());
        Ok(())
    }

    #[test]
    fn make_store_with_a_location_writes_that_file() -> Result<()> {
        let state = tempfile::tempdir()?;
        let root = tempfile::tempdir()?;
        let file = state.path().join("kasset").join("state");
        let mut store = make_store(false, Some(file.clone()), root.path());
        assert!(store.persists());
        let saved = store.save(&session(), None).map_err(|e| anyhow!(e))?;
        assert_eq!(saved, Saved::Written);
        let bytes = std::fs::read(&file).context("the state file")?;
        let crate::state::Parsed::Valid(parsed) = parse(&bytes) else {
            bail!("the written file does not parse")
        };
        assert_eq!(parsed.settings, Some(session().settings));
        Ok(())
    }

    /// Spec: *State kept outside the music folder*.
    #[test]
    fn saving_leaves_the_music_folder_unchanged() -> Result<()> {
        let state = tempfile::tempdir()?;
        let root = tempfile::tempdir()?;
        std::fs::create_dir_all(root.path().join("Band").join("LP"))?;
        std::fs::write(
            root.path().join("Band").join("LP").join("01 Song.wav"),
            b"x",
        )?;
        let before = fs_entries(root.path())?;
        let mut store = make_store(
            false,
            Some(state.path().join("kasset").join("state")),
            root.path(),
        );
        store.save(&session(), None).map_err(|e| anyhow!(e))?;
        assert!(state.path().join("kasset").join("state").is_file());
        assert_eq!(fs_entries(root.path())?, before);
        Ok(())
    }

    /// Every path under `dir`, recursively, sorted.
    fn fs_entries(dir: &Path) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(dir).min_depth(1).sort_by_file_name() {
            out.push(entry?.path().to_path_buf());
        }
        Ok(out)
    }
}
