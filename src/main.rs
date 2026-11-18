//! `kasset` — a keyboard-driven terminal music player for a local music folder.

mod app;
mod cli;
mod keys;
mod library;
mod panic_guard;
mod playback;
mod player;
mod playlist;
mod queue;
mod terminal;
#[cfg(test)]
mod test_support;
mod text;
mod theme;
mod ui;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::app::App;
use crate::cli::Cli;
use crate::library::Library;
use crate::playback::Notice;
use crate::player::Player;
use crate::playlist::FsStore;
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
    terminal::run(&mut app)
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
