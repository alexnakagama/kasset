//! `kasset` — a keyboard-driven terminal music player for a local music folder.

mod cli;
mod library;
mod text;

use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use crate::cli::Cli;
use crate::text::display_path;

fn main() -> ExitCode {
    // `parse` prints help/version/usage errors itself and exits with the right status.
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("kasset: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    let root = cli::resolve_root(cli.dir.as_deref())?;
    let shown = display_path(&root);
    eprintln!("Scanning {shown}…");
    let library = library::scan(&root)?;
    if library.skipped > 0 {
        let noun = if library.skipped == 1 {
            "entry"
        } else {
            "entries"
        };
        eprintln!(
            "kasset: {} {noun} skipped (unreadable, broken link or loop)",
            library.skipped
        );
    }
    if library.truncated {
        eprintln!("kasset: scan stopped early; the folder has too many entries");
    }
    let tracks = library.tracks;

    // Interim output until the TUI lands (task group 4).
    if tracks.is_empty() {
        println!("No music found in {shown}");
    } else {
        for t in &tracks {
            let no = t.number.map(|n| format!("{n:>2}. ")).unwrap_or_default();
            println!("{} — {} — {no}{}", t.artist, t.album, t.title);
        }
        println!("{} tracks found in {shown}", tracks.len());
    }
    Ok(())
}
