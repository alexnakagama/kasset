# Proposal

## Why

There is no lightweight, keyboard-driven way in this project to play music already downloaded to a local folder. `kasset` will be a fast terminal (TUI) music player written in Rust, so a user can type `kasset` in any terminal and browse and play their local library without leaving it.

## What Changes

- New Rust binary crate named `kasset`; installing it (`cargo install --path .`) provides the `kasset` command.
- CLI entry: `kasset [DIR]` opens the player on `DIR`; with no argument it uses the user's music directory (`$XDG_MUSIC_DIR` / `~/Music`), falling back to the current directory.
- Recursive scan of the music folder for supported audio files (mp3, flac, ogg/vorbis, wav, m4a/aac), reading title/artist/album/duration tags with filename fallback.
- Audio playback: play, pause/resume, stop, next/previous track, seek forward/backward, volume up/down, automatic advance to the next track when one ends.
- Terminal UI: track list with keyboard navigation, highlighted current track, "now playing" panel with metadata, progress bar with elapsed/total time, volume indicator, and a help/keybinding bar.
- Playback modes: shuffle and repeat (off / all / one).
- Simple filtering: `/` to search the track list by title/artist/album.

## Capabilities

### New Capabilities
- `music-library`: discovering audio files in a folder and reading their metadata.
- `audio-playback`: decoding and playing tracks, transport controls, volume, queue advance, shuffle/repeat.
- `player-tui`: the terminal interface, the `kasset` command-line entry, layout, keybindings, and search.

### Modified Capabilities
<!-- none: greenfield project -->

## Impact

- New project files: `Cargo.toml`, `src/` (main, cli, library, queue, player, app, ui modules).
- New dependencies: `ratatui` + `crossterm` (TUI), `rodio` with symphonia backends (audio), `lofty` (tags), `walkdir` (scan), `clap` (CLI), `dirs` (music dir), `rand` (shuffle), `anyhow` (errors).
- System dependency on Linux: ALSA dev headers (`libasound2-dev`) required to build `rodio`/`cpal`.
- Out of scope for this change: Opus support (not decodable by symphonia), playlists files (m3u), album-art rendering, streaming/online sources, config files, MPRIS/media keys, persistent state between runs.
