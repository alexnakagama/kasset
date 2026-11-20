# Design

## Context

See `proposal.md` for the motivation and `specs/music-library/spec.md` for the rules.

The parts of the current code that shape the approach:

- `library::read_track(path)` reads tags through `read_tagged` and fills each field from the first tag that has it (`tag_text`). If there is none, the title falls back to `title_from_path` (the sanitised stem) and artist/album fall back to `UNKNOWN`. It is called by `scan` (via `discover`'s paths, which all lie under `root`) and by tests in `player.rs` and `library.rs`.
- `text::sanitize` replaces control characters with spaces, collapses whitespace, trims, caps at `MAX_TEXT_CHARS` (256), and returns `None` for blank text.
- `sort_tracks` sorts by the stored strings, so inferred values sort like tag values without changes. Search and playlist `#EXTINF` lines also read the stored `Track` fields.

## Goals / Non-Goals

**Goals:**
- Inference is a pure function of a relative path (no I/O), unit-testable without files.
- Tags always win, field by field.
- Conservative: when in doubt, leave the field missing (it becomes "Unknown" or the bare stem, as today).

**Non-Goals:**
- Writing inferred values back into the files.
- Showing which fields were inferred.
- Configurable patterns, per-folder rules, or online lookups (MusicBrainz).
- Splitting "feat."/"ft." guests into the artist.

## Decisions

### D1. A pure `library/infer.rs` module

```
pub struct Inferred { artist: Option<String>, album: Option<String>,
                      title: Option<String>, number: Option<u32> }
pub fn infer(relative: &Path) -> Inferred
```

`relative` is the track's path relative to the library root, or just the file name when no root is known. Every returned string has already gone through `text::sanitize`, so `None` means missing. *Alternative:* inline the logic in `read_track`. Rejected: the rules have many edge cases, and a pure function keeps them easy to test, like `lyrics.rs`.

### D2. Parsing the stem: sanitise first, then split

The stem (`file_stem`, lossy UTF-8) is sanitised first, so whitespace is collapsed and a separator is always exactly `" - "`, `" – "` or `" — "`. Steps:
1. **Leading number.** Match `^\d{1,3}` followed by a spaced dash (`" - "`, `" – "`, `" — "`), `". "`, or a single space, and with a non-empty rest. That gives `number` (a value of 0 gives `None`, like a zero tag; the prefix is still removed), and the stem continues from the rest. A bare space counts only when the digit run starts with `0` (`01 Song`); otherwise `50 Cent - In Da Club`, `2 Chainz - Song` would lose their artist's first word (artists "Cent", "Chainz") and `99 Problems`, `7 Rings` would lose part of their title. The cost: `10 Song` keeps "10 Song" as its title with no number; `10 - Song` and `10. Song` still give 10. A 4-digit prefix (`1999 - Song`) is not a track number; it is left to the artist rule, which is the less surprising result for year-named folders and files. A stem that is only digits (`01`) has no rest, so it gives no number and no title (the caller falls back to the stem).
2. **Artist split.** Find the first separator. If both sides are non-empty after trimming, left = artist and right = title, else the whole stem is the title.
3. **Label stripping** (D3) on the title.
4. Each part is re-sanitised (trim; empty → `None`).

All hand-written string scanning; no `regex` dependency.

### D3. Label stripping

The vocabulary is a `const` slice: `official, music, lyric, lyrics, audio, video, visualizer, visualiser, hd, hq, 4k`. Words are compared ASCII case-insensitively after splitting on spaces.
- **Bracketed groups**: scan for `(`…`)` and `[`…`]` (no nesting). A group is removed, together with one space next to it, when it has at least one word and every word is in the vocabulary. This works anywhere in the title, so `All I Want Is You (Official Video) ft. J. Cole` becomes `All I Want Is You ft. J. Cole`. A group inside an unclosed bracket of the other kind (`Song (Live [HD])`) is kept, so no `Song (Live )` is left.
- **Trailing run**: walk words from the end while they are in the vocabulary. The run is cut from its first `official` on; a run without `official` is kept. This protects titles that end in "Music" or "Video" (`Born to the Music` stays, `Born to the Music Official Video` becomes `Born to the Music`).
- After either removal, runs of separators (`-`, `–`, `—`, `|`) left standing alone at either end of the title (a space or the end on their inner side) are trimmed with the spaces around them, so `Song - Official Video` becomes `Song`, not `Song -`, and `[HD] - Song` becomes `Song`, while `-1 (Official Video)` keeps `-1`.
- If the result is empty, keep the original title.

The scan is linear and the input is capped at 256 characters, so it can't stall.

### D4. Folder rule needs the root: `read_track_in(root, path)`

`scan` calls a new `read_track_in(root: &Path, path: PathBuf)` (owned, like `read_track(path: PathBuf)`, because the `Track` keeps the path). It computes `path.strip_prefix(root)` and passes the relative path (borrowed) to `infer`; if `strip_prefix` fails, it uses the file name only. The existing `read_track(path)` stays as a thin wrapper that infers from the file name alone (no folders), so `player.rs` and existing tests don't change. Folder components use `Path::components()` on the relative path and only `Normal` parts count, so this works on Windows too.

Folder rule: the parent components, with the last one dropped if it is a disc folder, must number at least two; then `album` = the last one and the folder `artist` = the one before. A disc folder is matched by hand, ASCII case-insensitively: `cd`, `disc` or `disk`, an optional single ` `, `-` or `_`, then 1–2 ASCII digits and nothing else (`CD1`, `Disc 2`, `disk-02`). Only the immediate parent is checked, so `<lib>/CD1/Band/LP/x` still uses `Band`/`LP`. The disc number is not used (no disc field in `Track`). *Alternative:* treat a single folder as the album. Rejected: a single folder is just as often `Downloads` or an artist name, and guessing wrong is worse than "Unknown".

### D5. Merge order in `read_track_in`

Per field, the first that is `Some` wins:
- title: tag → inferred title → `title_from_path` (still needed when the stem is only a label or a number, e.g. `01.mp3`)
- artist: tag → file-name artist → folder artist → `UNKNOWN`
- album: tag → folder album → `UNKNOWN`
- number: tag (> 0) → inferred number

`infer` returns file-name and folder artists in a single `artist` field, already merged (file name first), so `read_track_in` stays a simple chain.

## Risks / Trade-offs

- [A title with a spaced dash and no artist (`Intro - Live.mp3`) gets the artist "Intro"] → Accepted: only for untagged files, fixed by tagging. The spec only splits on spaced dashes, which keeps `Spider-Man` intact.
- [Library order changes for users with untagged files] → Expected and the point of the change; there is no stored state to migrate.
- [Other per-disc folder names (`Side A`, `Bonus`) are taken as the album] → Accepted; only the common `CD n`/`Disc n` forms are recognised.
- [Bigger cases, such as `Album (Deluxe Video Edition)`] → "edition" isn't in the vocabulary, so the group is kept; the vocabulary is deliberately small.

## Migration Plan

None. Inference runs on every scan; rolling back restores the old display.
