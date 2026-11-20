# Proposal

## Why

Files without tags show artist and album as "Unknown", and they get a raw file name as their title. This is the common case for MP3s converted from YouTube downloads: ffmpeg leaves an ID3 tag that holds only `TXXX` frames (`major_brand`, `minor_version`, …). Every such track lands in one "Unknown" group, and its title keeps noise like "(Official Audio)". The file name and the folders usually carry the missing information (`Kendrick Lamar - dodger blue (Official Audio).mp3`, `Artist/Album/01 Song.flac`), so kasset should use them.

## What Changes

- When a track has no artist tag, kasset infers the artist from the file name. The file stem is split at the first spaced dash (` - `, ` – ` or ` — `), and the left side becomes the artist.
- When a track has no title tag, the title comes from the file name as it does today. It is now the part after the artist (if one was split off), with common video suffixes removed: bracketed groups such as `(Official Audio)`, `[Official Music Video]`, `(Official Lyric Video)`, `(Lyrics)`, `(Visualizer)`, and the same words at the end without brackets (`… Official Lyric Video`). The cleaned title is never left empty.
- A leading track number in the file name (`01 - Song`, `01. Song`, `01 Song`) is used as the track number when there is no track-number tag. It is never taken as an artist.
- When a track has no album or artist tag and sits at least two folders below the library root (`<root>/Artist/Album/file`), the album is inferred from the parent folder and the artist from the folder above it. For artist, a name found in the file name wins over the folder.
- Tags always win: inference only fills fields whose tag is missing or empty. Fields that can't be inferred still show "Unknown".
- Inferred text goes through `text::sanitize` like tag text, so it is shown, sorted, searched and written into new playlist `#EXTINF` lines exactly like tag values.

## Capabilities

### New Capabilities

_None._

### Modified Capabilities

- `music-library`: the "Track metadata" requirement changes. Missing artist, album, title and track number are inferred from the file name and folders before falling back to "Unknown" or the bare file name.

## Impact

- `src/library.rs`: `read_track` and `scan` (the folder rule needs the library root); new pure inference helpers, probably as a `library/infer.rs` submodule with unit tests.
- `src/player.rs` tests call `read_track`; they keep working whatever its final signature is.
- Library sort order and grouping change for untagged files, because they now get artist and album values.
- `README.md` feature list ("file name as fallback") and `openspec/specs/music-library/spec.md`.
- No new dependencies. No changes to files on disk (kasset never writes tags).
